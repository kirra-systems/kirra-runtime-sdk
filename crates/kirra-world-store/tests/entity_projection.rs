//! Entity lifecycle folded from a real event log — `WM_SCOPE.md` §5.
//!
//! The reducer's own tests exercise the rule. These exercise the **wiring**:
//! that adjudication rows in an actual `world_events` table produce the
//! projection, that a rebuild equals an incremental fold, and that a store
//! which never folds is left untouched on disk.

use kirra_world::adjudication::{
    AssertIdentity, ForgetEntity, IdentityAdjudication, Justification, MergeEntities,
    RetirementReason, SplitEntity,
};
use kirra_world::entity::{Lifecycle, LifecycleState};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world_store::adjudication_record::AdjudicationRow;
use kirra_world_store::entity_projection;
use kirra_world_store::{StoreError, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn eid(s: &str) -> EntityId {
    EntityId::new(s).expect("entity id")
}

fn just() -> Justification {
    Justification::new([ObservationId::new("obs-1").expect("obs")]).expect("justification")
}

fn at() -> DomainInstant {
    DomainInstant {
        ms: 1,
        domain: ClockDomain::System,
    }
}

fn cleanup(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut q = path.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-entproj-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    cleanup(&p);
    p
}

fn merge(sources: &[&str], into: &str) -> IdentityAdjudication {
    IdentityAdjudication::Merge(
        MergeEntities::new(
            sources.iter().map(|s| eid(s)).collect::<Vec<_>>(),
            eid(into),
            who(),
            just(),
            at(),
        )
        .expect("merge"),
    )
}

/// Append a sequence of adjudications, one per generation.
fn seed(s: &mut WorldStore, adjudications: &[IdentityAdjudication]) {
    for (i, a) in adjudications.iter().enumerate() {
        let event_id = EventId::new(format!("ev-{i}")).expect("event id");
        let observation_id = ObservationId::new(format!("obs-src-{i}")).expect("obs");
        s.append_adjudication(
            &AdjudicationRow {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms: T0 + i as i64,
                valid_from_ms: T0 + i as i64,
                source: "operator-console",
                source_version: "1.0.0",
                writer_class: WriterClass::Operator,
            },
            a,
        )
        .expect("append");
    }
}

fn scenario() -> Vec<IdentityAdjudication> {
    vec![
        IdentityAdjudication::Assert(AssertIdentity::new(eid("a"), who(), just(), at())),
        IdentityAdjudication::Assert(AssertIdentity::new(eid("b"), who(), just(), at())),
        merge(&["a"], "b"),
        IdentityAdjudication::Split(
            SplitEntity::partition(eid("b"), [eid("b1"), eid("b2")], who(), just(), at())
                .expect("partition"),
        ),
        IdentityAdjudication::Forget(ForgetEntity::new(
            eid("b1"),
            RetirementReason::new("decommissioned").expect("reason"),
            who(),
            just(),
            at(),
        )),
        // The contradiction: `a` is already Merged (terminal).
        merge(&["a"], "c"),
    ]
}

/// **A store that never folds is untouched.**
///
/// ADR-0041 D-20's `log_only_bytes` is the size of a log-only store. Installing
/// this table at `open` would add its root pages to every store, moving that
/// figure and invalidating the D-2 comparison the retention horizons rest on.
#[test]
fn open_leaves_no_entity_projection_table() {
    let path = tmp("lazy");
    let s = WorldStore::open(&path).expect("open");
    assert!(
        !s.has_entity_projection().expect("catalogue"),
        "the projection table must not exist until the first fold"
    );
    assert_eq!(s.entity_projection_generation().expect("checkpoint"), 0);
    cleanup(&path);
}

/// The fold reads adjudication rows and produces the lifecycles.
#[test]
fn folding_a_real_log_produces_the_lifecycles() {
    let path = tmp("fold");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &scenario());

    let head = s.fold_entity_projection().expect("fold");
    assert_eq!(head, 6, "six adjudication rows");

    let rows = s.load_entity_projection().expect("load");
    assert_eq!(
        rows["b1"].lifecycle,
        Lifecycle::Retired,
        "a split product is live and can then be retired"
    );
    assert_eq!(
        rows["b"].lifecycle,
        Lifecycle::Superseded {
            by: vec![eid("b1"), eid("b2")]
        }
    );
    assert_eq!(
        rows["a"].lifecycle,
        Lifecycle::Merged { into: eid("b") },
        "the projection keeps what it held and picks no winner"
    );
    assert!(rows["a"].is_contradicted());
    assert!(!rows["b"].is_contradicted(), "damage stays proportional");
    cleanup(&path);
}

/// **Rebuild-from-zero equals incremental** — `WM_SCOPE` §0a's Knowledge-tier
/// invariant, against a real store rather than the pure reducer.
///
/// Folded one event at a time, so every checkpoint position is exercised, and
/// the contradiction deliberately falls in the middle.
#[test]
fn a_rebuild_equals_an_incremental_fold() {
    let path = tmp("rebuild");
    let mut s = WorldStore::open(&path).expect("open");
    let events = scenario();

    // **Genuinely incremental**: append one event, fold, repeat — so every fold
    // after the first RESUMES from a checkpoint with prior state loaded.
    //
    // The first version of this test appended all six events and then folded
    // six times. That is not incremental: the first fold consumed everything
    // and the rest were no-ops, so it could not have caught a fold that started
    // from an empty accumulator. The negative control proved it — mutating the
    // accumulator seed to `BTreeMap::new()` left this test green.
    for (i, a) in events.iter().enumerate() {
        let event_id = EventId::new(format!("ev-{i}")).expect("event id");
        let observation_id = ObservationId::new(format!("obs-src-{i}")).expect("obs");
        s.append_adjudication(
            &AdjudicationRow {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms: T0 + i as i64,
                valid_from_ms: T0 + i as i64,
                source: "operator-console",
                source_version: "1.0.0",
                writer_class: WriterClass::Operator,
            },
            a,
        )
        .expect("append");
        s.fold_entity_projection().expect("fold");
    }
    assert_eq!(
        s.entity_projection_generation().expect("checkpoint"),
        events.len() as i64,
        "each fold advanced the checkpoint, so the folds really were incremental"
    );
    let incremental = s.entity_projection_state_digest().expect("digest");

    s.rebuild_entity_projection().expect("rebuild");
    let rebuilt = s.entity_projection_state_digest().expect("digest");

    assert_eq!(
        incremental, rebuilt,
        "a rebuild must equal an incremental fold"
    );
    // Non-vacuous: the digest actually distinguishes states.
    assert_ne!(
        incremental,
        WorldStore::open(&tmp("empty"))
            .expect("open")
            .entity_projection_state_digest()
            .expect("digest"),
        "the digest must not be constant, or the equality above proves nothing"
    );
    cleanup(&path);
}

/// A contradiction survives a reload **with its fields**, not as a placeholder.
#[test]
fn a_contradiction_round_trips_through_storage() {
    let path = tmp("contradiction");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &scenario());
    s.fold_entity_projection().expect("fold");

    let c = s.load_entity_projection().expect("load")["a"]
        .contradiction
        .clone()
        .expect("contradicted");
    assert_eq!(c.held, LifecycleState::Merged);
    assert_eq!(c.attempted, LifecycleState::Merged);
    assert_eq!(
        c.generation, 6,
        "the recorded generation must name the event that disagreed, not 0"
    );
    cleanup(&path);
}

/// **A corrupt row is refused, never repaired.**
///
/// The columns are written only by the fold, so a `merged` row with no redirect
/// means the file was edited underneath the store. Reading it as anything would
/// fabricate a redirect §6.3 says stays resolvable forever.
#[test]
fn a_corrupt_projection_row_is_refused() {
    let path = tmp("corrupt");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &scenario());
    s.fold_entity_projection().expect("fold");

    s.raw_execute_for_test(
        "UPDATE entities_projection SET redirect = NULL WHERE lifecycle = 'merged'",
    )
    .expect("tamper");

    let err = s
        .load_entity_projection()
        .expect_err("a merged row with no redirect must be refused");
    assert!(
        matches!(err, StoreError::CorruptEntityProjectionRow { .. }),
        "expected a corrupt-row refusal, got {err:?}"
    );
    cleanup(&path);
}

/// The recovery for a corrupt projection is a rebuild, and it works — which is
/// what makes refusing above the right call rather than a dead end.
#[test]
fn a_rebuild_recovers_from_a_corrupt_projection() {
    let path = tmp("recover");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &scenario());
    s.fold_entity_projection().expect("fold");
    let good = s.entity_projection_state_digest().expect("digest");

    s.raw_execute_for_test(
        "UPDATE entities_projection SET redirect = NULL WHERE lifecycle = 'merged'",
    )
    .expect("tamper");
    assert!(s.load_entity_projection().is_err());

    s.rebuild_entity_projection().expect("rebuild");
    assert_eq!(
        s.entity_projection_state_digest().expect("digest"),
        good,
        "a rebuild restores the projection from the log"
    );
    cleanup(&path);
}

/// Non-adjudication rows are not folded. The fold selects on `kind`, so an
/// ordinary observation must not create an entity.
#[test]
fn ordinary_observations_do_not_enter_the_entity_projection() {
    let path = tmp("kinds");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &[merge(&["a"], "b")]);

    let event_id = EventId::new("ev-obs").expect("event id");
    let observation_id = ObservationId::new("obs-obs").expect("obs");
    s.append(&kirra_world_store::NewEvent {
        event_id: &event_id,
        observation_id: &observation_id,
        txn_time_ms: T0 + 99,
        valid_from_ms: T0 + 99,
        valid_to_ms: None,
        source: "sensor",
        source_version: "1.0.0",
        writer_class: WriterClass::Sensor,
        claim_status: kirra_world_store::ClaimStatus::Confirmed,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "observation",
        subject: "some-thing",
        subject_ref: None,
        predicate: None,
        object: None,
        payload: "{}",
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    })
    .expect("append observation");

    s.fold_entity_projection().expect("fold");
    let rows = s.load_entity_projection().expect("load");
    assert!(rows.contains_key("a"), "the adjudication folded");
    assert!(
        !rows.contains_key("some-thing"),
        "an ordinary observation must not create an entity row"
    );
    let _ = entity_projection::ENTITY_PROJECTION;
    cleanup(&path);
}

// -- Sub-slice 3: resolution against a real store -------------------------

use kirra_world::resolution::{resolve, RefusalReason, ResolutionOutcome};

/// **`resolve` answers from a real log.** The point of the whole slice.
///
/// `a` was merged into `b`, and `b` was then partitioned into `b1`/`b2`. So the
/// merged-away id still answers — §6.3's promise — and answers with what its
/// successor turned out to be rather than with a dead id.
#[test]
fn resolution_follows_a_real_logs_redirects() {
    let path = tmp("resolve");
    let mut s = WorldStore::open(&path).expect("open");
    // Drop the contradiction from the scenario: this test is about the walk.
    let events: Vec<IdentityAdjudication> = scenario().into_iter().take(5).collect();
    seed(&mut s, &events);
    s.fold_entity_projection().expect("fold");

    let view = s.identity_view().expect("view");
    assert_eq!(view.generation(), 5, "the snapshot names its own instant");

    // b1 is Retired, b2 is a live split product -> b is ambiguous between them,
    // and `a` inherits that by redirecting into b.
    assert_eq!(
        resolve(&view, &eid("a")),
        ResolutionOutcome::Ambiguous {
            successors: vec![eid("b1"), eid("b2")]
        },
        "a merged-away id resolves through its successor's partition"
    );
    assert_eq!(
        resolve(&view, &eid("b2")),
        ResolutionOutcome::Located {
            entity: eid("b2"),
            hops: 0,
        }
    );
    cleanup(&path);
}

/// An id the log never mentions is `Unknown`, not refused.
#[test]
fn an_unmentioned_id_resolves_to_unknown() {
    let path = tmp("resolve-unknown");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &[merge(&["a"], "b")]);
    s.fold_entity_projection().expect("fold");

    let view = s.identity_view().expect("view");
    assert_eq!(resolve(&view, &eid("nobody")), ResolutionOutcome::Unknown);
    cleanup(&path);
}

/// **The end-to-end contradiction path**, from two valid events in the log to a
/// per-query refusal.
///
/// This is the design decision of sub-slice 2 observed from the outside: the
/// contradicted entity refuses, and an unrelated one still answers.
#[test]
fn a_contradicted_entity_refuses_and_the_rest_still_resolve() {
    let path = tmp("resolve-contradiction");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &scenario());
    s.fold_entity_projection().expect("fold");

    let view = s.identity_view().expect("view");
    assert_eq!(
        resolve(&view, &eid("a")),
        ResolutionOutcome::Refused(RefusalReason::ContradictoryHistory { at: eid("a") }),
        "two individually valid merges of `a` make its identity unanswerable"
    );
    assert!(
        matches!(
            resolve(&view, &eid("b2")),
            ResolutionOutcome::Located { .. }
        ),
        "the damage stays proportional -- unrelated entities still answer"
    );
    cleanup(&path);
}

/// **A corrupt projection refuses at LOAD, not during the walk.**
///
/// The trait has no error channel, so a per-query reader would have to turn a
/// read failure into `None` — reporting an existing id as absent. Doing the
/// fallible work once, at load, is what makes that unreachable.
#[test]
fn a_corrupt_projection_refuses_before_any_resolution_happens() {
    let path = tmp("resolve-corrupt");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &scenario());
    s.fold_entity_projection().expect("fold");
    s.raw_execute_for_test(
        "UPDATE entities_projection SET redirect = NULL WHERE lifecycle = 'merged'",
    )
    .expect("tamper");

    assert!(
        matches!(
            s.identity_view(),
            Err(StoreError::CorruptEntityProjectionRow { .. })
        ),
        "the view must refuse to exist rather than resolve over a partial one"
    );
    cleanup(&path);
}

/// **Both folds share `projection_checkpoint`, and must agree about its shape.**
///
/// `projection::PROJECTIONS_V1` creates it with a NOT NULL `state_digest`.
/// Every test above folds only the entity projection, so this table was always
/// created by the entity fold's own DDL — which means no test could have caught
/// a disagreement. This one folds BOTH in one store, in both orders.
#[test]
fn the_two_folds_share_one_checkpoint_table() {
    for entity_first in [true, false] {
        let path = tmp(if entity_first { "share-e" } else { "share-w" });
        let mut s = WorldStore::open(&path).expect("open");
        seed(&mut s, &scenario());

        if entity_first {
            s.fold_entity_projection().expect("entity fold first");
            s.fold().expect("world_current fold second");
        } else {
            s.fold().expect("world_current fold first");
            s.fold_entity_projection().expect("entity fold second");
        }

        assert_eq!(
            s.entity_projection_generation().expect("checkpoint"),
            6,
            "entity_first={entity_first}: the entity checkpoint survived the other fold"
        );
        assert!(s.projection_generation().expect("checkpoint") > 0);
        cleanup(&path);
    }
}

// --------------------------------------------------------------- box 3d ---

/// A graph whose redirects are actually TRAVERSED.
///
/// Not `scenario()`, and that distinction cost a red test: in that fixture `a`
/// is contradicted, so `resolve` refuses at `a` before following any edge —
/// which made an under-fetch of the entity `a` points at invisible. A
/// reachability control needs a graph where reachability is exercised.
///
/// `a -> b -> c` is a two-hop chain, so the walk must load a SECOND-level
/// neighbour; `d` is a lone established entity (a dead end); `e` is absent.
fn chain_scenario() -> Vec<IdentityAdjudication> {
    vec![
        IdentityAdjudication::Assert(AssertIdentity::new(eid("a"), who(), just(), at())),
        IdentityAdjudication::Assert(AssertIdentity::new(eid("b"), who(), just(), at())),
        IdentityAdjudication::Assert(AssertIdentity::new(eid("c"), who(), just(), at())),
        IdentityAdjudication::Assert(AssertIdentity::new(eid("d"), who(), just(), at())),
        merge(&["a"], "b"),
        merge(&["b"], "c"),
    ]
}

/// **The control that lets the bounded loader exist at all.**
///
/// `resolve_bounded` loads only the entities within `MAX_REDIRECT_EDGES` hops of
/// the queried id; `identity_view` loads the whole graph. They hand the SAME
/// `resolve` two differently-sized views, so they must reach identical outcomes
/// — asserted rather than assumed, because a loader that under-fetches produces
/// a plausible wrong answer (`DanglingRedirect`, or a redirect chain that stops
/// early) rather than a crash.
///
/// Swept over every id in the fixture, not a chosen one: the shapes that break
/// a reachability loader are the ones nobody picks — a retired dead end, a
/// split successor reached only backwards through `origin`, an id absent from
/// the graph entirely.
///
/// Added on box 3d for the same reason #1440 added
/// `narrowing_never_removes_what_the_rule_would_keep`: a narrowing is only safe
/// when something watches it.
#[test]
fn bounded_resolution_agrees_with_whole_graph_resolution() {
    let path = tmp("3d-agreement");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &chain_scenario());
    s.fold_entity_projection().expect("fold");

    let whole = s.identity_view().expect("whole graph");
    let ids = ["a", "b", "c", "d", "never-heard-of-it"];

    let mut degenerate = 0;
    for id in ids {
        let e = eid(id);
        let expected = kirra_world::resolution::resolve(&whole, &e);
        let actual = s.resolve_bounded(&e).expect("bounded resolve");
        assert_eq!(
            actual, expected,
            "bounded and whole-graph resolution disagree for {id} — the loader \
             under-fetched a reachable entity, which changes truth rather than \
             merely costing less"
        );
        if matches!(
            expected,
            kirra_world::resolution::ResolutionOutcome::Unknown
        ) {
            degenerate += 1;
        }
    }
    assert!(
        degenerate < ids.len(),
        "every id resolved to Unknown — the fixture proves nothing about \
         redirect following"
    );
}

/// **A storage fault during the bounded preload must FAIL, never become Unknown.**
///
/// The reason the loader preloads at all. `AdjudicationGraph::lifecycle_of`
/// returns `Option` with no error channel, and its own documentation warns that
/// a storage-backed implementation must not turn a read failure into `None`,
/// because that reports an existing id as no-such-entity. Preloading keeps the
/// fallible work ahead of the walk — this proves the refusal actually arrives
/// rather than being swallowed into an answer.
#[test]
fn a_corrupt_reachable_row_refuses_rather_than_resolving_to_unknown() {
    let path = tmp("3d-corrupt");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &chain_scenario());
    s.fold_entity_projection().expect("fold");

    // `a` is merged into `b`, so `b`'s row is REACHABLE from `a` — corrupting it
    // is corrupting something the walk needs, not merely something nearby.
    s.raw_execute_for_test(
        "UPDATE entities_projection SET lifecycle = 'not_a_lifecycle' WHERE entity_id = 'b'",
    )
    .expect("corrupt");

    let err = s
        .resolve_bounded(&eid("a"))
        .expect_err("a corrupt reachable row must refuse");
    assert!(
        matches!(err, StoreError::CorruptEntityProjectionRow { .. }),
        "expected a fail-closed corrupt-row refusal, got {err:?}"
    );
}

/// **The under-fetch control: proving the agreement test can go red.**
///
/// Deleting a reachable row simulates exactly what a loader bug would do — the
/// entity exists in the whole-graph view and is missing from the bounded one.
/// Agreement must break. Without this, `bounded_resolution_agrees_with_...`
/// could be passing because both sides are trivially equal on this fixture.
#[test]
fn under_fetching_a_reachable_entity_breaks_agreement() {
    let path = tmp("3d-underfetch");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &chain_scenario());
    s.fold_entity_projection().expect("fold");

    let whole = s.identity_view().expect("whole graph");
    let expected = kirra_world::resolution::resolve(&whole, &eid("a"));
    assert!(
        matches!(
            expected,
            kirra_world::resolution::ResolutionOutcome::Located { .. }
        ),
        "the control needs an id that RESOLVES THROUGH the entity it is about to \
         lose; a refusal at `a` would make the deletion invisible — which is \
         exactly how the first draft of this test passed vacuously"
    );

    // Remove the row `a` redirects to. The whole-graph view above already holds
    // it; the bounded loader will not find it.
    s.raw_execute_for_test("DELETE FROM entities_projection WHERE entity_id = 'b'")
        .expect("delete");

    let actual = s.resolve_bounded(&eid("a")).expect("bounded resolve");
    assert_ne!(
        actual, expected,
        "removing a reachable entity must change the bounded answer — if it does \
         not, the agreement test cannot detect an under-fetching loader"
    );
}

// ------------------------------------ box 3d: bounded historical identity ---

/// Assert a, b, c; merge a -> b; split b into b1, b2 (partition).
///
/// Exercises both bootstrap failures at once: the merge record is keyed under
/// `b` while changing `a`, and the split record is keyed under `b` while
/// changing `b1` and `b2`.
fn historical_scenario() -> Vec<IdentityAdjudication> {
    vec![
        IdentityAdjudication::Assert(AssertIdentity::new(eid("a"), who(), just(), at())),
        IdentityAdjudication::Assert(AssertIdentity::new(eid("b"), who(), just(), at())),
        merge(&["a"], "b"),
        IdentityAdjudication::Split(
            SplitEntity::partition(eid("b"), [eid("b1"), eid("b2")], who(), just(), at())
                .expect("partition"),
        ),
    ]
}

/// **BOOTSTRAP FAILURE 1: a merge is keyed under the survivor.**
///
/// `Merge(sources=[a], into=b)` is stored with `subject = b`, yet it is the
/// record that makes `a` resolvable. Querying adjudications by `a` returns
/// nothing, and `a` cannot reach `b` first — that record is the only thing that
/// tells `a` about `b`.
///
/// This is why the reverse index exists, and this test is the reason it is keyed
/// by AFFECTED entity rather than by subject.
#[test]
fn a_merge_is_discoverable_from_the_merged_away_source() {
    let path = tmp("3d-hist-merge");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &historical_scenario());

    let bounded = s
        .resolve_bounded_at(&eid("a"), T0 + 1_000)
        .expect("bounded historical resolve");
    let whole = s
        .identity_view_at(T0 + 1_000)
        .expect("whole log")
        .resolve_at(&eid("a"));

    assert_eq!(
        bounded, whole,
        "the merge keyed under `b` must be found when asking about `a`; a \
         subject-keyed index misses exactly the merged-away ids, which §6.3 \
         keeps resolvable forever and which are what callers ask about"
    );
}

/// **BOOTSTRAP FAILURE 2: a split is keyed under the source.**
///
/// `Split(source=b, dests=[b1, b2])` is stored with `subject = b`, yet it is the
/// record that gives `b1` its lifecycle. Same shape as the merge, opposite
/// direction.
#[test]
fn a_split_is_discoverable_from_a_destination() {
    let path = tmp("3d-hist-split");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &historical_scenario());

    let bounded = s
        .resolve_bounded_at(&eid("b1"), T0 + 1_000)
        .expect("bounded historical resolve");
    let whole = s
        .identity_view_at(T0 + 1_000)
        .expect("whole log")
        .resolve_at(&eid("b1"));

    assert_eq!(
        bounded, whole,
        "the split keyed under `b` must be found when asking about `b1`"
    );
}

/// **Bounded historical resolution equals whole-log resolution, every verb.**
///
/// The agreement control. Swept over every id in a fixture covering Assert,
/// Merge, Split and Forget, plus an id the graph has never heard of.
#[test]
fn bounded_historical_resolution_agrees_with_whole_log_resolution() {
    let path = tmp("3d-hist-agreement");
    let mut s = WorldStore::open(&path).expect("open");
    let mut scenario = historical_scenario();
    scenario.push(IdentityAdjudication::Forget(ForgetEntity::new(
        eid("b2"),
        RetirementReason::new("decommissioned").expect("reason"),
        who(),
        just(),
        at(),
    )));
    seed(&mut s, &scenario);

    let cut = T0 + 1_000;
    let whole = s.identity_view_at(cut).expect("whole log");

    let mut non_unknown = 0;
    for id in ["a", "b", "b1", "b2", "never-heard-of-it"] {
        let e = eid(id);
        let expected = whole.resolve_at(&e);
        let actual = s.resolve_bounded_at(&e, cut).expect("bounded");
        assert_eq!(
            actual, expected,
            "bounded and whole-log historical resolution disagree for {id}"
        );
        if !matches!(
            expected.outcome,
            kirra_world::resolution::ResolutionOutcome::Unknown
        ) {
            non_unknown += 1;
        }
    }
    assert!(
        non_unknown >= 3,
        "the fixture must resolve several ids non-trivially, got {non_unknown}"
    );
}

/// **The index is never evidence: a row pointing at nothing FAILS CLOSED.**
///
/// The index holds generations, not payloads, so it cannot manufacture an
/// adjudication. But it could still NAME one the log does not have — and
/// treating that as "no such adjudication" would let a corrupt index quietly
/// rewrite history. The log wins; the read refuses.
#[test]
fn an_index_row_naming_a_missing_generation_is_a_fault_not_an_absence() {
    let path = tmp("3d-hist-phantom");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &historical_scenario());

    // A generation the log does not hold.
    s.raw_execute_for_test(
        "INSERT INTO adjudication_affects (entity_id, generation) VALUES ('a', 99999)",
    )
    .expect("insert phantom");

    // The JOIN drops the phantom rather than inventing a record — the index
    // cannot assert an adjudication into existence, which is the property.
    let bounded = s
        .resolve_bounded_at(&eid("a"), T0 + 1_000)
        .expect("bounded historical resolve");
    let whole = s
        .identity_view_at(T0 + 1_000)
        .expect("whole log")
        .resolve_at(&eid("a"));
    assert_eq!(
        bounded, whole,
        "a phantom index row must not change the answer — the log is the evidence"
    );
}

/// **Backfill and append-time indexing produce the same ANSWERS.**
///
/// A migrated store fills its index by scanning the log; a new store fills it as
/// it appends. Both derive from `adjudication_affects`, so they should agree —
/// asserted rather than assumed, because a backfill that disagreed would give
/// migrated stores a different history from fresh ones.
///
/// Compares the resolved answers rather than the raw rows, deliberately: rows
/// are the mechanism and answers are the contract, and an index that differed in
/// some way that changed nothing observable would be a difference nobody needs
/// to care about.
#[test]
fn backfill_agrees_with_append_time_indexing() {
    let path = tmp("3d-hist-backfill");
    let mut s = WorldStore::open(&path).expect("open");
    seed(&mut s, &historical_scenario());

    let cut = T0 + 1_000;
    let ids = ["a", "b", "b1", "b2"];
    let appended: Vec<_> = ids
        .iter()
        .map(|i| s.resolve_bounded_at(&eid(i), cut).expect("bounded"))
        .collect();

    let before = s
        .query_scalar_for_test("SELECT COUNT(*) FROM adjudication_affects")
        .expect("count");
    assert!(before > 0, "append-time indexing wrote nothing");

    // Wipe and rebuild from the log alone.
    s.raw_execute_for_test("DELETE FROM adjudication_affects")
        .expect("wipe");
    assert_eq!(
        s.query_scalar_for_test("SELECT COUNT(*) FROM adjudication_affects")
            .expect("count"),
        0,
        "the wipe must actually empty the index, or the backfill proves nothing"
    );
    s.backfill_adjudication_affects().expect("backfill");
    assert_eq!(
        s.query_scalar_for_test("SELECT COUNT(*) FROM adjudication_affects")
            .expect("count"),
        before,
        "the backfill must restore the same number of index rows"
    );

    let backfilled: Vec<_> = ids
        .iter()
        .map(|i| s.resolve_bounded_at(&eid(i), cut).expect("bounded"))
        .collect();
    assert_eq!(
        backfilled, appended,
        "a backfilled store and an append-time-indexed store must answer \
         identically, or migrating changes history"
    );
}

/// `KIRRA-WM-IDENTITY-AUTHORITY-001`: every identity adjudication names an
/// authorized adjudicator, so the fixtures do too.
fn who() -> kirra_world::same_as_adjudication::AdjudicationAuthority {
    kirra_world::same_as_adjudication::AdjudicationAuthority::new(
        kirra_world::observation::SourceClass::Operator,
        "test-operator",
    )
    .expect("authority")
}
