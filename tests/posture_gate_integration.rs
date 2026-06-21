//! Integration test for the command-classification + posture-routing gate
//! (`enforce_posture_routing` in `src/gateway/policy_layer.rs`).
//!
//! Pre-this-test, `classify_http_command` and `should_route_command` were
//! unit-tested and green but had ZERO production callers — the safety
//! property they encode ("Unknown denied in all postures; stale cache
//! blocks everything; posture-gated routing") was therefore NOT enforced
//! on the running router. This is the test that proves rejection through
//! an assembled axum app, not yet another unit test of the pure functions
//! (the unit tests are exactly what masked the gap).
//!
//! TEST-SCOPE NOTE: this out-of-crate integration test cannot see the
//! binary's inline router assembly, so it builds a REPRESENTATIVE router
//! using stub handlers at the same paths the production binary mounts
//! (`/fleet/posture`, `/actuator/motion/command`, `/health`, ...) and
//! applies the same `enforce_posture_routing` middleware as outermost
//! layer with the same `Arc<ServiceState>` shape. The middleware behavior
//! is what's safety-critical here — and that IS what this test exercises.
//!
//! The complementary assertion that the gate is mounted on the ACTUAL
//! assembled production router lives binary-internal (issue #72): the
//! `posture_gate_real_router_tests` module in
//! `src/bin/kirra_verifier_service.rs` drives requests through the
//! extracted `build_app()` — the exact router `main()` serves. That test
//! must stay binary-internal because `build_app` is not exported from the
//! binary crate and is not callable from here.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use tower::ServiceExt; // for `oneshot`

use kirra_runtime_sdk::gateway::policy_layer::enforce_posture_routing;
use kirra_runtime_sdk::posture_cache::{
    CachedFleetPosture, ServiceState, SharedPostureCache,
};
use kirra_runtime_sdk::verifier::{AppState, FleetPosture, VerifierOperationMode};
use kirra_runtime_sdk::verifier_store::VerifierStore;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn build_state_with_posture(posture: FleetPosture) -> Arc<ServiceState> {
    build_state(Some(CachedFleetPosture::new(posture)))
}

fn build_state_cold() -> Arc<ServiceState> {
    build_state(None)
}

fn build_state(initial: Option<CachedFleetPosture>) -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let posture_cache: SharedPostureCache =
        Arc::new(std::sync::RwLock::new(initial));
    Arc::new(ServiceState {
        app,
        posture_cache,
        started_at_ms: kirra_runtime_sdk::posture_cache::now_ms(),
        audit_verifying_key: None,
        fabric_router: Arc::new(kirra_runtime_sdk::fabric::router::FabricRouter::new()),
        fabric_telemetry: Arc::new(kirra_runtime_sdk::fabric::telemetry::FabricTelemetry::new()),
        fabric_causal_log: Arc::new(kirra_runtime_sdk::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
        posture_engine_tx: std::sync::OnceLock::new(),
        perception_cap: kirra_runtime_sdk::gateway::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
    })
}

async fn ok_handler() -> &'static str {
    "ok"
}

/// Builds a router mirroring the production binary's relevant route
/// groups, with stub handlers that return 200 OK. The
/// `enforce_posture_routing` middleware is mounted as the outermost
/// layer with the same shape as in `kirra_verifier_service.rs`.
fn build_test_app(svc: Arc<ServiceState>) -> Router {
    let probe_routes = Router::new()
        .route("/health", get(ok_handler))
        .route("/ready", get(ok_handler));

    let read_routes = Router::new()
        .route("/fleet/posture", get(ok_handler));

    let actuator_routes = Router::new()
        .route("/actuator/motion/command", post(ok_handler));

    Router::new()
        .merge(probe_routes)
        .merge(read_routes)
        .merge(actuator_routes)
        .with_state(svc.clone())
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc),
            enforce_posture_routing,
        ))
}

async fn req_status(app: Router, method: &str, path: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .expect("build request");
    app.oneshot(req)
        .await
        .expect("router service should not panic")
        .status()
}

// ---------------------------------------------------------------------------
// 1. Unknown METHOD on any gated route → 503 (fail-closed) regardless of
// posture. Confirms the SG-006 invariant ("Unknown denied in all postures
// before posture check") is now enforced on the LIVE router.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_unknown_method_returns_503_under_any_posture() {
    for posture in [
        FleetPosture::Nominal,
        FleetPosture::Degraded,
        FleetPosture::LockedOut,
    ] {
        let svc = build_state_with_posture(posture.clone());
        let status = req_status(build_test_app(svc), "PATCH", "/fleet/posture").await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "Unknown HTTP method must be denied in posture {posture:?}; got {status}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Cold start (posture_cache = None): a functional READ is denied 503;
// /health is exempt and still passes (200).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_cold_start_read_denied_but_health_reachable() {
    // Two separate apps because Router consumption by oneshot is not
    // shareable across calls.
    let read_status =
        req_status(build_test_app(build_state_cold()), "GET", "/fleet/posture").await;
    assert_eq!(
        read_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "Cold-start functional READ must be denied 503; got {read_status}"
    );

    let health_status =
        req_status(build_test_app(build_state_cold()), "GET", "/health").await;
    assert_eq!(
        health_status,
        StatusCode::OK,
        "/health is exempt and must remain reachable cold; got {health_status}"
    );
}

// ---------------------------------------------------------------------------
// 3. LockedOut posture: a functional READ is denied 503. This is the
// "LockedOut blocks reads" property — now actually enforced.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_lockedout_blocks_functional_reads() {
    let svc = build_state_with_posture(FleetPosture::LockedOut);
    let status = req_status(build_test_app(svc), "GET", "/fleet/posture").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "LockedOut must block functional read /fleet/posture; got {status}"
    );
}

// ---------------------------------------------------------------------------
// 4. Degraded posture: a WriteState command is denied 503. `should_route_command`
// only permits `ReadTelemetry` under Degraded.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_degraded_blocks_writes() {
    let svc = build_state_with_posture(FleetPosture::Degraded);
    let status = req_status(build_test_app(svc), "POST", "/actuator/motion/command").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "Degraded must block actuator writes (only ReadTelemetry allowed); got {status}"
    );
}

// ---------------------------------------------------------------------------
// 5. Nominal posture: a read route with no auth layer passes the gate
// and returns the handler's 200. Asserts the concrete 200 (not merely
// != 503) so we cannot be fooled by other middlewares emitting 503.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_nominal_read_returns_handler_200() {
    let svc = build_state_with_posture(FleetPosture::Nominal);
    let status = req_status(build_test_app(svc), "GET", "/fleet/posture").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Nominal posture must let GET /fleet/posture through to the handler; got {status}"
    );
}

// ---------------------------------------------------------------------------
// 6. Exempt path under LockedOut is NOT gated. /health is a real route on
// this test router — strong-check it returns 200.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_health_exempt_under_lockedout() {
    let svc = build_state_with_posture(FleetPosture::LockedOut);
    let status = req_status(build_test_app(svc), "GET", "/health").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "/health must remain reachable under LockedOut (exempt); got {status}"
    );

    // Negative check: /metrics is not a route on this test router (the
    // production binary doesn't mount it either today). The gate exempts
    // it nonetheless — so it must NOT return 503; a 404 from axum's
    // route-miss path is the expected non-blocked outcome.
    let metrics_status =
        req_status(build_test_app(build_state_with_posture(FleetPosture::LockedOut)), "GET", "/metrics").await;
    assert_ne!(
        metrics_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "/metrics is exempt — gate must not have blocked it (got 503); the \
         route may 404 if not mounted, which still proves the gate stepped aside"
    );
}
