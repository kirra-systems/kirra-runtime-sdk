// local_asset_lockedout_seed_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// #88 tightening — the LOCAL fed asset is seeded fail-closed LockedOut; PEERS
// keep the Degraded interim seed; the feed lifts the local asset on recalc.
// ---------------------------------------------------------------------------

use super::{seed_local_asset_lockedout_inner, sync_local_asset_posture};

use std::sync::Arc;

use kirra_fabric_types::asset::{AssetType, FabricAsset, KinematicProfileType};
use kirra_persistence::VerifierStore;
use kirra_verifier::fabric::router::FabricRouter;
use kirra_verifier::posture_cache::{now_ms, CachedFleetPosture, ServiceState, SharedPostureCache};
use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};

const LOCAL: &str = "av-local";
const PEER: &str = "av-peer";

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

/// ServiceState with LOCAL and PEER registered (both seeded Degraded by
/// `register_asset`), and `cached` as the fleet posture cache.
fn state(cached: Option<CachedFleetPosture>) -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(cached));
    let fabric_router = Arc::new(FabricRouter::new());
    fabric_router.register_asset(&asset(LOCAL));
    fabric_router.register_asset(&asset(PEER));
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
        perception_cap: kirra_core::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
        last_actuator_verdict: kirra_verifier::posture_cache::empty_last_verdict_cell(),
    })
}

fn posture_of(svc: &ServiceState, id: &str) -> FleetPosture {
    svc.fabric_router
        .asset_posture(id)
        .expect("asset registered")
        .posture
}

#[test]
fn local_asset_seeded_lockedout_peer_stays_degraded() {
    let svc = state(None);
    // register_asset seeds BOTH Degraded.
    assert_eq!(posture_of(&svc, LOCAL), FleetPosture::Degraded);
    assert_eq!(posture_of(&svc, PEER), FleetPosture::Degraded);

    // The seed runs once per registered id with LOCAL configured.
    seed_local_asset_lockedout_inner(&svc, LOCAL, Some(LOCAL));
    seed_local_asset_lockedout_inner(&svc, PEER, Some(LOCAL));

    assert_eq!(
        posture_of(&svc, LOCAL),
        FleetPosture::LockedOut,
        "the configured local asset is fail-closed LockedOut"
    );
    assert_eq!(
        posture_of(&svc, PEER),
        FleetPosture::Degraded,
        "peers keep the documented Degraded interim seed"
    );
}

#[test]
fn unset_local_id_leaves_degraded_seed_unchanged() {
    let svc = state(None);
    seed_local_asset_lockedout_inner(&svc, LOCAL, None);
    seed_local_asset_lockedout_inner(&svc, PEER, None);
    assert_eq!(
        posture_of(&svc, LOCAL),
        FleetPosture::Degraded,
        "unset → no local asset to special-case"
    );
    assert_eq!(posture_of(&svc, PEER), FleetPosture::Degraded);
}

#[test]
fn feed_lifts_lockedout_local_asset_on_recalc() {
    // Fresh Nominal fleet posture in the cache (as after the first Active recalc).
    let svc = state(Some(CachedFleetPosture::new_with_generation(
        FleetPosture::Nominal,
        1,
        now_ms(),
    )));
    seed_local_asset_lockedout_inner(&svc, LOCAL, Some(LOCAL));
    assert_eq!(
        posture_of(&svc, LOCAL),
        FleetPosture::LockedOut,
        "starts fail-closed LockedOut"
    );

    // The feed lifts it to the real fleet posture.
    sync_local_asset_posture(&svc, LOCAL);
    assert_eq!(
        posture_of(&svc, LOCAL),
        FleetPosture::Nominal,
        "the feed lifts the local asset out of LockedOut on recalc"
    );
}
