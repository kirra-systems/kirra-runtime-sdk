//! PROOF — the Kirra governor closes the loop and CHANGES THE OUTCOME on a moving
//! (sim) vehicle. Headless, deterministic, CI-able: no GPU, no CARLA, no live HTTP
//! server. Commands are driven through the REAL gateway enforcement
//! (`enforce_actuator_safety_envelope`) in-process via axum `oneshot` — the same
//! `/actuator/motion/command` path the production service runs — and the enforced
//! result is integrated by the bicycle-model simulator (`kinematics_sim`).
//!
//! THE CLAIM is not "the loop runs" — it is "the governor changes what the
//! vehicle does." So every governed assertion is paired with a NEGATIVE CONTROL:
//! the SAME adversarial commands run with the governor BYPASSED (raw commands
//! integrated directly). The DIFFERENCE between governed and bypassed is the
//! evidence — the same rigor as the Phase 0 negative control. A governed run that
//! passes is not enough on its own; the on-vs-off delta is the proof.
//!
//! TWO AXES (different governor paths):
//!   - Per-command actuator gating: /actuator/motion/command enforcement.
//!   - Fleet-posture fault response: sensor/RSS degradation → posture engine →
//!     safe behavior; proven in two rigorous halves (fault→posture via the real
//!     engine; posture→safe-vehicle via the real gateway), composed.
//!
//! What this is NOT: CARLA physics/perception fidelity and Autoware-as-planner
//! are fidelity follow-ons (GPU/sim-heavy, and a realistic planner proves
//! integration, not enforcement — it won't issue the deliberate violations this
//! needs). This headless kinematic proof is the CI-gated centerpiece.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Extension, Json, Router};
use serde_json::Value;
use tower::ServiceExt; // oneshot

use kirra_runtime_sdk::gateway::kinematics_contract::{
    ProposedVehicleCommand, VehicleKinematicsContract,
};
use kirra_runtime_sdk::gateway::policy_layer::{
    enforce_actuator_safety_envelope, EnforcementOutcome,
};
use kirra_runtime_sdk::kinematics_sim::VehicleState;
use kirra_runtime_sdk::posture_cache::{
    CachedFleetPosture, ServiceState, SharedPostureCache,
};
use kirra_runtime_sdk::scenario_runner::{PostureAssertion, ScenarioEvent, ScenarioRunner};
use kirra_runtime_sdk::verifier::{
    AppState, FleetPosture, NodeTrustState, RegisteredNode, VerifierOperationMode,
};
use kirra_runtime_sdk::verifier_store::VerifierStore;

const DT_S: f64 = 0.05;
const TOL: f64 = 1e-6;

// ---------------------------------------------------------------------------
// Harness: real gateway enforcement via oneshot + bicycle-model integration
// ---------------------------------------------------------------------------

/// Probe handler: reads the threaded `EnforcementOutcome` and returns the
/// canonical response body — byte-identical to the production handler's response
/// path (`handle_actuator_motion_command`).
async fn echo_outcome(Extension(outcome): Extension<EnforcementOutcome>) -> Json<Value> {
    Json(outcome.response_body())
}

fn service_state(posture: FleetPosture) -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let cache: SharedPostureCache =
        Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture::new(posture))));
    service_state_from(app, cache)
}

fn service_state_from(app: Arc<AppState>, cache: SharedPostureCache) -> Arc<ServiceState> {
    Arc::new(ServiceState {
        app,
        posture_cache: cache,
        audit_verifying_key: None,
        fabric_router: Arc::new(kirra_runtime_sdk::fabric::router::FabricRouter::new()),
        fabric_telemetry: Arc::new(kirra_runtime_sdk::fabric::telemetry::FabricTelemetry::new()),
        fabric_causal_log: Arc::new(kirra_runtime_sdk::fabric::causal_log::FabricCausalLog::new(None)),
        posture_engine_tx: std::sync::OnceLock::new(),
    })
}

fn gateway_router(svc: Arc<ServiceState>) -> Router {
    Router::new()
        .route("/actuator/motion/command", post(echo_outcome))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc),
            enforce_actuator_safety_envelope,
        ))
        .with_state(svc)
}

/// One enforced verdict from the real gateway.
struct Enforced {
    denied: bool,
    clamped: bool,
    v: f64,
    delta: f64,
}

async fn enforce_once(router: &Router, cmd: &ProposedVehicleCommand) -> Enforced {
    let body = serde_json::to_vec(cmd).unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/actuator/motion/command")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("router must not panic");
    let status = resp.status();
    if status == StatusCode::OK {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let action = v["action"].as_str().unwrap_or("").to_string();
        Enforced {
            denied: false,
            clamped: action.starts_with("Clamp"),
            v: v["enforced_linear_velocity_mps"].as_f64().unwrap(),
            delta: v["enforced_steering_angle_deg"].as_f64().unwrap(),
        }
    } else {
        // 403 (LockedOut) / 400 (DenyBreach) → command rejected, vehicle holds.
        Enforced { denied: true, clamped: false, v: 0.0, delta: 0.0 }
    }
}

#[derive(Debug, Default)]
struct Trajectory {
    peak_speed: f64,
    peak_lateral: f64,
    clamp_count: usize,
    deny_count: usize,
    max_enforced_speed: f64,
    max_enforced_steer: f64,
}

/// GOVERNED run: each command goes through the REAL gateway; the enforced result
/// is integrated. On deny (403/400) the vehicle holds (safe-stop).
async fn governed_run(
    svc: Arc<ServiceState>,
    wb: f64,
    commands: &[ProposedVehicleCommand],
) -> Trajectory {
    let router = gateway_router(svc);
    let mut state = VehicleState::at_rest();
    let mut tr = Trajectory::default();
    for cmd in commands {
        let c = ProposedVehicleCommand {
            current_velocity_mps: state.velocity_mps,
            current_steering_angle_deg: state.steering_angle_deg,
            ..cmd.clone()
        };
        let e = enforce_once(&router, &c).await;
        if e.denied {
            tr.deny_count += 1;
            // vehicle holds — no step.
        } else {
            if e.clamped {
                tr.clamp_count += 1;
            }
            tr.max_enforced_speed = tr.max_enforced_speed.max(e.v.abs());
            tr.max_enforced_steer = tr.max_enforced_steer.max(e.delta.abs());
            let enforced = ProposedVehicleCommand {
                linear_velocity_mps: e.v,
                steering_angle_deg: e.delta,
                ..c.clone()
            };
            state = state.step(&enforced, wb);
        }
        tr.peak_speed = tr.peak_speed.max(state.velocity_mps.abs());
        tr.peak_lateral = tr.peak_lateral.max(state.lateral_accel_mps2(wb));
    }
    tr
}

/// NEGATIVE CONTROL: the SAME commands integrated RAW — no governor in the loop.
fn bypassed_run(wb: f64, commands: &[ProposedVehicleCommand]) -> Trajectory {
    let mut state = VehicleState::at_rest();
    let mut tr = Trajectory::default();
    for cmd in commands {
        let c = ProposedVehicleCommand {
            current_velocity_mps: state.velocity_mps,
            current_steering_angle_deg: state.steering_angle_deg,
            ..cmd.clone()
        };
        state = state.step(&c, wb);
        tr.peak_speed = tr.peak_speed.max(state.velocity_mps.abs());
        tr.peak_lateral = tr.peak_lateral.max(state.lateral_accel_mps2(wb));
    }
    tr
}

fn cmd(v: f64, delta: f64) -> ProposedVehicleCommand {
    ProposedVehicleCommand {
        linear_velocity_mps: v,
        current_velocity_mps: 0.0,
        delta_time_s: DT_S,
        steering_angle_deg: delta,
        current_steering_angle_deg: 0.0,
    }
}

/// Adversarial sequence: a sustained over-SPEED phase then a sustained
/// over-STEER phase — deliberately envelope-violating on both axes.
fn adversarial_commands() -> Vec<ProposedVehicleCommand> {
    let mut v = Vec::new();
    for _ in 0..40 {
        v.push(cmd(100.0, 0.0)); // over-speed: 100 m/s vs 35 ceiling
    }
    for _ in 0..40 {
        v.push(cmd(15.0, 80.0)); // over-steer: 80° at 15 m/s → enormous lateral accel
    }
    v
}

// ===========================================================================
// AXIS 1 — per-command actuator gating + loop-level trajectory consequence,
//          with the governed-vs-bypassed negative control as the centerpiece.
// ===========================================================================

#[tokio::test]
async fn axis1_governor_gates_violations_and_bounds_the_trajectory_vs_bypassed() {
    let contract = VehicleKinematicsContract::nominal_reference_profile();
    let wb = contract.wheelbase_m;
    let max_speed = contract.effective_max_speed_mps();
    let max_lateral = contract.max_lateral_accel_mps2;
    let commands = adversarial_commands();

    // GOVERNED: through the real /actuator/motion/command gateway (Nominal).
    let gov = governed_run(service_state(FleetPosture::Nominal), wb, &commands).await;
    // BYPASSED negative control: same commands, raw.
    let raw = bypassed_run(wb, &commands);

    // (1) Violations are GATED — the governor clamped (the over-speed and
    //     over-steer commands were not passed through untouched).
    assert!(gov.clamp_count > 0, "governor must clamp the violating commands");
    // ...and every governed command honors the envelope per-command.
    assert!(
        gov.max_enforced_speed <= max_speed + TOL,
        "enforced speed {} exceeded ceiling {}", gov.max_enforced_speed, max_speed
    );

    // (2) The integrated TRAJECTORY stays within the safe envelope across the run
    //     — the loop-level consequence, not just the per-command check.
    assert!(
        gov.peak_speed <= max_speed + TOL,
        "governed trajectory peak speed {} exceeded {}", gov.peak_speed, max_speed
    );
    assert!(
        gov.peak_lateral <= max_lateral + TOL,
        "governed trajectory peak lateral {} exceeded {}", gov.peak_lateral, max_lateral
    );

    // NEGATIVE CONTROL — the SAME commands WITHOUT the governor WOULD violate.
    assert!(
        raw.peak_speed > max_speed + TOL,
        "bypassed run must violate the speed envelope (got {} vs {})", raw.peak_speed, max_speed
    );
    assert!(
        raw.peak_lateral > max_lateral + TOL,
        "bypassed run must violate the lateral envelope (got {} vs {})", raw.peak_lateral, max_lateral
    );

    // THE PROOF — the on-vs-off DELTA. The governor changed the outcome.
    assert!(
        raw.peak_speed > gov.peak_speed + TOL,
        "governor must reduce peak speed ({} bypassed vs {} governed)", raw.peak_speed, gov.peak_speed
    );
    assert!(
        raw.peak_lateral > gov.peak_lateral + TOL,
        "governor must reduce peak lateral accel ({} bypassed vs {} governed)",
        raw.peak_lateral, gov.peak_lateral
    );
}

// ===========================================================================
// AXIS 2 — fleet-posture fault response → safe vehicle behavior.
//   2a: a fault drives the real posture engine to a SAFE posture.
//   2b: that posture drives the real gateway to SAFE vehicle behavior.
//   Composed (2a ∧ 2b) = "the fault makes the vehicle go safe", with the
//   no-fault (Nominal) run as the negative control.
// ===========================================================================

fn register_av_infra(app: &Arc<AppState>) {
    {
        let store = app.store.lock().unwrap();
        for (id, ty, hw) in [
            ("lidar_front", "Perception", "LIDAR-001"),
            ("camera_front", "Perception", "CAM-001"),
            ("perception_fusion", "Planning", "FUSION-001"),
            ("trajectory_planner", "Planning", "PLAN-001"),
        ] {
            let _ = store.register_av_subsystem_meta(id, ty, hw, 0.70, 0);
        }
    }
    app.dependency_graph.insert(
        "perception_fusion".to_string(),
        vec!["lidar_front".to_string(), "camera_front".to_string()],
    );
    app.dependency_graph.insert(
        "trajectory_planner".to_string(),
        vec!["perception_fusion".to_string()],
    );
    for id in ["lidar_front", "camera_front", "perception_fusion", "trajectory_planner"] {
        app.nodes.insert(id.to_string(), RegisteredNode {
            node_id: id.to_string(),
            status: NodeTrustState::Trusted,
            registered_at_ms: 0,
            last_trust_update_ms: 0,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
        });
    }
}

fn cruise_commands(v: f64, steps: usize) -> Vec<ProposedVehicleCommand> {
    (0..steps).map(|_| cmd(v, 0.0)).collect()
}

/// 2a — a SENSOR fault drives the posture engine to LockedOut (real DAG).
#[tokio::test]
async fn axis2a_sensor_fault_drives_posture_to_locked_out() {
    let store = VerifierStore::new(":memory:").unwrap();
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    register_av_infra(&app);
    let cache: SharedPostureCache =
        Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture::new(FleetPosture::Nominal))));

    // A critical perception node faults → Untrusted propagates LockedOut up the DAG.
    ScenarioRunner::new(Arc::clone(&app), Arc::clone(&cache))
        .at_ms(0, ScenarioEvent::MarkUntrusted {
            node_id: "lidar_front".to_string(),
            reason: "hardware_fault".to_string(),
        })
        .assert_at_ms(0, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(0, PostureAssertion::FleetPostureIs(FleetPosture::LockedOut))
        .run()
        .await;
}

/// 2a' — an RSS safe-distance fault drives the posture engine to Degraded.
#[tokio::test]
async fn axis2a_rss_fault_drives_posture_to_degraded() {
    let store = VerifierStore::new(":memory:").unwrap();
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let cache: SharedPostureCache =
        Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture::new(FleetPosture::Nominal))));

    ScenarioRunner::new(Arc::clone(&app), Arc::clone(&cache))
        .at_ms(0, ScenarioEvent::RssReport(parko_core::RssState {
            safe: false,
            longitudinal_margin: 0.0,
            lateral_margin: f64::MAX,
        }))
        .assert_at_ms(0, PostureAssertion::FleetPostureIs(FleetPosture::Degraded))
        .run()
        .await;
}

/// 2b — Degraded posture (the RSS fault's result) makes the gateway clamp the
///      vehicle to the MRC crawl envelope; Nominal (no fault) does not. The
///      delta is the fault-driven safe response.
#[tokio::test]
async fn axis2b_degraded_posture_holds_vehicle_to_mrc_vs_nominal() {
    let wb = VehicleKinematicsContract::mrc_fallback_profile().wheelbase_m;
    let mrc_ceiling = VehicleKinematicsContract::mrc_fallback_profile().effective_max_speed_mps();
    // Enough steps for the Nominal accel-limiter to ramp the 10 m/s cruise well
    // past the 5 m/s MRC crawl. Degraded clamps to 5 immediately (P2 ceiling), so
    // the two plateau apart — the fault-driven delta.
    let cruise = cruise_commands(10.0, 200);

    // Fault-driven (Degraded): MRC clamp holds the vehicle to the crawl ceiling.
    let degraded = governed_run(service_state(FleetPosture::Degraded), wb, &cruise).await;
    assert!(
        degraded.peak_speed <= mrc_ceiling + TOL,
        "Degraded must hold the vehicle to the MRC crawl ({} <= {})", degraded.peak_speed, mrc_ceiling
    );
    assert!(degraded.clamp_count > 0, "Degraded cruise above MRC must be clamped");

    // Negative control (Nominal, no fault): the SAME cruise runs at full 10 m/s.
    let nominal = governed_run(service_state(FleetPosture::Nominal), wb, &cruise).await;
    assert!(
        nominal.peak_speed > mrc_ceiling + TOL,
        "Nominal cruise should exceed the MRC ceiling ({} > {})", nominal.peak_speed, mrc_ceiling
    );
    // The fault-driven posture is what produced the safe behavior.
    assert!(
        nominal.peak_speed > degraded.peak_speed + TOL,
        "the fault-driven Degraded posture must reduce vehicle speed ({} nominal vs {} degraded)",
        nominal.peak_speed, degraded.peak_speed
    );
}

/// 2b' — LockedOut posture (the sensor fault's result) makes the gateway DENY
///       every command; the vehicle comes to and holds a safe stop.
#[tokio::test]
async fn axis2b_locked_out_posture_holds_vehicle_stopped_vs_nominal() {
    let wb = VehicleKinematicsContract::nominal_reference_profile().wheelbase_m;
    let cruise = cruise_commands(10.0, 40);

    let locked = governed_run(service_state(FleetPosture::LockedOut), wb, &cruise).await;
    assert_eq!(locked.deny_count, cruise.len(), "LockedOut must deny every command");
    assert!(
        locked.peak_speed <= TOL,
        "LockedOut must hold the vehicle stopped (peak speed {})", locked.peak_speed
    );

    // Negative control: Nominal moves the vehicle on the same commands.
    let nominal = governed_run(service_state(FleetPosture::Nominal), wb, &cruise).await;
    assert!(
        nominal.peak_speed > 1.0,
        "Nominal must move the vehicle on the same cruise (peak speed {})", nominal.peak_speed
    );
}
