// src/bin/kirra_verifier_service/principals.rs
// WS-1 (#G7) — API principal registry handlers (mint / list / revoke).
//
// `use super::*` pulls the binary root's helpers, DTOs and `use` imports (the
// authz helpers, `ServiceState`, `now_ms`, `valid_identifier`, `admin_token_
// fingerprint`, axum extractors). These routes are ADMIN-scoped at the router
// layer (`require_admin_token` / SCOPE_ADMIN) — only an admin may mint or revoke.

use super::*;

#[derive(Deserialize)]
pub(crate) struct RegisterApiPrincipalRequest {
    principal_id: String,
    /// One of `admin` | `integrator` | `auditor` | `operator`.
    role: String,
}

/// POST /system/principals — mint (or rotate) a scoped API principal. ADMIN-scoped.
/// The server generates a 256-bit token, stores ONLY its SHA-256 hex, and returns
/// the plaintext token EXACTLY ONCE (it can never be recovered later). Re-minting an
/// existing `principal_id` rotates its token and clears any revocation.
pub(crate) async fn register_api_principal_handler(
    State(svc): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Json(req): Json<RegisterApiPrincipalRequest>,
) -> impl IntoResponse {
    let principal_id = req.principal_id.trim();
    if principal_id.is_empty() {
        return (StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "principal_id must be non-empty" }))).into_response();
    }
    // Same identifier hygiene as the operator registry (#326): no `|` / control chars.
    if !valid_identifier(principal_id) {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "principal_id must not contain '|' or control characters"
        }))).into_response();
    }
    // Fail-closed: the role must be one of the known RBAC roles.
    let role = match ApiRole::parse_role(req.role.trim()) {
        Some(r) => r,
        None => return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({
            "error": "role must be one of: admin, integrator, auditor, operator"
        }))).into_response(),
    };

    // Generate the token and derive its stored hash + short fingerprint. The
    // plaintext exists ONLY in this response; the store sees only the hash.
    // Fail-closed: a CSPRNG failure is a 500, never a panic and never a token
    // minted from degraded entropy.
    let token = match generate_api_token() {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "CSPRNG unavailable — refusing to mint API token");
            return (StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "token generation failed" }))).into_response();
        }
    };
    let token_hash = token_sha256_hex(&token);
    // The fingerprint is the first 16 hex chars of the SHA-256 — derive it from the
    // already-computed hash rather than re-hashing the token.
    let token_fp = token_hash[..16].to_string();
    let admin_fp = admin_token_fingerprint(&headers);
    let now = now_ms();

    let id = principal_id.to_string();
    let role_str = role.as_str().to_string();
    let fp_for_audit = token_fp.clone();
    let persisted = svc.app.store.call(move |store| {
        if store.register_api_principal(&id, &token_hash, &role_str, now).is_err() {
            return false;
        }
        // Attributed, non-repudiable registration event (never records the token).
        let _ = store.append_clearance_audit_event(
            "ApiPrincipalRegistered",
            &json!({
                "principal_id": id,
                "role": role_str,
                "token_fingerprint": fp_for_audit,
                "registered_by_admin_fingerprint": admin_fp,
            }).to_string(),
            now,
        );
        true
    }).await;

    match persisted {
        // `Cache-Control: no-store` — the body carries a one-time plaintext secret;
        // no intermediary (proxy / client cache) may persist it.
        Ok(true) => (StatusCode::CREATED,
                     [(header::CACHE_CONTROL, "no-store")],
                     Json(json!({
            "principal_id": principal_id,
            "role": role.as_str(),
            // Shown ONCE — store it now; it cannot be retrieved again.
            "token": token,
            "token_fingerprint": token_fp,
            "note": "the token is shown only once; store it securely — it cannot be recovered later",
        }))).into_response(),
        Ok(false) => (StatusCode::INTERNAL_SERVER_ERROR,
                      Json(json!({ "error": "persist failed" }))).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store task failed" }))).into_response(),
    }
}

/// GET /system/principals — list registered principals. ADMIN-scoped. Never
/// returns the token or its hash — only id / role / status.
pub(crate) async fn list_api_principals_handler(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    match svc.app.store.call_read(|store| store.load_api_principals()).await {
        Ok(Ok(list)) => {
            let out: Vec<_> = list.into_iter().map(|p| json!({
                "principal_id": p.principal_id,
                "role": p.role,
                "created_at_ms": p.created_at_ms,
                "active": p.is_active(),
                "revoked_at_ms": p.revoked_at_ms,
            })).collect();
            (StatusCode::OK, Json(json!({ "principals": out }))).into_response()
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR,
              Json(json!({ "error": "query failed" }))).into_response(),
    }
}

/// POST /system/principals/{principal_id}/revoke — revoke a principal. ADMIN-scoped.
/// A revoked principal's token no longer authorizes (resolves to 401).
pub(crate) async fn revoke_api_principal_handler(
    State(svc): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Path(principal_id): Path<String>,
) -> impl IntoResponse {
    let now = now_ms();
    let admin_fp = admin_token_fingerprint(&headers);
    let id = principal_id.clone();
    match svc.app.store.call(move |store| match store.revoke_api_principal(&id, now) {
        Ok(true) => {
            let _ = store.append_clearance_audit_event(
                "ApiPrincipalRevoked",
                &json!({ "principal_id": id, "revoked_by_admin_fingerprint": admin_fp }).to_string(),
                now,
            );
            (StatusCode::OK, Json(json!({ "principal_id": id, "status": "revoked" }))).into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND,
                      Json(json!({ "error": "principal not found or already revoked" }))).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "persist failed" }))).into_response(),
    }).await {
        Ok(r) => r,
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store task failed" }))).into_response(),
    }
}

// ===========================================================================
// mTLS cert principals (Track 1.2). A client cert — CA-verified by rustls at the
// TLS layer — is pinned to a principal by the SHA-256 hex of its leaf DER. Unlike
// the token path the server NEVER mints or sees a secret: the admin SUPPLIES the
// fingerprint (computed offline from the cert), and the client holds the private
// key. ADMIN-scoped at the router layer.
// ===========================================================================

#[derive(Deserialize)]
pub(crate) struct RegisterCertPrincipalRequest {
    principal_id: String,
    /// SHA-256 hex (64 chars) of the client certificate's leaf DER.
    cert_sha256: String,
    /// One of `admin` | `integrator` | `auditor` | `operator`.
    role: String,
    /// WP-15 (MGA G-19) — the pinned cert's X.509 notAfter, epoch ms. The admin
    /// computes it offline alongside the fingerprint. Optional: omitted → no expiry
    /// tracked (the pin never ages out, back-compat). Present → the cert stops
    /// authorizing at/after this instant (renewal = re-register with a later value).
    #[serde(default)]
    not_after_ms: Option<u64>,
}

/// POST /system/cert-principals — pin (or rotate) a client cert to a scoped
/// principal by its SHA-256 leaf fingerprint. ADMIN-scoped. No secret is generated
/// or returned — the client's private key never reaches the server.
pub(crate) async fn register_cert_principal_handler(
    State(svc): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Json(req): Json<RegisterCertPrincipalRequest>,
) -> impl IntoResponse {
    let principal_id = req.principal_id.trim();
    if principal_id.is_empty() {
        return (StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "principal_id must be non-empty" }))).into_response();
    }
    if !valid_identifier(principal_id) {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "principal_id must not contain '|' or control characters"
        }))).into_response();
    }
    let role = match ApiRole::parse_role(req.role.trim()) {
        Some(r) => r,
        None => return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({
            "error": "role must be one of: admin, integrator, auditor, operator"
        }))).into_response(),
    };
    // Normalize + validate the fingerprint: exactly 64 lowercase hex chars (SHA-256),
    // matching the form serve_tls derives from the leaf DER.
    let fingerprint = req.cert_sha256.trim().to_ascii_lowercase();
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({
            "error": "cert_sha256 must be a 64-character SHA-256 hex string"
        }))).into_response();
    }

    // Reject a plainly-invalid expiry: notAfter must be in the FUTURE at
    // registration time (a cert already lapsed the moment it is pinned is an
    // operator mistake — accepting it would silently 401 every request under it).
    let now = now_ms();
    if let Some(exp) = req.not_after_ms {
        if exp <= now {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({
                "error": "not_after_ms must be in the future (the cert would be expired on registration)"
            }))).into_response();
        }
    }
    let not_after_ms = req.not_after_ms;
    let admin_fp = admin_token_fingerprint(&headers);
    let id = principal_id.to_string();
    let role_str = role.as_str().to_string();
    let fp_store = fingerprint.clone();
    let fp_audit = fingerprint.clone();
    // Propagate the rusqlite error so a UNIQUE(cert_sha256) conflict — the same
    // fingerprint already pinned to a DIFFERENT principal (`ON CONFLICT(principal_id)`
    // only rotates the SAME id) — maps to 409, not a generic 500. cert_sha256 is
    // operator-supplied, so this collision is a plausible, actionable mistake.
    let persisted = svc.app.store.call(move |store| {
        store.register_cert_principal(&id, &fp_store, &role_str, not_after_ms, now)?;
        let _ = store.append_clearance_audit_event(
            "CertPrincipalRegistered",
            &json!({
                "principal_id": id,
                "role": role_str,
                "cert_sha256": fp_audit,
                "not_after_ms": not_after_ms,
                "registered_by_admin_fingerprint": admin_fp,
            }).to_string(),
            now,
        );
        Ok::<(), rusqlite::Error>(())
    }).await;

    match persisted {
        Ok(Ok(())) => (StatusCode::CREATED, Json(json!({
            "principal_id": principal_id,
            "role": role.as_str(),
            "cert_sha256": fingerprint,
            "not_after_ms": not_after_ms,
        }))).into_response(),
        Ok(Err(e)) if is_unique_violation(&e) => (StatusCode::CONFLICT, Json(json!({
            "error": "cert_sha256 is already pinned to another principal"
        }))).into_response(),
        Ok(Err(_)) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "persist failed" }))).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store task failed" }))).into_response(),
    }
}

/// A SQLite UNIQUE / constraint violation — used to distinguish a duplicate-pin
/// conflict (409) from a genuine internal persistence failure (500).
fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ffi::ErrorCode::ConstraintViolation
    )
}

/// GET /system/cert-principals — list pinned cert principals. ADMIN-scoped. Never
/// returns the fingerprint — only id / role / status.
pub(crate) async fn list_cert_principals_handler(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    // Evaluate expiry against a single "now" so every row's `expired`/`valid` is
    // consistent within one listing.
    let now = now_ms();
    match svc.app.store.call_read(|store| store.load_cert_principals()).await {
        Ok(Ok(list)) => {
            let out: Vec<_> = list.into_iter().map(|p| json!({
                "principal_id": p.principal_id,
                "role": p.role,
                "created_at_ms": p.created_at_ms,
                // `active` = not revoked (the pin's on/off state); `expired` and
                // `valid` add the WP-15 lifecycle view (valid = authorizes right now).
                "active": p.is_active(),
                "revoked_at_ms": p.revoked_at_ms,
                "not_after_ms": p.not_after_ms,
                "expired": p.is_expired(now),
                "valid": p.is_valid_at(now),
            })).collect();
            (StatusCode::OK, Json(json!({ "cert_principals": out }))).into_response()
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR,
              Json(json!({ "error": "query failed" }))).into_response(),
    }
}

/// POST /system/cert-principals/{principal_id}/revoke — revoke a cert principal.
/// ADMIN-scoped. A revoked cert no longer authorizes (resolves to 401).
pub(crate) async fn revoke_cert_principal_handler(
    State(svc): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Path(principal_id): Path<String>,
) -> impl IntoResponse {
    let now = now_ms();
    let admin_fp = admin_token_fingerprint(&headers);
    let id = principal_id.clone();
    match svc.app.store.call(move |store| match store.revoke_cert_principal(&id, now) {
        Ok(true) => {
            let _ = store.append_clearance_audit_event(
                "CertPrincipalRevoked",
                &json!({ "principal_id": id, "revoked_by_admin_fingerprint": admin_fp }).to_string(),
                now,
            );
            (StatusCode::OK, Json(json!({ "principal_id": id, "status": "revoked" }))).into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND,
                      Json(json!({ "error": "principal not found or already revoked" }))).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "persist failed" }))).into_response(),
    }).await {
        Ok(r) => r,
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store task failed" }))).into_response(),
    }
}
