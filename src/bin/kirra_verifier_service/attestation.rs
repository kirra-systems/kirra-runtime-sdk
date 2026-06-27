// src/bin/kirra_verifier_service/attestation.rs
// attestation route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use attestation::*`) lets build_app/tests name them unqualified.

use super::*;

pub(crate) async fn register_node(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterNodeRequest>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    let now = now_ms();
    let node = RegisteredNode {
        node_id: req.node_id.clone(),
        status: NodeTrustState::Unknown,
        registered_at_ms: now,
        last_trust_update_ms: 0,
        ak_public_pem: req.ak_public_pem,
        expected_pcr16_digest_hex: req.expected_pcr16_digest_hex,
        site: req.site,
        firmware_version: req.firmware_version,
    };

    // TPM-quote policy (#572 follow-up) is committed BEFORE the node record, so
    // a node that requires a hardware quote is never live without its policy
    // (fail-closed: no window where the requirement silently does not apply). A
    // store error here fails the whole registration — the node is not inserted.
    {
        let policy_err = svc.app.store.with(|store| {
            store.set_node_attestation_policy(&req.node_id, req.require_tpm_quote).is_err()
        });
        if policy_err {
            return (StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "failed to persist attestation policy" }))).into_response();
        }
    }

    if svc.app.persist_and_insert_node(node).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to persist node" }))).into_response();
    }

    (StatusCode::CREATED, Json(json!({ "node_id": req.node_id, "status": "registered" }))).into_response()
}

pub(crate) async fn issue_challenge(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    if !svc.app.nodes.contains_key(&node_id) {
        return (StatusCode::NOT_FOUND,
                Json(json!({ "error": "node not registered" }))).into_response();
    }
    // #147: the challenge nonce comes from a CSPRNG (OsRng), NEVER the wall
    // clock. A `SystemTime`-derived nonce is predictable and can collide within
    // a single nanosecond; single-use + TTL + node-binding are enforced by the
    // challenge store and the verify-then-consume order in `verify_attestation`.
    let nonce = kirra_verifier::verifier::generate_challenge_nonce();
    svc.app.issue_challenge(&node_id, nonce, now_ms());
    (StatusCode::OK, Json(json!({ "node_id": node_id, "nonce": nonce }))).into_response()
}

pub(crate) async fn verify_attestation(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<VerifyAttestationRequest>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    let now = now_ms();

    // SAFETY: SG9 | REQ: attestation-node-proven-identity | TEST: valid_signature_verifies,legacy_admin_token_hmac_proof_is_rejected,absent_registered_key_fails_closed,wrong_key_is_rejected
    // (#73) Node-PROVEN identity: the node must prove possession of the
    // PRIVATE attestation key matching the `ak_public_pem` it registered, by
    // signing the (node_id, nonce) challenge with Ed25519. The prior
    // `HMAC(KIRRA_ADMIN_TOKEN, nonce)` proof was admin-ASSERTED trust —
    // anyone with the admin token could attest any node. Fail-closed: a node
    // with no registered AK, a malformed key, a malformed proof, or a bad
    // signature is rejected here, before the nonce is consumed or any trust
    // state is written. PCR16 (measured-boot) binding: when the node registered an
    // `expected_pcr16_digest_hex`, the proof must carry a matching digest BOUND
    // into the AK signature (`verify_attestation_proof_with_pcr16`); a node with no
    // expectation is unaffected. A hardware TPM *quote* (the deeper measured-boot
    // root) is enforced just below for a node whose policy requires it.
    let (ak_public_pem, expected_pcr16) = match svc.app.nodes.get(&req.node_id) {
        Some(node) => (node.ak_public_pem.clone(), node.expected_pcr16_digest_hex.clone()),
        None => return (StatusCode::NOT_FOUND,
                        Json(json!({ "error": "node not registered" }))).into_response(),
    };

    if let Err(reason) = kirra_verifier::attestation::verify_attestation_proof_with_pcr16(
        ak_public_pem.as_deref(),
        &req.node_id,
        req.nonce,
        &req.proof_hex,
        expected_pcr16.as_deref(),
        req.presented_pcr16_digest_hex.as_deref(),
    ) {
        // No registered key is a precondition failure (403); a measured-boot
        // mismatch is a forbidden boot state (403); a present-but-failing
        // signature is an authentication failure (401). Either way the
        // attestation is REFUSED — never accepted by default, and the nonce is
        // NOT consumed (this is before `consume_challenge`), so a node can retry
        // with a corrected measured boot.
        use kirra_verifier::attestation::AttestationError;
        let status = match reason {
            AttestationError::NoRegisteredKey | AttestationError::Pcr16Mismatch => {
                StatusCode::FORBIDDEN
            }
            _ => StatusCode::UNAUTHORIZED,
        };
        tracing::warn!(node_id = %req.node_id, reason = %reason.as_str(),
            "attestation proof rejected (fail-closed, #73)");
        return (status, Json(json!({ "error": reason.as_str() }))).into_response();
    }

    // SAFETY: SG9 | REQ: attestation-tpm-quote-enforcement | TEST: tpm_quote_required_but_absent_is_refused,tpm_quote_valid_attests_node_trusted,tpm_quote_invalid_is_refused_and_nonce_preserved,tpm_quote_policy_absent_is_back_compat
    // Hardware-rooted measured boot. The #73/#572 check above proves a
    // SELF-REPORTED PCR16 digest under the AK — a node in control of its AK
    // could sign a FALSE digest. A node enrolled with `require_tpm_quote` must
    // additionally present a TPM QUOTE: the TPM itself signs the live PCR bank +
    // the challenge nonce, so a forged boot state cannot be minted in software.
    // Fail-closed: a policy-lookup error, a required-but-absent quote, an absent
    // expectation to check against, or an invalid quote all REJECT. Runs BEFORE
    // `consume_challenge`, so a quote failure does NOT burn the nonce (retry).
    let require_quote = match svc.app.store.with(|store| store.node_requires_tpm_quote(&req.node_id)) {
        Ok(v) => v,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR,
                          Json(json!({ "error": "attestation policy lookup failed" }))).into_response(),
    };
    match (&req.tpm_quote, require_quote) {
        (Some(quote), _) => {
            // The quote attests a HASH OVER the PCR16 value; the registered datum
            // is the value itself. Bridge via `expected_single_pcr_digest_hex`.
            let expected_digest = match expected_pcr16
                .as_deref()
                .and_then(kirra_verifier::tpm_quote::expected_single_pcr_digest_hex)
            {
                Some(d) => d,
                None => {
                    tracing::warn!(node_id = %req.node_id,
                        "tpm quote presented but node has no expected PCR16 to verify against");
                    return (StatusCode::FORBIDDEN,
                            Json(json!({ "error": "tpm quote presented but no expected PCR16 registered" }))).into_response();
                }
            };
            let nonce_bytes = req.nonce.to_be_bytes();
            if let Err(e) = kirra_verifier::tpm_quote::verify_tpm_quote(
                ak_public_pem.as_deref(),
                &nonce_bytes,
                &expected_digest,
                &quote.quote_msg_hex,
                &quote.signature_hex,
            ) {
                use kirra_verifier::tpm_quote::TpmQuoteError;
                // A bad signature / unparseable bytes is an authentication failure
                // (401); everything else (no key, wrong magic/type, nonce, PCR
                // selection, digest) is a forbidden boot/identity state (403).
                let status = match e {
                    TpmQuoteError::SignatureInvalid
                    | TpmQuoteError::MalformedEncoding
                    | TpmQuoteError::MalformedQuote => StatusCode::UNAUTHORIZED,
                    _ => StatusCode::FORBIDDEN,
                };
                tracing::warn!(node_id = %req.node_id, reason = %e.as_str(),
                    "tpm quote rejected (fail-closed) — nonce preserved");
                return (status, Json(json!({ "error": e.as_str() }))).into_response();
            }
        }
        (None, true) => {
            tracing::warn!(node_id = %req.node_id,
                "node policy requires a tpm quote but none was presented");
            return (StatusCode::FORBIDDEN,
                    Json(json!({ "error": "tpm quote required by node policy but not presented" }))).into_response();
        }
        (None, false) => { /* back-compat: no quote required, none presented */ }
    }

    if !svc.app.consume_challenge(&req.node_id, req.nonce, now) {
        return (StatusCode::CONFLICT,
                Json(json!({ "error": "nonce absent, expired, or already consumed" }))).into_response();
    }

    let updated = match svc.app.nodes.get(&req.node_id) {
        Some(existing) => RegisteredNode {
            node_id: existing.node_id.clone(),
            status: NodeTrustState::Trusted,
            registered_at_ms: existing.registered_at_ms,
            last_trust_update_ms: now,
            ak_public_pem: existing.ak_public_pem.clone(),
            expected_pcr16_digest_hex: existing.expected_pcr16_digest_hex.clone(),
            site: existing.site.clone(),
            firmware_version: existing.firmware_version.clone(),
        },
        None => return (StatusCode::NOT_FOUND,
                        Json(json!({ "error": "node not registered" }))).into_response(),
    };

    if svc.app.persist_and_insert_node(updated).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to persist trust state" }))).into_response();
    }

    let posture = svc.app.calculate_posture(&req.node_id);
    if let Ok(posture_json) = serde_json::to_string(&posture) {
        // P1: durable audit write off the worker pool. Own the node id (reused below).
        let node_id_c = req.node_id.clone();
        let _ = svc.app.store.call(move |store| {
            if let Err(e) = store.save_posture_event_chained(
                &node_id_c, "ATTESTATION_TRUSTED", &posture_json, None, now,
            ) {
                tracing::error!(error=%e, node_id=%node_id_c,
                    "AUDIT-CHAIN WRITE FAILED for ATTESTATION_TRUSTED — event missing from tamper-evident log");
            }
        }).await;
    }
    emit_posture_event(&svc.app, "NODE_STATUS_CHANGED", Some(req.node_id.clone()));
    enqueue_recalc(&svc, kirra_verifier::posture_engine_v2::PostureRecalcTrigger::NodeTrustChanged {
        node_id: req.node_id.clone(),
        reason:  "ATTESTATION_TRUSTED".to_string(),
    });

    (StatusCode::OK, Json(json!({ "node_id": req.node_id, "attested": true }))).into_response()
}

pub(crate) async fn get_node_status(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    match svc.app.nodes.get(&node_id) {
        Some(node) => {
            let status = match &node.status {
                NodeTrustState::Trusted => "Trusted",
                NodeTrustState::Untrusted(_) => "Untrusted",
                NodeTrustState::Unknown => "Unknown",
            };
            (StatusCode::OK, Json(AttestationStatusResponse {
                node_id: node_id.clone(),
                status: status.to_string(),
                registered_at_ms: node.registered_at_ms,
            })).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
    }
}

pub(crate) async fn register_node_identity(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterIdentityRequest>,
) -> impl IntoResponse {
    if req.node_id.trim().is_empty() || req.ak_public_fingerprint_hex.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({ "error": "node_id and ak_public_fingerprint_hex are required" }))).into_response();
    }
    // P1: durable identity write off the worker pool. Own the request fields.
    let now = now_ms();
    let node_id_c = req.node_id.clone();
    let fingerprint = req.ak_public_fingerprint_hex.clone();
    let registered = svc.app.store.call(move |store| store.register_attestation_identity(
        &node_id_c, &fingerprint, "admin", now,
    )).await;
    match registered {
        Ok(Ok(())) => {
            emit_posture_event(&svc.app, "NODE_IDENTITY_PROVISIONED", Some(req.node_id.clone()));
            (StatusCode::CREATED,
             Json(json!({ "node_id": req.node_id, "registered": true }))).into_response()
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "failed to register identity" }))).into_response(),
    }
}
