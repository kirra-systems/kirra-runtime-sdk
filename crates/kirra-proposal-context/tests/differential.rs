//! **Tier 2.5 — the differential harness.** §5.5.
//!
//! Two runs of one scenario:
//!
//! ```text
//!   Run A: world knows nothing   → context A → proposal A
//!   Run B: world knows the fact  → context B → proposal B
//! ```
//!
//! WHAT THIS PROVES, EXACTLY: world knowledge changes the SYMBOLIC context, and a
//! proposal-producing function fed by that context produces a different proposal.
//!
//! WHAT IT DOES NOT PROVE: that a PRODUCTION behaviour path changed. The producer
//! below is test-local and deliberately so — the production orchestration
//! boundary has not been ruled, and wiring `kirra-planner` to Kirra World would
//! create `kirra-sidecars → kirra-planner → kirra-world*` on a crate that
//! implements `CorridorSource`, which the fence's check 4 should refuse. Building
//! that route to make this test look stronger would be building the forbidden
//! route. So: Tier 2.5 EVIDENCE, not Tier 2.5 closure. §5.5's Goal 1 stays open.
//!
//! THE THREE CONTROLS, and why the first alone would be vacuous:
//!
//! 1. World absent vs present yields a different context. *(the positive result)*
//! 2. The proposal function is genuinely SENSITIVE to context — a producer that
//!    ignores it must fail to show a difference. Without this, control 1 could
//!    hold while the proposals differed for some incidental reason, and the
//!    harness would report success having tested nothing about the seam.
//! 3. The seam cannot encode a physical bound. That control lives with the gate
//!    that enforces it (`ci/test_proposal_context_symbolic.py`), because it has
//!    to run against synthetic source the crate would never contain.

use kirra_proposal_context::{mission_context, ContextHint, ContextId, ProposalContext};
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

/// The instant every claim is recorded and queried at.
const T0: i64 = 1_700_000_000_000;

/// A store path unique to THIS call.
///
/// The pid alone is not enough: cargo runs the tests below as parallel threads
/// of ONE process, and several of them call the same scenario helper, so a
/// pid-only path has two tests opening one SQLite file at once. Found by running
/// them — the collision is invisible with `--test-threads=1`.
fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-proposal-context-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn id(s: &str) -> ContextId {
    ContextId::new(s).expect("non-empty id")
}

/// The scenario's candidates, in the order the caller would have used with no
/// world knowledge at all. `dock_a` first is what makes the world-present result
/// observable: the fact has to MOVE something.
fn candidates() -> Vec<ContextId> {
    vec![id("dock_a"), id("dock_b")]
}

/// Record `package_17 last_seen_at dock_b` — the one fact this scenario turns on.
fn record_last_seen(store: &mut WorldStore, object: &str) {
    let event_id = EventId::new("ev-last-seen").expect("event id");
    let observation_id = ObservationId::new("obs-last-seen").expect("observation id");
    store
        .append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
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
    // `fold`, NOT `rebuild_entity_projection`: the latter folds the IDENTITY
    // projection (what box 2d resolves against), while `current()` reads
    // `world_current`, which `fold` populates. Getting this wrong is silent —
    // the store accepts the append, the fold succeeds, and `current()` simply
    // returns nothing, which looks exactly like "the world knows nothing".
    store.fold().expect("fold the current projection");
}

/// Run A: a world that knows nothing about the package.
fn context_world_silent() -> ProposalContext {
    let path = tmp("silent");
    let store = WorldStore::open(&path).expect("open");
    let ctx = mission_context(
        &store,
        &id("package_17"),
        &id("last_seen_at"),
        &candidates(),
        T0,
    )
    .expect("context");
    drop(store);
    let _ = std::fs::remove_file(&path);
    ctx
}

/// Run B: a world that knows the package was last seen at `dock_b`.
fn context_world_knows() -> ProposalContext {
    let path = tmp("knows");
    let mut store = WorldStore::open(&path).expect("open");
    record_last_seen(&mut store, "dock_b");
    let ctx = mission_context(
        &store,
        &id("package_17"),
        &id("last_seen_at"),
        &candidates(),
        T0,
    )
    .expect("context");
    drop(store);
    let _ = std::fs::remove_file(&path);
    ctx
}

// ---------------------------------------------------------------------------
// The proposal producers
// ---------------------------------------------------------------------------

/// The "proposal": which destination the producer would head for. A symbol, not
/// a trajectory — this stands in for a proposal, it does not pretend to be one.
type Proposal = String;

/// A CONTEXT-SENSITIVE producer: takes the first candidate the context prefers,
/// falling back to the caller's own order.
fn produce_with_context(ctx: &ProposalContext, fallback: &[ContextId]) -> Proposal {
    ctx.candidate_priority()
        .and_then(|p| p.first())
        .or_else(|| fallback.first())
        .map(|c| c.as_str().to_string())
        .expect("at least one candidate")
}

/// A CONTEXT-BLIND producer — the control-2 mutant. Structurally identical, but
/// it never reads the context. Used to show the harness detects the difference
/// because of the seam and not because of anything incidental.
fn produce_ignoring_context(_ctx: &ProposalContext, fallback: &[ContextId]) -> Proposal {
    fallback
        .first()
        .map(|c| c.as_str().to_string())
        .expect("at least one candidate")
}

// ---------------------------------------------------------------------------
// Control 1 — the positive result
// ---------------------------------------------------------------------------

/// World knowledge changes the context.
#[test]
fn world_knowledge_changes_the_symbolic_context() {
    let a = context_world_silent();
    let b = context_world_knows();
    assert_ne!(a, b, "the seam must carry a difference");

    // And specifically: silence expresses no preference; knowledge does.
    assert_eq!(a.preferred_destination(), None);
    assert_eq!(b.preferred_destination(), Some(&id("dock_b")));

    // The ordering moved, which is what a producer can act on.
    assert_eq!(a.candidate_priority(), Some(candidates().as_slice()));
    assert_eq!(
        b.candidate_priority(),
        Some([id("dock_b"), id("dock_a")].as_slice())
    );
}

/// And the changed context changes the proposal.
#[test]
fn the_changed_context_changes_the_proposal() {
    let fallback = candidates();
    let a = produce_with_context(&context_world_silent(), &fallback);
    let b = produce_with_context(&context_world_knows(), &fallback);

    assert_eq!(a, "dock_a", "no world knowledge → the caller's own order");
    assert_eq!(b, "dock_b", "world knowledge → the world-derived order");
    assert_ne!(a, b);
}

// ---------------------------------------------------------------------------
// Control 2 — the harness is detecting the SEAM, not an incidental difference
// ---------------------------------------------------------------------------

/// **The mutant must fail to show a difference.**
///
/// A producer that ignores the context returns the SAME proposal in both runs.
/// If this ever starts differing, the difference the positive test reports is
/// coming from somewhere other than the seam, and that test is proving nothing.
#[test]
fn a_context_blind_producer_shows_no_difference() {
    let fallback = candidates();
    let a = produce_ignoring_context(&context_world_silent(), &fallback);
    let b = produce_ignoring_context(&context_world_knows(), &fallback);
    assert_eq!(
        a, b,
        "a producer that ignores context must be unaffected by world knowledge — \
         if this differs, the positive test's difference is not the seam's doing"
    );
}

/// The two runs differ ONLY in what the world knows. Same candidates, same query
/// instant, same producer — so the difference cannot be attributed to setup.
#[test]
fn the_two_runs_differ_only_in_world_knowledge() {
    let fallback = candidates();
    // Same inputs, same producer, two stores that differ by one recorded claim.
    let silent = context_world_silent();
    let knows = context_world_knows();

    // Re-running the SAME arm twice is stable — so the difference between arms
    // is not run-to-run noise.
    assert_eq!(silent, context_world_silent(), "run A is deterministic");
    assert_eq!(knows, context_world_knows(), "run B is deterministic");

    assert_eq!(produce_with_context(&silent, &fallback), "dock_a");
    assert_eq!(produce_with_context(&knows, &fallback), "dock_b");
}

/// A world fact about something ELSE must not move the proposal. Guards against
/// a producer that reacts to the store being non-empty rather than to the claim.
#[test]
fn an_unrelated_world_fact_does_not_change_the_proposal() {
    let path = tmp("unrelated");
    let mut store = WorldStore::open(&path).expect("open");
    record_last_seen(&mut store, "dock_b");

    // Ask about a DIFFERENT subject: the store is populated, but not about this.
    let ctx = mission_context(
        &store,
        &id("package_99"),
        &id("last_seen_at"),
        &candidates(),
        T0,
    )
    .expect("context");
    drop(store);
    let _ = std::fs::remove_file(&path);

    assert_eq!(ctx.preferred_destination(), None);
    assert_eq!(produce_with_context(&ctx, &candidates()), "dock_a");
}

/// A claim naming a destination that is not a candidate is ignored — the context
/// never invents a candidate the caller did not offer.
#[test]
fn a_claim_outside_the_candidate_set_is_ignored() {
    let path = tmp("offmenu");
    let mut store = WorldStore::open(&path).expect("open");
    record_last_seen(&mut store, "dock_zzz");

    let ctx = mission_context(
        &store,
        &id("package_17"),
        &id("last_seen_at"),
        &candidates(),
        T0,
    )
    .expect("context");
    drop(store);
    let _ = std::fs::remove_file(&path);

    assert_eq!(ctx.preferred_destination(), None);
    assert_eq!(ctx.candidate_priority(), Some(candidates().as_slice()));
}

/// The context carries the fact it acted on, so a proposal is explainable
/// without re-reading the world.
#[test]
fn the_context_carries_the_fact_it_acted_on() {
    let ctx = context_world_knows();
    let fact = ctx
        .hints()
        .iter()
        .find(|h| matches!(h, ContextHint::MissionFact { .. }))
        .expect("a mission fact");
    assert_eq!(
        fact,
        &ContextHint::MissionFact {
            subject: id("package_17"),
            relation: id("last_seen_at"),
            object: id("dock_b"),
        }
    );
}
