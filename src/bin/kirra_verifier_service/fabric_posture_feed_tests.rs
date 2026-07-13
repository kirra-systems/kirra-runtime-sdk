// fabric_posture_feed_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// #88: verifier→fabric posture feed (single-local-asset model).
//
// Exercises `sync_local_asset_posture` directly (the env-gated spawn wrapper
// is thin): a registered local asset's fabric posture must track the cached
// fleet posture, fail closed on a stale cache, avoid churn when unchanged,
// and run the bounded cross-asset propagation pass.
// ---------------------------------------------------------------------------

use super::{force_local_asset_lockedout, sync_local_asset_posture};

use std::sync::Arc;

use kirra_verifier::fabric::asset::{AssetType, FabricAsset, KinematicProfileType};
use kirra_verifier::fabric::router::FabricRouter;
use kirra_verifier::posture_cache::{now_ms, CachedFleetPosture, ServiceState, SharedPostureCache};
use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

const LOCAL: &str = "local-asset";

fn asset(id: &str) -> FabricAsset {
    let now = now_ms();
    FabricAsset {
        asset_id: id.to_string(),
        asset_type: AssetType::AutonomousVehicle,
        display_name: id.to_string(),
        kinematic_profile: KinematicProfileType::RobotNominal,
        registered_at_ms: now,
        last_seen_ms: now,
        metadata: Default::default(),
    }
}

/// Builds an Active `ServiceState` whose cache holds `cached` and whose
/// fabric router has `LOCAL` registered (seeded Degraded).
fn state(cached: Option<CachedFleetPosture>) -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(cached));
    let fabric_router = Arc::new(FabricRouter::new());
    fabric_router.register_asset(&asset(LOCAL));
    Arc::new(ServiceState {
        app,
        posture_cache,
        started_at_ms: now_ms(),
        audit_verifying_key: None,
        fabric_router,
        fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
        fabric_causal_log: Arc::new(
            kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None),
        ),
        posture_engine_tx: std::sync::OnceLock::new(),
        perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
        last_actuator_verdict: kirra_verifier::posture_cache::empty_last_verdict_cell(),
    })
}

/// A FRESH cache entry (generated now) carrying `posture`.
fn fresh(posture: FleetPosture) -> CachedFleetPosture {
    CachedFleetPosture::new_with_generation(posture, 1, now_ms())
}

#[test]
fn fresh_cache_pushes_fleet_posture_to_local_asset() {
    let svc = state(Some(fresh(FleetPosture::Nominal)));
    // Seeded Degraded by register_asset.
    assert_eq!(
        svc.fabric_router.asset_posture(LOCAL).unwrap().posture,
        FleetPosture::Degraded
    );

    sync_local_asset_posture(&svc, LOCAL);

    let after = svc.fabric_router.asset_posture(LOCAL).unwrap();
    assert_eq!(
        after.posture,
        FleetPosture::Nominal,
        "feed must mirror the fleet posture"
    );
    assert!(
        after.blocked_by.is_empty(),
        "Nominal carries no blocked_by reason"
    );
    assert_eq!(after.generation, 1, "seed gen 0 → first feed write gen 1");
}

#[test]
fn stale_cache_does_not_push_keeps_last_good() {
    // generated_at far in the past → is_stale(now) == true.
    let stale = CachedFleetPosture::new_with_generation(
        FleetPosture::Nominal,
        7,
        now_ms().saturating_sub(60_000),
    );
    let svc = state(Some(stale));

    sync_local_asset_posture(&svc, LOCAL);

    assert_eq!(
        svc.fabric_router.asset_posture(LOCAL).unwrap().posture,
        FleetPosture::Degraded,
        "a stale cache must NOT propagate forward (fail-closed): seed is kept"
    );
}

#[test]
fn empty_cache_does_not_push() {
    let svc = state(None);
    sync_local_asset_posture(&svc, LOCAL);
    assert_eq!(
        svc.fabric_router.asset_posture(LOCAL).unwrap().posture,
        FleetPosture::Degraded,
        "a not-yet-computed cache must not overwrite the seed"
    );
}

#[test]
fn unchanged_posture_does_not_bump_generation() {
    // Seed is Degraded; feeding Degraded again must be a no-op.
    let svc = state(Some(fresh(FleetPosture::Degraded)));
    let gen_before = svc.fabric_router.asset_posture(LOCAL).unwrap().generation;

    sync_local_asset_posture(&svc, LOCAL);

    let after = svc.fabric_router.asset_posture(LOCAL).unwrap();
    assert_eq!(after.posture, FleetPosture::Degraded);
    assert_eq!(
        after.generation, gen_before,
        "an unchanged posture must not bump the generation (no churn)"
    );
}

/// Bug 7: when the supervisor escalates a wedged feed, the local asset is
/// pinned LockedOut so `route_command` fail-closes — no stale posture is left
/// admitting fabric commands. Mirrors the escalation the supervisor invokes on
/// restart-budget exhaustion.
#[test]
fn feed_escalation_pins_local_asset_locked_out() {
    // A LIVE feed had lifted the asset to Nominal (route_command would admit).
    let svc = state(Some(fresh(FleetPosture::Nominal)));
    sync_local_asset_posture(&svc, LOCAL);
    assert_eq!(
        svc.fabric_router.asset_posture(LOCAL).unwrap().posture,
        FleetPosture::Nominal,
        "precondition: a healthy feed leaves the asset at the live fleet posture"
    );

    // The feed wedges (deterministic panic exhausts the restart budget) → the
    // supervisor runs the escalation.
    force_local_asset_lockedout(&svc, LOCAL);

    let after = svc.fabric_router.asset_posture(LOCAL).unwrap();
    assert_eq!(
        after.posture,
        FleetPosture::LockedOut,
        "a wedged feed must fail the local asset CLOSED, not leave it stale-Nominal"
    );
    assert_eq!(
        after.blocked_by,
        vec!["POSTURE_FEED_WEDGED_FAILCLOSED".to_string()],
        "the fail-closed reason must be tagged for operators"
    );
    assert!(
        after.generation >= 2,
        "escalation supersedes the prior live-feed generation"
    );
}

#[test]
fn lockedout_fleet_posture_locks_the_local_asset() {
    let svc = state(Some(fresh(FleetPosture::LockedOut)));
    sync_local_asset_posture(&svc, LOCAL);
    let after = svc.fabric_router.asset_posture(LOCAL).unwrap();
    assert_eq!(after.posture, FleetPosture::LockedOut);
    assert_eq!(
        after.blocked_by,
        vec!["VERIFIER_FLEET_POSTURE_LOCKED_OUT".to_string()],
        "LockedOut feed must tag the reason for operators"
    );
}
