//! Auth middleware — extracted from the bin entry point to keep it lean (the #721
//! `startup.rs` pattern), reclaiming quality-guardrail headroom. Holds the
//! fail-closed admin auth + RBAC, the transport-identity and transport-security
//! gates, and the admin-action audit-attribution middleware. `use super::*`
//! inherits the bin's shared imports / DTOs (descendant-module visibility),
//! exactly like the route-handler submodules.
//!
//! SAFETY: this is the CRITICAL auth path — CRITICAL INVARIANTS #1/#2/#6/#13 are
//! enforced here. The extraction is behaviour-preserving (byte-identical to the
//! pre-extraction code); the G7 tests below moved with the functions and are the
//! regression net.
use super::*;

/// Process-wide per-principal admin token registry (#G7), loaded once from
/// `KIRRA_PRINCIPAL_TOKENS`. Lazy `OnceLock` (env is fixed at process start);
/// malformed entries are dropped by the parser, so a bad config degrades to
/// fewer principals, never a fail-open.
fn principal_registry() -> &'static PrincipalRegistry {
    static REGISTRY: std::sync::OnceLock<PrincipalRegistry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| {
        let reg = PrincipalRegistry::from_env();
        if !reg.is_empty() {
            tracing::info!(principal_tokens = reg.len(), "loaded per-principal admin tokens (#G7)");
        }
        reg
    })
}

pub(crate) async fn require_admin_token(mut request: Request, next: Next) -> Result<Response, StatusCode> {
    let expected = std::env::var("KIRRA_ADMIN_TOKEN")
        .unwrap_or_default();

    // Fail-closed: absent or empty admin token → 503 (CRITICAL INVARIANT #1/#6).
    // Kept distinct from the 401 below so an unconfigured server is never
    // mistaken for a bad credential. This gate stays FIRST, so a per-principal
    // token can never authorize without a configured root token (no fail-open).
    if expected.is_empty() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    // Own the token before the mutable borrow below (attaching the principal to
    // the request extensions). The extra allocation on the admin path is trivial.
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_owned)
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // Fail-closed authorization (#G7): the root KIRRA_ADMIN_TOKEN (constant-time,
    // SG-015) OR a registered per-principal token. `expected` is non-empty here,
    // so `authorize_admin` reduces to the prior admin_token_ok decision plus the
    // additive principal-registry lookup; a `None` denies (401). The resolved
    // principal is attached to the request extensions for audit attribution.
    match authorize_admin(Some(&provided), Some(&expected), principal_registry()) {
        Some(principal) => {
            // RBAC (#G7 slice 2): a scoped principal (e.g. `readonly`) may only
            // issue nullipotent requests. The pure `admin_rbac_allows` decision
            // (unit-tested) denies any mutating method (not GET/HEAD/OPTIONS) for a
            // role without mutation rights — a read-only token is thus 403'd on
            // every POST admin route AND the actuator, from this one middleware.
            // The root token and `admin`-role principals are unrestricted.
            if !admin_rbac_allows(principal.role(), request.method().as_str()) {
                tracing::warn!(
                    principal = principal.label(),
                    role = principal.role().label(),
                    method = request.method().as_str(),
                    path = request.uri().path(),
                    "RBAC deny (#G7): scoped principal attempted a mutating request"
                );
                return Err(StatusCode::FORBIDDEN);
            }
            // Attribution (#G7): make the authenticated identity visible now (logs)
            // and available to the attribution middleware / handlers via the request
            // extension. The audit RECORDING happens in `record_admin_action_audit`,
            // layered on the admin state-mutation routes only (see below).
            tracing::debug!(
                principal = principal.label(),
                role = principal.role().label(),
                "admin request authorized (#G7)"
            );
            request.extensions_mut().insert(principal);
            Ok(next.run(request).await)
        }
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

/// #G7 slice 3 — admin-mutation attribution middleware. Layered ONLY on the admin
/// **state-mutation** routes (register / backup / rotate-key / dependencies /
/// operators / federation / fabric-command), INNER of `require_admin_token` (which
/// authenticates and inserts the [`AdminPrincipal`] extension). After a SUCCESSFUL
/// MUTATION it appends an `ADMIN_ACTION` event to the signed, hash-chained audit
/// ledger, naming WHO did WHAT — the accountability the single shared token could
/// never provide.
///
/// Deliberately NOT applied to the actuator route (a high-rate control path — a
/// per-command signed audit row would be prohibitive) nor the identity-gated
/// evaluation routes (which already self-audit). Because the only latency-sensitive
/// path is thus excluded, the audit write is AWAITED so the attribution is durably
/// committed to the chain before the (rare, sensitive) mutation is acknowledged —
/// the stronger accountability guarantee. A write failure is logged and never fails
/// the already-completed mutation (mirrors the `action_filter` audit path).
pub(crate) async fn record_admin_action_audit(
    State(svc): State<Arc<ServiceState>>,
    request: Request,
    next: Next,
) -> Response {
    // Capture attribution facts before the request moves into `next.run`.
    // `require_admin_token` (outer) always inserts the principal; fall back
    // defensively rather than panic if the extension is somehow absent.
    let (actor, role) = request
        .extensions()
        .get::<AdminPrincipal>()
        .map(|p| (p.label().to_owned(), p.role().label()))
        .unwrap_or_else(|| ("unknown".to_owned(), "unknown"));
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();

    let response = next.run(request).await;

    if should_record_admin_action(&method, response.status()) {
        let event = json!({
            "principal": actor,
            "role": role,
            "method": method,
            "path": path,
        })
        .to_string();
        let now = now_ms();
        let _ = svc
            .app
            .store
            .call(move |store| {
                if let Err(e) = store.save_posture_event_chained(
                    "admin_action",
                    "ADMIN_ACTION",
                    &event,
                    None,
                    now,
                ) {
                    tracing::warn!(error = %e, "failed to record admin-action attribution (#G7)");
                }
            })
            .await;
    }
    response
}

/// Should a COMPLETED admin request be recorded as an attributed `ADMIN_ACTION`
/// audit event (#G7 slice 3)? Only a SUCCESSFUL (2xx) MUTATING (non-safe method)
/// request — reads (GET/HEAD/OPTIONS) and non-2xx failures are not attributed, so
/// the ledger records who actually CHANGED state, not every authorized touch.
/// Pure, so the middleware's decision is unit-tested without process env.
fn should_record_admin_action(method: &str, status: StatusCode) -> bool {
    let mutating = !matches!(method, "GET" | "HEAD" | "OPTIONS");
    mutating && status.is_success()
}

pub(crate) async fn require_client_identity(
    State(svc): State<Arc<ServiceState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let cfg = &svc.app.transport_identity;
    if !validate_client_identity_headers(
        cfg.trusted_ingress_mode,
        &cfg.client_id_header,
        request.headers(),
    ) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

/// #G7 — fail-closed transport-security gate. When `KIRRA_REQUIRE_SECURE_TRANSPORT`
/// is on, a request that the trusted proxy/mesh does not assert arrived over TLS
/// (via the forwarded-proto header) is rejected with 403 BEFORE authentication —
/// a bearer token or attestation nonce must never be processed off a plaintext leg.
/// When off, this is a no-op (byte-identical to before). Layered OUTERMOST on the
/// sensitive route groups (admin, actuator, identity-gated, attestation).
pub(crate) async fn require_secure_transport(
    State(svc): State<Arc<ServiceState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let cfg = &svc.app.transport_security;
    if !request_transport_is_secure(
        cfg.require_secure_transport,
        &cfg.forwarded_proto_header,
        request.headers(),
    ) {
        tracing::warn!(
            path = request.uri().path(),
            "transport-security deny (#G7): request not asserted over TLS (KIRRA_REQUIRE_SECURE_TRANSPORT)"
        );
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}

#[cfg(test)]
mod g7_admin_action_attribution_tests {
    use super::should_record_admin_action;
    use axum::http::StatusCode;

    /// #G7 slice 3 — only a SUCCESSFUL MUTATION is attributed; reads and failures
    /// are not (so the ledger names who CHANGED state, not every authorized touch).
    #[test]
    fn admin_action_recorded_only_on_successful_mutation() {
        // Successful mutations → recorded.
        assert!(should_record_admin_action("POST", StatusCode::OK));
        assert!(should_record_admin_action("POST", StatusCode::CREATED));
        assert!(should_record_admin_action("DELETE", StatusCode::NO_CONTENT));
        assert!(should_record_admin_action("PUT", StatusCode::ACCEPTED));
        // Reads → never (even on success).
        assert!(!should_record_admin_action("GET", StatusCode::OK));
        assert!(!should_record_admin_action("HEAD", StatusCode::OK));
        assert!(!should_record_admin_action("OPTIONS", StatusCode::OK));
        // Failed mutations → never (nothing changed).
        assert!(!should_record_admin_action("POST", StatusCode::INTERNAL_SERVER_ERROR));
        assert!(!should_record_admin_action("POST", StatusCode::FORBIDDEN));
        assert!(!should_record_admin_action("POST", StatusCode::BAD_REQUEST));
    }
}

#[cfg(test)]
mod g7_transport_security_router_tests {
    // `build_app` lives at the bin crate root (not in this submodule).
    use crate::build_app;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    use kirra_verifier::posture_cache::{now_ms, CachedFleetPosture, ServiceState, SharedPostureCache};
    use kirra_verifier::verifier::{AppState, FleetPosture, TransportSecurityConfig, VerifierOperationMode};
    use kirra_verifier::verifier_store::VerifierStore;

    // Inject the transport-security config via the PUBLIC field (no env mutation —
    // INVARIANT #13), then assemble the REAL router. The posture cache is seeded
    // Nominal so the GLOBAL posture gate steps aside and the request actually
    // reaches the per-group transport-security gate under test.
    fn state_requiring_secure_transport(require: bool) -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let mut app = AppState::new(store, VerifierOperationMode::Active);
        app.transport_security = TransportSecurityConfig {
            require_secure_transport: require,
            forwarded_proto_header: "x-forwarded-proto".to_string(),
        };
        let posture_cache: SharedPostureCache =
            Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture::new(FleetPosture::Nominal))));
        Arc::new(ServiceState {
            app: Arc::new(app),
            posture_cache,
            started_at_ms: now_ms(),
            audit_verifying_key: None,
            fabric_router: Arc::new(kirra_verifier::fabric::router::FabricRouter::new()),
            fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    // A protected admin mutation route. With no auth header, `require_admin_token`
    // returns 401/503 — never 403 — so a 403 can ONLY come from the (outer)
    // transport-security gate. That is what makes 403-vs-not-403 an unambiguous
    // ordering probe.
    async fn dependencies_post_status(svc: Arc<ServiceState>, proto: Option<&str>) -> StatusCode {
        let mut b = Request::builder().method("POST").uri("/fleet/dependencies");
        if let Some(p) = proto {
            b = b.header("x-forwarded-proto", p);
        }
        build_app(svc)
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .expect("router should not panic")
            .status()
    }

    /// #G7 (Copilot #805) — the transport-security gate is WIRED on the real router,
    /// runs OUTERMOST (before auth), and is off by default.
    #[tokio::test]
    async fn secure_transport_gate_is_wired_outermost_and_fail_closed() {
        // Enforced + no https assertion → 403 from the transport gate, BEFORE auth
        // (which would otherwise 401/503) — proves it is wired AND outermost.
        assert_eq!(
            dependencies_post_status(state_requiring_secure_transport(true), None).await,
            StatusCode::FORBIDDEN,
            "enforced + no forwarded-proto must 403 at the transport gate, before auth"
        );
        // Enforced + https assertion → passes the transport gate; auth then denies
        // (401/503), never 403 — proves an https leg satisfies the gate.
        assert_ne!(
            dependencies_post_status(state_requiring_secure_transport(true), Some("https")).await,
            StatusCode::FORBIDDEN,
            "an https-asserted request must pass the transport gate"
        );
        // Not enforced (default) → the gate is a no-op; auth denies, never 403.
        assert_ne!(
            dependencies_post_status(state_requiring_secure_transport(false), None).await,
            StatusCode::FORBIDDEN,
            "off by default: the transport gate must not fire"
        );
    }

    /// The attestation nonce flow (Copilot #805) is gated too: enforced + no https
    /// → 403 before the unauthenticated challenge handler runs.
    #[tokio::test]
    async fn attestation_challenge_is_gated_by_secure_transport() {
        let status = build_app(state_requiring_secure_transport(true))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/attestation/challenge/node-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("router should not panic")
            .status();
        assert_eq!(status, StatusCode::FORBIDDEN, "attestation nonce flow must require secure transport");
    }
}
