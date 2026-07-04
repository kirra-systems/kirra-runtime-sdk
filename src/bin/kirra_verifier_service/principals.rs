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
    let token = generate_api_token();
    let token_hash = token_sha256_hex(&token);
    let token_fp = token_fingerprint(&token);
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
        Ok(true) => (StatusCode::CREATED, Json(json!({
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
