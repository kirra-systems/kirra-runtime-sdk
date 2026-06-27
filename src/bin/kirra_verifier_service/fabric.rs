// src/bin/kirra_verifier_service/fabric.rs
// fabric route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use fabric::*`) lets build_app/tests name them unqualified.

use super::*;

pub(crate) async fn handle_register_fabric_asset(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<RegisterFabricAssetRequest>, JsonRejection>,
) -> impl IntoResponse {
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.body_text()}))).into_response(),
    };
    if req.asset_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "asset_id is required"}))).into_response();
    }
    let asset = FabricAsset {
        asset_id: req.asset_id.clone(),
        asset_type: req.asset_type,
        display_name: req.display_name,
        kinematic_profile: req.kinematic_profile,
        registered_at_ms: now_ms(),
        last_seen_ms: now_ms(),
        metadata: req.metadata.unwrap_or_default(),
    };
    svc.fabric_router.register_asset(&asset);
    // #88: if this IS the configured local asset, override the Degraded seed
    // with fail-closed LockedOut (the feed lifts it); a no-op for peers.
    seed_local_asset_lockedout(&svc, &req.asset_id);
    svc.app.store.with(|store| {
        let _ = store.save_fabric_asset(&asset);
    });
    (StatusCode::CREATED, Json(json!({"asset_id": req.asset_id, "registered": true}))).into_response()
}

pub(crate) async fn handle_list_fabric_assets(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    let assets = svc.fabric_router.list_assets();
    let total = assets.len();
    Json(json!({"assets": assets, "total": total})).into_response()
}

pub(crate) async fn handle_fabric_state(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    // SG-007: propagate AND record each rule-firing to the causal log (decisions
    // unchanged). Use the current fabric generation for the recorded events.
    let fabric_generation = svc.fabric_router.fabric_state().fabric_generation;
    let changes = svc.fabric_router.propagate_and_record(&svc.fabric_causal_log, fabric_generation);
    for (asset_id, new_posture) in changes {
        let gen = svc.fabric_router.fabric_state().fabric_generation + 1;
        svc.fabric_router.update_asset_posture(&asset_id, AssetPosture {
            asset_id: asset_id.clone(),
            posture: new_posture,
            generation: gen,
            computed_at_ms: now_ms(),
            contributing_nodes: vec![],
            blocked_by: vec!["cross_asset_propagation".to_string()],
        });
    }
    let state = svc.fabric_router.fabric_state();
    Json(state).into_response()
}

pub(crate) async fn handle_fabric_telemetry(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    let summary = svc.fabric_telemetry.summary();
    Json(summary).into_response()
}

pub(crate) async fn handle_fabric_telemetry_asset(
    State(svc): State<Arc<ServiceState>>,
    Path(asset_id): Path<String>,
) -> impl IntoResponse {
    match svc.fabric_telemetry.asset_snapshot(&asset_id) {
        Some(snap) => Json(snap).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "asset not found"}))).into_response(),
    }
}

pub(crate) async fn handle_fabric_command(
    State(svc): State<Arc<ServiceState>>,
    Path(asset_id): Path<String>,
    body: Result<Json<ProposedVehicleCommand>, JsonRejection>,
) -> impl IntoResponse {
    let cmd = match body {
        Ok(Json(c)) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.body_text()}))).into_response(),
    };

    // KIRRA-OCCY-PMON-002: resolve the perception-derate cap O(1) here (the
    // handler holds `svc`/`ServiceState`) and thread it through to the fabric
    // governor's Nominal arm. `None` while the monitor is disabled (default).
    let perception_cap = kirra_verifier::gateway::perception_monitor::resolve_perception_cap(
        svc.perception_monitor_enabled,
        &svc.perception_cap,
        now_ms(),
    );
    match svc.fabric_router.route_command(&asset_id, &cmd, perception_cap) {
        Ok(action) => {
            use kirra_verifier::gateway::kinematics_contract::EnforceAction;
            let action_str = format!("{:?}", action);
            let now = now_ms();
            let fabric_generation = svc.fabric_router.fabric_state().fabric_generation;
            let clamp_occurred =
                matches!(action, EnforceAction::ClampLinear(_) | EnforceAction::ClampSteering(_));

            // #86: APPLY the verdict server-side and return the ENFORCED command.
            // `apply_enforce_action` substitutes the safe clamp value(s); a clamp
            // therefore lands in the response's `command` field, so a client using
            // it is within envelope even if it ignores the `action` label.
            //
            // FAIL-CLOSED: deny when no enforced command can be produced —
            // `DenyBreach`, OR (defensively) a clamp whose enforced value is
            // non-finite. We NEVER return the unclamped command.
            let enforced = kirra_verifier::kinematics_sim::apply_enforce_action(&cmd, &action)
                .filter(|c| c.linear_velocity_mps.is_finite() && c.steering_angle_deg.is_finite());

            match enforced {
                None => {
                    // `DenyCode -> &'static str` keeps this path alloc-free; the previous
                    // `r.clone()` of a `String` allocated per denial (S3 / #115).
                    let denial_reason: &'static str = match &action {
                        EnforceAction::DenyBreach(c) => c.reason(),
                        // Clamp produced a non-finite enforced value (contract bug):
                        // fail closed rather than forward an invalid command.
                        _ => "ENFORCED_COMMAND_UNPRODUCIBLE",
                    };
                    svc.fabric_causal_log.record(
                        &asset_id,
                        "COMMAND_DENIED",
                        &json!({"reason": denial_reason, "command": serde_json::to_value(&cmd).unwrap_or_default()}).to_string(),
                        vec![],
                        vec![],
                        fabric_generation,
                    );
                    // P1: durable audit write off the worker pool (own the asset id,
                    // materialize the payload).
                    let asset_id_owned = asset_id.to_string();
                    let denied_payload =
                        json!({"asset_id": asset_id, "action": action_str}).to_string();
                    let _ = svc.app.store.call(move |store| {
                        let _ = store.save_posture_event_chained(
                            &asset_id_owned, "FABRIC_COMMAND_DENIED",
                            &denied_payload,
                            None, now,
                        );
                    }).await;
                    Json(json!({
                        "asset_id": asset_id,
                        "action": action_str,
                        "allowed": false,
                        "clamp_occurred": false,
                        "denial_reason": denial_reason,
                    })).into_response()
                }
                Some(enforced_cmd) => {
                    // A clamp is safety ENFORCEMENT, not a silent pass: record it
                    // (causal log + tamper-evident audit) with the original-vs-enforced
                    // values, mirroring the deny path and the actuator-handler pattern.
                    if clamp_occurred {
                        let enforcement = json!({
                            "asset_id": asset_id,
                            "action": action_str,
                            "clamp_occurred": true,
                            "original_linear_velocity_mps": cmd.linear_velocity_mps,
                            "original_steering_angle_deg": cmd.steering_angle_deg,
                            "enforced_linear_velocity_mps": enforced_cmd.linear_velocity_mps,
                            "enforced_steering_angle_deg": enforced_cmd.steering_angle_deg,
                        });
                        svc.fabric_causal_log.record(
                            &asset_id,
                            "COMMAND_CLAMPED",
                            &enforcement.to_string(),
                            vec![],
                            vec![],
                            fabric_generation,
                        );
                        // P1: durable audit write off the worker pool.
                        let asset_id_owned = asset_id.to_string();
                        let enforcement_str = enforcement.to_string();
                        let _ = svc.app.store.call(move |store| {
                            let _ = store.save_posture_event_chained(
                                &asset_id_owned, "FABRIC_COMMAND_CLAMPED",
                                &enforcement_str,
                                None, now,
                            );
                        }).await;
                    }

                    Json(json!({
                        "asset_id": asset_id,
                        "action": action_str,
                        "allowed": true,
                        "clamp_occurred": clamp_occurred,
                        "original_linear_velocity_mps": cmd.linear_velocity_mps,
                        "original_steering_angle_deg": cmd.steering_angle_deg,
                        "enforced_linear_velocity_mps": enforced_cmd.linear_velocity_mps,
                        "enforced_steering_angle_deg": enforced_cmd.steering_angle_deg,
                        // AUTHORITATIVE output: the enforced (post-clamp) command.
                        "command": enforced_cmd,
                    })).into_response()
                }
            }
        }
        Err(e) => {
            (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

pub(crate) async fn handle_fabric_causal_log(
    State(svc): State<Arc<ServiceState>>,
    Query(q): Query<CausalLogQuery>,
) -> impl IntoResponse {
    let from = q.from_ms.unwrap_or(0);
    let to = q.to_ms.unwrap_or(u64::MAX);
    // #87: bounded + paginated. `limit` is clamped to CAUSAL_EXPORT_MAX_PAGE
    // inside export_page so a forensic export is never unbounded.
    let limit = q
        .limit
        .unwrap_or(kirra_verifier::fabric::causal_log::CAUSAL_EXPORT_MAX_PAGE);
    let offset = q.offset.unwrap_or(0);
    let entries = svc.fabric_causal_log.export_page(from, to, limit, offset);
    let total = entries.len();
    Json(json!({"entries": entries, "total": total, "limit": limit, "offset": offset})).into_response()
}

/// #87: admin-gated verification of the causal-log forensic chain. Mirrors
/// `/system/audit/verify`. Mounted at `/system/audit/causal/verify` (NOT under
/// `/fabric/causal-log/...`, to avoid colliding with the `{entry_id}` wildcard).
pub(crate) async fn verify_causal_chain(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    let vk = svc.audit_verifying_key.as_ref();
    match svc.app.store.with(|store| store.verify_causal_chain_integrity(vk)) {
        Ok(r) => Json(json!({
            "chain_intact": r.chain_intact,
            "total_entries": r.total_entries,
            "latest_hash": r.latest_hash,
            "signing_enabled": r.signing_enabled,
            "signed_entries": r.signed_entries,
            "unsigned_entries": r.unsigned_entries,
            "signature_valid": r.signature_valid,
            "first_signed_at_ms": r.first_signed_at_ms,
            "public_key_b64": r.public_key_b64,
            "head_verified": r.head_verified,
            "head_status": r.head_status,
            "verified": r.chain_intact && r.signature_valid && r.head_verified,
        })).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "causal chain query failed" }))).into_response(),
    }
}

pub(crate) async fn handle_fabric_causal_chain(
    State(svc): State<Arc<ServiceState>>,
    Path(entry_id): Path<String>,
) -> impl IntoResponse {
    let chain = svc.fabric_causal_log.causal_chain(&entry_id);
    if chain.is_empty() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "entry not found"}))).into_response();
    }
    let depth = chain.len();
    Json(json!({"entry_id": entry_id, "chain": chain, "depth": depth})).into_response()
}
