// fabric_command_authoritative_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// #86 — the fabric command endpoint is AUTHORITATIVE: it applies the clamp
// server-side and returns the ENFORCED command (closing the prior fail-open
// where a clamp was reported but not applied). These tests drive the handler
// directly (no auth/router), asserting the response `command` carries the safe
// values, that a clamp is reported, Allow is unchanged, and Deny is denied.
// ---------------------------------------------------------------------------

use super::handle_fabric_command;

use std::sync::Arc;

use axum::body::to_bytes;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;

use kirra_verifier::fabric::asset::{AssetPosture, AssetType, FabricAsset, KinematicProfileType};
use kirra_verifier::fabric::router::FabricRouter;
use kirra_verifier::gateway::kinematics_contract::ProposedVehicleCommand;
use kirra_verifier::posture_cache::{now_ms, ServiceState, SharedPostureCache};
use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

const ASSET: &str = "av-01";

fn svc_with_asset(posture: FleetPosture) -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
    let fabric_router = Arc::new(FabricRouter::new());

    let asset = FabricAsset {
        asset_id: ASSET.to_string(),
        asset_type: AssetType::AutonomousVehicle,
        display_name: ASSET.to_string(),
        kinematic_profile: KinematicProfileType::AutomotiveNominal,
        registered_at_ms: 0,
        last_seen_ms: 0,
        metadata: Default::default(),
    };
    fabric_router.register_asset(&asset);
    // route_command reads the asset's fabric posture; set the one under test.
    fabric_router.update_asset_posture(
        ASSET,
        AssetPosture {
            asset_id: ASSET.to_string(),
            posture,
            generation: 1,
            computed_at_ms: 0,
            contributing_nodes: vec![],
            blocked_by: vec![],
        },
    );

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
    })
}

async fn post_command(svc: Arc<ServiceState>, cmd: ProposedVehicleCommand) -> serde_json::Value {
    let resp = handle_fabric_command(State(svc), Path(ASSET.to_string()), Ok(Json(cmd)))
        .await
        .into_response();
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("json body")
}

fn cmd(linear: f64, current: f64, steering: f64) -> ProposedVehicleCommand {
    ProposedVehicleCommand {
        linear_velocity_mps: linear,
        current_velocity_mps: current,
        delta_time_s: 0.1,
        steering_angle_deg: steering,
        current_steering_angle_deg: steering,
    }
}

#[tokio::test]
async fn clamped_command_response_carries_enforced_values_within_envelope() {
    // 40 m/s exceeds the AutomotiveNominal envelope → ClampLinear.
    let v = post_command(svc_with_asset(FleetPosture::Nominal), cmd(40.0, 34.0, 0.0)).await;

    assert_eq!(v["allowed"], true);
    assert_eq!(
        v["clamp_occurred"], true,
        "a clamp must be reported as enforcement"
    );
    assert_eq!(v["original_linear_velocity_mps"], 40.0);

    let enforced = v["enforced_linear_velocity_mps"]
        .as_f64()
        .expect("enforced velocity");
    assert!(
        enforced < 40.0,
        "enforced velocity must be clamped below the proposal (within envelope)"
    );

    // THE KEY ASSERTION: the authoritative `command` carries the SAFE value,
    // so a client applying it is within envelope even ignoring `action`.
    let cmd_v = v["command"]["linear_velocity_mps"]
        .as_f64()
        .expect("command.linear");
    assert_eq!(
        cmd_v, enforced,
        "response.command must carry the enforced (clamped) velocity"
    );
    assert!(
        cmd_v < 40.0,
        "the returned command is NOT the unclamped 40.0"
    );
}

#[tokio::test]
async fn allow_returns_command_unchanged() {
    // current == proposed → no rate-of-change clamp; within envelope → Allow.
    let v = post_command(svc_with_asset(FleetPosture::Nominal), cmd(10.0, 10.0, 1.0)).await;
    assert_eq!(v["allowed"], true);
    assert_eq!(v["clamp_occurred"], false);
    assert_eq!(v["command"]["linear_velocity_mps"].as_f64().unwrap(), 10.0);
    assert_eq!(v["command"]["steering_angle_deg"].as_f64().unwrap(), 1.0);
}

#[tokio::test]
async fn lockedout_denies_and_omits_command() {
    let v = post_command(
        svc_with_asset(FleetPosture::LockedOut),
        cmd(10.0, 10.0, 0.0),
    )
    .await;
    assert_eq!(v["allowed"], false, "LockedOut denies the command");
    assert!(
        v.get("command").is_none(),
        "a denied command carries no enforced command"
    );
    assert!(
        v["denial_reason"].is_string(),
        "denial is recorded with a reason"
    );
}
