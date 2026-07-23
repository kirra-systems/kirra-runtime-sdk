// src/bin/kirra_verifier_service/operators.rs
// operators route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use operators::*`) lets build_app/tests name them unqualified.

use super::*;

/// POST /console/operators — register (or rotate) an operator's Ed25519 PUBLIC key
/// (#314 Phase 1). **ADMIN-token-gated** (the node-registration precedent). Admin
/// and supervisor are SEPARATE powers: this route lives behind
/// `require_admin_token`, so the **supervisor key cannot register operators**. The
/// private key NEVER reaches the server — only the public key is stored.
pub(crate) async fn register_operator(
    State(svc): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Json(req): Json<RegisterOperatorRequest>,
) -> impl IntoResponse {
    let operator_id = req.operator_id.trim();
    if operator_id.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "operator_id must be non-empty" })),
        )
            .into_response();
    }
    // #326: reject the legacy `|` delimiter and control characters in the id.
    if !valid_identifier(operator_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "operator_id must not contain '|' or control characters"
            })),
        )
            .into_response();
    }
    // Fail-closed: the PEM must parse as a valid Ed25519 SPKI public key.
    let fingerprint = match kirra_safety_authority::attestation::operator_key_fingerprint(
        &req.ed25519_pubkey_pem,
    ) {
        Some(fp) => fp,
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": "ed25519_pubkey_pem is not a valid Ed25519 SubjectPublicKeyInfo PEM"
                })),
            )
                .into_response()
        }
    };
    let now = now_ms();
    // The revoked-check + register + audit run under ONE acquisition (Rule 5).
    // The closure returns the fully-built response; `call` offloads it off the
    // worker pool (SAFETY: SG-HA-3).
    let op_id = operator_id.to_string();
    let pubkey = req.ed25519_pubkey_pem.clone();
    let fp = fingerprint.clone();
    let admin_fp = admin_token_fingerprint(&headers);
    let resp = match svc
        .app
        .store
        .call_shared(move |shared| {
            // #327: detect a prior REVOKED row BEFORE registering — register_operator
            // silently clears revoked_at, so reactivation would otherwise be
            // invisible in the ledger. Record it as a distinct, attributed event.
            let was_revoked = shared
                .load_operator(&op_id)
                .ok()
                .flatten()
                .is_some_and(|o| o.revoked_at_ms.is_some());
            if shared.register_operator(&op_id, &pubkey, now).is_err() {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "persist failed" })),
                )
                    .into_response();
            }
            if was_revoked {
                // A previously-revoked operator is now active again — the
                // reactivating admin is attributed by token fingerprint (#327).
                let _ = shared.ledger_append_checked(
                    "OperatorReactivated",
                    &json!({
                        "operator_id": op_id,
                        "operator_key_fingerprint": fp,
                        "reactivated_by_admin_fingerprint": admin_fp,
                    })
                    .to_string(),
                    now,
                );
                (
                    StatusCode::CREATED,
                    Json(json!({
                        "operator_id": op_id,
                        "operator_key_fingerprint": fp,
                        "status": "reactivated",
                    })),
                )
                    .into_response()
            } else {
                let _ = shared.ledger_append_checked(
                    "OperatorRegistered",
                    &json!({ "operator_id": op_id, "operator_key_fingerprint": fp }).to_string(),
                    now,
                );
                (
                    StatusCode::CREATED,
                    Json(json!({
                        "operator_id": op_id,
                        "operator_key_fingerprint": fp,
                        "status": "registered",
                    })),
                )
                    .into_response()
            }
        })
        .await
    {
        Ok(r) => r,
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "store task failed" })),
        )
            .into_response(),
    };
    resp
}

/// POST /console/operators/{operator_id}/revoke — revoke an operator (#314).
/// ADMIN-token-gated. A revoked operator can never clear a grant.
pub(crate) async fn revoke_operator(
    State(svc): State<Arc<ServiceState>>,
    Path(operator_id): Path<String>,
) -> impl IntoResponse {
    let now = now_ms();
    // revoke + its audit event share one acquisition (Rule 5); the closure
    // returns the built response.
    // SAFETY: SG-HA-3 — write off the worker pool.
    let op_id = operator_id.to_string();
    match svc
        .app
        .store
        .call_shared(move |shared| match shared.revoke_operator(&op_id, now) {
            Ok(true) => {
                let _ = shared.ledger_append_checked(
                    "OperatorRevoked",
                    &json!({ "operator_id": op_id }).to_string(),
                    now,
                );
                (
                    StatusCode::OK,
                    Json(json!({ "operator_id": op_id, "status": "revoked" })),
                )
                    .into_response()
            }
            Ok(false) => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "operator not found or already revoked" })),
            )
                .into_response(),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "persist failed" })),
            )
                .into_response(),
        })
        .await
    {
        Ok(r) => r,
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "store task failed" })),
        )
            .into_response(),
    }
}

/// GET /console/clearance-challenge?operator_id=&node_id= — issue a one-time nonce
/// the operator signs to prove key possession (#314 Phase 1; the attestation
/// challenge-issuance pattern). Unauthenticated (the nonce alone grants nothing —
/// only a valid signature over it does). #325: NO enumeration oracle — every
/// well-formed request returns a uniform 200 with a nonce-shaped body, so an
/// unknown/revoked operator is indistinguishable from a known one. A real challenge
/// is stored ONLY for an ACTIVE operator (verify-then-consume at grant time); anyone
/// else gets a DECOY nonce that is never stored (no map growth) and that a grant
/// attempt still rejects at the unchanged grant-time operator check.
pub(crate) async fn clearance_challenge(
    State(svc): State<Arc<ServiceState>>,
    Query(q): Query<ClearanceChallengeQuery>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "instance is in passive standby mode" })),
        )
            .into_response();
    }
    let operator_id = q.operator_id.trim();
    let node_id = q.node_id.trim();
    if operator_id.is_empty() || node_id.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "operator_id and node_id are required" })),
        )
            .into_response();
    }
    let op_id = operator_id.to_string();
    let active = match svc
        .app
        .store
        .call_shared(move |shared| {
            Ok::<bool, ()>(
                shared
                    .load_operator(&op_id)
                    .ok()
                    .flatten()
                    .map(|o| o.is_active())
                    .unwrap_or(false),
            )
        })
        .await
    {
        Ok(Ok(v)) => v,
        _ => false,
    };
    // Hex string (not a u64) so the in-browser signing flow never loses precision.
    // Sec3 (#1050): a transient CSPRNG stall returns 503, never aborts the process.
    let nonce_raw = match kirra_verifier::verifier::generate_challenge_nonce() {
        Ok(n) => n,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "entropy source unavailable — retry" })),
            )
                .into_response();
        }
    };
    let nonce_hex = format!("{nonce_raw:016x}");
    // #325: store a REAL challenge only for an active operator; everyone else gets a
    // decoy nonce that is never recorded (no oracle, no map growth). #326: the
    // challenge-map key is length-prefixed (unambiguous operator/node split).
    if active {
        let key = composite_challenge_key(operator_id, node_id);
        svc.app
            .issue_clearance_challenge(&key, nonce_hex.clone(), now_ms());
    }
    (
        StatusCode::OK,
        Json(json!({
            "operator_id": operator_id,
            "node_id": node_id,
            "nonce": nonce_hex,
            "ttl_ms": 30_000,
        })),
    )
        .into_response()
}

/// POST /console/clearance-grants — the ONE inbound console affordance, upgraded
/// to operator-proven identity (#314 Phase 1).
///
/// **RECORD-ONLY (Phase A):** records + signs a clearance grant; it does NOT
/// release the node (delivery to the node `ClearanceLoop` is Phase B). The server
/// stamps `granted_at_ms` from ITS clock (no client time → no future-dating).
/// Never mutates posture.
///
/// Two auth paths:
///   * **operator-signed (PRIMARY):** a registered operator proves key possession
///     by signing the challenge nonce. Handler order mirrors `verify_attestation`:
///     load operator (unknown/revoked → 403) → VERIFY signature → CONSUME nonce
///     (verify-then-consume; replay → 401) → well-formedness → record with
///     `auth_method="operator-signed"` + the key fingerprint, BOTH embedded in the
///     signed chain event (the non-repudiation payoff).
///   * **supervisor-break-glass (NAMED FALLBACK):** the shared `KIRRA_SUPERVISOR_
///     RESET_KEY` (#255) REMAINS as an explicitly-named break-glass path —
///     `auth_method="supervisor-break-glass"`, audited distinctly and loudly —
///     because a fleet locked out of recovery by a lost operator key is its OWN
///     safety failure. Deployments disable it by unsetting the env. The console UI
///     presents operator-signed as the primary flow.
pub(crate) async fn console_clearance_grant(
    State(svc): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Json(req): Json<ClearanceGrantRequest>,
) -> impl IntoResponse {
    // HA split-brain guard (#323): recording a grant is a store MUTATION. The
    // `/console` posture-exemption keeps this reachable during LockedOut, but that
    // is about POSTURE — it does NOT justify a passive-standby instance writing
    // grants and diverging from the primary. Fail-closed like every other mutation.
    if !svc.app.is_active() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "instance is in passive standby mode" })),
        )
            .into_response();
    }
    let now = now_ms();
    let operator_id = req.operator_id.trim().to_string();
    let node_id = req.node_id.trim().to_string();

    let operator_signed = req.signature_b64.as_deref().is_some_and(|s| !s.is_empty());

    let (auth_method, fingerprint): (&str, Option<String>) = if operator_signed {
        // === OPERATOR-SIGNED PATH — verify-then-consume (mirrors verify_attestation).
        let nonce = req.nonce.clone().unwrap_or_default();
        let sig_b64 = req.signature_b64.clone().unwrap_or_default();
        if nonce.is_empty() {
            audit_grant_rejection(&svc.app, "missing_nonce", &node_id, &operator_id, now).await;
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "status": "rejected", "reason": "nonce required for an operator-signed grant"
                })),
            )
                .into_response();
        }
        // 1. Load operator — unknown / revoked → 403, audited.
        let op_id_c = operator_id.clone();
        let maybe_op = svc
            .app
            .store
            .call_shared(move |s| Ok::<_, ()>(s.load_operator(&op_id_c).ok().flatten()))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten();
        let operator = match maybe_op {
            Some(op) if op.is_active() => op,
            Some(_) => {
                audit_grant_rejection(&svc.app, "revoked_operator", &node_id, &operator_id, now)
                    .await;
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "status": "rejected", "reason": "operator is revoked"
                    })),
                )
                    .into_response();
            }
            None => {
                audit_grant_rejection(&svc.app, "unknown_operator", &node_id, &operator_id, now)
                    .await;
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "status": "rejected", "reason": "operator is not registered"
                    })),
                )
                    .into_response();
            }
        };
        // 2. VERIFY signature FIRST (before the nonce is consumed).
        use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
        let sig_bytes = match b64e.decode(sig_b64.trim()) {
            Ok(b) => b,
            Err(_) => {
                audit_grant_rejection(&svc.app, "malformed_signature", &node_id, &operator_id, now)
                    .await;
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({
                        "status": "rejected", "reason": "signature is not valid base64"
                    })),
                )
                    .into_response();
            }
        };
        let payload = kirra_safety_authority::attestation::operator_grant_signing_payload(
            &operator_id,
            &node_id,
            &nonce,
        );
        if !kirra_safety_authority::attestation::verify_ed25519_pem_signature(
            &operator.pubkey_pem,
            &payload,
            &sig_bytes,
        ) {
            audit_grant_rejection(&svc.app, "bad_signature", &node_id, &operator_id, now).await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "status": "rejected", "reason": "signature verification failed"
                })),
            )
                .into_response();
        }
        // 3. CONSUME the nonce (verify-then-consume; replay/expired → 401, audited).
        //    #326: same length-prefixed composite key the challenge issuer used.
        let key = composite_challenge_key(&operator_id, &node_id);
        if !svc.app.consume_clearance_challenge(&key, &nonce, now) {
            audit_grant_rejection(
                &svc.app,
                "nonce_replay_or_expired",
                &node_id,
                &operator_id,
                now,
            )
            .await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "status": "rejected",
                    "reason": "challenge nonce absent, expired, or already used (replay rejected)"
                })),
            )
                .into_response();
        }
        let fp =
            kirra_safety_authority::attestation::operator_key_fingerprint(&operator.pubkey_pem);
        ("operator-signed", fp)
    } else {
        // === BREAK-GLASS PATH — the named, distinctly-audited supervisor fallback.
        if let Err(code) = check_supervisor_key(&headers) {
            if code == StatusCode::UNAUTHORIZED {
                audit_grant_rejection(
                    &svc.app,
                    "supervisor_unauthorized",
                    &node_id,
                    &operator_id,
                    now,
                )
                .await;
            }
            return (
                code,
                Json(json!({
                    "status": "rejected",
                    "reason": "no operator signature and supervisor authorization failed"
                })),
            )
                .into_response();
        }
        tracing::warn!(node_id = %node_id, operator_id = %operator_id,
            "BREAK-GLASS clearance grant via supervisor key — operator-signed path bypassed \
             (auth_method=supervisor-break-glass; audited distinctly)");
        ("supervisor-break-glass", None)
    };

    // Shared well-formedness (mirrors OperatorClearanceGrant::is_well_formed).
    if operator_id.is_empty() {
        audit_grant_rejection(&svc.app, "empty_operator_id", &node_id, &operator_id, now).await;
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "status": "rejected", "reason": "operator_id must be non-empty"
            })),
        )
            .into_response();
    }
    let node_id_c = node_id.clone();
    let registered = svc
        .app
        .store
        .call_shared(move |shared| Ok::<bool, ()>(shared.node_exists(&node_id_c).unwrap_or(false)))
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or(false);
    if !registered {
        audit_grant_rejection(&svc.app, "unregistered_node", &node_id, &operator_id, now).await;
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "status": "rejected", "reason": "node_id is not a registered node"
            })),
        )
            .into_response();
    }

    // Record + sign — RECORD-ONLY. Delivery is Phase B; posture is untouched.
    // SAFETY: SG-HA-3 — durable write off the worker pool.
    let node_id_c = node_id.clone();
    let op_id_c = operator_id.clone();
    let auth_method_s = auth_method.to_string();
    let fp_c = fingerprint.clone();
    match svc.app.store.call_shared(move |shared| match shared.save_clearance_grant_chained_with_auth(
        &node_id_c, &op_id_c, now, &auth_method_s, fp_c.as_deref(),
    ) {
        Ok(_id) => (StatusCode::OK, Json(json!({
            "status": "recorded",
            "delivery": "pending-phase-b",
            "node_id": node_id_c,
            "operator_id": op_id_c,
            "granted_at_ms": now,
            "auth_method": auth_method_s,
            "operator_key_fingerprint": fp_c,
            "note": "grant recorded and signed; the vehicle is NOT released — delivery to the node ClearanceLoop is Phase B",
        }))).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "status": "error", "reason": "persist failed" }))).into_response(),
    }).await {
        Ok(r) => r,
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "status": "error", "reason": "store task failed" }))).into_response(),
    }
}

/// POST /console/estop-requests — a governor-routed authenticated EMERGENCY-STOP
/// request (#412 / ADR-0013). The clearance verb INVERTED: an authenticated
/// operator REQUESTS a stop and the governor, judging the request, commands the
/// MRC under ITS OWN authority. The console asks; the governor acts — the QM-domain
/// console NEVER touches the actuator (ADR-0006 boundary).
///
/// Operator-signed ONLY (no supervisor break-glass): ADR-0013 constraint #2
/// requires the stop be NON-REPUDIABLE — provably the operator's in the chain —
/// which a shared key cannot give. Handler order mirrors `console_clearance_grant`
/// / `verify_attestation`: load operator (unknown/revoked → 403) → VERIFY the
/// stop signature (domain-distinct from a clearance grant) → CONSUME the nonce
/// (verify-then-consume; replay → 401) → node registered → chain
/// `OperatorStopRequested`, command the MRC (sticky fleet LockedOut), chain
/// `GovernorMRCCommanded`.
///
/// Unlike the RECORD-ONLY clearance grant, accepting this request DOES mutate
/// posture — that is the point: the governor commands the MRC. Recovery is a
/// deliberate supervisor reset (the clearance/release inverse), not automatic.
pub(crate) async fn console_estop_request(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<OperatorStopRequest>,
) -> impl IntoResponse {
    // HA split-brain guard: commanding the MRC is a posture mutation; a passive
    // standby must not act and diverge from the primary (mirrors the clearance grant).
    if !svc.app.is_active() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "instance is in passive standby mode" })),
        )
            .into_response();
    }
    let now = now_ms();
    let operator_id = req.operator_id.trim().to_string();
    let node_id = req.node_id.trim().to_string();
    let nonce = req.nonce.trim().to_string();
    let sig_b64 = req.signature_b64.trim().to_string();

    if operator_id.is_empty() || node_id.is_empty() || nonce.is_empty() || sig_b64.is_empty() {
        audit_estop_rejection(&svc.app, "malformed_request", &node_id, &operator_id, now).await;
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "status": "rejected",
                "reason": "operator_id, node_id, nonce, and signature_b64 are all required"
            })),
        )
            .into_response();
    }

    // 1. Load operator — unknown / revoked → 403, audited.
    let op_id_c = operator_id.clone();
    let maybe_op = svc
        .app
        .store
        .call_shared(move |s| Ok::<_, ()>(s.load_operator(&op_id_c).ok().flatten()))
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten();
    let operator = match maybe_op {
        Some(op) if op.is_active() => op,
        Some(_) => {
            audit_estop_rejection(&svc.app, "revoked_operator", &node_id, &operator_id, now).await;
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "status": "rejected", "reason": "operator is revoked"
                })),
            )
                .into_response();
        }
        None => {
            audit_estop_rejection(&svc.app, "unknown_operator", &node_id, &operator_id, now).await;
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "status": "rejected", "reason": "operator is not registered"
                })),
            )
                .into_response();
        }
    };

    // 2. VERIFY the stop signature FIRST (before the nonce is consumed). The
    //    payload is under OPERATOR_STOP_DOMAIN — a clearance signature cannot
    //    satisfy it, nor vice versa.
    use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
    let sig_bytes = match b64e.decode(sig_b64.as_str()) {
        Ok(b) => b,
        Err(_) => {
            audit_estop_rejection(&svc.app, "malformed_signature", &node_id, &operator_id, now)
                .await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "status": "rejected", "reason": "signature is not valid base64"
                })),
            )
                .into_response();
        }
    };
    let payload = kirra_safety_authority::attestation::operator_stop_signing_payload(
        &operator_id,
        &node_id,
        &nonce,
    );
    if !kirra_safety_authority::attestation::verify_ed25519_pem_signature(
        &operator.pubkey_pem,
        &payload,
        &sig_bytes,
    ) {
        audit_estop_rejection(&svc.app, "bad_signature", &node_id, &operator_id, now).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "status": "rejected", "reason": "signature verification failed"
            })),
        )
            .into_response();
    }

    // 3. CONSUME the nonce (verify-then-consume; replay/expired → 401). Reuses the
    //    SAME challenge channel as clearance — the domain-distinct payload above is
    //    what keeps the two verbs non-interchangeable.
    let key = composite_challenge_key(&operator_id, &node_id);
    if !svc.app.consume_clearance_challenge(&key, &nonce, now) {
        audit_estop_rejection(
            &svc.app,
            "nonce_replay_or_expired",
            &node_id,
            &operator_id,
            now,
        )
        .await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "status": "rejected",
                "reason": "challenge nonce absent, expired, or already used (replay rejected)"
            })),
        )
            .into_response();
    }

    // 4. The target must be a registered node. A store/read FAILURE is NOT an
    //    "unregistered node" — distinguish it: a genuine not-found → 422, but a
    //    lookup error → 500 (still fail-closed: the MRC is never commanded, and the
    //    audit reason is accurate rather than a misleading client-error). The
    //    closure preserves the query Result (not `unwrap_or(false)`) so the two
    //    cases are separable (Copilot PR #718).
    let node_id_c = node_id.clone();
    let lookup = svc
        .app
        .store
        .call_shared(move |shared| shared.node_exists(&node_id_c).map_err(|_| ()))
        .await;
    let registered = match lookup {
        Ok(Ok(exists)) => exists,
        _ => {
            audit_estop_rejection(&svc.app, "node_lookup_failed", &node_id, &operator_id, now)
                .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "status": "error", "reason": "node registration lookup failed"
                })),
            )
                .into_response();
        }
    };
    if !registered {
        audit_estop_rejection(&svc.app, "unregistered_node", &node_id, &operator_id, now).await;
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "status": "rejected", "reason": "node_id is not a registered node"
            })),
        )
            .into_response();
    }

    let fingerprint =
        kirra_safety_authority::attestation::operator_key_fingerprint(&operator.pubkey_pem);

    // 5a. Chain the authenticated REQUEST (who/when) — non-repudiable.
    {
        let node_id_c = node_id.clone();
        let op_id_c = operator_id.clone();
        let fp_c = fingerprint.clone();
        let _ = svc
            .app
            .store
            .call(move |s| {
                let _ = s.append_clearance_audit_event(
                    "OperatorStopRequested",
                    &json!({
                        "node_id": node_id_c,
                        "operator_id": op_id_c,
                        "operator_key_fingerprint": fp_c,
                        "auth_method": "operator-signed",
                    })
                    .to_string(),
                    now,
                );
            })
            .await;
    }

    // 5b. Command the MRC under the GOVERNOR's own authority — sticky fleet
    //     LockedOut. The exact escalation idiom (force_lockout doc / telemetry
    //     watchdog): set the sticky flag THEN force the cache, so any surviving
    //     recalc keeps producing LockedOut. The console asks; the governor acts.
    svc.app
        .escalation
        .supervisor_tripped
        .store(true, std::sync::atomic::Ordering::SeqCst);
    kirra_verifier::posture_engine::force_lockout(
        &svc.posture_cache,
        &svc.app.ha_fence.held_epoch,
        now,
    );

    // 5c. Chain what the governor DID (reconstructable after the fact).
    {
        let node_id_c = node_id.clone();
        let op_id_c = operator_id.clone();
        let _ = svc
            .app
            .store
            .call(move |s| {
                let _ = s.append_clearance_audit_event(
                    "GovernorMRCCommanded",
                    &json!({
                        "node_id": node_id_c,
                        "operator_id": op_id_c,
                        "mrc": "TRAJECTORY_MRC_FALLBACK",
                        "posture": "LockedOut",
                        "trigger": "operator-estop-request",
                    })
                    .to_string(),
                    now,
                );
            })
            .await;
    }

    tracing::warn!(node_id = %node_id, operator_id = %operator_id,
        "OPERATOR E-STOP REQUEST authenticated — governor commanded the MRC (sticky LockedOut). \
         The console did not touch the actuator; recovery is a deliberate supervisor reset.");

    (StatusCode::OK, Json(json!({
        "status": "stop_commanded",
        "node_id": node_id,
        "operator_id": operator_id,
        "operator_key_fingerprint": fingerprint,
        "governor_action": "MRC_COMMANDED",
        "posture": "LockedOut",
        "requested_at_ms": now,
        "note": "the governor commanded the MRC under its own authority; the console did not touch the actuator. Recovery requires a deliberate supervisor reset (the clearance inverse).",
    }))).into_response()
}
