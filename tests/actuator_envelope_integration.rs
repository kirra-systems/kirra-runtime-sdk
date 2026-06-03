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
use axum::{Extension, Json, Router};
use serde_json::Value;
use tower::ServiceExt; // for `oneshot`

use kirra_runtime_sdk::gateway::kinematics_contract::{
    ProposedVehicleCommand, VehicleKinematicsContract,
};
use kirra_runtime_sdk::gateway::policy_layer::{
    enforce_actuator_safety_envelope, enforce_posture_routing, EnforcementOutcome,
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

// ---------------------------------------------------------------------------
// Interceptor schema coherence (Phase 0): the 200 response must carry the keys
// the ROS cmd_vel_interceptor reads (action / enforced_*), accurately. This
// probe handler is byte-identical to handle_actuator_motion_command's response
// path — it reads the threaded EnforcementOutcome and returns response_body().
// ---------------------------------------------------------------------------

async fn echo_outcome(Extension(outcome): Extension<EnforcementOutcome>) -> Json<Value> {
    Json(outcome.response_body())
}

fn build_schema_app(svc: Arc<ServiceState>) -> Router {
    Router::new()
        .route("/actuator/motion/command", post(echo_outcome))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc),
            enforce_actuator_safety_envelope,
        ))
        .with_state(svc)
}

async fn send_json(app: Router, body: Body) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/actuator/motion/command")
        .header("content-type", "application/json")
        .body(body)
        .expect("build request");
    let resp = app.oneshot(req).await.expect("router service must not panic");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read response body");
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

fn cmd_json(linear: f64, current_v: f64, dt: f64, steer: f64, current_s: f64) -> Vec<u8> {
    serde_json::to_vec(&ProposedVehicleCommand {
        linear_velocity_mps: linear,
        current_velocity_mps: current_v,
        delta_time_s: dt,
        steering_angle_deg: steer,
        current_steering_angle_deg: current_s,
    })
    .unwrap()
}

/// An in-envelope command → action "Allow", enforced == original, and the
/// interceptor keys are present.
#[tokio::test]
async fn test_response_schema_allow_carries_interceptor_keys() {
    let svc = build_state_with_posture(FleetPosture::Nominal);
    let (status, v) =
        send_json(build_schema_app(svc), Body::from(cmd_json(10.0, 9.8, 0.1, 2.0, 1.8))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["action"], "Allow");
    // interceptor keys present and equal to the original (no clamp).
    assert_eq!(v["enforced_linear_velocity_mps"], 10.0);
    assert_eq!(v["enforced_steering_angle_deg"], 2.0);
    // legacy keys accurate too.
    assert_eq!(v["enforcement_action"], "Allow");
    assert_eq!(v["linear_velocity_mps"], 10.0);
}

/// THE FIX: an over-speed command is clamped, and the clamp is reported under
/// the key the interceptor reads (`enforced_linear_velocity_mps`) AND
/// `action == "ClampLinear"` — so the interceptor forwards 35.0, not 100.0.
#[tokio::test]
async fn test_response_schema_clamp_linear_is_visible_to_interceptor() {
    let svc = build_state_with_posture(FleetPosture::Nominal);
    let (status, v) =
        send_json(build_schema_app(svc), Body::from(cmd_json(100.0, 30.0, 0.1, 0.0, 0.0))).await;
    assert_eq!(status, StatusCode::OK, "over-speed is clamped, not denied");
    assert_eq!(v["action"], "ClampLinear", "response must report the clamp");

    // The interceptor key MUST be present and carry the clamped value (Nominal
    // ceiling 35.0) — not the original 100.0 it would otherwise forward.
    let enforced = v["enforced_linear_velocity_mps"].as_f64().expect("key present");
    assert!((enforced - 35.0).abs() < 0.01, "clamped to 35.0, got {enforced}");
    assert!(enforced < 100.0);
    // Legacy value key carries the same enforced value (internal consistency).
    assert_eq!(v["linear_velocity_mps"], v["enforced_linear_velocity_mps"]);
    // Original preserved for observability.
    assert_eq!(v["original_linear_velocity_mps"], 100.0);
}

/// Over-steer at low speed → action "ClampSteering", enforced steering visible
/// to the interceptor and below the requested 90°.
#[tokio::test]
async fn test_response_schema_clamp_steering_is_visible_to_interceptor() {
    let svc = build_state_with_posture(FleetPosture::Nominal);
    let (status, v) =
        send_json(build_schema_app(svc), Body::from(cmd_json(2.0, 2.0, 0.1, 90.0, 0.0))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["action"], "ClampSteering");
    let enforced = v["enforced_steering_angle_deg"].as_f64().expect("key present");
    assert!(enforced < 90.0 && enforced > 0.0, "clamped below 90°, sign kept, got {enforced}");
    assert_eq!(v["steering_angle_deg"], v["enforced_steering_angle_deg"]);
    assert_eq!(v["original_steering_angle_deg"], 90.0);
}

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

// ---------------------------------------------------------------------------
// PHASE 0 CAPSTONE — actuator response-schema coherence (#151), CI-gating.
//
// WHAT THIS GUARDS: the gateway once returned a hardcoded "Allow" carrying the
// ORIGINAL velocity/steering even when the envelope clamped; the ROS interceptor
// read the canonical keys, saw no clamp, and forwarded the ORIGINAL unclamped
// command to the motor topic. #151 threads a typed `EnforcementOutcome` and emits
// the clamp under BOTH canonical (`action` / `enforced_*`) and legacy keys. These
// tests FAIL if anyone reintroduces the hardcoded "Allow" or breaks the schema.
//
// HANDLER REACHABILITY (flagged): the real `handle_actuator_motion_command` lives
// in the BINARY crate (src/bin/kirra_verifier_service.rs), which integration tests
// cannot import — they link only against the lib. Its response path is a PURE
// delegation: `(StatusCode::OK, Json(outcome.response_body()))` (verified at the
// handler site). `response_body()` lives in the lib and owns 100% of the wire
// schema. So the `echo_outcome` probe below — `Json(outcome.response_body())`
// behind the real `enforce_actuator_safety_envelope` middleware — is the faithful
// twin of the handler's response, exercised over the full HTTP frame via oneshot.
// This is the prompt-sanctioned `response_body()` fallback; a bin refactor would
// add churn for zero added coverage (the lib already owns the schema).
// ---------------------------------------------------------------------------

/// The keys the ROS interceptor / CARLA client read AS THE COMMAND VALUE — the
/// ones whose corruption WAS the #151 bug. Asserts every one carries something
/// other than the original over-ceiling input. (The `original_*` keys, which
/// intentionally preserve the pre-clamp input for observability, are NOT in this
/// set and are checked separately.)
fn assert_axis_keys_hide_original(v: &Value, enforced_key: &str, legacy_key: &str, original: f64) {
    for key in [enforced_key, legacy_key] {
        let x = v[key].as_f64().unwrap_or_else(|| panic!("interceptor key `{key}` must be present"));
        assert!(
            (x - original).abs() > 1e-9,
            "interceptor-read key `{key}` carries the ORIGINAL unclamped value {original} — \
             this is exactly the #151 bug (clamp dropped at the HTTP boundary)"
        );
    }
}

/// THE FIX, tightest case: an over-speed command in DEGRADED posture clamps to the
/// MRC ceiling (5.0 m/s), and the clamp is visible under both canonical and legacy
/// keys — never the original 100.0. Priority-2 clamps linear exactly and returns,
/// so the ceiling is asserted precisely (computed from the contract, not hardcoded).
#[tokio::test]
async fn test_capstone_degraded_overspeed_clamps_and_hides_original() {
    let svc = build_state_with_posture(FleetPosture::Degraded);
    let ceiling = VehicleKinematicsContract::mrc_fallback_profile().effective_max_speed_mps();
    let original = 100.0;

    let (status, v) =
        send_json(build_schema_app(svc), Body::from(cmd_json(original, 4.0, 0.1, 0.0, 0.0))).await;
    assert_eq!(status, StatusCode::OK, "over-speed is clamped, not denied");

    // Clamp is reported (not a hardcoded "Allow") under both key families.
    assert_eq!(v["action"], "ClampLinear");
    assert_eq!(v["enforcement_action"], "ClampLinear");

    // Canonical + legacy value keys carry the MRC ceiling, not the original.
    let enforced = v["enforced_linear_velocity_mps"].as_f64().expect("canonical key present");
    assert!((enforced - ceiling).abs() < 1e-9, "clamped to MRC ceiling {ceiling}, got {enforced}");
    assert_eq!(v["linear_velocity_mps"], v["enforced_linear_velocity_mps"],
        "legacy value key must equal the enforced (clamped) value");

    // CRUCIAL: no interceptor-read key carries the original 100.0.
    assert_axis_keys_hide_original(&v, "enforced_linear_velocity_mps", "linear_velocity_mps", original);

    // Original is preserved ONLY under the observability key (intentional).
    assert_eq!(v["original_linear_velocity_mps"], original);
}

/// Over-steer in DEGRADED posture: steering is clamped (sign kept), reflected in
/// both canonical and legacy keys, never the original 90°. The exact clamped
/// magnitude is rate-limiter-dependent (P5b), so this asserts clamped-and-bounded
/// rather than an exact ceiling.
#[tokio::test]
async fn test_capstone_degraded_oversteer_clamps_and_hides_original() {
    let svc = build_state_with_posture(FleetPosture::Degraded);
    let original = 90.0;

    let (status, v) =
        send_json(build_schema_app(svc), Body::from(cmd_json(2.0, 2.0, 0.1, original, 0.0))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["action"], "ClampSteering");
    assert_eq!(v["enforcement_action"], "ClampSteering");

    let enforced = v["enforced_steering_angle_deg"].as_f64().expect("canonical key present");
    assert!(enforced > 0.0 && enforced < original,
        "steering clamped below {original}° with sign kept, got {enforced}");
    assert_eq!(v["steering_angle_deg"], v["enforced_steering_angle_deg"],
        "legacy steering key must equal the enforced (clamped) value");

    // CRUCIAL: no interceptor-read steering key carries the original 90°.
    assert_axis_keys_hide_original(&v, "enforced_steering_angle_deg", "steering_angle_deg", original);
    assert_eq!(v["original_steering_angle_deg"], original);
}

/// In-envelope command under the TIGHTER Degraded envelope (3.0 < 5.0 ceiling):
/// action "Allow", values pass through unchanged — proves the clamp path does not
/// over-clamp an admissible command even when the envelope is tight.
#[tokio::test]
async fn test_capstone_degraded_in_envelope_passes_through_unclamped() {
    let svc = build_state_with_posture(FleetPosture::Degraded);
    let (status, v) =
        send_json(build_schema_app(svc), Body::from(cmd_json(3.0, 3.0, 0.1, 1.0, 1.0))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["action"], "Allow");
    assert_eq!(v["enforcement_action"], "Allow");
    assert_eq!(v["enforced_linear_velocity_mps"], 3.0);
    assert_eq!(v["linear_velocity_mps"], 3.0);
    assert_eq!(v["enforced_steering_angle_deg"], 1.0);
    assert_eq!(v["steering_angle_deg"], 1.0);
}

/// FAIL-CLOSED: the handler's signature declares `Extension<EnforcementOutcome>`
/// (same extractor as the real `handle_actuator_motion_command`). Mounted WITHOUT
/// the envelope middleware that inserts it, axum's extractor rejects the request
/// 500 — the handler structurally CANNOT run, so it can never emit a defaulted
/// "Allow". This is the fail-closed contract the #151 fix relies on.
#[tokio::test]
async fn test_capstone_missing_enforcement_outcome_extension_fails_closed_500() {
    let svc = build_state_with_posture(FleetPosture::Nominal);
    let app = Router::new()
        .route("/actuator/motion/command", post(echo_outcome))
        .with_state(svc);
    let status = send(app, Body::from(valid_cmd_json())).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR,
        "missing EnforcementOutcome extension must fail closed 500, never a defaulted Allow");
}
