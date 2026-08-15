//! **Box 3h — historical queries use HISTORICAL identity, never today's graph.**
//!
//! The box in one sentence:
//!
//! > Historical queries use historical identity (2d) and historical evidence —
//! > never today's entity graph applied to old evidence.
//!
//! # Why the pairing is the dangerous part
//!
//! Pinning the *evidence* was box 3b's `read_at_generation`, and it worked. The
//! failure this box is about survives that fix completely: replay the claims at
//! generation `g`, then resolve the object they name against the identity graph
//! **as it is now**. Every claim is historically correct, the coordinate in the
//! answer is honest, and the object is silently wrong — because a merge recorded
//! last week says the thing that claim pointed at is now called something else.
//!
//! Nothing about that reads as a bug at the call site. It is what you get by
//! writing the obvious code, which is why the fix is a composed read
//! (`read_composed_at_generation`) rather than a rule about which functions to
//! call in which order.
//!
//! # The fixture, and why the merge lands where it does
//!
//! ```text
//! gen 1   assert dock_alpha            an entity exists
//! gen 2   claim  package_17 -> dock_alpha
//!         ---- PIN HERE ----
//! gen 3   assert dock_beta
//! gen 4   merge  dock_alpha -> dock_beta      the graph changes AFTER the pin
//! ```
//!
//! A ref pinned at generation 2 must resolve `dock_alpha` to **itself**. The
//! live read must resolve it to `dock_beta`. Both are correct answers to
//! different questions, and a composed read that used today's graph would give
//! the live answer to the historical question while every other field said
//! otherwise.
//!
//! The two arms differ in the OBJECT RESOLUTION, not in the claim: the stored
//! object string is `dock_alpha` in both. That is deliberate — a test where the
//! two arms returned different claims would pass on the evidence pin alone and
//! prove nothing about identity.

use kirra_world::adjudication::{
    AssertIdentity, IdentityAdjudication, Justification, MergeEntities,
};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world_service::answer_ref::{AnswerRef, RefResolution};
use kirra_world_service::freshness::{FreshnessPolicy, FreshnessSource};
use kirra_world_service::query::{Ask, QueryEngine, ReplayAnswer};
use kirra_world_service::read_view::{AskError, ObjectIdentity, WorldLookup};
use kirra_world_store::adjudication_record::AdjudicationRow;
use kirra_world_store::snapshot::PinnedComposedRead;
use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const LATER: i64 = T0 + 1_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-hist-comp-{name}-{}-{n}.sqlite",
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

/// The fixture in the module docs. Returns the store and the generation to pin
/// at — read from the store rather than assumed, so the test does not silently
/// pin the wrong coordinate if event numbering changes.
fn store_where_the_graph_moved_after_the_pin(name: &str) -> (WorldStore, std::path::PathBuf, i64) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");

    adjudicate(
        &mut store,
        "assert-alpha",
        T0,
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_alpha"), just(), at())),
    );
    claim_pointing_at(&mut store, "claim", "dock_alpha", T0 + 1);
    store.fold().expect("fold");
    store.fold_entity_projection().expect("fold entities");

    let pin = store
        .projection_coordinate()
        .expect("coordinate")
        .world_current()
        .generation();

    // Everything below happens AFTER the pin.
    adjudicate(
        &mut store,
        "assert-beta",
        T0 + 2,
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_beta"), just(), at())),
    );
    adjudicate(
        &mut store,
        "merge",
        T0 + 3,
        &IdentityAdjudication::Merge(
            MergeEntities::new(vec![eid("dock_alpha")], eid("dock_beta"), just(), at())
                .expect("merge"),
        ),
    );
    store.fold().expect("fold");
    // The LIVE arm reads the entity PROJECTION, so it must be folded. The
    // pinned arms deliberately do not need this: they replay adjudications
    // from the log themselves, which is why a pinned answer cannot be stale.
    store.fold_entity_projection().expect("fold entities");

    (store, path, pin)
}

/// Replay a recorded answer through the sanctioned surface — box 3d.
///
/// `AnswerRef::resolve` is `pub(crate)`, so a test cannot reach it directly any
/// more than a consumer can. The engine's freshness source is inert here on
/// purpose: a recorded reference carries its OWN staleness budget, which is the
/// contract that was in force when the answer was taken.
fn replay(store: &WorldStore, reference: AnswerRef) -> Result<RefResolution, AskError> {
    QueryEngine::new(store, FreshnessSource::Ruled).execute(ReplayAnswer { reference })
}

fn sole_identity(res: &RefResolution) -> ObjectIdentity {
    let answers = res.resolved().expect("the ref must resolve");
    assert_eq!(answers.len(), 1, "fixture holds one claim for the subject");
    answers[0].object_identity().clone()
}

// ---------------------------------------------------------------------------
// The property
// ---------------------------------------------------------------------------

/// **The live read follows the merge.** The control arm.
///
/// Without this the historical arm below proves nothing: if the graph never
/// resolved `dock_alpha` to `dock_beta` at all, the historical answer would be
/// "unmerged" for a reason that has nothing to do with the pin.
#[test]
fn the_live_read_resolves_the_object_through_the_merge() {
    let (store, path, _pin) = store_where_the_graph_moved_after_the_pin("live");
    let view = QueryEngine::new(&store, FreshnessSource::Caller(FreshnessPolicy::Timeless));

    let composed = view
        .execute(Ask {
            subject: "package_17".to_owned(),
            now_ms: LATER,
        })
        .expect("ask");
    let WorldLookup::Answered(answers) = composed.lookup() else {
        panic!("the fixture must answer, got {:?}", composed.lookup());
    };
    assert_eq!(answers.len(), 1);
    assert_eq!(
        *answers[0].object_identity(),
        ObjectIdentity::Resolved {
            entity: "dock_beta".to_string(),
            hops: 1,
        },
        "today's graph merges dock_alpha into dock_beta — if this fails the \
         fixture is not exercising identity at all"
    );

    drop(store);
    cleanup(&path);
}

/// **THE BOX.** A ref pinned before the merge resolves the object to itself.
///
/// This is the assertion 3h exists for. A composed read that reached for the
/// live graph would return `dock_beta` here — the same value the control arm
/// above returns — and every other field of the answer would still be correct.
#[test]
fn a_ref_pinned_before_the_merge_resolves_identity_as_it_stood_then() {
    let (store, path, pin) = store_where_the_graph_moved_after_the_pin("pinned");

    let resolved = replay(
        &store,
        AnswerRef::current_subject("package_17", LATER, None, pin),
    )
    .expect("resolve");

    assert_eq!(
        sole_identity(&resolved),
        ObjectIdentity::Resolved {
            entity: "dock_alpha".to_string(),
            hops: 0,
        },
        "the merge was recorded AFTER the pinned generation, so it must not \
         reach an answer pinned before it — this is today's entity graph \
         applied to old evidence, which is precisely what 3h forbids"
    );

    drop(store);
    cleanup(&path);
}

/// The two arms disagree, and that disagreement is the whole result.
///
/// Asserted as a pair rather than left implicit across two tests: if a future
/// change made both arms return the same identity, each test above could still
/// be edited into passing separately, while the property they exist to
/// establish had quietly died.
#[test]
fn the_historical_and_live_identities_genuinely_differ() {
    let (store, path, pin) = store_where_the_graph_moved_after_the_pin("differ");

    let historical = sole_identity(
        &replay(
            &store,
            AnswerRef::current_subject("package_17", LATER, None, pin),
        )
        .expect("resolve"),
    );

    let view = QueryEngine::new(&store, FreshnessSource::Caller(FreshnessPolicy::Timeless));
    let composed = view
        .execute(Ask {
            subject: "package_17".to_owned(),
            now_ms: LATER,
        })
        .expect("ask");
    let WorldLookup::Answered(live) = composed.lookup() else {
        panic!("the fixture must answer");
    };

    assert_ne!(
        historical,
        *live[0].object_identity(),
        "the historical and live reads returned the SAME object identity, so \
         this suite cannot tell a composed pinned read from a live one"
    );

    drop(store);
    cleanup(&path);
}

/// A ref pinned AFTER the merge does follow it.
///
/// The pin is a coordinate, not a preference for old answers: pin later and the
/// merge is inside the replay, so it applies. Without this the suite would be
/// satisfied by an implementation that never resolved identity at all.
#[test]
fn a_ref_pinned_after_the_merge_does_follow_it() {
    let (store, path, _pin) = store_where_the_graph_moved_after_the_pin("after");

    let head = store
        .projection_coordinate()
        .expect("coordinate")
        .world_current()
        .generation();

    let resolved = replay(
        &store,
        AnswerRef::current_subject("package_17", LATER, None, head),
    )
    .expect("resolve");

    assert_eq!(
        sole_identity(&resolved),
        ObjectIdentity::Resolved {
            entity: "dock_beta".to_string(),
            hops: 1,
        },
        "an adjudication at or below the pin is part of the replay"
    );

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// The composed read's own contract
// ---------------------------------------------------------------------------

/// Both halves come from ONE coordinate and ONE refusal.
#[test]
fn the_composed_read_reconstructs_both_halves_at_the_same_generation() {
    let (store, path, pin) = store_where_the_graph_moved_after_the_pin("composed");

    let PinnedComposedRead::Reproduced(c) = store
        .read_composed_at_generation(pin)
        .expect("composed read")
    else {
        panic!("the fixture must reproduce at its own pin");
    };

    assert_eq!(c.generation(), pin);
    assert_eq!(
        c.claims().current("package_17", LATER).len(),
        1,
        "the claims half must hold the fixture's claim"
    );
    // The identity half holds dock_alpha (asserted before the pin) and NOT
    // dock_beta (asserted after it).
    assert!(c.identity().get(&eid("dock_alpha")).is_some());
    assert!(
        c.identity().get(&eid("dock_beta")).is_none(),
        "an entity asserted after the pin must be absent from the pinned graph"
    );

    drop(store);
    cleanup(&path);
}

/// A generation past the head refuses, and refuses for BOTH halves together.
#[test]
fn a_future_generation_refuses_the_whole_composition() {
    let (store, path, _pin) = store_where_the_graph_moved_after_the_pin("future");

    match store.read_composed_at_generation(99_999).expect("composed") {
        PinnedComposedRead::Irreproducible(_) => {}
        PinnedComposedRead::Reproduced(_) => {
            panic!("a generation the store has not reached must not reproduce")
        }
    }

    drop(store);
    cleanup(&path);
}

/// **The entity checkpoint is NOT the bound**, and this is the regression guard.
///
/// `SnapshotCoordinate` records that the two checkpoints are not comparable:
/// `world_current` advances past every event considered, the entity fold only to
/// the last adjudication it folded. The fixture ends with ordinary claims after
/// the last adjudication, so the entity checkpoint sits legitimately behind —
/// and a composed read that used it as its head would refuse a perfectly
/// reproducible generation.
#[test]
fn a_lagging_entity_checkpoint_does_not_refuse_a_reproducible_generation() {
    let (mut store, path, _pin) = store_where_the_graph_moved_after_the_pin("lagging");

    // Append claims only. The entity checkpoint cannot advance past the last
    // adjudication; `world_current`'s does.
    claim_pointing_at(&mut store, "tail-1", "dock_alpha", T0 + 10);
    claim_pointing_at(&mut store, "tail-2", "dock_alpha", T0 + 11);
    store.fold().expect("fold");

    let coord = store.projection_coordinate().expect("coordinate");
    let head = coord.world_current().generation();
    assert!(
        coord.entities().generation() < head,
        "fixture precondition: the entity checkpoint must lag, or this test \
         cannot distinguish the two bounds (entities {}, world_current {head})",
        coord.entities().generation(),
    );

    match store.read_composed_at_generation(head).expect("composed") {
        PinnedComposedRead::Reproduced(c) => assert_eq!(c.generation(), head),
        PinnedComposedRead::Irreproducible(r) => panic!(
            "a reproducible generation was refused — the composed read is \
             bounded by the entity checkpoint instead of the log's progress: {r:?}"
        ),
    }

    drop(store);
    cleanup(&path);
}
