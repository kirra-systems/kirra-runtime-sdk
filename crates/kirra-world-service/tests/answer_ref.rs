//! **The ruled `AnswerRef` — step 2 of stable snapshot → answer identity →
//! honest degradation.**
//!
//! `KIRRA-WM-ANSWERREF-NAMING-001` reserved the name for a descriptor that
//! re-executes against the same snapshot, and forbade putting it on a drift
//! detector. The name is taken now because
//! `ReadSnapshot::read_at_generation` exists.
//!
//! The acceptance set, in order:
//!
//! 1. the same ref resolves identically before compaction;
//! 2. a future generation refuses;
//! 3. compaction BELOW the pinned generation makes the ref irreproducible;
//! 4. compaction ABOVE it does not;
//! 5. a version mismatch refuses rather than replaying under new semantics;
//! 6. changing query parameters changes the ref;
//! 7. same query + same generation + same version produces the same ref;
//! 8. the "plausible wrong current answer" fall-forward remains caught.
//!
//! Cases 3 and 4 are the pair that make the reproducibility bound a decision:
//! the store-level suite was vacuous without the second, and a ref inherits that
//! bound wholesale, so it is re-asserted here at the level a caller sees.

use kirra_world_service::answer_ref::{current_semantics, AnswerRef, QueryKind, RefResolution};
use kirra_world_service::semantics::{RuleVersion, SemanticVersions};
use kirra_world_store::snapshot::Irreproducible;
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const LATER: i64 = T0 + 1_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-answer-ref-{name}-{}-{n}.sqlite",
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

fn claim(store: &mut WorldStore, tag: &str, object: &str, at_ms: i64) {
    store
        .append(&NewEvent {
            event_id: &EventId::new(format!("ev-{tag}")).expect("event id"),
            observation_id: &ObservationId::new(format!("obs-{tag}")).expect("obs"),
            txn_time_ms: at_ms,
            valid_from_ms: at_ms,
            valid_to_ms: None,
            source: "warehouse-scanner",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "mission",
            subject: "package_17",
            subject_ref: None,
            predicate: Some("last_seen_at"),
            object: Some(object),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
}

/// `dock_old` at generation 1, `dock_a` at 2 — an answer that CHANGED, so a ref
/// pinned at 1 can be told apart from one that fell forward.
fn store_that_changed_its_mind(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "1", "dock_old", T0);
    store.fold().expect("fold");
    claim(&mut store, "2", "dock_a", T0 + 1);
    store.fold().expect("fold");
    (store, path)
}

fn objects(res: &RefResolution) -> Vec<String> {
    res.resolved()
        .expect("resolved")
        .iter()
        .filter_map(|a| a.object().map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------------------
// 1, 7, 8 — identity and reproduction
// ---------------------------------------------------------------------------

/// **The same ref resolves identically, and to the PAST answer.**
///
/// Cases 1 and 8 together: resolving twice agrees, and what it agrees on is
/// `dock_old` — the answer at the pinned generation — not `dock_a`, which is
/// what a fall-forward would return and what the live read returns.
#[test]
fn the_same_ref_resolves_identically_and_to_the_pinned_answer() {
    let (store, path) = store_that_changed_its_mind("stable");
    let r = AnswerRef::current_subject("package_17", LATER, None, 1);

    let first = r.resolve(&store).expect("resolve");
    let second = r.resolve(&store).expect("resolve again");

    assert_eq!(objects(&first), vec!["dock_old".to_string()]);
    assert_eq!(
        objects(&first),
        objects(&second),
        "the same ref must re-execute to the same answer"
    );

    // Non-vacuity: the pinned answer really differs from the current one, so
    // "resolved correctly" is distinguishing something.
    let live = AnswerRef::current_subject("package_17", LATER, None, 2);
    assert_eq!(
        objects(&live.resolve(&store).expect("resolve")),
        vec!["dock_a"]
    );

    drop(store);
    cleanup(&path);
}

/// **Same query + same generation + same version produces the same ref.**
///
/// Ref identity is structural, so recording one and rebuilding it later must
/// yield an equal value — otherwise a stored ref could never be matched against
/// a fresh one.
#[test]
fn the_same_query_at_the_same_coordinate_produces_the_same_ref() {
    let a = AnswerRef::current_subject("package_17", LATER, Some(60_000), 7);
    let b = AnswerRef::current_subject("package_17", LATER, Some(60_000), 7);
    assert_eq!(a, b);

    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let hash = |r: &AnswerRef| {
        let mut h = DefaultHasher::new();
        r.hash(&mut h);
        h.finish()
    };
    assert_eq!(hash(&a), hash(&b), "equal refs must hash equally");

    assert_eq!(a.kind(), QueryKind::CurrentSubject);
    assert_eq!(*a.semantics(), current_semantics());
}

/// **Changing any query parameter changes the ref.**
///
/// Each field is varied one at a time. A ref that ignored a parameter would
/// describe a different query than the one that produced the answer, and would
/// re-execute to something else while comparing equal.
#[test]
fn changing_any_parameter_changes_the_ref() {
    let base = AnswerRef::current_subject("package_17", LATER, Some(60_000), 7);

    let variants = [
        (
            "subject",
            AnswerRef::current_subject("package_18", LATER, Some(60_000), 7),
        ),
        (
            "now_ms",
            AnswerRef::current_subject("package_17", LATER + 1, Some(60_000), 7),
        ),
        (
            "budget",
            AnswerRef::current_subject("package_17", LATER, Some(59_999), 7),
        ),
        (
            "budget-none",
            AnswerRef::current_subject("package_17", LATER, None, 7),
        ),
        (
            "generation",
            AnswerRef::current_subject("package_17", LATER, Some(60_000), 8),
        ),
        // One rule moving is enough. Varying the WHOLE set would also pass
        // while proving less: a ref must differ when any single dependency it
        // names is at a different version, not merely when all of them are.
        (
            "fold-version",
            AnswerRef::current_subject("package_17", LATER, Some(60_000), 7)
                .recorded_with("world_current_fold", 99),
        ),
        (
            "boundary-version",
            AnswerRef::current_subject("package_17", LATER, Some(60_000), 7)
                .recorded_with("answer_admissibility", 99),
        ),
        // A ref that named a dependency this build does not have is a different
        // ref, even though every SHARED rule agrees.
        (
            "extra-rule",
            AnswerRef::current_subject("package_17", LATER, Some(60_000), 7)
                .recorded_with("entity_fold", 1),
        ),
    ];

    for (field, other) in variants {
        assert_ne!(base, other, "changing {field} must change the ref");
    }
}

// ---------------------------------------------------------------------------
// 2, 3, 4 — the reproducibility horizon, at the ref level
// ---------------------------------------------------------------------------

/// **A future generation refuses.**
#[test]
fn a_future_generation_refuses() {
    let (store, path) = store_that_changed_its_mind("future");
    let r = AnswerRef::current_subject("package_17", LATER, None, 9_999);

    match r.resolve(&store).expect("resolve") {
        RefResolution::Irreproducible(Irreproducible::NotYetReached { .. }) => {}
        other => panic!("a future coordinate must refuse, got {other:?}"),
    }

    drop(store);
    cleanup(&path);
}

/// **Compaction BELOW the pinned generation makes the ref irreproducible.**
///
/// The horizon reaching a recorded ref, which is the failure
/// `KIRRA-WM-REPRODUCIBILITY-HORIZON-001` exists to keep visible. Note the ref
/// resolved fine a moment earlier — nothing about the ref changed, the world
/// under it did.
#[test]
fn compaction_below_the_pin_makes_the_ref_irreproducible() {
    let (mut store, path) = store_that_changed_its_mind("compacted");
    let r = AnswerRef::current_subject("package_17", LATER, None, 1);

    assert_eq!(
        objects(&r.resolve(&store).expect("resolve")),
        vec!["dock_old"]
    );

    store.compact_range(1, 1, T0 + 5_000).expect("compact");

    match r.resolve(&store).expect("resolve") {
        RefResolution::Irreproducible(Irreproducible::Compacted { spans }) => {
            assert!(!spans.is_empty(), "the refusal must name what was removed");
        }
        RefResolution::Resolved(a) => panic!(
            "fell forward: a ref past the horizon returned {:?}",
            a.iter().filter_map(|x| x.object()).collect::<Vec<_>>()
        ),
        other => panic!("expected a compaction refusal, got {other:?}"),
    }

    drop(store);
    cleanup(&path);
}

/// **Compaction ABOVE the pinned generation does not.**
///
/// The control without which case 3 is vacuous: an implementation refusing on
/// the mere existence of any citation would pass every other case here, and
/// would make recorded refs useless on exactly the aged, compacted stores they
/// are kept for.
#[test]
fn compaction_above_the_pin_leaves_the_ref_resolvable() {
    let path = tmp("above");
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "1", "dock_old", T0);
    claim(&mut store, "2", "dock_a", T0 + 1);
    claim(&mut store, "3", "dock_b", T0 + 2);
    store.fold().expect("fold");

    let r = AnswerRef::current_subject("package_17", LATER, None, 1);
    store.compact_range(2, 2, T0 + 5_000).expect("compact");

    assert_eq!(
        objects(&r.resolve(&store).expect("resolve")),
        vec!["dock_old"],
        "a span removed above the pin took none of the events the ref folds"
    );

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// 5 — semantics, not just coordinates
// ---------------------------------------------------------------------------

/// **A version mismatch refuses rather than replaying under new semantics.**
///
/// The subtler half of falling forward: the COORDINATE is honoured and the
/// RULES are silently swapped, so the answer looks right and describes a query
/// nobody asked. The ref pins the generation exactly, and the resolution would
/// succeed if the version were ignored — which is what makes this test sharp.
#[test]
fn a_version_mismatch_refuses_rather_than_replaying() {
    let (store, path) = store_that_changed_its_mind("version");

    let current = AnswerRef::current_subject("package_17", LATER, None, 1);
    assert!(
        current
            .resolve(&store)
            .expect("resolve")
            .resolved()
            .is_some(),
        "the fixture must resolve at the current version, or the refusal below \
         proves only that the coordinate was bad"
    );

    // ONE rule moving is enough, and the refusal must NAME it. A mismatch that
    // said only "the rules changed" would leave an operator holding a reference
    // they cannot act on.
    let stale = current.clone().recorded_with("world_current_fold", 0);
    match stale.resolve(&store).expect("resolve") {
        RefResolution::VersionMismatch { differences } => {
            assert_eq!(differences.len(), 1, "only one rule moved: {differences:?}");
            assert_eq!(differences[0].rule, "world_current_fold");
            assert_eq!(differences[0].recorded, Some(0));
            assert_eq!(
                differences[0].current,
                current_semantics().version_of("world_current_fold"),
            );
        }
        other => panic!("a stale fold version must refuse, got {other:?}"),
    }

    // The BOUNDARY rule is a separate dependency, and moving it alone must
    // refuse too — otherwise the set is decorative for every rule but one.
    match current
        .clone()
        .recorded_with("answer_admissibility", 99)
        .resolve(&store)
        .expect("resolve")
    {
        RefResolution::VersionMismatch { differences } => {
            assert_eq!(differences.len(), 1);
            assert_eq!(differences[0].rule, "answer_admissibility");
        }
        other => panic!("a moved boundary rule must refuse, got {other:?}"),
    }

    // A version from the FUTURE refuses too — a ref written by a newer build
    // describes semantics this one does not implement.
    match current
        .clone()
        .recorded_with("world_current_fold", u32::MAX)
        .resolve(&store)
        .expect("resolve")
    {
        RefResolution::VersionMismatch { .. } => {}
        other => panic!("an unknown future version must refuse, got {other:?}"),
    }

    // A ref naming a dependency this build does not have refuses, even though
    // every SHARED rule agrees. A query family that gained or lost a dependency
    // derives its answer from something else.
    match current
        .clone()
        .recorded_with("entity_fold", 1)
        .resolve(&store)
        .expect("resolve")
    {
        RefResolution::VersionMismatch { differences } => {
            assert_eq!(differences[0].rule, "entity_fold");
            assert_eq!(
                differences[0].current, None,
                "this build has no such dependency"
            );
        }
        other => panic!("an unknown dependency must refuse, got {other:?}"),
    }

    // And the refusal is decided BEFORE the store is touched: a mismatched ref
    // at an irreproducible coordinate reports the version, not the compaction.
    match AnswerRef::current_subject("package_17", LATER, None, 99_999)
        .recorded_with("world_current_fold", 0)
        .resolve(&store)
        .expect("resolve")
    {
        RefResolution::VersionMismatch { .. } => {}
        other => panic!("the version check must precede the read, got {other:?}"),
    }

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// The END-TO-END corpus pin, over a real store
// ---------------------------------------------------------------------------

/// **A ref's recorded versions are pinned to what a ref actually resolves to.**
///
/// The per-rule corpora in `kirra-world-store/tests/semantics_corpus.rs` and
/// `tests/boundary_semantics.rs` pin each rule in ISOLATION, over pure inputs.
/// This pins the composition over a REAL store, and the difference is not
/// ceremonial: the isolated corpora fold in-memory values, while a ref resolves
/// by replaying rows out of SQLite. Everything between — the event decode, the
/// confirmed-only filter, the generation cut — sits inside this test and outside
/// those. A change there moves what every ref answers while every per-rule
/// corpus stays green.
///
/// # It pins the ref's own output, and the first draft pinned the wrong thing
///
/// An earlier version digested `projection_state_digest()` — the LIVE
/// projection — and was insensitive to the rule the ref actually uses. The
/// reason is worth recording, because it is a real property of this store:
/// **supersession has two implementations.** The incremental fold does it in
/// SQL —
///
/// ```sql
/// WHERE (excluded.valid_from_ms, excluded.generation)
///     > (world_current.valid_from_ms, world_current.generation)
/// ```
///
/// — while `projection::supersedes` / `fold_step` is the pure reducer used by
/// `rebuild_projections` and by the pinned replay. The two are held equal by
/// `rebuild_from_zero_equals_incremental`; but a corpus digesting the live table
/// measured the SQL, so mutating `fold_step` left it green while changing what
/// every `AnswerRef` resolves to.
///
/// So this pins the resolved ANSWER, canonically rendered. That covers the fold
/// rule (through the replay) and the boundary's admissibility rule (through the
/// binding) — the two rules a `CurrentSubject` ref names — and it fails on a
/// change to either.
///
/// Rendered rather than hashed on purpose: a failure should show WHAT changed,
/// not merely that something did.
#[test]
fn a_refs_recorded_versions_are_pinned_to_what_it_resolves_to() {
    // This rendering belongs to the version set below. Move them together.
    const PINNED_ANSWER: &str = "package_17|last_seen_at|dock_second|{}|Timeless|Ungraded";

    // Spelled out rather than read from `current_semantics()`, which would make
    // the assertion tautological — it would agree with whatever the build
    // declares, including a version somebody bumped without touching a rule.
    let pinned = SemanticVersions::new([
        RuleVersion {
            rule: "answer_admissibility".into(),
            version: 1,
        },
        RuleVersion {
            rule: "world_current_fold".into(),
            version: 1,
        },
    ]);
    assert_eq!(
        current_semantics(),
        pinned,
        "the version set a ref records moved without re-pinning this rendering — \
         the two are a pair, and a version that moves alone tracks nothing"
    );

    let path = tmp("corpus");
    let mut store = WorldStore::open(&path).expect("open");

    // The corpus must exercise supersession in BOTH directions, or a mutated
    // fold can land on the same state and the pin proves nothing.
    //
    // ACCEPTED supersession — later valid time arriving later, must replace.
    claim(&mut store, "c1", "dock_first", T0);
    claim(&mut store, "c2", "dock_second", T0 + 10);
    // REJECTED supersession — earlier valid time arriving later, must not.
    claim(&mut store, "c3", "dock_backdated", T0 + 5);
    // A candidate, which the confirmed-only fold must ignore entirely.
    store
        .append(&NewEvent {
            event_id: &EventId::new("ev-cand").expect("id"),
            observation_id: &ObservationId::new("obs-cand").expect("obs"),
            txn_time_ms: T0 + 20,
            valid_from_ms: T0 + 20,
            valid_to_ms: None,
            source: "llm",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Candidate,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "mission",
            subject: "package_17",
            subject_ref: None,
            predicate: Some("last_seen_at"),
            object: Some("dock_hallucinated"),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append candidate");
    store.fold().expect("fold");

    let head = store
        .projection_coordinate()
        .expect("coordinate")
        .world_current()
        .generation();
    let resolved = AnswerRef::current_subject("package_17", T0 + 100, None, head)
        .resolve(&store)
        .expect("resolve");

    let rendered: Vec<String> = resolved
        .resolved()
        .expect("the corpus must resolve, or it is pinning a refusal")
        .iter()
        .map(|a| {
            format!(
                "{}|{}|{}|{}|{:?}|{:?}",
                a.subject(),
                a.predicate().unwrap_or(""),
                a.object().unwrap_or(""),
                a.value(),
                a.validity(),
                a.grade()
                    .map_or(FactGradeShim::Ungraded, |_| { FactGradeShim::Graded })
            )
        })
        .collect();

    assert_eq!(
        rendered.join("\n"),
        PINNED_ANSWER,
        "the semantics a ref resolves under changed. If that was deliberate, bump \
         the moved rule's version and re-pin this rendering in the same commit — \
         a recorded AnswerRef must not silently replay under the new rule."
    );

    drop(store);
    cleanup(&path);
}

/// A two-state stand-in for the grade, so the corpus rendering stays stable
/// against additions to the world's grade vocabulary while still moving if a
/// claim becomes graded when it was not.
#[derive(Debug)]
enum FactGradeShim {
    Graded,
    Ungraded,
}
