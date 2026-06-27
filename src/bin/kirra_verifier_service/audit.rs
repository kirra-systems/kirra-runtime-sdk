// src/bin/kirra_verifier_service/audit.rs
// audit route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use audit::*`) lets build_app/tests name them unqualified.

use super::*;

pub(crate) async fn verify_audit_chain(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    // `VerifyingKey` is `Copy`; copy it into the blocking task. A full-chain scan
    // with a per-row Ed25519 verification is the heaviest read-side op — `call_read`
    // runs it off the worker pool AND on a read-only replica, so it neither pins a
    // tokio worker nor contends the writer mutex.
    let vk = svc.audit_verifying_key;
    let result = svc.app.store.call_read(move |store| {
        store.verify_audit_chain_full(vk.as_ref())
    })
    .await;
    match result {
        Ok(Ok(r)) => Json(json!({
            "chain_intact": r.chain_intact,
            "total_entries": r.total_entries,
            "latest_hash": r.latest_hash,
            "signing_enabled": r.signing_enabled,
            "signed_entries": r.signed_entries,
            "unsigned_entries": r.unsigned_entries,
            "signature_valid": r.signature_valid,
            "first_signed_at_ms": r.first_signed_at_ms,
            "public_key_b64": r.public_key_b64,
            // #77 anchor-head high-water mark: detects tail truncation/deletion.
            "head_verified": r.head_verified,
            "head_status": r.head_status,
            // Overall verdict folds in the head check so a truncated chain
            // (rows internally consistent but tail deleted) reads as not-verified.
            "verified": r.chain_intact && r.signature_valid && r.head_verified,
        })).into_response(),
        Ok(Err(_)) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "audit chain query failed" }))).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

pub(crate) async fn handle_audit_export(
    State(svc): State<Arc<ServiceState>>,
    Query(params): Query<AuditExportQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(100).min(1000);
    let offset = params.offset.unwrap_or(0);
    let vk = svc.audit_verifying_key.as_ref();
    match svc.app.store.with_read(|store| store.load_audit_chain_page(limit, offset, vk)) {
        Ok(page) => Json(page).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "export query failed" }))).into_response(),
    }
}

pub(crate) async fn handle_audit_rotate_key(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RotateSigningKeyRequest>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    // Decode the new signing seed → SigningKey (32-byte Ed25519 seed).
    let new_signing_key = {
        use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
        match b64e.decode(req.new_signing_key_b64.trim())
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            .map(|seed| ed25519_dalek::SigningKey::from_bytes(&seed))
        {
            Some(sk) => sk,
            None => return (StatusCode::BAD_REQUEST,
                Json(json!({ "error": "new_signing_key_b64 must be a base64 32-byte ed25519 seed" }))).into_response(),
        }
    };
    let new_key_id = kirra_verifier::audit_chain::verifying_key_id(&new_signing_key.verifying_key());
    // #79: pass our held fencing token so the durable write re-checks it INSIDE
    // the transaction, closing the gate→commit TOCTOU.
    let held_epoch = svc.app.held_epoch.load(std::sync::atomic::Ordering::SeqCst);
    // The mutation runs under one acquisition; the closure returns the
    // record_key_rotation result. The Fenced self-demote (a mode_active store)
    // happens OUTSIDE the closure — the lock is already released by then,
    // matching the prior `drop(store)`-before-self-demote ordering (Rule 4).
    let rotation = svc.app.store.with(|store| {
        store.record_key_rotation(new_signing_key, &req.reason, now_ms(), held_epoch)
    });
    match rotation {
        Ok(_) => Json(json!({ "recorded": true, "event_type": "KEY_ROTATION", "new_key_id": new_key_id })).into_response(),
        Err(DurableWriteError::Fenced(reason)) => {
            // Superseded between the request-path gate and this commit.
            // Mirror the gate: self-demote and reject fail-closed (no write
            // landed). Subsequent mutations hit the standby check above.
            svc.app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
            tracing::error!(
                path = "/system/audit/rotate-signing-key",
                fence = ?reason,
                "FENCED at top-tier write (in-transaction epoch re-check) — self-demoting to PassiveStandby and rejecting"
            );
            (StatusCode::SERVICE_UNAVAILABLE,
             Json(json!({ "error": "fenced: epoch superseded; instance demoted to passive standby" }))).into_response()
        }
        // NonceReplay / GenerationRegress cannot arise here (key rotation touches
        // neither the nonce nor the federation-generation tables); fold them into the
        // generic server-error arm for exhaustiveness.
        Err(DurableWriteError::Db(_)
            | DurableWriteError::NonceReplay
            | DurableWriteError::GenerationRegress { .. }) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "failed to record key rotation" }))).into_response(),
    }
}
