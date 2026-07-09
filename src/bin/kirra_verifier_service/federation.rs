// src/bin/kirra_verifier_service/federation.rs
// federation route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use federation::*`) lets build_app/tests name them unqualified.

use super::*;

pub(crate) async fn register_federation_controller(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterFederationControllerRequest>,
) -> impl IntoResponse {
    if req.controller_id.trim().is_empty() || req.public_key_b64.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "controller_id and public_key_b64 are required" })),
        )
            .into_response();
    }
    // P1: durable write off the worker pool (`call` → spawn_blocking). Own the
    // captured request fields (still needed for the response). `call` wraps the
    // closure's own `Result` in an outer `Result<_, StoreError>`, so a DB error
    // OR a task failure both map to 500 (fail-closed, unchanged from `with`).
    let now = now_ms();
    let controller_id = req.controller_id.clone();
    let public_key_b64 = req.public_key_b64.clone();
    match svc
        .app
        .store
        .call(move |store| {
            store.save_trusted_federation_controller(&controller_id, &public_key_b64, now)
        })
        .await
    {
        Ok(Ok(())) => (
            StatusCode::CREATED,
            Json(json!({ "controller_id": req.controller_id, "registered": true })),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "failed to register controller" })),
        )
            .into_response(),
    }
}

pub(crate) async fn submit_federated_report(
    State(svc): State<Arc<ServiceState>>,
    // v2 wire: `source_generation` is optional, so a v1 report (no such field)
    // deserializes to a V2 with `source_generation: None` and its signed payload
    // is byte-identical to the v1 canonical payload — backward compatible. The
    // generation, when present, is inside the signed payload (cannot be forged
    // or stripped) and drives generation-ordered conflict resolution at read time.
    Json(report): Json<FederatedTrustReportV2>,
) -> impl IntoResponse {
    let received_at_ms = now_ms();

    let evaluation = evaluate_federated_report_v2(&report, received_at_ms);
    if !evaluation.accepted {
        return Json(evaluation).into_response();
    }

    // #79: held fencing token, read before locking the store. The durable commit
    // re-checks it INSIDE the transaction, closing the gate→commit TOCTOU.
    let held_epoch = svc.app.held_epoch.load(std::sync::atomic::Ordering::SeqCst);

    // The whole 5-step commit (key load → signature verify → nonce gate → chained
    // report+nonce-burn commit) runs under ONE lock; offload it to the blocking pool
    // so this multi-statement transaction — the heaviest write — can't pin a tokio
    // worker. The #79 epoch fence is preserved: `held_epoch` is read above (before
    // the lock, as before) and the durable commit re-checks it INSIDE its
    // transaction, so the slightly larger read→lock window remains harmless. All
    // rejection-path audit writes stay inside the locked closure (same atomicity).
    let outcome = svc
        .app
        .store
        .call(move |store| {
            let pk_b64 =
                match store.load_trusted_federation_controller_key(&report.source_controller_id) {
                    Ok(Some(key)) => key,
                    Ok(None) => {
                        let event = json!({ "source_controller_id": report.source_controller_id,
                                    "reason": "UNREGISTERED_FEDERATION_CONTROLLER" });
                        let _ = store.save_posture_event_chained(
                            "federation_gateway",
                            "FEDERATION_REJECTED",
                            &event.to_string(),
                            Some("unregistered source"),
                            received_at_ms,
                        );
                        return FedCommitOutcome::Rejected("UNREGISTERED_FEDERATION_CONTROLLER");
                    }
                    Err(_) => return FedCommitOutcome::InternalError("controller lookup failed"),
                };

            if !verify_federated_report_signature_v2(&report, &pk_b64) {
                let event = json!({ "source_controller_id": report.source_controller_id,
                                "reason": "INVALID_FEDERATION_SIGNATURE" });
                let _ = store.save_posture_event_chained(
                    "federation_gateway",
                    "FEDERATION_REJECTED",
                    &event.to_string(),
                    Some("signature mismatch"),
                    received_at_ms,
                );
                return FedCommitOutcome::Rejected("INVALID_FEDERATION_SIGNATURE");
            }

            match store.has_seen_federation_nonce(&report.nonce_hex) {
                Ok(true) => {
                    let event = json!({ "source_controller_id": report.source_controller_id,
                                    "nonce_hex": report.nonce_hex,
                                    "reason": "FEDERATION_NONCE_REPLAY" });
                    let _ = store.save_posture_event_chained(
                        "federation_gateway",
                        "FEDERATION_REJECTED",
                        &event.to_string(),
                        Some("nonce replay"),
                        received_at_ms,
                    );
                    return FedCommitOutcome::Rejected("FEDERATED_NONCE_REPLAY");
                }
                Ok(false) => {}
                Err(_) => return FedCommitOutcome::InternalError("nonce lookup failed"),
            }

            match store.save_federated_report_chained(
                &report.as_v1(),
                report.source_generation,
                received_at_ms,
                held_epoch,
            ) {
                Ok(()) => FedCommitOutcome::Accepted,
                Err(DurableWriteError::NonceReplay) => {
                    // H1: a replay raced past the `has_seen_federation_nonce` gate above and
                    // lost the durable single-use claim (PRIMARY KEY violation aborted the
                    // transaction — report NOT persisted, nonce NOT double-burned). Map it to
                    // the SAME clean rejection + audit as the gate path, not an opaque 500.
                    let event = json!({ "source_controller_id": report.source_controller_id,
                                    "nonce_hex": report.nonce_hex,
                                    "reason": "FEDERATION_NONCE_REPLAY" });
                    let _ = store.save_posture_event_chained(
                        "federation_gateway",
                        "FEDERATION_REJECTED",
                        &event.to_string(),
                        Some("nonce replay (concurrent)"),
                        received_at_ms,
                    );
                    FedCommitOutcome::Rejected("FEDERATED_NONCE_REPLAY")
                }
                Err(DurableWriteError::GenerationRegress { found, high_water }) => {
                    // Item 20: a validly-signed but stale (older-generation) report that
                    // slipped inside the freshness window. Reject fail-closed with a clean
                    // audit trail — the report was NOT persisted and no nonce was burned.
                    let event = json!({ "source_controller_id": report.source_controller_id,
                                    "asset_id": report.asset_id,
                                    "offered_generation": found,
                                    "high_water_generation": high_water,
                                    "reason": "FEDERATION_GENERATION_REGRESS" });
                    let _ = store.save_posture_event_chained(
                        "federation_gateway",
                        "FEDERATION_REJECTED",
                        &event.to_string(),
                        Some("generation regress/replay"),
                        received_at_ms,
                    );
                    FedCommitOutcome::Rejected("FEDERATED_GENERATION_REGRESS")
                }
                Err(DurableWriteError::Fenced(reason)) => {
                    FedCommitOutcome::Fenced(format!("{reason:?}"))
                }
                Err(DurableWriteError::Db(_)) => {
                    FedCommitOutcome::InternalError("failed to persist federated report")
                }
            }
        })
        .await;

    let outcome = match outcome {
        Ok(o) => o,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "store lock poisoned" })),
            )
                .into_response()
        }
    };

    match outcome {
        FedCommitOutcome::Accepted => Json(evaluation).into_response(),
        FedCommitOutcome::Rejected(reason) => Json(ReportEvaluation {
            accepted: false,
            reason: reason.to_string(),
        })
        .into_response(),
        FedCommitOutcome::InternalError(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": msg })),
        )
            .into_response(),
        FedCommitOutcome::Fenced(reason) => {
            // Superseded between the request-path gate and this commit. Mirror the
            // gate: self-demote and reject fail-closed — the report was NOT persisted
            // and the nonce was NOT burned (the transaction was dropped in the closure).
            svc.app
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            tracing::error!(
                path = "/federation/reports/submit",
                fence = %reason,
                "FENCED at top-tier write (in-transaction epoch re-check) — self-demoting to PassiveStandby and rejecting"
            );
            (StatusCode::SERVICE_UNAVAILABLE,
             Json(json!({ "error": "fenced: epoch superseded; instance demoted to passive standby" }))).into_response()
        }
    }
}

pub(crate) async fn get_federated_reports(
    State(svc): State<Arc<ServiceState>>,
    Path(asset_id): Path<String>,
) -> impl IntoResponse {
    // Both reads share ONE acquisition (Rule 5). The closure returns a Result so
    // the per-read error responses are produced OUTSIDE the closure (Rule 4).
    let now = now_ms();
    let loaded = svc.app.store.with_read(|store| {
        let reports = store
            .load_federated_reports_for_asset(&asset_id)
            .map_err(|_| "failed to load reports")?;
        // #329 v2 — generation-ordered conflict resolution. Reconcile the stored
        // reports into the single authoritative posture (higher generation wins;
        // ties fall back to issued_at_ms, then fail closed to the more restrictive
        // posture). `null` when no reports exist for the asset. This is a read-time
        // view only — it does NOT feed the local posture engine that gates actuators.
        let v2s = store
            .load_federated_report_v2s_for_asset(&asset_id)
            .map_err(|_| "failed to reconcile reports")?;
        let authoritative = authoritative_posture(&v2s);
        // CR1 (#692) dissent overlay: the gen-ordered `authoritative` can mask a
        // lagging peer's genuine LockedOut. Surface the most restrictive posture
        // among still-FRESH reports that is strictly above the authoritative one,
        // so an operator is never blind to a live lockout. Additive + advisory:
        // never relaxes the authoritative value, never feeds the actuator gate.
        let dissent = authoritative.and_then(|auth| dissenting_restriction(&v2s, auth, now));
        Ok::<_, &'static str>((reports, authoritative, dissent))
    });
    let (reports, authoritative, dissent) = match loaded {
        Ok(triple) => triple,
        Err(msg) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": msg })),
            )
                .into_response()
        }
    };

    Json(json!({
        "asset_id": asset_id,
        "reports": reports,
        "authoritative_posture": authoritative,
        // `null` when no fresh report dissents above the authoritative posture.
        "dissenting_restriction": dissent,
    }))
    .into_response()
}
