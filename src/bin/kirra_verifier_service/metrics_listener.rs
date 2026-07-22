// metrics_listener.rs — the dedicated `/metrics` OPERATIONS listener (#1123,
// the #793 F2 remainder; retires AOU-METRICS-SEGMENTATION-001 when enabled).
//
// WHY: `/metrics` is unauthenticated + posture-exempt so the Prometheus scrape
// survives LockedOut — but serving it on the COMMAND-PLANE port
// (`KIRRA_VERIFIER_ADDR`) makes fleet posture / HA role / denial telemetry a
// reconnaissance surface for any peer that can reach the API. The industry
// norm is diagnostics on a dedicated ops port. This module serves exactly one
// route on a second listener bound to `KIRRA_METRICS_ADDR` (an ops/management
// interface or loopback behind a scrape proxy), and — MOVE, not copy — the
// command-plane router then answers `/metrics` with 404 + a pointer, so the
// exposure the AoU records actually ENDS when the listener is on.
//
// Semantics (all fail-closed, matching the KIRRA_HTTP_* conventions):
//   unset/empty  → OFF: `/metrics` stays on the main router, byte-identical,
//                  and AOU-METRICS-SEGMENTATION-001's obligation stands.
//   valid addr   → ON: ops listener serves `/metrics`; main port 404s it.
//   invalid addr → startup ABORT (never a silent fallback to the exposed port).
//
// The ops router gets its own small backpressure pool (the console-pool
// precedent: a scrape storm cannot touch the API/console pools, and vice
// versa) — sized by KIRRA_METRICS_MAX_CONCURRENCY (default 16). Plaintext by
// design: the whole point of the AoU is that this listener lives on a trusted
// ops segment; in-process TLS remains the command listener's concern
// (TRANSPORT_SECURITY.md §4).

use super::*;

/// Default concurrency for the ops listener's own shed pool. A Prometheus
/// scrape is one request per interval; 16 absorbs scrape-federation fan-in.
const METRICS_POOL_DEFAULT: usize = 16;
/// `/metrics` is GET-only; any body is noise. Cap tiny.
const METRICS_MAX_BODY_BYTES: usize = 1024;

/// Parse `KIRRA_METRICS_ADDR`. Pure (tested below): `None`/empty → off;
/// a valid socket address → on; anything else → an operator-readable error
/// the caller turns into a startup abort (a typo must never silently leave
/// `/metrics` on the command plane when the operator asked to move it).
pub(crate) fn parse_metrics_addr(
    raw: Option<&str>,
) -> Result<Option<std::net::SocketAddr>, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) => v.parse::<std::net::SocketAddr>().map(Some).map_err(|e| {
            format!(
                "KIRRA_METRICS_ADDR is set but not a valid socket address \
                 (got {v:?}: {e}) — aborting startup rather than silently \
                 keeping /metrics on the command-plane port"
            )
        }),
    }
}

/// The ops-plane router: exactly `/metrics`, in its own shed pool. Everything
/// else on this listener is 404 by construction — the ops port must never
/// grow command-plane surface by accident.
pub(crate) fn build_metrics_router(svc: Arc<ServiceState>) -> Router {
    let pool = backpressure::env_limit_or("KIRRA_METRICS_MAX_CONCURRENCY", METRICS_POOL_DEFAULT);
    backpressure::with_backpressure(
        Router::new().route("/metrics", get(metrics_endpoint)),
        pool,
        METRICS_MAX_BODY_BYTES,
    )
    .with_state(svc)
}

/// What the COMMAND-PLANE port serves at `/metrics` once the ops listener is
/// on: a 404 with a pointer, so a misconfigured scraper fails loud and
/// diagnosable instead of silently keeping the recon surface alive.
pub(crate) async fn metrics_moved_to_ops_listener() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "metrics are served on the dedicated operations listener \
                      (KIRRA_METRICS_ADDR) — point the scraper there"
        })),
    )
}

/// Bind + spawn the ops listener. Bind failure is a startup ABORT signal
/// (`Err`), exactly like the main listener — a requested-but-unbound ops
/// plane must not degrade to command-plane serving. The served task drains
/// on the same OS shutdown signal as the main server (the WAL checkpoint
/// stays the main server's shutdown job — one checkpoint, not two).
pub(crate) async fn bind_and_spawn(
    addr: std::net::SocketAddr,
    svc: Arc<ServiceState>,
) -> Result<(), ()> {
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(err) => {
            tracing::error!(
                error = %err,
                metrics_addr = %addr,
                "startup failed: could not bind the dedicated /metrics ops listener (fail-closed)"
            );
            return Err(());
        }
    };
    println!("Kirra Verifier /metrics ops listener on {addr} (command-plane /metrics now 404s)");
    let router = build_metrics_router(svc);
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await
        {
            tracing::error!(error = %err, "metrics ops listener exited with error");
        }
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests — the parse policy, the move semantics on both routers, and the
// ops router's single-route surface.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod metrics_listener_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use kirra_persistence::VerifierStore;
    use kirra_verifier::posture_cache::{CachedFleetPosture, SharedPostureCache};
    use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use tower::ServiceExt; // oneshot

    fn test_state() -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
            CachedFleetPosture::new(FleetPosture::Nominal),
        )));
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
            perception_cap: kirra_core::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
            last_actuator_verdict: kirra_verifier::posture_cache::empty_last_verdict_cell(),
        })
    }

    async fn get_status_body(router: Router, path: &str) -> (StatusCode, String) {
        let resp = router
            .oneshot(
                HttpRequest::builder()
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[test]
    fn parse_unset_and_empty_are_off() {
        assert_eq!(parse_metrics_addr(None).unwrap(), None);
        assert_eq!(parse_metrics_addr(Some("")).unwrap(), None);
        assert_eq!(parse_metrics_addr(Some("   ")).unwrap(), None);
    }

    #[test]
    fn parse_valid_addr_is_on() {
        assert_eq!(
            parse_metrics_addr(Some("127.0.0.1:9090")).unwrap(),
            Some("127.0.0.1:9090".parse().unwrap())
        );
    }

    #[test]
    fn parse_invalid_addr_is_a_startup_abort_signal() {
        for bad in ["9090", "localhost:9090", "not-an-addr", "127.0.0.1:"] {
            let err = parse_metrics_addr(Some(bad)).unwrap_err();
            assert!(err.contains("KIRRA_METRICS_ADDR"), "{bad}: {err}");
        }
    }

    #[tokio::test]
    async fn ops_router_serves_the_real_exposition() {
        let (status, body) = get_status_body(build_metrics_router(test_state()), "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("kirra_fleet_posture"),
            "the ops listener must serve the real exposition, got: {body:.100}"
        );
    }

    #[tokio::test]
    async fn ops_router_surface_is_metrics_only() {
        // The ops port must never grow command-plane surface: everything but
        // /metrics is 404 (no /health, no /fleet/*, no console).
        for path in ["/health", "/ready", "/fleet/posture", "/console", "/"] {
            let (status, _) = get_status_body(build_metrics_router(test_state()), path).await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{path} must 404 on the ops listener"
            );
        }
    }

    #[tokio::test]
    async fn main_router_moves_metrics_when_ops_listener_is_on() {
        // MOVE, not copy: with the ops listener enabled the command-plane
        // router 404s /metrics with the pointer body; disabled keeps it live
        // (the LockedOut-scrape DoD test pins that path separately).
        let on = super::super::build_app(test_state(), None, true);
        let (status, body) = get_status_body(on, "/metrics").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("KIRRA_METRICS_ADDR"), "{body}");

        let off = super::super::build_app(test_state(), None, false);
        let (status, body) = get_status_body(off, "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("kirra_fleet_posture"));
    }
}
