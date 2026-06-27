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

use kirra_verifier::gateway::kinematics_contract::{
    ProposedVehicleCommand, VehicleKinematicsContract,
};
use kirra_verifier::gateway::policy_layer::{
    enforce_actuator_safety_envelope, EnforcementOutcome,
};
use kirra_verifier::kinematics_sim::VehicleState;
use kirra_verifier::posture_cache::{
    CachedFleetPosture, ServiceState, SharedPostureCache,
};
use kirra_verifier::scenario_runner::{PostureAssertion, ScenarioEvent, ScenarioRunner};
use kirra_verifier::verifier::{
    AppState, FleetPosture, NodeTrustState, RegisteredNode, VerifierOperationMode,
};
use kirra_verifier::verifier_store::VerifierStore;

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
        started_at_ms: kirra_verifier::posture_cache::now_ms(),
        audit_verifying_key: None,
        fabric_router: Arc::new(kirra_verifier::fabric::router::FabricRouter::new()),
        fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
        fabric_causal_log: Arc::new(kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
        posture_engine_tx: std::sync::OnceLock::new(),
        perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
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
    /// Speed at the end of the run — distinguishes "held at stop" (Issue #70
    /// Degraded) from "re-accelerated" (Nominal) when peak speed alone cannot.
    final_speed: f64,
}

/// GOVERNED run: each command goes through the REAL gateway; the enforced result
/// is integrated. On deny (403/400) the vehicle holds (safe-stop).
async fn governed_run(
    svc: Arc<ServiceState>,
    wb: f64,
    commands: &[ProposedVehicleCommand],
) -> Trajectory {
    governed_run_seeded(svc, wb, VehicleState::at_rest(), commands).await
}

/// As `governed_run`, but the vehicle starts from `initial` rather than rest.
/// Used by the Issue #70 decel-to-stop-and-hold axis, where the safe behavior
/// (bleed speed to zero, then HOLD) only manifests from a moving start.
async fn governed_run_seeded(
    svc: Arc<ServiceState>,
    wb: f64,
    initial: VehicleState,
    commands: &[ProposedVehicleCommand],
) -> Trajectory {
    let router = gateway_router(svc);
    let mut state = initial;
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
    tr.final_speed = state.velocity_mps.abs();
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
    app.store.with(|store| {
        for (id, ty, hw) in [
            ("lidar_front", "Perception", "LIDAR-001"),
            ("camera_front", "Perception", "CAM-001"),
            ("perception_fusion", "Planning", "FUSION-001"),
            ("trajectory_planner", "Planning", "PLAN-001"),
        ] {
            let _ = store.register_av_subsystem_meta(id, ty, hw, 0.70, 0);
        }
    });
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
            site: None,
            firmware_version: None,
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
    // A real, Trusted AV DAG → genuine Nominal baseline (matching the sibling
    // sensor-fault test), so the RSS fault escalates Nominal → Degraded. (An
    // EMPTY live set now fails closed to LockedOut — the M-9 guard.)
    register_av_infra(&app);
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

/// 2b — Degraded posture (the RSS fault's result) makes the gateway drive the
///      vehicle to a CONTROLLED DECEL-TO-STOP and then HOLD — it does not
///      sustain a crawl, and it does not re-initiate motion. (Issue #70 —
///      retires the old "Degraded = 5 m/s MRC crawl" behavior in favor of
///      decel-to-stop-and-hold; the Cruise Oct-2023 SF pullover-drag is the
///      motivating incident.) Nominal (no fault) keeps cruising as the
///      negative control.
#[tokio::test]
async fn axis2b_degraded_posture_decels_to_stop_and_holds_vs_nominal() {
    let wb = VehicleKinematicsContract::mrc_fallback_profile().wheelbase_m;

    // The vehicle is already moving at 1 m/s when the fault drives it Degraded.
    // The planner cooperatively commands a stop, the vehicle bleeds speed to
    // zero under the MRC decel bound and HOLDS; then the planner tries to
    // re-accelerate (the unsafe pullover-from-stop the Cruise incident teaches
    // us to refuse).
    let initial = VehicleState::new(0.0, 0.0, 0.0, 1.0);
    let mut commands: Vec<ProposedVehicleCommand> = Vec::new();
    for _ in 0..20 {
        commands.push(cmd(0.0, 0.0));          // cooperative decel + hold at stop
    }
    for _ in 0..20 {
        commands.push(cmd(3.0, 0.0));          // attempt to re-initiate motion
    }

    // Fault-driven (Degraded): bleeds speed to 0, HOLDS, and DENIES every
    // re-initiation attempt.
    let degraded =
        governed_run_seeded(service_state(FleetPosture::Degraded), wb, initial.clone(), &commands).await;
    assert!(
        degraded.peak_speed <= 1.0 + TOL,
        "Degraded must never increase speed beyond the initial 1 m/s (got peak {})",
        degraded.peak_speed
    );
    assert!(
        degraded.final_speed <= TOL,
        "Degraded must come to rest and HOLD (final speed {})", degraded.final_speed
    );
    assert_eq!(
        degraded.deny_count, 20,
        "Degraded must DENY all 20 re-initiation-from-stop attempts (Cruise lesson)"
    );

    // Negative control (Nominal, no fault): the re-acceleration tail is honored
    // — the vehicle is moving again by the end of the run, and nothing is denied.
    let nominal =
        governed_run_seeded(service_state(FleetPosture::Nominal), wb, initial, &commands).await;
    assert_eq!(nominal.deny_count, 0, "Nominal must not deny any of these commands");
    assert!(
        nominal.final_speed > degraded.final_speed + TOL,
        "the fault-driven Degraded posture must keep the vehicle stopped while \
         Nominal re-accelerates (nominal final {} vs degraded final {})",
        nominal.final_speed, degraded.final_speed
    );
}

/// 2b″ — THE CRUISE LESSON, per-command. Under Degraded:
///   - a re-initiation command from a stop is DENIED (the post-stop pullover);
///   - a speed-increase command while moving is DENIED;
///   - a decelerating-toward-zero command within the MRC envelope is ALLOWED.
/// This is the narrow Degraded allow that the old MRC-crawl ceiling would have
/// wrongly permitted (a 3 m/s pullover from a stop sits *under* the 5 m/s
/// crawl ceiling, so the old behavior would have let it through).
#[tokio::test]
async fn axis2b_degraded_denies_reinitiation_and_speed_increase() {
    let router = gateway_router(service_state(FleetPosture::Degraded));

    // (1) Re-initiation from a stop — the Cruise pullover. DENIED.
    let from_stop = ProposedVehicleCommand {
        linear_velocity_mps: 3.0,
        current_velocity_mps: 0.0,
        delta_time_s: DT_S,
        steering_angle_deg: 0.0,
        current_steering_angle_deg: 0.0,
    };
    assert!(
        enforce_once(&router, &from_stop).await.denied,
        "Degraded must deny re-initiation of motion from a stop"
    );

    // (2) Speed increase while moving — DENIED.
    let speed_up = ProposedVehicleCommand {
        linear_velocity_mps: 4.0,
        current_velocity_mps: 2.0,
        delta_time_s: DT_S,
        steering_angle_deg: 0.0,
        current_steering_angle_deg: 0.0,
    };
    assert!(
        enforce_once(&router, &speed_up).await.denied,
        "Degraded must deny a speed increase while moving"
    );

    // (3) Decelerating toward zero within the MRC envelope — ALLOWED.
    let decel = ProposedVehicleCommand {
        linear_velocity_mps: 2.0,
        current_velocity_mps: 3.0,
        delta_time_s: DT_S,
        steering_angle_deg: 0.0,
        current_steering_angle_deg: 0.0,
    };
    let e = enforce_once(&router, &decel).await;
    assert!(
        !e.denied,
        "Degraded must ALLOW a decelerating-toward-zero command within the MRC envelope"
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
