//! Integration tests for `enforce_actuator_safety_envelope` and the
//! `enforce_posture_routing` HA-epoch-fence arm.
//!
//! These exercise the safety arms that pure unit tests of
//! `validate_vehicle_command` cannot reach: middleware-level status codes
//! (403 / 413 / 400 / 503), the audit-row persistence on DenyBreach, and
//! the HA split-brain fence path. Closes MC/DC discovery GAPs 14, 15, 16.
//!
//! Test-only — no production code changes.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use tower::ServiceExt; // for `oneshot`

use kirra_runtime_sdk::gateway::kinematics_contract::ProposedVehicleCommand;
use kirra_runtime_sdk::gateway::policy_layer::{
    enforce_actuator_safety_envelope, enforce_posture_routing,
};
use kirra_runtime_sdk::posture_cache::{
    CachedFleetPosture, ServiceState, SharedPostureCache,
};
use kirra_runtime_sdk::verifier::{AppState, FleetPosture, VerifierOperationMode};
use kirra_runtime_sdk::verifier_store::VerifierStore;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn build_state_with_posture(posture: FleetPosture) -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let posture_cache: SharedPostureCache =
        Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture::new(posture))));
    Arc::new(ServiceState {
        app,
        posture_cache,
        audit_verifying_key: None,
        fabric_router: Arc::new(kirra_runtime_sdk::fabric::router::FabricRouter::new()),
        fabric_telemetry: Arc::new(kirra_runtime_sdk::fabric::telemetry::FabricTelemetry::new()),
        fabric_causal_log: Arc::new(kirra_runtime_sdk::fabric::causal_log::FabricCausalLog::new(None)),
        posture_engine_tx: std::sync::OnceLock::new(),
    })
}

async fn ok_handler() -> &'static str { "ok" }

fn build_actuator_app(svc: Arc<ServiceState>) -> Router {
    Router::new()
        .route("/actuator/motion/command", post(ok_handler))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc),
            enforce_actuator_safety_envelope,
        ))
        .with_state(svc)
}

fn build_posture_gate_app(svc: Arc<ServiceState>) -> Router {
    Router::new()
        .route("/actuator/motion/command", post(ok_handler))
        .with_state(svc.clone())
        .layer(axum::middleware::from_fn_with_state(
            svc,
            enforce_posture_routing,
        ))
}

fn valid_cmd_json() -> Vec<u8> {
    serde_json::to_vec(&ProposedVehicleCommand {
        linear_velocity_mps: 5.0,
        current_velocity_mps: 4.0,
        delta_time_s: 0.1,
        steering_angle_deg: 1.0,
        current_steering_angle_deg: 0.5,
    }).unwrap()
}

async fn send(app: Router, body: Body) -> StatusCode {
    let req = Request::builder()
        .method("POST")
        .uri("/actuator/motion/command")
        .header("content-type", "application/json")
        .body(body)
        .expect("build request");
    app.oneshot(req).await.expect("router service must not panic").status()
}

// ---------------------------------------------------------------------------
// GAP 14: 403 / 413 / 400 arms of enforce_actuator_safety_envelope
// ---------------------------------------------------------------------------

/// SG8 / GAP 14a: LockedOut posture → 403 FORBIDDEN before any body read.
/// Exercises l.88–94 of policy_layer.rs.
#[tokio::test]
async fn test_actuator_envelope_lockedout_returns_403() {
    let svc = build_state_with_posture(FleetPosture::LockedOut);
    let status = send(build_actuator_app(svc), Body::from(valid_cmd_json())).await;
    assert_eq!(status, StatusCode::FORBIDDEN,
        "LockedOut posture must short-circuit to 403, got {status}");
}

/// SG8 / GAP 14b: body exceeding MAX_VEHICLE_COMMAND_BYTES (16 KiB) → 413.
/// Exercises l.105–107 — the bounded `to_bytes` cap.
#[tokio::test]
async fn test_actuator_envelope_oversize_body_returns_413() {
    let svc = build_state_with_posture(FleetPosture::Nominal);
    // 32 KiB of arbitrary bytes — well past the 16 KiB cap.
    let oversize = vec![b'x'; 32 * 1024];
    let status = send(build_actuator_app(svc), Body::from(oversize)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE,
        "body > 16 KiB must be rejected 413, got {status}");
}

/// SG8 / GAP 14c: malformed JSON in the body → 400 BAD_REQUEST.
/// Exercises l.109–110.
#[tokio::test]
async fn test_actuator_envelope_malformed_json_returns_400() {
    let svc = build_state_with_posture(FleetPosture::Nominal);
    let bogus = b"{ this is not json".to_vec();
    let status = send(build_actuator_app(svc), Body::from(bogus)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST,
        "malformed JSON must be rejected 400, got {status}");
}

// ---------------------------------------------------------------------------
// GAP 15: DenyBreach → 400 + audit row appended
// ---------------------------------------------------------------------------

/// SG8 / GAP 15: a DenyBreach (NaN linear velocity) must (1) return 400
/// and (2) append a KINEMATIC_CONTRACT_VIOLATION row to the audit chain.
/// Uses the fallback persist path at l.197–221 (no audit_writer task is
/// installed in this test ServiceState — exactly the documented test
/// pathway).
#[tokio::test]
async fn test_actuator_envelope_deny_breach_persists_audit_row() {
    let svc = build_state_with_posture(FleetPosture::Nominal);
    let app_ref = Arc::clone(&svc.app);

    // Pre-condition: chain is empty.
    let before = {
        let store = app_ref.store.lock().expect("store lock");
        store.load_audit_chain_page(100, 0, None)
            .expect("read page")
            .entries
            .into_iter()
            .filter(|e| e.event_type == "KINEMATIC_CONTRACT_VIOLATION")
            .count()
    };
    assert_eq!(before, 0, "test setup: no KINEMATIC_CONTRACT_VIOLATION rows yet");

    // Submit a non-physical-dt command — Priority-1 InvalidTimeDelta triggers
    // DenyBreach. (NaN can't be expressed in JSON; serde_json renders NaN as
    // `null` and rejects literal NaN — see strict-JSON behavior. A negative
    // dt is the cheapest deterministic DenyBreach that round-trips JSON.)
    let deny_cmd = serde_json::to_vec(&ProposedVehicleCommand {
        linear_velocity_mps: 5.0,
        current_velocity_mps: 4.0,
        delta_time_s: -0.1,
        steering_angle_deg: 0.0,
        current_steering_angle_deg: 0.0,
    }).unwrap();

    let status = send(build_actuator_app(svc), Body::from(deny_cmd)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST,
        "DenyBreach must return 400; got {status}");

    // Post-condition: exactly one new KINEMATIC_CONTRACT_VIOLATION row.
    let after = {
        let store = app_ref.store.lock().expect("store lock");
        store.load_audit_chain_page(100, 0, None)
            .expect("read page")
            .entries
            .into_iter()
            .filter(|e| e.event_type == "KINEMATIC_CONTRACT_VIOLATION")
            .count()
    };
    assert_eq!(after, 1,
        "DenyBreach must persist exactly one audit-chain row (persist-on-deny)");
}

// ---------------------------------------------------------------------------
// GAP 16: HA epoch fence — diverged held/db epoch → 503 + mode_active cleared
// ---------------------------------------------------------------------------

/// SG8 / SG9 / GAP 16: when this instance's held_epoch is non-zero and
/// differs from the cached DB epoch (both observed, both non-zero), the
/// next state-mutating request must be rejected 503 and `mode_active`
/// must be cleared (self-demote). Exercises the fence arm at
/// policy_layer.rs l.296–323.
#[tokio::test]
async fn test_posture_gate_fences_diverged_epoch_and_demotes() {
    let svc = build_state_with_posture(FleetPosture::Nominal);

    // Arrange the fence: held != db, both non-zero.
    svc.app.held_epoch.store(7, Ordering::SeqCst);
    svc.app.cached_db_epoch.store(8, Ordering::SeqCst);
    assert!(svc.app.mode_active.load(Ordering::SeqCst),
        "test precondition: mode_active starts true on an Active instance");

    let status = send(build_posture_gate_app(Arc::clone(&svc)), Body::from(valid_cmd_json())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE,
        "diverged epoch on a mutation must be fenced 503; got {status}");
    assert!(!svc.app.mode_active.load(Ordering::SeqCst),
        "fenced mutation must clear mode_active (self-demote)");
}
