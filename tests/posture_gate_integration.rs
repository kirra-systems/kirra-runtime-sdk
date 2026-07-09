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

use kirra_verifier::gateway::policy_layer::enforce_posture_routing;
use kirra_verifier::posture_cache::{CachedFleetPosture, ServiceState, SharedPostureCache};
use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

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
    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(initial));
    Arc::new(ServiceState {
        app,
        posture_cache,
        started_at_ms: kirra_verifier::posture_cache::now_ms(),
        audit_verifying_key: None,
        fabric_router: Arc::new(kirra_verifier::fabric::router::FabricRouter::new()),
        fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
        fabric_causal_log: Arc::new(
            kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None),
        ),
        posture_engine_tx: std::sync::OnceLock::new(),
        perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
    })
}

fn claim_epoch(svc: &ServiceState, observed: u64, holder: &str, now_ms: u64) -> u64 {
    let claimed = svc
        .app
        .store
        .with(|store| store.try_claim_epoch(observed, holder, now_ms))
        .expect("epoch claim sql")
        .expect("epoch claim wins");
    svc.app.held_epoch.store(claimed, Ordering::SeqCst);
    claimed
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

    let read_routes = Router::new().route("/fleet/posture", get(ok_handler));

    let actuator_routes = Router::new().route("/actuator/motion/command", post(ok_handler));

    // A generic state-write route (classifies as WriteState, has NO inner
    // kinematic gate) — used to prove Option A relaxes ONLY the inner-gated
    // actuator route under Degraded, while every other write stays 503.
    let generic_write_routes = Router::new().route("/fleet/dependencies", post(ok_handler));

    Router::new()
        .merge(probe_routes)
        .merge(read_routes)
        .merge(actuator_routes)
        .merge(generic_write_routes)
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

// An OPTIONS request carrying the two headers a browser CORS preflight always
// sends (`Origin` + `Access-Control-Request-Method`) — what the posture-gate
// bypass keys on (#696 / Copilot PR #710).
async fn options_preflight_status(app: Router, path: &str) -> StatusCode {
    let req = Request::builder()
        .method("OPTIONS")
        .uri(path)
        .header("origin", "https://dashboard.example")
        .header("access-control-request-method", "GET")
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
        let svc = build_state_with_posture(posture);
        let status = req_status(build_test_app(svc), "PATCH", "/fleet/posture").await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "Unknown HTTP method must be denied in posture {posture:?}; got {status}"
        );
    }
}

// ---------------------------------------------------------------------------
// #696 (HT1): a CORS preflight (OPTIONS) must NOT be 503'd by the posture gate
// — it carries no command and authorizes nothing, and must reach the CorsLayer.
// Before the fix, OPTIONS classified as Unknown → 503 in every posture. The test
// app has no CorsLayer, so a gate-passed OPTIONS reaches the router and 405s
// (no OPTIONS handler) — the point is only that it is NOT the gate's 503.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_options_preflight_not_blocked_by_posture_gate() {
    for posture in [
        FleetPosture::Nominal,
        FleetPosture::Degraded,
        FleetPosture::LockedOut,
    ] {
        let svc = build_state_with_posture(posture);
        let status = options_preflight_status(build_test_app(svc), "/fleet/posture").await;
        assert_ne!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "a CORS preflight (OPTIONS) must pass the posture gate in {posture:?}, not 503; got {status}"
        );
    }
}

// #696 / Copilot PR #710: the bypass is scoped to an ACTUAL preflight. A bare
// OPTIONS WITHOUT the preflight headers is not a CORS preflight and must stay
// subject to the posture gate (OPTIONS classifies as Unknown → 503 in every
// posture), so the exemption can't widen into a posture-gate hole if a route
// ever serves OPTIONS directly.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_bare_options_without_preflight_headers_is_gated() {
    for posture in [
        FleetPosture::Nominal,
        FleetPosture::Degraded,
        FleetPosture::LockedOut,
    ] {
        let svc = build_state_with_posture(posture);
        let status = req_status(build_test_app(svc), "OPTIONS", "/fleet/posture").await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "a non-preflight OPTIONS must remain posture-gated in {posture:?}; got {status}"
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
    let read_status = req_status(build_test_app(build_state_cold()), "GET", "/fleet/posture").await;
    assert_eq!(
        read_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "Cold-start functional READ must be denied 503; got {read_status}"
    );

    let health_status = req_status(build_test_app(build_state_cold()), "GET", "/health").await;
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
// 4. Degraded posture (Option A / ADR-0011): the inner-gated actuator-motion
// command is DEFERRED past the outer gate to its inner kinematic envelope
// (here a stub returns 200, proving the gate stepped aside), while EVERY OTHER
// WriteState — one with no inner gate — is still denied 503. This is the
// authoritative auth-free proof of the Option A relaxation: the representative
// router carries only `enforce_posture_routing`, so the actuator 200 vs generic
// 503 contrast is the posture gate's decision, not masked by auth or the
// envelope. (`should_route_command` Degraded now admits ReadTelemetry +
// ActuatorMotion only.)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_degraded_defers_actuator_motion_but_blocks_other_writes() {
    let svc = build_state_with_posture(FleetPosture::Degraded);
    let now = kirra_verifier::posture_cache::now_ms();
    assert_eq!(
        claim_epoch(&svc, 0, "primary", now),
        1,
        "the actuator-path proof must satisfy the live epoch fence before exercising posture deferral"
    );

    // The inner-gated actuator route passes the outer gate under Degraded.
    let actuator = req_status(
        build_test_app(Arc::clone(&svc)),
        "POST",
        "/actuator/motion/command",
    )
    .await;
    assert_eq!(
        actuator,
        StatusCode::OK,
        "Degraded must DEFER /actuator/motion/command to the inner gate (Option A); got {actuator}"
    );

    // A generic write (no inner kinematic gate) is still denied.
    let generic = req_status(
        build_test_app(build_state_with_posture(FleetPosture::Degraded)),
        "POST",
        "/fleet/dependencies",
    )
    .await;
    assert_eq!(
        generic,
        StatusCode::SERVICE_UNAVAILABLE,
        "Degraded must still block a generic WriteState with no inner gate; got {generic}"
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
    let metrics_status = req_status(
        build_test_app(build_state_with_posture(FleetPosture::LockedOut)),
        "GET",
        "/metrics",
    )
    .await;
    assert_ne!(
        metrics_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "/metrics is exempt — gate must not have blocked it (got 503); the \
         route may 404 if not mounted, which still proves the gate stepped aside"
    );
}

// ---------------------------------------------------------------------------
// 1b. Actuator-path LIVE epoch fence. The actuator command performs no durable
// write (so no in-transaction epoch re-check is possible); it now reads the LIVE
// durable epoch on the admit path instead of a ~2s-stale cached value. The owner
// passes; a superseded primary is fenced 503 and self-demotes IMMEDIATELY —
// closing the failover two-writer window the cached fence left open.
// ---------------------------------------------------------------------------

use std::sync::atomic::Ordering;

#[tokio::test]
async fn test_actuator_live_epoch_fence_admits_the_owner() {
    let svc = build_state_with_posture(FleetPosture::Nominal);
    let now = kirra_verifier::posture_cache::now_ms();
    // Claim epoch 1 durably, and hold it in memory (this instance IS the owner).
    let claimed = svc
        .app
        .store
        .with(|s| s.try_claim_epoch(0, "primary", now).unwrap());
    assert_eq!(
        claimed,
        Some(1),
        "fresh in-memory store claims epoch 1 from genesis 0"
    );
    svc.app.held_epoch.store(1, Ordering::SeqCst);

    let status = req_status(build_test_app(svc), "POST", "/actuator/motion/command").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the live epoch owner (held == durable) must pass the actuator fence; got {status}"
    );
}

#[tokio::test]
async fn test_actuator_live_epoch_fence_rejects_a_superseded_primary() {
    let svc = build_state_with_posture(FleetPosture::Nominal);
    let now = kirra_verifier::posture_cache::now_ms();
    // This instance claimed epoch 1 and holds it...
    assert_eq!(
        svc.app
            .store
            .with(|s| s.try_claim_epoch(0, "old", now).unwrap()),
        Some(1)
    );
    svc.app.held_epoch.store(1, Ordering::SeqCst);
    // ...then a NEW primary claims epoch 2 durably (failover). The old primary's
    // in-memory held_epoch (1) is now stale; its cached_db_epoch hasn't caught up.
    assert_eq!(
        svc.app
            .store
            .with(|s| s.try_claim_epoch(1, "new", now).unwrap()),
        Some(2)
    );

    assert!(
        svc.app.is_active(),
        "precondition: still Active before the fenced request"
    );
    let status = req_status(
        build_test_app(Arc::clone(&svc)),
        "POST",
        "/actuator/motion/command",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a superseded primary's actuator command must be fenced 503 by the LIVE read; got {status}"
    );
    assert!(
        !svc.app.is_active(),
        "the fenced actuator request must self-demote the instance (mode_active → false)"
    );
}
