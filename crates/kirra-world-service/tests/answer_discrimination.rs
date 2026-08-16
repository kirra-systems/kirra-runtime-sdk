//! **Tier 3 closing condition, cases 7 and 8 — the answer law's discriminations.**
//!
//! §6's minimum proving set ends with the two cases it calls out as fragile:
//!
//! > 7. contradicted identity → `Refused` with its reason preserved through the
//! >    envelope — the case Tier 2 spent three slices establishing, and the one
//! >    an envelope most easily flattens into an empty answer.
//! > 8. discrimination: `Unknown` ≠ `Refused` ≠ empty-but-`Full` — three
//! >    distinct facts that are one keystroke from collapsing into each other.
//!
//! # Two refusals, not one
//!
//! The boundary has two refusal channels, and they mean fundamentally different
//! things:
//!
//! | Channel | Meaning |
//! |---|---|
//! | [`AskError`] | the query could not be EVALUATED |
//! | [`ObjectIdentity::Refused`] | the query evaluated, and deterministically refused the identity |
//!
//! Collapsing either into `Unknown` breaks the answer law: `Unknown` is a
//! SUCCESS meaning *nothing is known*, and both refusals are statements that
//! something IS known and cannot be served. Collapsing the identity refusal into
//! the error channel is the subtler mistake — it would discard a perfectly good
//! answer because one of its objects has a contradictory history.
//!
//! # Why case 7 was the gap
//!
//! An audit of the eight cases found `ObjectIdentity::Refused` in no test in
//! this crate. Its SIBLING was covered end-to-end —
//! `as_of_composition::a_split_before_the_cut_reports_ambiguous_with_its_successors`
//! drives a real split through the engine — so the machinery for identity
//! outcomes reaching the boundary was proven, and the contradiction path
//! specifically was not.
//!
//! It is reachable, and `kirra_world::resolution` says exactly how:
//! `MergeEntities` refuses a merge into one of its own sources, so no single
//! event can build a cycle — but *a* merged into *b* and, later, *b* merged into
//! *a* are two individually valid events, and neither can see the other. The
//! contradiction exists only in the accumulated graph, which is what the fixture
//! below builds.

use kirra_world::adjudication::{
    AssertIdentity, IdentityAdjudication, Justification, MergeEntities, SplitEntity,
};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world::resolution::RefusalReason;
use kirra_world_service::freshness::{FreshnessPolicy, FreshnessSource};
use kirra_world_service::query::{Ask, QueryEngine};
use kirra_world_service::read_view::{AskError, ComposedLookup, ObjectIdentity, WorldLookup};
use kirra_world_store::adjudication_record::AdjudicationRow;
use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const SUBJECT: &str = "package_17";

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-disc-{name}-{}-{n}.sqlite",
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

fn eid(s: &str) -> EntityId {
    EntityId::new(s).expect("entity id")
}

fn just() -> Justification {
    Justification::new([ObservationId::new("obs-j").expect("obs")]).expect("justification")
}

fn at() -> DomainInstant {
    DomainInstant {
        ms: 1,
        domain: ClockDomain::System,
    }
}

fn adjudicate(store: &mut WorldStore, tag: &str, at_ms: i64, a: &IdentityAdjudication) {
    let event_id = EventId::new(format!("ev-{tag}")).expect("event id");
    let observation_id = ObservationId::new(format!("obs-{tag}")).expect("obs");
    store
        .append_adjudication(
            &AdjudicationRow {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms: at_ms,
                valid_from_ms: at_ms,
                source: "operator-console",
                source_version: "1.0.0",
                writer_class: WriterClass::Operator,
            },
            a,
        )
        .expect("append adjudication");
}

fn claim_pointing_at(store: &mut WorldStore, tag: &str, object: &str, at_ms: i64) {
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
            subject: SUBJECT,
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

fn assert_entity(store: &mut WorldStore, tag: &str, id: &str, at_ms: i64) {
    adjudicate(
        store,
        tag,
        at_ms,
        &IdentityAdjudication::Assert(AssertIdentity::new(eid(id), just(), at())),
    );
}

fn engine(store: &WorldStore) -> QueryEngine<'_> {
    QueryEngine::new(store, FreshnessSource::Caller(FreshnessPolicy::Timeless))
}

fn ask(store: &WorldStore, subject: &str) -> Result<ComposedLookup, AskError> {
    engine(store).execute(Ask {
        subject: subject.to_owned(),
        now_ms: T0 + 10_000,
    })
}

/// The identity of the one claim this fixture family records.
fn sole_identity(lookup: &WorldLookup) -> ObjectIdentity {
    match lookup {
        WorldLookup::Answered(answers) => {
            assert_eq!(answers.len(), 1, "fixture holds one claim for the subject");
            answers[0].object_identity().clone()
        }
        other => panic!("expected an answer, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Fixtures — one per outcome the boundary must keep distinct
// ---------------------------------------------------------------------------

/// `a` merged into `b`, then `b` merged into `a`.
///
/// Neither event can see the other, and `MergeEntities` accepts both: the
/// contradiction exists only once the graph accumulates them. The claim points
/// at `dock_a`, so resolving it walks straight into the cycle.
fn store_with_a_contradicted_identity(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    assert_entity(&mut store, "assert-a", "dock_a", T0);
    assert_entity(&mut store, "assert-b", "dock_b", T0 + 1);
    claim_pointing_at(&mut store, "claim", "dock_a", T0 + 2);
    adjudicate(
        &mut store,
        "merge-a-into-b",
        T0 + 3,
        &IdentityAdjudication::Merge(
            MergeEntities::new(vec![eid("dock_a")], eid("dock_b"), just(), at()).expect("a into b"),
        ),
    );
    adjudicate(
        &mut store,
        "merge-b-into-a",
        T0 + 4,
        &IdentityAdjudication::Merge(
            MergeEntities::new(vec![eid("dock_b")], eid("dock_a"), just(), at()).expect("b into a"),
        ),
    );
    store.fold().expect("fold");
    store.fold_entity_projection().expect("fold entities");
    (store, path)
}

/// `dock_a` partitioned into two successors that never reconverge.
fn store_with_an_ambiguous_identity(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    assert_entity(&mut store, "assert-a", "dock_a", T0);
    claim_pointing_at(&mut store, "claim", "dock_a", T0 + 1);
    adjudicate(
        &mut store,
        "split",
        T0 + 2,
        &IdentityAdjudication::Split(
            SplitEntity::partition(eid("dock_a"), [eid("dock_x"), eid("dock_y")], just(), at())
                .expect("partition"),
        ),
    );
    store.fold().expect("fold");
    store.fold_entity_projection().expect("fold entities");
    (store, path)
}

/// One claim, one asserted entity, nothing contradictory.
fn store_with_a_plain_answer(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    assert_entity(&mut store, "assert-a", "dock_a", T0);
    claim_pointing_at(&mut store, "claim", "dock_a", T0 + 1);
    store.fold().expect("fold");
    store.fold_entity_projection().expect("fold entities");
    (store, path)
}

/// A plain store whose stored provenance handle has been corrupted.
///
/// The ERROR channel: the query cannot be evaluated at all, because an answer
/// whose handle will not parse cannot be cited.
fn store_that_cannot_be_read(name: &str) -> (WorldStore, std::path::PathBuf) {
    let (store, path) = store_with_a_plain_answer(name);
    store
        .raw_execute_for_test("UPDATE world_current SET chain_digest = ''")
        .expect("plant the corruption");
    (store, path)
}

// ---------------------------------------------------------------------------
// Case 7 — contradicted identity is REFUSED, with its reason
// ---------------------------------------------------------------------------

/// **A contradicted identity surfaces as `Refused`, carrying WHY.**
///
/// Closing-condition case 7. The reason is asserted in full rather than by
/// variant: `RedirectCycle { at }` names the entity the walk was standing on
/// when it closed the loop, and an operator repairing the graph needs that id.
/// A test matching only the variant would pass against an implementation that
/// reported every contradiction at a fixed id.
#[test]
fn a_contradicted_identity_is_refused_with_its_reason_preserved() {
    let (store, path) = store_with_a_contradicted_identity("refused");

    let answered = ask(&store, SUBJECT).expect("the QUERY evaluates; only the identity is refused");

    assert_eq!(
        sole_identity(answered.lookup()),
        ObjectIdentity::Refused(RefusalReason::RedirectCycle { at: eid("dock_a") }),
        "a contradicted identity must be refused with the reason intact, \
         through the engine and out of the envelope"
    );

    drop(store);
    cleanup(&path);
}

/// **The refusal is caused by the contradiction, not by the fixture.**
///
/// The same graph WITHOUT the second merge resolves cleanly. Without this, a
/// fixture that never resolved anything — a mis-seeded claim, an unfolded
/// projection — would produce the refusal above and prove nothing.
#[test]
fn the_same_fixture_without_the_second_merge_resolves() {
    let path = tmp("refused-control");
    let mut store = WorldStore::open(&path).expect("open");
    assert_entity(&mut store, "assert-a", "dock_a", T0);
    assert_entity(&mut store, "assert-b", "dock_b", T0 + 1);
    claim_pointing_at(&mut store, "claim", "dock_a", T0 + 2);
    adjudicate(
        &mut store,
        "merge-a-into-b",
        T0 + 3,
        &IdentityAdjudication::Merge(
            MergeEntities::new(vec![eid("dock_a")], eid("dock_b"), just(), at()).expect("a into b"),
        ),
    );
    store.fold().expect("fold");
    store.fold_entity_projection().expect("fold entities");

    let answered = ask(&store, SUBJECT).expect("ask");
    assert_eq!(
        sole_identity(answered.lookup()),
        ObjectIdentity::Resolved {
            entity: "dock_b".to_string(),
            hops: 1,
        },
        "one merge is not a contradiction — it redirects"
    );

    drop(store);
    cleanup(&path);
}

/// **A refused identity is not `matchable`.**
///
/// The consumer-facing half, mirroring the same property for `Ambiguous`. A
/// contradicted history is precisely when a consumer must NOT compare the
/// object against its candidates, and `matchable` is what a consumer consults.
#[test]
fn a_refused_identity_is_not_matchable() {
    let (store, path) = store_with_a_contradicted_identity("not-matchable");
    let answered = ask(&store, SUBJECT).expect("ask");
    let identity = sole_identity(answered.lookup());

    assert!(
        identity.matchable(Some("dock_a")).is_none(),
        "a contradicted identity must fail closed for a consumer, not fall back \
         to the raw stored object"
    );

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// Case 8 — the four outcomes are pairwise distinguishable
// ---------------------------------------------------------------------------

/// What a caller can observe at the public surface.
///
/// Deliberately derived from the PUBLIC API only — `Result`, [`WorldLookup`],
/// [`ObjectIdentity`] — so a collapse anywhere between the store and the
/// envelope shows up as two fixtures classifying the same.
#[derive(Debug, PartialEq, Eq)]
enum Observed {
    /// The query could not be evaluated.
    Error,
    /// Evaluated; nothing is known about the subject.
    Unknown,
    /// Evaluated; the identity is contradictory and was refused.
    RefusedIdentity,
    /// Evaluated; the identity is plural.
    AmbiguousIdentity,
    /// Evaluated; the identity resolved.
    ResolvedIdentity,
}

fn observe(outcome: Result<ComposedLookup, AskError>) -> Observed {
    let Ok(composed) = outcome else {
        return Observed::Error;
    };
    match composed.lookup() {
        WorldLookup::Unknown(_) => Observed::Unknown,
        WorldLookup::Answered(answers) => match answers[0].object_identity() {
            ObjectIdentity::Refused(_) => Observed::RefusedIdentity,
            ObjectIdentity::Ambiguous { .. } => Observed::AmbiguousIdentity,
            _ => Observed::ResolvedIdentity,
        },
    }
}

/// **`Error`, `Unknown`, `Refused`, `Ambiguous` and a plain answer are five
/// distinct observations.**
///
/// Closing-condition case 8, widened to the four outcomes the audit found are
/// genuinely separate facts, plus a resolved answer so the classifier is not
/// vacuous — a discrimination test over failure modes alone would pass against
/// an API that could never succeed.
///
/// The pairwise assertion is the point rather than the individual arms. Each
/// arm is proven elsewhere; what nothing proved before is that no two of them
/// arrive at a caller looking the same.
#[test]
fn every_outcome_is_distinguishable_from_every_other() {
    let (unknown_store, p1) = store_with_a_plain_answer("disc-unknown");
    let (refused_store, p2) = store_with_a_contradicted_identity("disc-refused");
    let (ambiguous_store, p3) = store_with_an_ambiguous_identity("disc-ambiguous");
    let (resolved_store, p4) = store_with_a_plain_answer("disc-resolved");
    let (broken_store, p5) = store_that_cannot_be_read("disc-error");

    let observations = [
        ("error", observe(ask(&broken_store, SUBJECT))),
        // A subject nothing was ever claimed about.
        ("unknown", observe(ask(&unknown_store, "package_99"))),
        ("refused", observe(ask(&refused_store, SUBJECT))),
        ("ambiguous", observe(ask(&ambiguous_store, SUBJECT))),
        ("resolved", observe(ask(&resolved_store, SUBJECT))),
    ];

    // Each fixture lands on the outcome it was built for. Asserting only
    // distinctness would pass if two fixtures swapped.
    assert_eq!(observations[0].1, Observed::Error);
    assert_eq!(observations[1].1, Observed::Unknown);
    assert_eq!(observations[2].1, Observed::RefusedIdentity);
    assert_eq!(observations[3].1, Observed::AmbiguousIdentity);
    assert_eq!(observations[4].1, Observed::ResolvedIdentity);

    // …and no two are the same, stated as the pairwise property the closing
    // condition asks for rather than inferred from the five equalities above.
    for (i, (name_a, a)) in observations.iter().enumerate() {
        for (name_b, b) in observations.iter().skip(i + 1) {
            assert_ne!(
                a, b,
                "{name_a} and {name_b} are different facts and must not arrive \
                 at a caller looking the same"
            );
        }
    }

    drop(unknown_store);
    drop(refused_store);
    drop(ambiguous_store);
    drop(resolved_store);
    drop(broken_store);
    for p in [p1, p2, p3, p4, p5] {
        cleanup(&p);
    }
}

/// **An identity refusal is NOT an error.**
///
/// The subtler of the two collapses. A boundary that treated a contradictory
/// object as a query fault would discard an otherwise good answer — the claim,
/// its validity, its trust axes and its provenance are all intact and citable;
/// exactly one of its objects cannot be resolved.
///
/// Stated separately from the discrimination above because it fixes the
/// DIRECTION: that test would still pass if the identity refusal were an error
/// and the error channel were something else again.
#[test]
fn an_identity_refusal_is_not_reported_as_a_query_error() {
    let (store, path) = store_with_a_contradicted_identity("not-an-error");

    let outcome = ask(&store, SUBJECT);
    assert!(
        outcome.is_ok(),
        "the query evaluated; refusing one object's identity must not fail the \
         whole query — got {:?}",
        outcome.err()
    );

    let composed = outcome.expect("checked above");
    let WorldLookup::Answered(answers) = composed.lookup() else {
        panic!("a refused identity must not collapse the answer into Unknown");
    };
    // The rest of the envelope survived intact — this is an ANSWER with one
    // unresolvable object, not a damaged one.
    assert_eq!(answers[0].subject(), SUBJECT);
    assert!(
        !answers[0].provenance().as_str().is_empty(),
        "the answer is still citable; only its object's identity is refused"
    );

    drop(store);
    cleanup(&path);
}

/// **A query error is not an answer, and not `Unknown`.**
///
/// The other direction, and the one `read_view` already pins for the corrupt
/// handle. Repeated here so both directions sit beside the discrimination they
/// belong to: the closing condition is about the SET of outcomes staying
/// separate, and half of it proven in another file is half of it.
#[test]
fn a_query_error_is_neither_an_answer_nor_unknown() {
    let (store, path) = store_that_cannot_be_read("error-not-unknown");

    let outcome = ask(&store, SUBJECT);
    assert!(
        outcome.is_err(),
        "an unreadable provenance handle must not be served as an answer"
    );
    assert!(
        !matches!(&outcome, Ok(c) if matches!(c.lookup(), WorldLookup::Unknown(_))),
        "damage must not be reported as absence of knowledge"
    );

    drop(store);
    cleanup(&path);
}
