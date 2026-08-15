//! **Continuation cursors are opaque, query-bound, versioned, and fail closed.**
//!
//! Tier 3 §6 cross-cutting. The rule these pin:
//!
//! > A cursor names a continuation of ONE query contract under ONE
//! > semantic-version set, not merely a position in SQLite.
//!
//! # What was wrong, and why it needed more than a newtype
//!
//! Pagination shipped with the cursor as a bare `i64` generation, handed out by
//! `PageBoundary::More` and taken back by `LineagePage::after_generation`. Every
//! page of every family passed a raw log position across the domain boundary.
//!
//! Wrapping that integer in a struct would have satisfied the word "opaque" and
//! fixed nothing. The hazard was never that a caller could READ the coordinate;
//! it is that a cursor carried no evidence of what it continued, so a cursor
//! from another family, another rule version, or another store returned a page
//! rather than an error — right shape, right subject, plausible contents, wrong
//! question.
//!
//! # The decisive control
//!
//! `a_history_cursor_is_refused_by_lineage` takes a cursor minted by one family
//! and presents it to the other. Both carry the same kind of coordinate, and on
//! this fixture the same VALUE. If "opaque" meant only "an integer in a struct",
//! it would be accepted. It is refused, which is what makes the opacity a
//! capability rather than a spelling.
//!
//! # Every failure is a refusal
//!
//! No test here asserts a fallback, because there is no fallback to assert. A
//! reset to page 1 and a jump to the next surviving generation are both
//! available, both look like recovery, and both silently answer a different
//! question — re-serving rows already seen, or skipping the ones that vanished.

use kirra_world_service::answer_ref::QueryKind;
use kirra_world_service::cursor::{CursorError, CursorFamily, PageCursor};
use kirra_world_service::freshness::FreshnessSource;
use kirra_world_service::lineage::LineageRef;
use kirra_world_service::query::{History, Lineage, QueryEngine};
use kirra_world_service::read_view::AskError;
use kirra_world_service::semantics::{RuleVersion, SemanticVersions};
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const SUBJECT: &str = "package_17";

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-cursor-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    cleanup(&p);
    p
}

fn cleanup(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut q = path.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
}

fn append(store: &mut WorldStore, tag: &str, offset: i64) {
    store
        .append(&NewEvent {
            event_id: &EventId::new(format!("ev-{tag}")).expect("event id"),
            observation_id: &ObservationId::new(format!("obs-{tag}")).expect("obs"),
            txn_time_ms: T0 + offset,
            valid_from_ms: T0 + offset,
            valid_to_ms: None,
            source: "warehouse-scanner",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "mission",
            subject: SUBJECT,
            subject_ref: None,
            predicate: Some("last_seen_at"),
            object: Some("dock_b"),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
}

/// Four confirmed events about `SUBJECT`, folded. Returns the fold coordinate.
fn store_with_four(name: &str) -> (WorldStore, std::path::PathBuf, i64) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    for i in 1..=4 {
        append(&mut store, &format!("{i}"), i);
    }
    let generation = store.fold().expect("fold");
    (store, path, generation)
}

fn engine(store: &WorldStore) -> QueryEngine<'_> {
    QueryEngine::new(store, FreshnessSource::Ruled)
}

/// The cursor a first `History` page hands back.
fn history_cursor(store: &WorldStore) -> PageCursor {
    engine(store)
        .execute(History {
            subject: SUBJECT.to_owned(),
            limit: 2,
            after: None,
        })
        .expect("history")
        .continuation()
        .cursor()
        .expect("four events over a limit of two must continue")
        .clone()
}

/// The cursor a first `Lineage` page hands back.
fn lineage_cursor(store: &WorldStore, generation: i64) -> PageCursor {
    let reference = LineageRef::subject_lineage(SUBJECT, generation, 2);
    let resolution = engine(store)
        .execute(Lineage { reference })
        .expect("lineage");
    resolution
        .resolved()
        .expect("a resolved page")
        .continuation()
        .cursor()
        .expect("four events over a limit of two must continue")
        .clone()
}

fn cursor_error(e: AskError) -> CursorError {
    match e {
        AskError::Cursor(c) => c,
        other => panic!("expected a cursor refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Non-vacuity: the happy path must work, or every refusal below is trivial
// ---------------------------------------------------------------------------

/// **A cursor round-trips.** The control for every refusal in this file.
///
/// Without it, a `PageCursor` that refused EVERYTHING would pass the whole
/// suite. Every negative test below is only meaningful because this one shows
/// the positive path is reachable.
#[test]
fn a_minted_cursor_continues_its_own_query() {
    let (store, path, _g) = store_with_four("roundtrip");
    let cursor = history_cursor(&store);

    let second = engine(&store)
        .execute(History {
            subject: SUBJECT.to_owned(),
            limit: 2,
            after: Some(cursor),
        })
        .expect("a cursor minted by this family must continue it");

    // Four events, two per page: the second page completes the record.
    assert!(
        !second.continuation().is_truncated(),
        "the second page holds the rest, so nothing follows it"
    );
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// THE decisive control — opacity is capability, not wrapping
// ---------------------------------------------------------------------------

/// **A cursor differing ONLY in family is refused.**
///
/// The decisive control, and it took a mutation to get right. The obvious
/// version — mint from each family, swap them — passes for the wrong reason:
/// `SubjectHistory` and `SubjectLineage` happen to declare DIFFERENT rule sets
/// today, so a naive swap is caught by the version check and the family binding
/// is never exercised. Deleting the family check entirely still refused the
/// swap, with `SemanticsChanged` instead of `WrongFamily`.
///
/// So the cursor here is stamped with the TARGET family's live semantics before
/// being presented. Coordinate: identical. Versions: identical, and current.
/// Family: the only thing that differs. A newtype-only cursor would be accepted,
/// because nothing about its value would be wrong.
///
/// That the two families' version sets differ today is not a defence — it is a
/// coincidence of which rules each depends on, and a future family sharing a
/// rule set would collapse it. The family binding is what holds when it does.
#[test]
fn a_history_cursor_is_refused_by_lineage() {
    let (store, path, generation) = store_with_four("family-swap");

    let from_history = history_cursor(&store);
    let from_lineage = lineage_cursor(&store, generation);

    assert_eq!(
        from_history.family(),
        CursorFamily::History,
        "fixture: the history cursor must name its own family"
    );
    assert_eq!(from_lineage.family(), CursorFamily::Lineage);

    // The HISTORY cursor, re-stamped with LINEAGE's live semantics, presented
    // to LINEAGE. Every field the validator inspects now matches except one.
    let disguised = from_history
        .clone()
        .recorded_under(SemanticVersions::for_query(QueryKind::SubjectLineage));
    let reference = LineageRef::subject_lineage(SUBJECT, generation, 2).continuing_from(disguised);
    let err = cursor_error(
        engine(&store)
            .execute(Lineage { reference })
            .expect_err("a history cursor must not continue a lineage query"),
    );
    assert_eq!(
        err,
        CursorError::WrongFamily {
            presented: CursorFamily::History,
            expected: CursorFamily::Lineage,
        },
        "the ONLY difference was the family, so the family is what must refuse"
    );

    // And the mirror: the LINEAGE cursor, re-stamped with HISTORY's semantics.
    let disguised =
        from_lineage.recorded_under(SemanticVersions::for_query(QueryKind::SubjectHistory));
    let err = cursor_error(
        engine(&store)
            .execute(History {
                subject: SUBJECT.to_owned(),
                limit: 2,
                after: Some(disguised),
            })
            .expect_err("a lineage cursor must not continue a history query"),
    );
    assert_eq!(
        err,
        CursorError::WrongFamily {
            presented: CursorFamily::Lineage,
            expected: CursorFamily::History,
        }
    );
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// Semantic-version binding
// ---------------------------------------------------------------------------

/// **A cursor minted under different rules is refused.**
///
/// Continuing would splice pages from two query contracts into one sequence —
/// box 3b's failure, arrived at through pagination. The refusal carries WHICH
/// rules differ, so an operator sees the cause rather than only the effect.
#[test]
fn a_cursor_from_a_superseded_rule_set_is_refused() {
    let (store, path, _g) = store_with_four("stale-semantics");
    let cursor = history_cursor(&store);

    let stale = cursor.recorded_under(SemanticVersions::new([RuleVersion {
        rule: "world_current_fold".to_string(),
        version: 1,
    }]));

    let err = cursor_error(
        engine(&store)
            .execute(History {
                subject: SUBJECT.to_owned(),
                limit: 2,
                after: Some(stale),
            })
            .expect_err("a cursor from another rule set must not continue"),
    );
    match err {
        CursorError::SemanticsChanged { differences } => assert!(
            !differences.is_empty(),
            "a version refusal must name which rules moved"
        ),
        other => panic!("expected SemanticsChanged, got {other:?}"),
    }
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// Reproducibility — the compaction case
// ---------------------------------------------------------------------------

/// **A cursor whose coordinate compaction removed is refused, not advanced.**
///
/// The tempting alternative is to continue from the nearest surviving
/// generation. It returns a page, and it is wrong: the caller asked to continue
/// a sequence, and the position they named no longer exists, so what follows it
/// is undefined rather than merely later.
#[test]
fn a_cursor_whose_generation_was_compacted_is_refused() {
    let (mut store, path, _g) = store_with_four("compacted");
    let cursor = history_cursor(&store);

    // Remove the span the cursor's coordinate falls in. The cursor named the
    // last generation of page one, so compacting the early span takes it.
    store.compact_range(1, 2, T0 + 10_000).expect("compact");

    let err = cursor_error(
        engine(&store)
            .execute(History {
                subject: SUBJECT.to_owned(),
                limit: 2,
                after: Some(cursor),
            })
            .expect_err("a cursor naming a removed generation must not continue"),
    );
    assert!(
        matches!(err, CursorError::Unreproducible { .. }),
        "expected Unreproducible, got {err:?}"
    );
    cleanup(&path);
}

/// **The compaction refusal is not vacuous** — the same page succeeds intact.
///
/// Without this, a `compact_range` that failed, or a cursor that was already
/// invalid, would produce the same refusal above and prove nothing about
/// compaction.
#[test]
fn the_same_cursor_continues_before_the_compaction() {
    let (store, path, _g) = store_with_four("compaction-control");
    let cursor = history_cursor(&store);

    engine(&store)
        .execute(History {
            subject: SUBJECT.to_owned(),
            limit: 2,
            after: Some(cursor),
        })
        .expect("before compaction the identical cursor must continue");
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// Coordinates that cannot have come from this log
// ---------------------------------------------------------------------------

/// **A cursor from another store — one whose coordinate is past this head — is
/// refused.**
///
/// Built by minting against a LONGER log and presenting it to a shorter one,
/// which is what a cursor carried between environments looks like.
#[test]
fn a_cursor_past_this_logs_head_is_refused() {
    let (long_store, long_path, _g) = store_with_four("beyond-head-source");
    let cursor = history_cursor(&long_store);

    // A different, shorter store: two events, so the cursor's coordinate is
    // past its head.
    let short_path = tmp("beyond-head-target");
    let mut short = WorldStore::open(&short_path).expect("open");
    append(&mut short, "s1", 1);
    short.fold().expect("fold");

    let err = cursor_error(
        engine(&short)
            .execute(History {
                subject: SUBJECT.to_owned(),
                limit: 2,
                after: Some(cursor),
            })
            .expect_err("a coordinate past this log's head must not continue"),
    );
    assert!(
        matches!(err, CursorError::BeyondHead { .. }),
        "expected BeyondHead, got {err:?}"
    );
    cleanup(&long_path);
    cleanup(&short_path);
}

// ---------------------------------------------------------------------------
// No fallback, in the one place a fallback would be invisible
// ---------------------------------------------------------------------------

/// **A refused cursor returns NO page — not page 1.**
///
/// The failure mode this whole module exists to prevent. A reset to the first
/// page is the most natural-looking recovery there is, and a caller cannot
/// distinguish it from a legitimate continuation: same shape, same subject,
/// plausible contents, silently re-serving rows they have already processed.
///
/// Asserting on the error alone would not catch it — a fallback implementation
/// could return `Ok(page_one)`, and `expect_err` would be the only thing that
/// noticed.
#[test]
fn a_refused_cursor_yields_no_answer_at_all() {
    let (store, path, generation) = store_with_four("no-fallback");
    let lineage = lineage_cursor(&store, generation);

    let outcome = engine(&store).execute(History {
        subject: SUBJECT.to_owned(),
        limit: 2,
        after: Some(lineage),
    });

    assert!(
        outcome.is_err(),
        "a refused continuation must not serve a page; \
         resetting to page 1 would re-serve rows the caller already has, \
         and nothing in the answer would say so"
    );
    cleanup(&path);
}
