//! WP-03 (MGA G-10) — control-plane HTTP backpressure.
//!
//! Fail-fast overload protection for the verifier's axum plane: a route group
//! wrapped by [`with_backpressure`] gets, outermost-first, `DefaultBodyLimit`
//! (added after the `ServiceBuilder`, so it wraps it — position is immaterial
//! though: it only sets the request-extension cap that body-consuming
//! extractors enforce, it is not an active gate), then a 429 error mapper,
//! `LoadShedLayer` (reject-at-capacity instead of queueing without bound),
//! and a `GlobalConcurrencyLimitLayer` (ONE shared semaphore across every
//! route in the group — not per-route).
//!
//! Wiring lives in `build_app` (the entry point): the API plane and the
//! operator console get TWO ISOLATED pools, so an API flood cannot starve the
//! console (the LockedOut recovery surface — clearance grants + the ADR-0013
//! e-stop request) and vice versa. Probe routes (`/health`, `/ready`,
//! `/metrics`) are exempt: liveness and the Prometheus scrape must survive
//! overload exactly as they survive LockedOut. The posture gate remains the
//! outermost *gate* on everything (the WP-05 request-observability layer
//! wraps it as the outermost *layer*, but makes no admission decisions).
//!
//! Shedding is fail-closed by construction: for the actuator route a 429 is a
//! DENIED command, and the consumer already treats any non-200 as safe-stop
//! (#405). Long-lived SSE responses hold a permit only until the response
//! head is produced (the permit drops when the service future resolves), so a
//! streaming client does not pin a slot for its lifetime.

use axum::error_handling::HandleErrorLayer;
use axum::extract::DefaultBodyLimit;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;
use tower::{
    limit::GlobalConcurrencyLimitLayer, load_shed::LoadShedLayer, BoxError, ServiceBuilder,
};

/// Parse a positive-integer backpressure limit from the environment.
/// Unset/empty → `default`. Set-but-invalid (unparseable or 0) → **startup
/// abort**, matching the `KIRRA_VEHICLE_CLASS` fail-closed config discipline:
/// a limit someone *tried* to set and got wrong must never silently become a
/// different number.
pub(crate) fn env_limit_or(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => match v.trim().parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::error!(
                    var = name,
                    value = %v,
                    "invalid backpressure limit (must be a positive integer) — \
                     aborting startup (fail-closed) rather than guessing"
                );
                std::process::exit(1);
            }
        },
        _ => default,
    }
}

/// The overload response for a load-shed request: `429 Too Many Requests` +
/// `Retry-After`. 429 (not 503) deliberately: the posture gate already owns
/// 503 for posture denial, and a client must be able to tell "fleet is not
/// Nominal" from "control plane is at capacity".
async fn overloaded_response(_err: BoxError) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "1")],
        Json(json!({ "error": "control plane at capacity — retry" })),
    )
        .into_response()
}

/// Wrap a route group in the fail-fast backpressure stack (see module docs).
pub(crate) fn with_backpressure<S>(
    router: Router<S>,
    max_concurrency: usize,
    max_body_bytes: usize,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(overloaded_response))
                .layer(LoadShedLayer::new())
                .layer(GlobalConcurrencyLimitLayer::new(max_concurrency)),
        )
        .layer(DefaultBodyLimit::max(max_body_bytes))
}

// ---------------------------------------------------------------------------
// Deterministic, stateless tests of the stack itself (shed-at-capacity, 429
// shape, recovery after release, body cap). The production wiring — which
// groups are pooled, probe exemption, pool isolation — is a structural
// property of `build_app`; these tests pin the stack's BEHAVIOR so a tower
// upgrade or layer reorder cannot silently change it.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod backpressure_tests {
    use super::with_backpressure;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use axum::Router;
    use std::sync::Arc;
    use tower::ServiceExt; // for `oneshot`

    /// At capacity 1, a second in-flight request is shed as 429 with
    /// `Retry-After` — it does NOT queue — and once the first request
    /// releases, the pool admits again (recovery, not a latch).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn second_in_flight_request_is_shed_then_pool_recovers() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (entered_h, release_h) = (entered.clone(), release.clone());

        let app = with_backpressure(
            Router::new().route(
                "/slow",
                get(move || {
                    let entered = entered_h.clone();
                    let release = release_h.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        "done"
                    }
                }),
            ),
            1,
            1024,
        );

        // Occupy the single slot and wait until the handler is genuinely inside.
        let occupied = app.clone();
        let first = tokio::spawn(async move {
            occupied
                .oneshot(Request::get("/slow").body(Body::empty()).unwrap())
                .await
                .unwrap()
        });
        entered.notified().await;

        // The pool is full: the next request must shed immediately as 429.
        let shed = app
            .clone()
            .oneshot(Request::get("/slow").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(shed.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            shed.headers().get(axum::http::header::RETRY_AFTER).map(|v| v.as_bytes()),
            Some(&b"1"[..])
        );

        // Release the first request; the pool must admit again.
        release.notify_one();
        assert_eq!(first.await.unwrap().status(), StatusCode::OK);
        release.notify_one(); // pre-arm for the recovery request below
        let recovered = app
            .oneshot(Request::get("/slow").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(recovered.status(), StatusCode::OK);
    }

    /// A request body over the cap is refused (413) before the handler runs;
    /// one at the cap is admitted.
    #[tokio::test]
    async fn oversized_body_is_refused_at_the_cap() {
        let app = with_backpressure(
            Router::new().route("/echo", post(|body: String| async move { body })),
            8,
            16, // 16-byte cap
        );

        let over = app
            .clone()
            .oneshot(Request::post("/echo").body(Body::from(vec![b'x'; 17])).unwrap())
            .await
            .unwrap();
        assert_eq!(over.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let at_cap = app
            .oneshot(Request::post("/echo").body(Body::from(vec![b'x'; 16])).unwrap())
            .await
            .unwrap();
        assert_eq!(at_cap.status(), StatusCode::OK);
    }
}
