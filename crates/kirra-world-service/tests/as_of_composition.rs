//! **`ask_as_of` composes identity at its own cut** — 3h's other temporal axis.
//!
//! Box 3h closed *"historical queries use historical identity, never today's
//! entity graph applied to old evidence"* for the **generation-pinned** family.
//! `ask_as_of` was left reporting `NotResolvedInReplay`, which was honest but
//! left the same architectural question answered on one axis and open on the
//! other.
//!
//! This is that question on transaction time. The property is identical, and so
//! is the way it fails: replay the claims as they were known at `T`, then
//! resolve the object they name against today's graph. Every field is correct
//! except the one that quietly is not.
//!
//! # The fixture, mirrored from `historical_composition.rs`
//!
//! ```text
//! txn T0     assert dock_alpha
//! txn T0+1   claim  package_17 -> dock_alpha
//!            ---- as_known_at = T0+1 ----
//! txn T0+2   assert dock_beta
//! txn T0+3   merge  dock_alpha -> dock_beta
//! ```
//!
//! Asked at `as_known_at = T0+1`, the object must resolve to **`dock_alpha`**.
//! Asked at `as_known_at = T0+10`, it must resolve to **`dock_beta`**. The
//! claim is the same row in both, and the stored object string is `dock_alpha`
//! in both — so the pair cannot be satisfied by the bitemporal claim filter
//! alone, only by the identity cut moving with it.
//!
//! # What this deliberately does NOT change
//!
//! No new resolver: object resolution goes through the same
//! `WorldView::resolve_object` / `kirra_world::resolution::resolve` the live and
//! pinned paths use. No valid-time interpretation of identity — adjudications
//! are cut on transaction time only, because that is the axis they carry. No
//! fallback to the current graph when the historical one is empty. And
//! contradiction and refusal outcomes propagate exactly as they do elsewhere,
//! which the last two tests pin.

use kirra_world::adjudication::{
    AssertIdentity, IdentityAdjudication, Justification, MergeEntities, SplitEntity,
};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world_service::answer_ref::QueryKind;
use kirra_world_service::freshness::{FreshnessPolicy, FreshnessSource};
use kirra_world_service::query::{Ask, AskAsOf, QueryEngine};
use kirra_world_service::read_view::{ObjectIdentity, WorldLookup};
use kirra_world_service::semantics::SemanticVersions;
use kirra_world_store::adjudication_record::AdjudicationRow;
use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
/// The instant facts are asked to be VALID at. Held constant across every test
/// so the only axis moving is transaction time.
const VALID_AT: i64 = T0 + 100;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-asof-comp-{name}-{}-{n}.sqlite",
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

fn adjudicate(store: &mut WorldStore, tag: &str, txn_time_ms: i64, a: &IdentityAdjudication) {
    let event_id = EventId::new(format!("ev-{tag}")).expect("event id");
    let observation_id = ObservationId::new(format!("obs-{tag}")).expect("obs");
    store
        .append_adjudication(
            &AdjudicationRow {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms,
                valid_from_ms: txn_time_ms,
                source: "operator-console",
                source_version: "1.0.0",
                writer_class: WriterClass::Operator,
            },
            a,
        )
        .expect("append adjudication");
}

/// A claim valid from `T0` — before `VALID_AT` — so valid time never filters it
/// out and transaction time is the only axis under test.
fn claim_pointing_at(store: &mut WorldStore, tag: &str, object: &str, txn_time_ms: i64) {
    store
        .append(&NewEvent {
            event_id: &EventId::new(format!("ev-{tag}")).expect("event id"),
            observation_id: &ObservationId::new(format!("obs-{tag}")).expect("obs"),
            txn_time_ms,
            valid_from_ms: T0,
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

/// The fixture in the module docs. `CUT` is the `as_known_at` before the graph
/// moved; anything later sees the merge.
const CUT: i64 = T0 + 1;
const AFTER: i64 = T0 + 10;

fn store_where_the_graph_moved_after_the_cut(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");

    adjudicate(
        &mut store,
        "assert-alpha",
        T0,
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_alpha"), just(), at())),
    );
    claim_pointing_at(&mut store, "claim", "dock_alpha", CUT);

    // Everything below is recorded AFTER the cut.
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
    store.fold_entity_projection().expect("fold entities");
    (store, path)
}

fn sole_identity(store: &WorldStore, as_known_at_ms: i64) -> ObjectIdentity {
    let view = QueryEngine::new(store, FreshnessSource::Caller(FreshnessPolicy::Timeless));
    let temporal = view
        .execute(AskAsOf {
            subject: "package_17".to_owned(),
            valid_at_ms: VALID_AT,
            as_known_at_ms,
        })
        .expect("ask_as_of");
    let WorldLookup::Answered(answers) = temporal.lookup() else {
        panic!(
            "the fixture must answer at {as_known_at_ms}, got {:?}",
            temporal.lookup()
        );
    };
    assert_eq!(answers.len(), 1, "fixture holds one claim for the subject");
    answers[0].object_identity().clone()
}

// ---------------------------------------------------------------------------
// The property
// ---------------------------------------------------------------------------

/// **THE BOX, on transaction time.** An identity change recorded after
/// `as_known_at` must not rewrite the earlier answer.
#[test]
fn a_merge_recorded_after_the_cut_does_not_rewrite_the_earlier_answer() {
    let (store, path) = store_where_the_graph_moved_after_the_cut("before");

    assert_eq!(
        sole_identity(&store, CUT),
        ObjectIdentity::Resolved {
            entity: "dock_alpha".to_string(),
            hops: 0,
        },
        "the merge was recorded AFTER as_known_at, so it must not reach this \
         answer — resolving through today's graph would return dock_beta while \
         every other field still described the earlier state"
    );

    drop(store);
    cleanup(&path);
}

/// The control arm: asked later, the same query DOES follow the merge.
///
/// Without this the assertion above proves nothing — an implementation that
/// never resolved identity, or one whose graph was empty, would satisfy it.
#[test]
fn the_same_query_asked_later_follows_the_merge() {
    let (store, path) = store_where_the_graph_moved_after_the_cut("after");

    assert_eq!(
        sole_identity(&store, AFTER),
        ObjectIdentity::Resolved {
            entity: "dock_beta".to_string(),
            hops: 1,
        },
        "an adjudication at or before as_known_at is part of the cut"
    );

    drop(store);
    cleanup(&path);
}

/// The two cuts disagree, and that disagreement is the result.
///
/// Asserted as a pair so a future change that made both return the same
/// identity cannot be absorbed by editing the two tests above separately.
#[test]
fn the_two_cuts_genuinely_differ() {
    let (store, path) = store_where_the_graph_moved_after_the_cut("differ");

    assert_ne!(
        sole_identity(&store, CUT),
        sole_identity(&store, AFTER),
        "both cuts returned the same object identity, so this suite cannot tell \
         a transaction-time identity cut from a live one"
    );

    drop(store);
    cleanup(&path);
}

/// The CLAIM is identical across both cuts — only the resolution moves.
///
/// This is what stops the pair above being satisfied by the bitemporal claim
/// filter. If the two arms returned different claims, the evidence cut alone
/// would explain the difference and identity would be untested.
#[test]
fn the_claim_is_the_same_in_both_arms_only_its_object_resolution_moves() {
    let (store, path) = store_where_the_graph_moved_after_the_cut("sameclaim");
    let view = QueryEngine::new(&store, FreshnessSource::Caller(FreshnessPolicy::Timeless));

    let render = |as_known_at: i64| {
        let t = view
            .execute(AskAsOf {
                subject: "package_17".to_owned(),
                valid_at_ms: VALID_AT,
                as_known_at_ms: as_known_at,
            })
            .expect("ask_as_of");
        let WorldLookup::Answered(a) = t.lookup() else {
            panic!("must answer");
        };
        (
            a[0].subject().to_string(),
            a[0].predicate().map(str::to_string),
            a[0].object().map(str::to_string),
            a[0].value().to_string(),
        )
    };

    assert_eq!(
        render(CUT),
        render(AFTER),
        "the claim itself must be identical across the two cuts — the stored \
         object string is `dock_alpha` in both, and only the RESOLUTION differs"
    );

    drop(store);
    cleanup(&path);
}

/// **No fallback to the current graph.** A cut before any adjudication exists
/// must report the object as not-an-entity, not silently borrow today's graph.
#[test]
fn a_cut_before_any_adjudication_does_not_borrow_todays_graph() {
    let path = tmp("nofallback");
    let mut store = WorldStore::open(&path).expect("open");

    // The claim is recorded FIRST; the entity is asserted afterwards.
    claim_pointing_at(&mut store, "claim", "dock_alpha", T0);
    adjudicate(
        &mut store,
        "assert-alpha",
        T0 + 5,
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_alpha"), just(), at())),
    );
    store.fold().expect("fold");
    store.fold_entity_projection().expect("fold entities");

    assert_eq!(
        sole_identity(&store, T0),
        ObjectIdentity::NotAnEntity,
        "at this cut the graph held nothing; reporting the object as resolved \
         would mean the answer had reached forward for a graph that did not \
         exist yet"
    );
    // …and the same query later sees it, so the arm above is not vacuous.
    assert_eq!(
        sole_identity(&store, T0 + 5),
        ObjectIdentity::Resolved {
            entity: "dock_alpha".to_string(),
            hops: 0,
        },
    );

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// Refusal / ambiguity semantics propagate unchanged
// ---------------------------------------------------------------------------

/// A SPLIT before the cut yields `Ambiguous`, carrying its successors.
///
/// The point is that the historical path reports exactly what the live resolver
/// reports — no separate handling, no flattening of ambiguity into a pick.
#[test]
fn a_split_before_the_cut_reports_ambiguous_with_its_successors() {
    let path = tmp("split");
    let mut store = WorldStore::open(&path).expect("open");

    adjudicate(
        &mut store,
        "assert-alpha",
        T0,
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_alpha"), just(), at())),
    );
    claim_pointing_at(&mut store, "claim", "dock_alpha", T0);
    adjudicate(
        &mut store,
        "split",
        T0 + 2,
        &IdentityAdjudication::Split(
            SplitEntity::partition(
                eid("dock_alpha"),
                [eid("dock_x"), eid("dock_y")],
                just(),
                at(),
            )
            .expect("partition"),
        ),
    );
    store.fold().expect("fold");
    store.fold_entity_projection().expect("fold entities");

    // Before the split: a plain resolution.
    assert_eq!(
        sole_identity(&store, T0),
        ObjectIdentity::Resolved {
            entity: "dock_alpha".to_string(),
            hops: 0,
        },
    );

    // After it: ambiguity, reported rather than resolved away.
    match sole_identity(&store, T0 + 2) {
        ObjectIdentity::Ambiguous { successors } => {
            let mut s = successors;
            s.sort();
            assert_eq!(s, vec!["dock_x".to_string(), "dock_y".to_string()]);
        }
        other => panic!("a split must report Ambiguous, got {other:?}"),
    }

    drop(store);
    cleanup(&path);
}

/// An ambiguous identity is not `matchable`, on the historical path as on the
/// live one.
///
/// The consumer-facing half of the property above: `matchable` is what a
/// proposal consults, and it must fail closed for a historically ambiguous
/// object exactly as it does for a live one.
#[test]
fn a_historically_ambiguous_object_is_not_matchable() {
    let path = tmp("matchable");
    let mut store = WorldStore::open(&path).expect("open");

    adjudicate(
        &mut store,
        "assert-alpha",
        T0,
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_alpha"), just(), at())),
    );
    claim_pointing_at(&mut store, "claim", "dock_alpha", T0);
    adjudicate(
        &mut store,
        "split",
        T0 + 2,
        &IdentityAdjudication::Split(
            SplitEntity::partition(
                eid("dock_alpha"),
                [eid("dock_x"), eid("dock_y")],
                just(),
                at(),
            )
            .expect("partition"),
        ),
    );
    store.fold().expect("fold");
    store.fold_entity_projection().expect("fold entities");

    assert!(
        sole_identity(&store, T0)
            .matchable(Some("dock_alpha"))
            .is_some(),
        "a cleanly resolved historical object is matchable"
    );
    assert!(
        sole_identity(&store, T0 + 2)
            .matchable(Some("dock_alpha"))
            .is_none(),
        "an ambiguous historical object must fail closed, like a live one"
    );

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// The version set
// ---------------------------------------------------------------------------

/// The `as_of` family declares the same three rules, and says so on its answers.
#[test]
fn an_as_of_answer_carries_the_familys_version_set() {
    let (store, path) = store_where_the_graph_moved_after_the_cut("versions");
    let view = QueryEngine::new(&store, FreshnessSource::Caller(FreshnessPolicy::Timeless));

    let temporal = view
        .execute(AskAsOf {
            subject: "package_17".to_owned(),
            valid_at_ms: VALID_AT,
            as_known_at_ms: CUT,
        })
        .expect("ask_as_of");

    assert_eq!(
        *temporal.semantics(),
        SemanticVersions::for_query(QueryKind::AsOfSubject),
    );
    assert!(
        temporal.semantics().version_of("entity_fold").is_some(),
        "this family resolves objects through the identity graph now, so the \
         fold that builds it can change what the answer says"
    );
    assert!(temporal
        .semantics()
        .version_of("world_current_fold")
        .is_some());
    assert!(temporal
        .semantics()
        .version_of("subject_summary_fold")
        .is_none());

    drop(store);
    cleanup(&path);
}

/// Completeness still rides on the answer — 3g's property is untouched.
///
/// Included because this change rewrote `ask_as_of`'s body, and 3g's guarantee
/// lives in exactly the lines that moved.
#[test]
fn completeness_still_rides_on_the_answer() {
    let (store, path) = store_where_the_graph_moved_after_the_cut("completeness");
    let view = QueryEngine::new(&store, FreshnessSource::Caller(FreshnessPolicy::Timeless));

    let temporal = view
        .execute(AskAsOf {
            subject: "package_17".to_owned(),
            valid_at_ms: VALID_AT,
            as_known_at_ms: CUT,
        })
        .expect("ask_as_of");
    assert!(
        !temporal.is_degraded(),
        "nothing was compacted in this fixture"
    );

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// The composed read's own contract
// ---------------------------------------------------------------------------

/// **Both halves come from the same cut** — the store-level assertion.
///
/// The transaction-time mirror of 3h's
/// `the_composed_read_reconstructs_both_halves_at_the_same_generation`. Every
/// test above reaches the composition through `ask_as_of`, which is the path
/// that matters but also the path that could hide a wrong half behind a right
/// answer: if the claims half were empty the identity half would never be
/// consulted, and the suite would still be green.
///
/// This is also what exercises [`TemporalComposition::answer`] and
/// [`TemporalComposition::identity`] directly. `ask_as_of` moves both halves out
/// with `into_parts`, so without this the two accessors would be public surface
/// no test had ever called.
///
/// [`TemporalComposition::answer`]: kirra_world_store::snapshot::TemporalComposition::answer
/// [`TemporalComposition::identity`]: kirra_world_store::snapshot::TemporalComposition::identity
#[test]
fn the_composed_read_holds_both_halves_at_the_same_cut() {
    let (store, path) = store_where_the_graph_moved_after_the_cut("composed");

    let at_cut = store
        .as_of_composed("package_17", VALID_AT, CUT)
        .expect("composed read");
    assert_eq!(
        at_cut.answer().claims.len(),
        1,
        "the claims half must hold the fixture's claim — an empty one would \
         leave the identity half unconsulted and every arm above vacuous"
    );
    assert!(at_cut.identity().get(&eid("dock_alpha")).is_some());
    assert!(
        at_cut.identity().get(&eid("dock_beta")).is_none(),
        "an entity asserted after the cut must be absent from the graph at it"
    );

    // Later, the same read sees both — so the absence above is the cut doing
    // its job rather than the fixture never having recorded dock_beta.
    let later = store
        .as_of_composed("package_17", VALID_AT, AFTER)
        .expect("composed read");
    assert!(later.identity().get(&eid("dock_beta")).is_some());

    drop(store);
    cleanup(&path);
}

// --------------------------------------------------------------- box 3d ---

/// **`ask` resolves an object through a MULTI-HOP merge chain.**
///
/// The control for box 3d's bounded identity seeding. `ask` used to load the
/// WHOLE entity graph to resolve the objects its claims name; it now seeds only
/// those objects and loads their reachable closure.
///
/// A one-hop merge would not prove the seeding follows edges at all — resolving
/// `a -> b` needs `b`'s row to know `b` is live, so a seed-only loader fails
/// there too. But a TWO-hop chain is what separates "loads the seed and its
/// neighbours" from "loads the reachable closure": a loader that stopped after
/// one level would resolve `a` to a `DanglingRedirect` at `c`, which reads as a
/// broken graph rather than as a truncated read.
///
/// Nothing covered this before 3d — object resolution through `ask` was only
/// ever exercised at zero hops.
#[test]
fn ask_resolves_an_object_two_merges_deep() {
    let path = tmp("3d-two-hop");
    let mut store = WorldStore::open(&path).expect("open");

    adjudicate(
        &mut store,
        "a1",
        T0,
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_alpha"), just(), at())),
    );
    adjudicate(
        &mut store,
        "a2",
        T0 + 1,
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_beta"), just(), at())),
    );
    adjudicate(
        &mut store,
        "a3",
        T0 + 2,
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_gamma"), just(), at())),
    );
    adjudicate(
        &mut store,
        "m1",
        T0 + 3,
        &IdentityAdjudication::Merge(
            MergeEntities::new(vec![eid("dock_alpha")], eid("dock_beta"), just(), at())
                .expect("merge"),
        ),
    );
    adjudicate(
        &mut store,
        "m2",
        T0 + 4,
        &IdentityAdjudication::Merge(
            MergeEntities::new(vec![eid("dock_beta")], eid("dock_gamma"), just(), at())
                .expect("merge"),
        ),
    );
    claim_pointing_at(&mut store, "c1", "dock_alpha", T0 + 5);
    store.fold().expect("fold");
    store.fold_entity_projection().expect("fold entities");

    let view = QueryEngine::new(&store, FreshnessSource::Caller(FreshnessPolicy::Timeless));
    let answer = view
        .execute(Ask {
            subject: "package_17".to_owned(),
            now_ms: T0 + 10,
        })
        .expect("ask");

    let WorldLookup::Answered(answers) = answer.lookup() else {
        panic!("expected an answer, got {:?}", answer.lookup());
    };
    let identity = answers[0].object_identity();
    assert_eq!(
        identity,
        &ObjectIdentity::Resolved {
            entity: "dock_gamma".to_string(),
            hops: 2,
        },
        "the object must resolve two merges deep — a seed-only or one-level \
         loader reports a dangling redirect here, which looks like a broken \
         graph rather than a truncated read"
    );
}
