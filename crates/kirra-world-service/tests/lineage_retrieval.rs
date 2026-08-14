//! **Box 3f — lineage retrieval, against a real store.**
//!
//! `KIRRA-WM-EXPLAIN-TIER-001` attaches two constraints to Tier 3's lineage
//! contract, and this file exists to make both of them properties of the code
//! rather than of the prose:
//!
//! > * **Bounded and paginated, with truncation visible.**
//! > * **Historically correct.** Lineage for an answer true at *T* traverses the
//! >   evidence visible at *T*, not today's graph.
//!
//! # The load-bearing test, and why it is the same one twice
//!
//! `an_event_appended_after_the_pin_is_not_in_the_lineage` is 3f's version of
//! the test box 3h and `ask_as_of` each carry on their own axis: record
//! something *after* the coordinate, and check the earlier answer did not
//! change. The recurrence is the point — "resolve current state and label it
//! historical" is the failure this whole tier keeps almost making, and it looks
//! like ordinary code every time.
//!
//! The store-level rule has its own unit tests over synthetic events. These run
//! the whole path: real store, real append, real fold, real reference.
//!
//! # Two axes this file cannot pin, measured rather than assumed
//!
//! Both were found by running the mutation, not by reading the code, and both
//! are structural — the integration path masks them:
//!
//! **Ordering.** Deleting the sort from `select_lineage` reds nothing here.
//! `generation` is `world_events`' primary key, so SQLite hands rows back in
//! that order anyway and the unsorted rule accidentally agrees with the sorted
//! one on every store this file can build.
//!
//! **The subject filter.** Deleting it from the rule reds nothing here either,
//! because the SQL pre-filters by subject. So
//! `another_subjects_evidence_is_not_in_this_lineage` below genuinely tests the
//! *query*, and does not test the *rule* — which is worth knowing, since the two
//! are separately editable.
//!
//! Both axes are covered where they can be, and both mutations red there:
//! `lineage::tests::{events_come_back_oldest_first_regardless_of_input_order,
//! another_subjects_events_are_not_in_this_lineage}` feed the rule directly, and
//! `the_lineage_corpus_catches_{an_unordered_selection, ignoring_the_subject}`
//! render the divergence into the pinned digest.
//!
//! Stated plainly because the tempting conclusion — *"the integration tests
//! cover these"* — is false, and a later reader deleting the store-level tests as
//! redundant would remove the only coverage there is.

use kirra_world_service::answer_ref::QueryKind;
use kirra_world_service::freshness::FreshnessSource;
use kirra_world_service::lineage::{LineageRef, LineageResolution};
use kirra_world_service::query::{Lineage, QueryEngine};
use kirra_world_service::semantics::{RuleVersion, SemanticVersions};
use kirra_world_store::lineage::LineagePage;
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const SUBJECT: &str = "package_17";

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-lineage-{name}-{}-{n}.sqlite",
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

/// Append one event about `subject`, returning nothing — the generation is read
/// back from the fold, so a test never hard-codes a coordinate it did not
/// observe.
fn append(store: &mut WorldStore, tag: &str, subject: &str, status: ClaimStatus, offset: i64) {
    store
        .append(&NewEvent {
            event_id: &EventId::new(format!("ev-{tag}")).expect("event id"),
            observation_id: &ObservationId::new(format!("obs-{tag}")).expect("obs"),
            txn_time_ms: T0 + offset,
            valid_from_ms: T0 + offset,
            valid_to_ms: None,
            source: "warehouse-scanner",
            source_version: "1.0.0",
            writer_class: match status {
                ClaimStatus::Candidate => WriterClass::LlmCandidate,
                ClaimStatus::Confirmed => WriterClass::Sensor,
            },
            claim_status: status,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "mission",
            subject,
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

/// A store with `n` confirmed events about `SUBJECT`, folded.
fn store_with(n: i64, name: &str) -> (WorldStore, std::path::PathBuf, i64) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    for i in 1..=n {
        append(
            &mut store,
            &format!("{i}"),
            SUBJECT,
            ClaimStatus::Confirmed,
            i,
        );
    }
    let generation = store.fold().expect("fold");
    (store, path, generation)
}

fn resolve(store: &WorldStore, r: &LineageRef) -> LineageResolution {
    // Through the sanctioned surface — box 3d. `LineageRef::resolve` is
    // `pub(crate)` now, so this helper could not call it directly even if it
    // wanted to: the engine is the only route, and that is enforced by the
    // compiler rather than by this comment.
    QueryEngine::new(store, FreshnessSource::Ruled)
        .execute(Lineage {
            reference: r.clone(),
        })
        .expect("lineage resolves")
}

fn generations(res: &LineageResolution) -> Vec<i64> {
    res.resolved()
        .unwrap_or_else(|| panic!("expected a resolved page, got {res:?}"))
        .entries()
        .iter()
        .map(|e| e.generation())
        .collect()
}

// ---------------------------------------------------------------------------
// Historical correctness — the load-bearing half
// ---------------------------------------------------------------------------

/// **THE BOX.** Evidence recorded after the pin must not enter the earlier
/// answer.
///
/// The transaction-time and generation axes have each had this test; this is the
/// lineage family's. A reference resolved before and after a later append must
/// return the identical page — not a longer one.
#[test]
fn an_event_appended_after_the_pin_is_not_in_the_lineage() {
    let (mut store, path, generation) = store_with(3, "historical");
    let reference = LineageRef::subject_lineage(SUBJECT, generation, LineagePage::first());

    let before = generations(&resolve(&store, &reference));
    assert_eq!(before.len(), 3, "the fixture must have three events");

    // The world moves on: more evidence about the same subject, folded.
    append(&mut store, "late", SUBJECT, ClaimStatus::Confirmed, 500);
    let later_generation = store.fold().expect("fold");
    assert!(
        later_generation > generation,
        "the append must advance the coordinate, or this proves nothing"
    );

    let after = generations(&resolve(&store, &reference));
    assert_eq!(
        after, before,
        "a recorded lineage reference changed meaning because new evidence \
         arrived — the pinned coordinate was ignored and today's log was served \
         under yesterday's reference"
    );

    // The positive control: the SAME query at the NEW coordinate does see it.
    // Without this, a resolver that returned nothing at all would pass above.
    let now = LineageRef::subject_lineage(SUBJECT, later_generation, LineagePage::first());
    assert_eq!(
        generations(&resolve(&store, &now)).len(),
        4,
        "the later coordinate must include the later evidence, or the test \
         above is passing because lineage is broken rather than because it is \
         pinned"
    );

    drop(store);
    cleanup(&path);
}

/// A coordinate the log has not reached refuses rather than serving what exists.
#[test]
fn a_coordinate_beyond_the_log_refuses() {
    let (store, path, generation) = store_with(2, "unreached");
    let reference = LineageRef::subject_lineage(SUBJECT, generation + 1_000, LineagePage::first());
    assert!(
        matches!(
            resolve(&store, &reference),
            LineageResolution::Irreproducible(_)
        ),
        "a coordinate that does not exist yet must refuse, not fall back to the \
         head"
    );
    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// Bounded, paginated, truncation visible
// ---------------------------------------------------------------------------

/// A truncated page says so, and names where to continue.
#[test]
fn a_truncated_page_is_visibly_truncated() {
    let (store, path, generation) = store_with(5, "truncated");
    let reference = LineageRef::subject_lineage(
        SUBJECT,
        generation,
        LineagePage::new(2, None).expect("valid page"),
    );
    let res = resolve(&store, &reference);
    let page = res.resolved().expect("resolved");
    assert_eq!(page.entries().len(), 2);
    assert!(
        page.is_truncated(),
        "a page that stopped short must say so — a lineage response that \
         silently stops is worse than one that says it stopped"
    );
    assert!(page.boundary().next_after_generation().is_some());
    drop(store);
    cleanup(&path);
}

/// **Paginating to exhaustion through the REFERENCE visits everything once.**
///
/// The cursor is minted from the resolution rather than computed by the caller,
/// so this also pins that `next_page` is usable without the caller knowing how
/// generations are numbered — which they are not, contiguously.
#[test]
fn paginating_through_references_visits_every_event_exactly_once() {
    let (store, path, generation) = store_with(7, "paginate");
    let mut reference = LineageRef::subject_lineage(
        SUBJECT,
        generation,
        LineagePage::new(3, None).expect("valid page"),
    );

    let mut seen: Vec<i64> = Vec::new();
    let mut pages = 0;
    loop {
        let res = resolve(&store, &reference);
        let page = res.resolved().expect("resolved");
        seen.extend(page.entries().iter().map(|e| e.generation()));
        pages += 1;
        assert!(pages < 20, "pagination did not terminate");
        match reference.next_page(page) {
            Some(next) => reference = next,
            None => break,
        }
    }

    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        seen, sorted,
        "pagination repeated or reordered events across page boundaries"
    );
    assert_eq!(seen.len(), 7, "pagination lost events");

    drop(store);
    cleanup(&path);
}

/// The last page offers no successor, so a caller cannot paginate past the end.
///
/// # The bound is EXACTLY the event count, deliberately
///
/// A first draft asked for `LineagePage::first()` (limit 256) over two events,
/// which is a page that could not have been full — so it passed against a rule
/// that reported `More` on every merely-full page. Measured: the eager-`More`
/// mutation survived this test and was caught only at the store level.
///
/// Sizing the page to exactly the lineage's length puts the off-by-one under
/// this test as well, which is where a reader looking for it would expect to
/// find it.
#[test]
fn the_final_page_yields_no_next_reference() {
    let (store, path, generation) = store_with(2, "final");
    let reference = LineageRef::subject_lineage(
        SUBJECT,
        generation,
        LineagePage::new(2, None).expect("valid page"),
    );
    let res = resolve(&store, &reference);
    let page = res.resolved().expect("resolved");
    assert_eq!(page.entries().len(), 2, "the page must be exactly full");
    assert!(
        !page.is_truncated(),
        "a page that is exactly full and IS the whole lineage must not report a \
         successor — a caller paginating to exhaustion would make a wasted round \
         trip on every lineage whose length divides the page size, and would be \
         told there is more when there is not"
    );
    assert!(
        reference.next_page(page).is_none(),
        "a complete page must not mint a successor reference"
    );
    drop(store);
    cleanup(&path);
}

/// An oversized page is refused, not quietly cut down.
#[test]
fn an_oversized_page_bound_is_refused_rather_than_clamped() {
    assert!(
        LineagePage::new(kirra_world_store::lineage::MAX_LINEAGE_PAGE + 1, None).is_err(),
        "a clamp would answer a smaller question and report it as the one asked"
    );
}

// ---------------------------------------------------------------------------
// What lineage includes, and what a reference refuses
// ---------------------------------------------------------------------------

/// Unconfirmed proposals are in the lineage, though never in an answer.
///
/// The distinction 3f rests on: `WorldView::ask` serves facts, lineage explains
/// them, and *"an LLM proposed this and nobody confirmed it"* is an explanation.
#[test]
fn an_unconfirmed_candidate_is_lineage_though_it_is_never_an_answer() {
    let path = tmp("candidate");
    let mut store = WorldStore::open(&path).expect("open");
    append(&mut store, "c1", SUBJECT, ClaimStatus::Confirmed, 1);
    append(&mut store, "c2", SUBJECT, ClaimStatus::Candidate, 2);
    let generation = store.fold().expect("fold");

    let reference = LineageRef::subject_lineage(SUBJECT, generation, LineagePage::first());
    let res = resolve(&store, &reference);
    let page = res.resolved().expect("resolved");
    assert_eq!(page.entries().len(), 2);
    assert!(
        page.entries()
            .iter()
            .any(|e| e.claim_status() == ClaimStatus::Candidate
                && e.writer_class() == WriterClass::LlmCandidate),
        "the LLM's proposal must be visible in the record of what was proposed"
    );

    drop(store);
    cleanup(&path);
}

/// Another subject's evidence is not this subject's lineage.
#[test]
fn another_subjects_evidence_is_not_in_this_lineage() {
    let path = tmp("subject");
    let mut store = WorldStore::open(&path).expect("open");
    append(&mut store, "mine", SUBJECT, ClaimStatus::Confirmed, 1);
    append(&mut store, "theirs", "pallet_9", ClaimStatus::Confirmed, 2);
    let generation = store.fold().expect("fold");

    let reference = LineageRef::subject_lineage(SUBJECT, generation, LineagePage::first());
    let res = resolve(&store, &reference);
    let page = res.resolved().expect("resolved");
    assert_eq!(page.entries().len(), 1);
    assert_eq!(page.entries()[0].subject(), SUBJECT);

    drop(store);
    cleanup(&path);
}

/// Every entry carries a parseable provenance handle — rule 1, at this family.
#[test]
fn every_entry_is_citable() {
    let (store, path, generation) = store_with(3, "citable");
    let reference = LineageRef::subject_lineage(SUBJECT, generation, LineagePage::first());
    let res = resolve(&store, &reference);
    for entry in res.resolved().expect("resolved").entries() {
        assert_eq!(
            entry.provenance().as_str().len(),
            64,
            "an entry that cannot be cited is not lineage"
        );
        assert!(!entry.event_id().is_empty());
        assert!(!entry.observation_id().is_empty());
    }
    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// The version discipline
// ---------------------------------------------------------------------------

/// **A reference recorded under a different selection rule REFUSES.**
///
/// The one that makes the version set more than an annotation. A recorded page-2
/// cursor was minted by the old ordering, so replaying it under a new one
/// returns a set that is neither the old page 2 nor the new one — and looks
/// entirely ordinary.
#[test]
fn a_reference_recorded_under_another_selection_version_refuses() {
    let (store, path, generation) = store_with(3, "version");
    let reference = LineageRef::subject_lineage(SUBJECT, generation, LineagePage::first())
        .recorded_under(SemanticVersions::new([RuleVersion {
            rule: "lineage_selection".to_string(),
            version: 99,
        }]));

    match resolve(&store, &reference) {
        LineageResolution::VersionMismatch { differences } => {
            assert_eq!(differences.len(), 1);
            assert_eq!(differences[0].rule, "lineage_selection");
            assert_eq!(differences[0].recorded, Some(99));
        }
        other => panic!("a moved selection rule must refuse, got {other:?}"),
    }

    drop(store);
    cleanup(&path);
}

/// The version check runs **before** the store is touched.
///
/// Asserted through a coordinate that would otherwise refuse for a different
/// reason: if the store were consulted first, this would come back
/// `Irreproducible` rather than `VersionMismatch`. Ordering that could only be
/// read off the source is ordering nothing holds in place.
#[test]
fn the_version_check_precedes_the_coordinate_check() {
    let (store, path, generation) = store_with(1, "order");
    let reference = LineageRef::subject_lineage(SUBJECT, generation + 1_000, LineagePage::first())
        .recorded_under(SemanticVersions::new([RuleVersion {
            rule: "lineage_selection".to_string(),
            version: 99,
        }]));

    assert!(
        matches!(
            resolve(&store, &reference),
            LineageResolution::VersionMismatch { .. }
        ),
        "the coordinate was consulted before the versions — a page would have \
         been built under rules the reference never described"
    );

    drop(store);
    cleanup(&path);
}

/// A reference declares the family it belongs to, and stamps live versions.
#[test]
fn a_fresh_reference_carries_this_builds_semantics() {
    let reference = LineageRef::subject_lineage(SUBJECT, 1, LineagePage::first());
    assert_eq!(reference.kind(), QueryKind::SubjectLineage);
    assert_eq!(
        reference.semantics(),
        &SemanticVersions::for_query(QueryKind::SubjectLineage)
    );
}

/// Two references for the same query are equal; a different PAGE is a different
/// reference.
///
/// The pagination bound is part of a reference's identity — `KIRRA-WM-ANSWER-
/// IDENTITY-001` lists it among what a reference serializes — so two pages of
/// one lineage must not compare equal.
#[test]
fn the_page_bound_is_part_of_a_references_identity() {
    let a = LineageRef::subject_lineage(SUBJECT, 7, LineagePage::new(10, None).expect("valid"));
    let b = LineageRef::subject_lineage(SUBJECT, 7, LineagePage::new(10, None).expect("valid"));
    let second_page =
        LineageRef::subject_lineage(SUBJECT, 7, LineagePage::new(10, Some(3)).expect("valid"));
    let smaller =
        LineageRef::subject_lineage(SUBJECT, 7, LineagePage::new(5, None).expect("valid"));

    assert_eq!(a, b, "the same query must produce the same reference");
    assert_ne!(a, second_page, "a different page is a different reference");
    assert_ne!(a, smaller, "a different bound is a different reference");
}

// ---------------------------------------------------------------------------
// Completeness (box 3g's obligation, inherited)
// ---------------------------------------------------------------------------

/// An uncompacted store reports `Full`, and truncation is NOT degradation.
///
/// The two are independent and it is easy to conflate them: a page cut short by
/// the caller's own limit is complete evidence, bounded. A page missing a
/// compacted span is evidence that no longer exists. Reporting the first as
/// `Degraded` would cry wolf on every paginated read.
#[test]
fn a_truncated_page_of_intact_evidence_is_not_degraded() {
    let (store, path, generation) = store_with(5, "notdegraded");
    let reference = LineageRef::subject_lineage(
        SUBJECT,
        generation,
        LineagePage::new(2, None).expect("valid page"),
    );
    let res = resolve(&store, &reference);
    let page = res.resolved().expect("resolved");
    assert!(page.is_truncated(), "the fixture must truncate");
    assert!(
        !page.is_degraded(),
        "a page bounded by the caller's own limit is complete evidence, and \
         reporting it as degraded would make the signal worthless"
    );
    drop(store);
    cleanup(&path);
}

/// **Compaction DEGRADES a lineage page rather than refusing it.**
///
/// The split this family makes from the pinned projection read, and the
/// positive control for the `Degraded` arm — without it, `is_degraded()` could
/// be permanently `false` and every other test here would still pass.
///
/// A folded projection over a log with holes is silently *wrong*, so
/// `read_at_generation` refuses one. Lineage is the evidence itself, so a page
/// missing a compacted span is *incomplete* — and the citations name exactly
/// which generations went and under which digest. Refusing would throw away a
/// usable answer and tell an investigator nothing.
#[test]
fn a_compacted_span_degrades_the_page_instead_of_refusing_it() {
    let (mut store, path, _) = store_with(3, "compacted");
    // Generation 2 is superseded by 3, so it is compactable; the live head is
    // structurally protected.
    let outcome = store
        .compact_range(2, 2, T0 + 9_000)
        .expect("a superseded generation is compactable");
    assert_eq!(outcome.removed, 1, "the fixture must remove evidence");
    let generation = store.fold().expect("fold");

    let reference = LineageRef::subject_lineage(SUBJECT, generation, LineagePage::first());
    let res = resolve(&store, &reference);
    let page = res
        .resolved()
        .unwrap_or_else(|| panic!("compaction must DEGRADE, not refuse: {res:?}"));

    assert!(
        page.is_degraded(),
        "evidence was deleted at or below this coordinate and the page did not \
         say so — a lineage that under-reports its own gaps is the one thing an \
         incident reconstruction cannot tolerate"
    );
    assert!(
        !page.completeness().spans().is_empty(),
        "a degraded page must name the spans it lost; `Degraded` with nothing \
         to trace it to is the fail-open version of admitting the loss"
    );
    assert!(
        !page.is_truncated(),
        "the fixture's surviving evidence fits one page, so this is degradation \
         WITHOUT truncation — the two axes must be independently observable"
    );

    drop(store);
    cleanup(&path);
}

/// Every resolved page states the rules it was selected under.
#[test]
fn a_resolved_page_carries_its_semantics() {
    let (store, path, generation) = store_with(1, "semantics");
    let reference = LineageRef::subject_lineage(SUBJECT, generation, LineagePage::first());
    let res = resolve(&store, &reference);
    assert_eq!(
        res.resolved().expect("resolved").semantics(),
        &SemanticVersions::for_query(QueryKind::SubjectLineage)
    );
    drop(store);
    cleanup(&path);
}
