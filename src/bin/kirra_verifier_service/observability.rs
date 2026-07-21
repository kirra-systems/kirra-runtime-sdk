//! WP-05 (MGA G-10) — request observability: correlation id + tracing span +
//! end-to-end latency histogram.
//!
//! One middleware, mounted OUTERMOST in `build_app` — outside even the
//! posture gate — so every response is observed: posture 503s, load-shed
//! 429s, auth 401s, and successes alike. It makes NO admission decision of
//! any kind (the posture gate remains the outermost *gate*): it stamps a
//! request id, wraps the request in a `tracing` span carrying it (every
//! `tracing` event emitted while handling the request inherits the id — the
//! request-correlation the gap analysis found missing), records the duration
//! into `FleetSafetyMetrics::http_request_latency`, and echoes the id back as
//! the `x-kirra-request-id` response header.
//!
//! A syntactically valid CLIENT-SUPPLIED `x-kirra-request-id` (1–64 chars of
//! `[A-Za-z0-9._-]`) is honored so an upstream caller can stitch its trace
//! through this service; anything else is replaced with a fresh id — a
//! hostile header value must not be able to pollute the log stream or the
//! exposition (the id never becomes a metric label; cardinality discipline
//! is preserved).

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument as _;

use kirra_verifier::posture_cache::ServiceState;

pub(crate) const REQUEST_ID_HEADER: &str = "x-kirra-request-id";

/// Is a client-supplied request id safe to honor? Bounded length, restricted
/// alphabet — never trust free-form header bytes into the log stream.
fn client_id_acceptable(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Mint a fresh request id: 128 random bits (OS CSPRNG via `getrandom`, the
/// same primitive the attestation nonces use — `rand` is dev-only here), hex.
/// Random (not sequential) so ids are collision-free across restarts and HA
/// peers without coordination. On the never-observed entropy-failure path the
/// bytes stay zero — a degraded (colliding) id, never a panic: the id carries
/// no security property, only log correlation.
fn fresh_request_id() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    hex::encode(bytes)
}

pub(crate) async fn request_observability(
    State(svc): State<Arc<ServiceState>>,
    req: Request,
    next: Next,
) -> Response {
    let request_id = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| client_id_acceptable(v))
        .map(str::to_owned)
        .unwrap_or_else(fresh_request_id);

    let span = tracing::info_span!(
        "http_request",
        method = %req.method(),
        path = %req.uri().path(),
        request_id = %request_id,
    );

    let started = Instant::now();
    let mut res = next.run(req).instrument(span).await;
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    svc.app
        .observability
        .fleet_metrics
        .http_request_latency
        .record_micros(micros);

    // The id was validated (or freshly minted as hex), so it is always a
    // legal header value; fall back to dropping the header rather than
    // panicking if that ever stops holding.
    if let Ok(hv) = HeaderValue::from_str(&request_id) {
        res.headers_mut().insert(REQUEST_ID_HEADER, hv);
    }
    res
}

#[cfg(test)]
mod observability_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::get;
    use axum::Router;
    use kirra_verifier::posture_cache::{CachedFleetPosture, SharedPostureCache};
    use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_verifier::verifier_store::VerifierStore;
    use tower::ServiceExt; // for `oneshot`

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

    fn app(svc: Arc<ServiceState>) -> Router {
        Router::new()
            .route("/ping", get(|| async { "pong" }))
            .layer(axum::middleware::from_fn_with_state(
                svc.clone(),
                request_observability,
            ))
            .with_state(svc)
    }

    /// Every response carries a minted request id, and the request's latency
    /// lands in the http histogram.
    #[tokio::test]
    async fn response_carries_request_id_and_latency_is_recorded() {
        let svc = test_state();
        let res = app(svc.clone())
            .oneshot(HttpRequest::get("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let id = res
            .headers()
            .get(REQUEST_ID_HEADER)
            .expect("request id header")
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(id.len(), 32, "minted ids are 128-bit hex: {id}");
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            svc.app
                .observability
                .fleet_metrics
                .http_request_latency
                .observation_count(),
            1
        );
    }

    /// A well-formed client-supplied id is honored (cross-service
    /// correlation); a hostile one is replaced, never echoed.
    #[tokio::test]
    async fn client_id_is_honored_only_when_safe() {
        let svc = test_state();
        let ok = app(svc.clone())
            .oneshot(
                HttpRequest::get("/ping")
                    .header(REQUEST_ID_HEADER, "upstream-trace_01.a")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            ok.headers().get(REQUEST_ID_HEADER).unwrap(),
            "upstream-trace_01.a"
        );

        let hostile = app(svc)
            .oneshot(
                HttpRequest::get("/ping")
                    .header(REQUEST_ID_HEADER, "evil\"} injection {label=\"x")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let echoed = hostile
            .headers()
            .get(REQUEST_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap();
        assert_ne!(echoed, "evil\"} injection {label=\"x");
        assert_eq!(echoed.len(), 32, "hostile id replaced with a minted one");
    }
}
