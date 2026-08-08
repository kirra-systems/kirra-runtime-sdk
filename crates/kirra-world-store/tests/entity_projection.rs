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
        IdentityAdjudication::Assert(AssertIdentity::new(eid("a"), just(), at())),
        IdentityAdjudication::Assert(AssertIdentity::new(eid("b"), just(), at())),
        merge(&["a"], "b"),
        IdentityAdjudication::Split(
            SplitEntity::partition(eid("b"), [eid("b1"), eid("b2")], just(), at())
                .expect("partition"),
        ),
        IdentityAdjudication::Forget(ForgetEntity::new(
            eid("b1"),
            RetirementReason::new("decommissioned").expect("reason"),
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
