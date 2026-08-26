//! **`KIRRA-WM-EXPLAIN-FRESHNESS-DISCHARGE-001` — the two controls the ruling
//! needed that the existing suites do not carry.**
//!
//! The ruling:
//!
//! > **`Explain` and `Freshness` are DISCHARGED — as an operation and as a
//! > classifier respectively — not outstanding queries.**
//!
//! Most of the argument for it is already under test elsewhere, and this file
//! deliberately does not restate that. `explain_current_subject`'s bounds come
//! from this crate's constants rather than from the data
//! (`the_walk_is_bounded_by_this_crates_constants_not_by_the_data`) and it is a
//! pure function of the store (`two_calls_against_an_unchanged_store_are_
//! identical`); the ruled freshness table is exercised across both dispositions
//! and refuses what it has not ruled (`tests/freshness_policy.rs`, eleven cases).
//!
//! Two things were NOT covered, and they are the two the ruling turns on.
//!
//! # 1. The existing explain bound test would survive the change that matters
//!
//! `the_walk_is_bounded_by_this_crates_constants_not_by_the_data` proves the
//! DATA cannot widen the walk. It would keep passing if the operation grew a
//! caller-supplied `depth` parameter and the test passed a small value — and a
//! caller-supplied bound is exactly what turns an operation back into a query
//! surface. The module docs give the reason it must not: *"there is no argument
//! a caller could set to make the work larger, so there is nothing to abuse."*
//!
//! So the control here is on that axis rather than on the word *Explain*: the
//! signature admits a store and a subject NAME, and nothing else.
//!
//! # 2. Freshness is ruled in one place, and nothing checked the answer agrees
//!
//! The existing cases assert specific dispositions by hand — an old
//! `last_seen_at` is `Stale`, an equally old `colour` is `Timeless`. Every one
//! of those expectations is written out beside the row it tests, so the table
//! and the read path can drift apart while both halves of each test agree with
//! each other.
//!
//! [`the_validity_on_an_answer_agrees_with_the_ruled_table`] derives its
//! expectation from [`resolve_policy`] instead. That is the discharge argument
//! stated as a test: a `Freshness(FieldRef, At)` query would be a SECOND place
//! freshness is decided, and this fails the moment a second place disagrees
//! with the first.

use kirra_explain_types::ExplanationArtifact;
use kirra_world_service::explain_subject::{explain_current_subject, ExplainError};
use kirra_world_service::freshness::{
    resolve_policy, FreshnessPolicy, FreshnessSource, SemanticClass, RULED,
};
use kirra_world_service::query::{Ask, QueryEngine};
use kirra_world_service::read_view::WorldLookup;
use kirra_world_store::{
    ClaimStatus, EventId, NewEvent, ObservationId, Validity, WorldStore, WriterClass,
};

const T0: i64 = 1_700_000_000_000;
const SUBJECT: &str = "package_17";

/// Inside every `Bounded` row in the table. One millisecond, so the fixture
/// cannot pass by accident on a row whose bound is small.
const YOUNG: i64 = T0 + 1;
/// Past every `Bounded` row in the table.
const OLD: i64 = T0 + 60 * 60 * 1_000;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-discharge-{name}-{}-{n}.sqlite",
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

fn claim(store: &mut WorldStore, tag: &str, kind: &str, predicate: &str) {
    store
        .append(&NewEvent {
            event_id: &EventId::new(format!("ev-{tag}")).expect("event id"),
            observation_id: &ObservationId::new(format!("obs-{tag}")).expect("obs"),
            txn_time_ms: T0,
            valid_from_ms: T0,
            valid_to_ms: None,
            source: "warehouse-scanner",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind,
            subject: SUBJECT,
            predicate: Some(predicate),
            subject_ref: None,
            object: None,
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
}

// ---------------------------------------------------------------------------
// 1. Explain is an OPERATION: the caller supplies a name and nothing else
// ---------------------------------------------------------------------------

/// **The signature pin.**
///
/// A `fn` item coerces to a `fn` pointer only if the signature matches exactly,
/// so this stops compiling the moment `explain_current_subject` grows a
/// parameter — a depth, a page, a generation, a handle. Any of those would make
/// the work caller-settable, which is the property that distinguishes the
/// operation this ruling calls discharged from the query surface it declines to
/// become.
///
/// It is a `const` rather than a comment because a comment cannot fail.
const EXPLAIN_TAKES_A_STORE_AND_A_SUBJECT_NAME: fn(
    &WorldStore,
    &str,
) -> Result<ExplanationArtifact, ExplainError> = explain_current_subject;

/// The pin is exercised, not merely declared.
///
/// A `const` fn pointer nobody calls proves the signature typechecks and
/// nothing about the operation being reachable through it. This calls the
/// operation THROUGH the pinned pointer, so the pin is on the live route.
#[test]
fn the_explain_operation_is_reachable_through_the_pinned_signature() {
    let path = tmp("explain-pin");
    let store = WorldStore::open(&path).expect("open");

    // An empty store: the interesting part is the CALL, and `NothingRecorded`
    // is the honest empty this operation is documented to return.
    match EXPLAIN_TAKES_A_STORE_AND_A_SUBJECT_NAME(&store, SUBJECT) {
        Err(ExplainError::NothingRecorded) => {}
        other => panic!("expected NothingRecorded from an empty store, got {other:?}"),
    }

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// 2. Freshness is decided ONCE, and the answer carries that decision
// ---------------------------------------------------------------------------

/// What the ruled table SAYS an answer of this age should be.
///
/// The state machine from the `freshness` module docs, written once here and
/// applied to every row — as opposed to an expectation written out beside each
/// row, which is what lets a table and a read path drift while every test
/// agrees with itself.
fn expected(policy: FreshnessPolicy, age_ms: i64) -> Validity {
    match policy {
        FreshnessPolicy::Timeless => Validity::Timeless,
        FreshnessPolicy::Bounded { max_age_ms } => {
            if age_ms <= i64::try_from(max_age_ms).expect("bound fits an i64") {
                Validity::Fresh
            } else {
                Validity::Stale
            }
        }
    }
}

/// **A `Freshness` query would be a second place freshness is decided.**
///
/// This is that claim as a control: for every ruled class a sensor can write,
/// at a clock inside its bound and at a clock past it, the validity riding on
/// the ANSWER equals what [`resolve_policy`] independently returns.
///
/// It fails if the read path stops consulting the table, if the table stops
/// reaching the read path, and if a second classification appears anywhere and
/// disagrees with the first.
#[test]
fn the_validity_on_an_answer_agrees_with_the_ruled_table() {
    let path = tmp("agreement");
    let mut store = WorldStore::open(&path).expect("open");

    // EVERY ruled row, with no exception mechanism.
    //
    // A first draft had one — a named list of rows "a sensor claim cannot
    // produce", holding the adjudicated-identity row. Emptying that list was
    // the check on whether the exemption was real, and every test still passed:
    // the store does not constrain the `kind` string, so the row appends like
    // any other and was never uncovered. An exemption that exempts nothing, and
    // a guard comparing two lists that move together, are worse than no
    // scaffolding at all, so both were deleted rather than documented.
    //
    // What that leaves is honest about its own reach: the identity row is
    // written HERE as a sensor claim, which is not how production writes it
    // (`RecordMerge`, with an `AdjudicationAuthority`). This proves the read
    // path agrees with the TABLE for that class — the claim the ruling makes —
    // not that the production write path emits such a row.
    let ruled: Vec<SemanticClass> = RULED.iter().map(|(class, _)| *class).collect();

    for (n, class) in ruled.iter().enumerate() {
        claim(
            &mut store,
            &format!("row{n}"),
            class.kind,
            class
                .predicate
                .expect("every ruled row here has a predicate"),
        );
    }
    store.fold().expect("fold");

    // Non-vacuity, asserted rather than hoped for: the fixture must span both
    // dispositions AND both sides of a bound, or the loop below could pass
    // while `expected` collapsed to a constant.
    let mut seen: Vec<Validity> = Vec::new();
    let mut checked = 0_usize;

    for now_ms in [YOUNG, OLD] {
        let age = now_ms - T0;
        let composed = QueryEngine::new(&store, FreshnessSource::Ruled)
            .execute(Ask {
                subject: SUBJECT.to_owned(),
                now_ms,
            })
            .expect("every claim in the fixture is ruled, so the query answers");

        let WorldLookup::Answered(answers) = composed.lookup() else {
            panic!("the fixture must answer, got {:?}", composed.lookup());
        };

        for class in &ruled {
            let policy = resolve_policy(FreshnessSource::Ruled, class.kind, class.predicate)
                .expect("the class came from RULED, so it resolves");
            let answer = answers
                .iter()
                .find(|a| a.predicate() == class.predicate)
                .unwrap_or_else(|| panic!("no answer for {:?}", class.predicate));

            let want = expected(policy, age);
            assert_eq!(
                answer.validity(),
                want,
                "at age {age}ms the answer for {}/{:?} carries {:?}, but the ruled \
                 table says {want:?}. The read path and the table disagree, which \
                 is the second-decision-point this ruling exists to keep out.",
                class.kind,
                class.predicate,
                answer.validity(),
            );
            seen.push(want);
            checked += 1;
        }
    }

    // Guards the one coverage failure the loop above cannot report itself: a
    // filter reintroduced on the CHECK loop, which silently shrinks what is
    // asserted. (A row that is iterated but never written fails earlier and
    // louder, at the `no answer for ...` lookup — that case is covered, just
    // not by this assertion.)
    assert_eq!(
        checked,
        RULED.len() * 2,
        "every ruled row must be checked at both clocks — {checked} checks for \
         {} rows means a row was not reached",
        RULED.len(),
    );

    for required in [Validity::Fresh, Validity::Stale, Validity::Timeless] {
        assert!(
            seen.contains(&required),
            "the fixture never produced {required:?}, so the agreement above \
             could hold with `expected` collapsed to a constant. Non-vacuity \
             requires all three."
        );
    }

    drop(store);
    cleanup(&path);
}
