//! Auth middleware — the CRITICAL auth path, extracted from the bin entry point
//! (the #721 `startup.rs` pattern). `use super::*` inherits the bin's shared
//! imports / DTOs (descendant-module visibility), exactly like the route-handler
//! submodules.
//!
//! **Unified WS-1 (#G7) model.** One fail-closed authorization engine
//! (`kirra_verifier::authz`) gates every protected route group by SCOPE:
//! `SCOPE_ADMIN` / `SCOPE_INTEGRATION_EVALUATE` / `SCOPE_ACTUATOR_COMMAND` /
//! `SCOPE_AUDIT_READ`, resolved from EITHER the break-glass root
//! `KIRRA_ADMIN_TOKEN` (all scopes; constant-time; absent/empty → 503 —
//! INVARIANTS #1/#2/#6 verbatim) OR a DB-backed API principal (token stored as
//! SHA-256 hash only, minted/revoked via the admin-scoped `/system/principals`
//! endpoints). The pure decision predicate (`authz::authorize_request`) reads no
//! env and no store (INVARIANT #13), so the truth table is unit-tested without
//! `set_var`; this module is the thin composition layer that lifts env + store in.
//!
//! Composed around it, unchanged from their prior slices:
//! - [`require_secure_transport`] — the #G7 transport-security gate, OUTERMOST on
//!   every gated group (a credential is never processed off a plaintext leg).
//! - [`require_client_identity`] — the trusted-ingress header gate.
//! - [`record_admin_action_audit`] — #G7 slice-3 attribution: a successful admin
//!   mutation is recorded in the signed audit chain, naming the principal.
use super::*;

/// The authenticated identity attached to the request extensions on Allow, for
/// downstream audit attribution (#G7 slice 3). `label` is `"root"` for the
/// break-glass admin token, else the API principal id — never the token.
#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedPrincipal {
    pub label: String,
    pub role: ApiRole,
}

/// INVARIANT #1/#6 — the admin mutation gate, PRESERVED by name and role. WS-1
/// (#G7) implements it as the `SCOPE_ADMIN` specialization of the unified
/// [`authorize_scope`] gate: the break-glass `KIRRA_ADMIN_TOKEN` still authorizes
/// under `constant_time_compare` and an absent/empty token is still a fail-closed
/// 503 — it is only ever a STRICT SUPERSET (an `admin`-role API principal
/// additionally qualifies; only that role holds `SCOPE_ADMIN`). NEVER commented
/// out, bypassed, or removed from any mutation route.
pub(crate) async fn require_admin_token(
    State(svc): State<Arc<ServiceState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    authorize_scope(&svc, SCOPE_ADMIN, request, next).await
}

/// WS-1 (#G7) — the identity-gated integration surface (`SCOPE_INTEGRATION_EVALUATE`).
/// Admin token or an `integrator`-role principal qualifies.
pub(crate) async fn require_integration_scope(
    State(svc): State<Arc<ServiceState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    authorize_scope(&svc, SCOPE_INTEGRATION_EVALUATE, request, next).await
}

/// WS-1 (#G7) — actuator command submission (`SCOPE_ACTUATOR_COMMAND`). Admin token
/// or an `operator`-role principal qualifies (the inner safety envelope + the outer
/// posture gate independently bound WHAT command is accepted).
pub(crate) async fn require_actuator_scope(
    State(svc): State<Arc<ServiceState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    authorize_scope(&svc, SCOPE_ACTUATOR_COMMAND, request, next).await
}

/// WS-1 (#G7) — read-only audit-chain verification/export (`SCOPE_AUDIT_READ`).
/// Admin token or an `auditor`-role principal qualifies.
pub(crate) async fn require_audit_scope(
    State(svc): State<Arc<ServiceState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    authorize_scope(&svc, SCOPE_AUDIT_READ, request, next).await
}

/// The unified fail-closed authorization gate (WS-1 · #G7). Resolves the bearer to
/// EITHER the break-glass root admin token OR a scoped API principal (by token
/// hash), then delegates the verdict to the pure [`authorize_request`] predicate.
///
/// Store/env are lifted out of the predicate (INVARIANT #13-friendly): this thin
/// wrapper reads `KIRRA_ADMIN_TOKEN`, does the hashed principal lookup ONLY when
/// needed (admin configured, a non-admin bearer present), and fail-closes to
/// "no principal" (→ 401) on any store error — never an authorized default.
///
/// On Allow the resolved identity is attached to the request extensions as
/// [`AuthenticatedPrincipal`] so [`record_admin_action_audit`] (layered inner on
/// the admin state-mutation routes) can attribute the mutation in the signed
/// audit chain.
///
/// Auditing of DECISIONS: observed via structured `tracing` (denials warn;
/// api-principal allows info; admin-token allows debug). Denials are NOT written
/// to the tamper-evident audit chain here — an unauthenticated caller must not be
/// able to append unboundedly (denial-flood DoS); privileged MUTATIONS are chained
/// by `record_admin_action_audit` after they succeed.
async fn authorize_scope(
    svc: &Arc<ServiceState>,
    required_scope: &'static str,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let admin_env = std::env::var("KIRRA_ADMIN_TOKEN").unwrap_or_default();
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_string);

    // Resolve a scoped principal ONLY when it could matter: admin configured, a
    // bearer present, and it is NOT the root admin token (the admin token
    // short-circuits to Admin in the predicate, so no store hit is needed).
    let principal: Option<ResolvedPrincipal> = match (admin_env.is_empty(), bearer.as_deref()) {
        (false, Some(tok)) if !admin_token_ok(Some(tok), Some(&admin_env)) => {
            let hash = token_sha256_hex(tok);
            match svc
                .app
                .store
                .call_read(move |s| s.load_api_principal_by_token_hash(&hash))
                .await
            {
                Ok(Ok(Some(rec))) => Some(ResolvedPrincipal {
                    role: ApiRole::parse_role(&rec.role),
                    revoked: rec.revoked_at_ms.is_some(),
                    principal_id: rec.principal_id,
                }),
                // No such token, OR a store/read failure → fail-closed as "no
                // principal" (the predicate then denies with 401).
                _ => None,
            }
        }
        _ => None,
    };

    // mTLS fallback (Track 1.2): with NO bearer token presented, a CA-verified client
    // certificate whose SHA-256 leaf fingerprint is pinned to a cert-principal resolves
    // an identity. A presented bearer is the explicit credential and is NOT silently
    // rescued by a cert — only the no-bearer case consults the cert. Admin must be
    // configured (the predicate 503s otherwise regardless).
    // `resolved_via_cert` distinguishes the mTLS credential from a token in the
    // Allow log below (the pure predicate labels both "api-principal").
    let mut resolved_via_cert = false;
    let principal = match principal {
        Some(p) => Some(p),
        None if bearer.is_none() && !admin_env.is_empty() => {
            match request
                .extensions()
                .get::<super::tls::ClientCertFingerprint>()
            {
                Some(fp) => {
                    let fp = fp.0.clone();
                    match svc
                        .app
                        .store
                        .call_read(move |s| s.load_cert_principal_by_fingerprint(&fp))
                        .await
                    {
                        Ok(Ok(Some(rec))) => {
                            resolved_via_cert = true;
                            // WP-15 (MGA G-19) — a cert is a LIFECYCLE credential: it
                            // stops authorizing past its X.509 notAfter exactly as it
                            // does on revocation. Fold expiry into the same
                            // invalid-credential flag the pure predicate denies on
                            // (`revoked` = "this credential is no longer valid"), and
                            // WARN distinctly on an expired-but-not-revoked cert so an
                            // operator sees a lapsed cert, not a mystery 401.
                            let now = now_ms();
                            let expired = rec.is_expired(now);
                            if expired && rec.revoked_at_ms.is_none() {
                                tracing::warn!(
                                    principal_id = %rec.principal_id,
                                    not_after_ms = rec.not_after_ms.unwrap_or(0),
                                    now_ms = now,
                                    "mTLS cert principal is EXPIRED (past notAfter) → \
                                     denied 401 (fail-closed; renew the cert)"
                                );
                            }
                            Some(ResolvedPrincipal {
                                role: ApiRole::parse_role(&rec.role),
                                revoked: rec.revoked_at_ms.is_some() || expired,
                                principal_id: rec.principal_id,
                            })
                        }
                        // Unpinned fingerprint OR store/read failure → no principal
                        // (fail-closed; the predicate then denies).
                        _ => None,
                    }
                }
                None => None,
            }
        }
        None => None,
    };

    let decision = authorize_request(
        required_scope,
        Some(admin_env.as_str()),
        bearer.as_deref(),
        principal.as_ref(),
    );

    let fp = bearer.as_deref().map(token_fingerprint);
    match decision.outcome {
        AuthzOutcome::Allow => {
            if decision.auth_method == "api-principal" {
                // Credential type for incident/debug: an mTLS-resolved principal vs a
                // bearer token (the predicate can't tell — the middleware knows the source).
                let credential = if resolved_via_cert {
                    "mtls-cert"
                } else {
                    "api-token"
                };
                tracing::info!(
                    scope = required_scope,
                    principal_id = decision.principal_id.as_deref().unwrap_or("?"),
                    role = decision.role.map(ApiRole::as_str).unwrap_or("?"),
                    credential,
                    "authz allow (scoped principal)"
                );
            } else {
                tracing::debug!(scope = required_scope, "authz allow (admin-token)");
            }
            // Attribution (#G7 slice 3): record WHO was authorized so the
            // attribution middleware / handlers can name the actor. Fail-safe
            // defaults: the break-glass root is "root" with the Admin role.
            let identity = AuthenticatedPrincipal {
                label: decision.principal_id.unwrap_or_else(|| "root".to_string()),
                role: decision.role.unwrap_or(ApiRole::Admin),
            };
            request.extensions_mut().insert(identity);
            Ok(next.run(request).await)
        }
        AuthzOutcome::Unconfigured => {
            tracing::warn!(
                scope = required_scope,
                "authz denied: KIRRA_ADMIN_TOKEN absent/empty → 503 (fail-closed)"
            );
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
        AuthzOutcome::Unauthenticated => {
            tracing::warn!(
                scope = required_scope,
                token_fp = fp.as_deref().unwrap_or("none"),
                "authz denied: no/unknown/revoked credential → 401"
            );
            Err(StatusCode::UNAUTHORIZED)
        }
        AuthzOutcome::Forbidden => {
            tracing::warn!(
                scope = required_scope,
                principal_id = decision.principal_id.as_deref().unwrap_or("?"),
                role = decision.role.map(ApiRole::as_str).unwrap_or("?"),
                "authz denied: principal lacks required scope → 403"
            );
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// #G7 slice 3 — admin-mutation attribution middleware. Layered ONLY on the admin
/// **state-mutation** routes (register / backup / rotate-key / dependencies /
/// operators / principals / federation / fabric-command), INNER of
/// `require_admin_token` (which authenticates and inserts the
/// [`AuthenticatedPrincipal`] extension). After a SUCCESSFUL MUTATION it appends
/// an `ADMIN_ACTION` event to the signed, hash-chained audit ledger, naming WHO
/// did WHAT — the accountability the single shared token could never provide.
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
    // `require_admin_token` (outer) inserts the identity on Allow; fall back
    // defensively rather than panic if the extension is somehow absent.
    let (actor, role) = request
        .extensions()
        .get::<AuthenticatedPrincipal>()
        .map(|p| (p.label.clone(), p.role.as_str()))
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
    let cfg = &svc.app.transport.identity;
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
/// sensitive route groups (admin, auditor, actuator, identity-gated, attestation).
pub(crate) async fn require_secure_transport(
    State(svc): State<Arc<ServiceState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let cfg = &svc.app.transport.security;
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
        assert!(!should_record_admin_action(
            "POST",
            StatusCode::INTERNAL_SERVER_ERROR
        ));
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

    use kirra_verifier::posture_cache::{
        now_ms, CachedFleetPosture, ServiceState, SharedPostureCache,
    };
    use kirra_verifier::verifier::{
        AppState, FleetPosture, TransportSecurityConfig, VerifierOperationMode,
    };
    use kirra_verifier::verifier_store::VerifierStore;

    // Inject the transport-security config via the PUBLIC field (no env mutation —
    // INVARIANT #13), then assemble the REAL router. The posture cache is seeded
    // Nominal so the GLOBAL posture gate steps aside and the request actually
    // reaches the per-group transport-security gate under test.
    fn state_requiring_secure_transport(require: bool) -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let mut app = AppState::new(store, VerifierOperationMode::Active);
        app.transport.security = TransportSecurityConfig {
            require_secure_transport: require,
            forwarded_proto_header: "x-forwarded-proto".to_string(),
        };
        let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
            CachedFleetPosture::new(FleetPosture::Nominal),
        )));
        Arc::new(ServiceState {
            app: Arc::new(app),
            posture_cache,
            started_at_ms: now_ms(),
            audit_verifying_key: None,
            fabric_router: Arc::new(kirra_verifier::fabric::router::FabricRouter::new()),
            fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(
                kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None),
            ),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
            last_actuator_verdict: kirra_verifier::posture_cache::empty_last_verdict_cell(),
        })
    }

    // A protected admin mutation route. With no auth header, the unified
    // `authorize_scope` gate returns 401/503 — never 403 — so a 403 can ONLY come
    // from the (outer) transport-security gate. That is what makes 403-vs-not-403
    // an unambiguous ordering probe.
    async fn dependencies_post_status(svc: Arc<ServiceState>, proto: Option<&str>) -> StatusCode {
        let mut b = Request::builder().method("POST").uri("/fleet/dependencies");
        if let Some(p) = proto {
            b = b.header("x-forwarded-proto", p);
        }
        build_app(svc, None)
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
        let status = build_app(state_requiring_secure_transport(true), None)
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
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "attestation nonce flow must require secure transport"
        );
    }

    /// The carved-out auditor read routes (WS-1) carry the same transport boundary.
    #[tokio::test]
    async fn auditor_routes_are_gated_by_secure_transport() {
        let status = build_app(state_requiring_secure_transport(true), None)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/system/audit/verify")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("router should not panic")
            .status();
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "auditor routes must require secure transport"
        );
    }
}
