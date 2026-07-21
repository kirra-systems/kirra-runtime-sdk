// src/bin/kirra_verifier_service/industrial.rs
// industrial route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use industrial::*`) lets build_app/tests name them unqualified.

use super::*;

/// Enforce industrial-message replay + freshness (IEC 62443). Returns `Some(reason)`
/// to REJECT (fail-closed), or `None` to proceed — advancing the per-source sequence
/// high-water mark atomically on accept. Freshness is checked BEFORE the sequence
/// advance, so a stale/future message never burns sequence space. A rejection is
/// audit-chained. `source_id` should be caller-namespaced (e.g. `"dnp3:plc-01"`) so
/// the same source string under different protocols does not collide.
pub(crate) async fn enforce_industrial_replay(
    svc: &ServiceState,
    protocol: &str,
    source_id: &str,
    sequence: u64,
    timestamp_ms: u64,
) -> Option<&'static str> {
    let now = now_ms();
    let fresh = kirra_industrial::protocol_adapter::classify_industrial_freshness(
        timestamp_ms,
        now,
        kirra_industrial::protocol_adapter::INDUSTRIAL_FRESHNESS_WINDOW_MS,
    );
    // The seq check-and-advance and the rejection audit write share ONE store
    // acquisition (Rule 5: keep the read-then-write atomic). P1: run that whole
    // group OFF the worker pool (`call` → spawn_blocking) — it fires on EVERY
    // industrial command (the seq advance is itself a write), so it's the hottest
    // durable path among the SCADA handlers. The single-acquisition atomicity is
    // preserved (the entire closure runs under one writer lock on the blocking
    // thread). Own the borrowed protocol/source strings into the closure; a task
    // failure → fail-closed reject (store unavailable).
    let protocol_owned = protocol.to_string();
    let source_owned = source_id.to_string();
    match svc
        .app
        .store
        .call(move |store| {
            let reason = match fresh {
                Some(r) => Some(r),
                None => {
                    match store.industrial_seq_check_and_advance(&source_owned, sequence, now) {
                        Ok(true) => None,
                        Ok(false) => Some("INDUSTRIAL_MESSAGE_REPLAY"),
                        Err(_) => Some("INDUSTRIAL_REPLAY_STORE_UNAVAILABLE"),
                    }
                }
            };
            if let Some(r) = reason {
                let payload = json!({
                    "protocol": protocol_owned, "source_id": source_owned,
                    "sequence": sequence, "timestamp_ms": timestamp_ms, "reason": r,
                });
                let _ = store.save_posture_event_chained(
                    "industrial_replay_guard",
                    "INDUSTRIAL_MESSAGE_REJECTED",
                    &payload.to_string(),
                    Some(r),
                    now,
                );
            }
            reason
        })
        .await
    {
        Ok(reason) => reason,
        Err(_) => Some("INDUSTRIAL_REPLAY_STORE_UNAVAILABLE"),
    }
}

/// Standard rejection response for a replay/freshness denial (200 + allowed:false,
/// matching the industrial handlers' denial shape; the rejection precedes evaluation).
pub(crate) fn industrial_replay_rejection(
    protocol: &str,
    reason: &str,
) -> axum::response::Response {
    (
        StatusCode::OK,
        Json(json!({
            "protocol": protocol,
            "allowed": false,
            "denial_reason": reason,
            "replay_rejected": true,
        })),
    )
        .into_response()
}

pub(crate) async fn evaluate_industrial_adapter(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<UnifiedIndustrialRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let req = match body {
        Ok(Json(r)) => r,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "MALFORMED_REQUEST",
                    "detail": rejection.body_text(),
                    "allowed": false,
                })),
            )
                .into_response();
        }
    };

    let protocol_name = format!("{:?}", req.protocol);

    // Replay/freshness gate (IEC 62443) — reject a stale/replayed message before
    // evaluation. Key the per-source sequence by the CANONICAL protocol slug (NOT
    // the `Debug` repr) so a source's sequence namespace is SHARED with the
    // dedicated per-protocol endpoint (`replay_slug` is the single source of
    // truth); otherwise the same physical source has two independent namespaces
    // and a message replayed across endpoints is not detected.
    let replay_key = format!("{}:{}", req.protocol.replay_slug(), req.source_id);
    if let Some(reason) = enforce_industrial_replay(
        &svc,
        &protocol_name,
        &replay_key,
        req.sequence,
        req.timestamp_ms,
    )
    .await
    {
        return industrial_replay_rejection(&protocol_name, reason);
    }

    let (posture, lockout_reason) = gate_posture(&svc);

    let audit_ref = now_ms().to_string();

    match evaluate_unified_industrial_request(req, posture) {
        Ok(mut result) => {
            // SG-012 / H-011: a DNP3 broadcast (only DNP3 sets `is_broadcast`)
            // must carry a tamper-evident record; mirror the dedicated DNP3
            // handler's fail-closed policy on this generic path too.
            let is_broadcast = result
                .adapter_details
                .get("is_broadcast")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let should_audit = !result.allowed || is_broadcast;

            if should_audit {
                let audit = json!({
                    "protocol": result.protocol,
                    "command": format!("{:?}", result.command),
                    "allowed": result.allowed,
                    "denial_reason": result.denial_reason,
                    "posture": result.posture_at_evaluation,
                    "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
                    "audit_ref": audit_ref,
                });
                let event_type = if result.allowed {
                    "INDUSTRIAL_ACTION_ALLOWED_BROADCAST"
                } else {
                    "INDUSTRIAL_ACTION_DENIED"
                };
                // P1: durable audit-chain write off the worker pool. Materialize
                // the payload string first (move-able); a task failure → `false`
                // (fail-closed), identical to the DB-error arm.
                let now = now_ms();
                let audit_str = audit.to_string();
                let audit_ok = svc.app.store.call(move |store| match store.save_posture_event_chained(
                    "industrial_adapter", event_type,
                    &audit_str, None, now,
                ) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::error!(error = %e, event_type = event_type,
                            "AUDIT-CHAIN WRITE FAILED for industrial adapter event — event missing from tamper-evident log");
                        false
                    }
                }).await.unwrap_or_else(|e| {
                    tracing::error!(error = %e, event_type = event_type,
                        "AUDIT-CHAIN WRITE TASK FAILED for industrial adapter event — treating as unwritten (fail-closed)");
                    false
                });

                // TR-012a: a broadcast whose mandatory audit could not be
                // written is BLOCKED (fail-closed); non-broadcast audit failure
                // stays non-fatal (TR-012b).
                if is_broadcast && !audit_ok {
                    result.allowed = false;
                    result.denial_reason = Some("DNP3_BROADCAST_AUDIT_UNAVAILABLE".to_string());
                    tracing::error!(
                        "DNP3 broadcast BLOCKED (unified path) — mandatory audit write unavailable (SG-012 / H-011 fail-closed)");
                }
            }

            Json(json!({
                "protocol": result.protocol,
                "command": format!("{:?}", result.command),
                "allowed": result.allowed,
                "denial_reason": result.denial_reason,
                "posture_at_evaluation": result.posture_at_evaluation,
                "adapter_details": result.adapter_details,
                "audit_ref": audit_ref,
                "triggers_recalculation": result.triggers_recalculation,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "ADAPTER_PARSE_FAILURE",
                "detail": e,
                "protocol": protocol_name,
                "allowed": false,
            })),
        )
            .into_response(),
    }
}

pub(crate) async fn evaluate_ethernet_ip_adapter(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<ReplayGuarded<EtherNetIpMessage>>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let (msg, sequence, timestamp_ms) = match body {
        Ok(Json(g)) => (g.message, g.sequence, g.timestamp_ms),
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "MALFORMED_REQUEST",
                    "detail": rejection.body_text(),
                    "allowed": false,
                })),
            )
                .into_response();
        }
    };

    // Replay/freshness gate before evaluation.
    let replay_key = format!(
        "{}:{}",
        IndustrialProtocol::EthernetIp.replay_slug(),
        msg.source_node
    );
    if let Some(reason) =
        enforce_industrial_replay(&svc, "ethernet_ip", &replay_key, sequence, timestamp_ms).await
    {
        return industrial_replay_rejection("ethernet_ip", reason);
    }

    let (posture, lockout_reason) = gate_posture(&svc);

    let posture_str = format!("{:?}", posture);
    let eval = EtherNetIpAdapter::evaluate(&msg);
    let (mut allowed, mut denial_reason) =
        kirra_industrial::protocol_adapter::command_allowed_for_posture_pub(
            &eval.command,
            &posture,
        );
    // MAGNITUDE BOUND (parity with the unified `dispatch_adapter` path and the DNP3
    // handler): a posture-admitted CIP Set_Attribute_Single must also lie within the
    // configured per-attribute envelope — else REFUSE (fail-closed; also on an
    // undecodable value, or on any unconfigured target under KIRRA_CIP_STRICT_BOUNDS).
    // Only applied on the posture-allowed path; a posture denial outranks and is
    // reported as-is. Without this the dedicated route accepted out-of-range writes
    // the unified `/industrial/evaluate` path rejects.
    if allowed {
        if let Err(reason) =
            <EtherNetIpAdapter as kirra_industrial::adapters::IndustrialAdapter>::bound_magnitude(
                &msg,
            )
        {
            allowed = false;
            denial_reason = Some(reason.to_string());
        }
    }
    let audit_ref = now_ms().to_string();

    if !allowed {
        // P1: durable audit write off the worker pool. Materialize the payload
        // string first so the closure owns it (`eval`/`denial_reason` are reused
        // in the response below).
        let now = now_ms();
        let payload = json!({
            "service_name": eval.service_name,
            "safety_relevant": eval.safety_relevant,
            "posture": posture_str,
            "denial_reason": denial_reason,
            "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
        })
        .to_string();
        let _ = svc.app.store.call(move |store| {
            if let Err(e) = store.save_posture_event_chained(
                "ethernet_ip_adapter", "INDUSTRIAL_ACTION_DENIED",
                &payload, None, now,
            ) {
                tracing::error!(error = %e, event_type = "INDUSTRIAL_ACTION_DENIED",
                    "AUDIT-CHAIN WRITE FAILED for ethernet_ip adapter event — event missing from tamper-evident log");
            }
        }).await;
    }

    Json(json!({
        "protocol": "ethernet_ip",
        "command": format!("{:?}", eval.command),
        "allowed": allowed,
        "denial_reason": denial_reason,
        "posture_at_evaluation": posture_str,
        "adapter_details": {
            "service_name": eval.service_name,
            "is_write": eval.is_write,
            "target_description": eval.target_description,
            "safety_relevant": eval.safety_relevant,
        },
        "audit_ref": audit_ref,
    }))
    .into_response()
}

pub(crate) async fn evaluate_canopen_adapter(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<ReplayGuarded<CanOpenMessage>>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let (msg, sequence, timestamp_ms) = match body {
        Ok(Json(g)) => (g.message, g.sequence, g.timestamp_ms),
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "MALFORMED_REQUEST",
                    "detail": rejection.body_text(),
                    "allowed": false,
                })),
            )
                .into_response();
        }
    };

    // Replay/freshness gate before evaluation.
    let replay_key = format!(
        "{}:{}",
        IndustrialProtocol::CanOpen.replay_slug(),
        msg.source_node
    );
    if let Some(reason) =
        enforce_industrial_replay(&svc, "canopen", &replay_key, sequence, timestamp_ms).await
    {
        return industrial_replay_rejection("canopen", reason);
    }

    let (posture, lockout_reason) = gate_posture(&svc);

    let posture_str = format!("{:?}", posture);
    let eval = CanOpenAdapter::evaluate(&msg);
    let (mut allowed, mut denial_reason) =
        kirra_industrial::protocol_adapter::command_allowed_for_posture_pub(
            &eval.command,
            &posture,
        );
    // MAGNITUDE BOUND (parity with the unified `dispatch_adapter` path and the DNP3
    // handler): a posture-admitted SDO expedited-download must also lie within the
    // configured per-target envelope — else REFUSE (fail-closed; also on an
    // undecodable/segmented/width-mismatched value, or on any unconfigured target
    // under KIRRA_CANOPEN_STRICT_BOUNDS). Non-SDO frames (NMT/EMCY) self-report no
    // scalar and pass through. Only applied on the posture-allowed path; a posture
    // denial outranks. Without this the dedicated route accepted out-of-range SDO
    // downloads the unified `/industrial/evaluate` path rejects.
    if allowed {
        if let Err(reason) =
            <CanOpenAdapter as kirra_industrial::adapters::IndustrialAdapter>::bound_magnitude(&msg)
        {
            allowed = false;
            denial_reason = Some(reason.to_string());
        }
    }
    let audit_ref = now_ms().to_string();

    // #84: resolve the CANopen bus node-id to a FLEET node so an NMT-offline
    // event marks the correct asset and the recalc is EFFECTFUL. Unmapped or
    // unregistered ids are FAIL-CLOSED — surfaced as an unattributed offline
    // (distinct audit event + warning + response flag), never a silent no-op.
    let offline_outcome = if eval.triggers_recalculation {
        use kirra_industrial::adapters::canopen::{classify_nmt_offline, global_resolve};
        let resolved = global_resolve(eval.node_id);
        let registered = resolved
            .as_deref()
            .map(|n| svc.app.fleet.nodes.contains_key(n))
            .unwrap_or(false);
        Some(classify_nmt_offline(eval.node_id, resolved, registered))
    } else {
        None
    };

    // Apply the offline effect (mark the node + drive a recalc). The fleet node
    // actually marked offline (if any) is recorded for the audit + response.
    let mut attributed_fleet_node: Option<String> = None;
    if let Some(outcome) = &offline_outcome {
        use kirra_industrial::adapters::canopen::{NmtOfflineOutcome, UnattributedReason};
        use kirra_verifier::posture_engine_v2::PostureRecalcTrigger;
        match outcome {
            NmtOfflineOutcome::Attributed { fleet_node_id } => {
                match svc
                    .app
                    .mark_node_untrusted(fleet_node_id, "CANOPEN_NMT_OFFLINE", now_ms())
                {
                    Ok(true) => {
                        attributed_fleet_node = Some(fleet_node_id.clone());
                        tracing::warn!(
                            canopen_node_id = eval.node_id,
                            fleet_node_id = %fleet_node_id,
                            "CANopen NMT node-offline → fleet node marked Untrusted; effectful recalc enqueued"
                        );
                        // C1 (#1026): this is a trust DOWNGRADE — fail closed if
                        // the recalc can't be enqueued (never leave the cache
                        // admitting against a node we just marked Untrusted).
                        enqueue_downgrade_recalc(
                            &svc,
                            PostureRecalcTrigger::NodeTrustChanged {
                                node_id: fleet_node_id.clone(),
                                reason: "CANOPEN_NMT_OFFLINE".to_string(),
                            },
                        );
                    }
                    // Mapping raced a deregistration, or the store write failed:
                    // fail-closed exactly like an unattributed offline.
                    Ok(false) | Err(()) => {
                        tracing::error!(
                            canopen_node_id = eval.node_id,
                            fleet_node_id = %fleet_node_id,
                            "CANopen NMT node-offline: mapped node missing or store write failed — \
                             treating as UNATTRIBUTED (fail-closed)"
                        );
                        enqueue_recalc(&svc, PostureRecalcTrigger::DependencyGraphChanged);
                    }
                }
            }
            NmtOfflineOutcome::Unattributed {
                canopen_node_id,
                reason,
            } => {
                let reason_str = match reason {
                    UnattributedReason::NoMapping => "NO_MAPPING",
                    UnattributedReason::NodeNotRegistered => "NODE_NOT_REGISTERED",
                };
                tracing::warn!(
                    canopen_node_id = *canopen_node_id,
                    source_node = %msg.source_node,
                    reason = reason_str,
                    "CANopen NMT node-offline UNATTRIBUTED — recorded as unattributed offline; \
                     recalc enqueued (fail-closed, never silently dropped)"
                );
                enqueue_recalc(&svc, PostureRecalcTrigger::DependencyGraphChanged);
            }
        }
    }

    if !allowed || eval.triggers_recalculation {
        // P1: durable audit write off the worker pool. `event_type` (&'static str)
        // and the payload string are materialized first so the closure owns them
        // (`eval` is reused in the response below).
        let now = now_ms();
        let event_type = if eval.triggers_recalculation {
            if attributed_fleet_node.is_some() {
                "CANOPEN_NMT_NODE_OFFLINE"
            } else {
                "CANOPEN_NMT_OFFLINE_UNATTRIBUTED"
            }
        } else {
            "INDUSTRIAL_ACTION_DENIED"
        };
        let payload = json!({
            "node_id": eval.node_id,
            "fleet_node_id": attributed_fleet_node.clone(),
            "node_offline_attributed": attributed_fleet_node.is_some(),
            "message_type": format!("{:?}", eval.message_type),
            "is_emergency": eval.is_emergency,
            "triggers_recalculation": eval.triggers_recalculation,
            "posture": posture_str,
            "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
        })
        .to_string();
        let _ = svc.app.store.call(move |store| {
            if let Err(e) = store.save_posture_event_chained(
                "canopen_adapter", event_type,
                &payload, None, now,
            ) {
                tracing::error!(error = %e, event_type = event_type,
                    "AUDIT-CHAIN WRITE FAILED for canopen adapter event — event missing from tamper-evident log");
            }
        }).await;
    }

    Json(json!({
        "protocol": "canopen",
        "command": format!("{:?}", eval.command),
        "allowed": allowed,
        "denial_reason": denial_reason,
        "posture_at_evaluation": posture_str,
        // #84: whether the NMT-offline was attributed to a fleet node (and which).
        // `false` on an offline event means the id was unmapped/unregistered and
        // handled fail-closed — surfaced here so callers never read silence as success.
        "node_offline_attributed": attributed_fleet_node.is_some(),
        "fleet_node_id": attributed_fleet_node,
        "adapter_details": {
            "message_type": format!("{:?}", eval.message_type),
            "node_id": eval.node_id,
            "is_emergency": eval.is_emergency,
            "emergency_code": eval.emergency_code,
            "triggers_recalculation": eval.triggers_recalculation,
        },
        "audit_ref": audit_ref,
    }))
    .into_response()
}

pub(crate) async fn evaluate_dnp3_adapter(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<ReplayGuarded<Dnp3Message>>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let (msg, sequence, timestamp_ms) = match body {
        Ok(Json(g)) => (g.message, g.sequence, g.timestamp_ms),
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "MALFORMED_REQUEST",
                    "detail": rejection.body_text(),
                    "allowed": false,
                })),
            )
                .into_response();
        }
    };

    // Replay/freshness gate before evaluation.
    let replay_key = format!(
        "{}:{}",
        IndustrialProtocol::Dnp3.replay_slug(),
        msg.source_node
    );
    if let Some(reason) =
        enforce_industrial_replay(&svc, "dnp3", &replay_key, sequence, timestamp_ms).await
    {
        return industrial_replay_rejection("dnp3", reason);
    }

    let (posture, lockout_reason) = gate_posture(&svc);

    let posture_str = format!("{:?}", posture);
    let eval = Dnp3Adapter::evaluate(&msg);
    let (allowed, denial_reason) =
        kirra_industrial::protocol_adapter::command_allowed_for_posture_pub(
            &eval.command,
            &posture,
        );
    let audit_ref = now_ms().to_string();

    // SG-012 / H-011 — a DNP3 broadcast control command must carry a
    // tamper-evident record. Kirra CLASSIFIES, it does not actuate: "before
    // control output" means before this handler returns its verdict, and the
    // integrator MUST NOT actuate ahead of this audited verdict (a documented
    // assumption-of-use). Audit policy:
    //   * Broadcast (TR-012 / TR-012a): MUST be audited; if the mandatory audit
    //     write fails (or the store lock is poisoned), the command is BLOCKED
    //     (fail-closed) — H-011's hazard is a broadcast control executed without
    //     a tamper-evident record.
    //   * Unicast control (TR-012b): also audited, but an audit-write failure is
    //     NON-fatal — for a single target the enforcement decision outranks the
    //     record (blocking on a transient disk error would be fail-open).
    //   * Denials: audited.
    let mut allowed = allowed;
    let mut denial_reason = denial_reason;
    let mut status = StatusCode::OK;

    // MAGNITUDE BOUND: a posture-admitted Analog Output (g41) control must also lie
    // within the configured envelope — else REFUSE (fail-closed: also when no
    // envelope is configured or the value is undecodable). Only applied on the
    // posture-allowed path; a posture denial outranks and is reported as-is. The
    // override-to-denied flows into the audit block below as an action-denied event.
    if allowed {
        if let Err(reason) = Dnp3Adapter::bound_analog_control(
            &msg,
            kirra_industrial::adapters::dnp3::global_analog_envelope().as_ref(),
        ) {
            allowed = false;
            denial_reason = Some(reason.to_string());
        }
    }

    if eval.is_broadcast || eval.is_control || !allowed {
        let event_type = if eval.is_broadcast {
            "DNP3_BROADCAST_COMMAND"
        } else if !allowed {
            "INDUSTRIAL_ACTION_DENIED"
        } else {
            "DNP3_CONTROL_COMMAND"
        };
        let audit_payload = json!({
            "function_name": eval.function_name,
            "is_broadcast": eval.is_broadcast,
            "is_control": eval.is_control,
            "critical_infrastructure_relevant": eval.critical_infrastructure_relevant,
            "dest_address": msg.dest_address,
            "posture": posture_str,
            "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
        })
        .to_string();
        // P1: the per-command audit-chain write (durable fsync) runs off the
        // worker pool. `event_type` is `&'static str` and the payload is owned, so
        // both move into the closure cleanly. A task failure → `false` (fail-closed:
        // audit unconfirmed), identical to the DB-error arm below.
        let now = now_ms();
        let audit_payload_owned = audit_payload;
        let audit_ok = svc.app.store.call(move |store| match store.save_posture_event_chained(
            "dnp3_adapter", event_type, &audit_payload_owned, None, now,
        ) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!(error = %e, event_type = event_type,
                    "AUDIT-CHAIN WRITE FAILED for dnp3 adapter event — event missing from tamper-evident log");
                false
            }
        }).await.unwrap_or_else(|e| {
            tracing::error!(error = %e, event_type = event_type,
                "AUDIT-CHAIN WRITE TASK FAILED for dnp3 adapter event — treating as unwritten (fail-closed)");
            false
        });

        // TR-012a: a BROADCAST whose mandatory audit could not be written is
        // BLOCKED (fail-closed). Unicast audit failure is non-fatal (TR-012b).
        if eval.is_broadcast && !audit_ok {
            allowed = false;
            denial_reason = Some("DNP3_BROADCAST_AUDIT_UNAVAILABLE".to_string());
            status = StatusCode::SERVICE_UNAVAILABLE;
            tracing::error!(dest_address = msg.dest_address,
                "DNP3 BROADCAST control BLOCKED — mandatory audit write unavailable (SG-012 / H-011 fail-closed)");
        }
    }

    (
        status,
        Json(json!({
            "protocol": "dnp3",
            "command": format!("{:?}", eval.command),
            "allowed": allowed,
            "denial_reason": denial_reason,
            "posture_at_evaluation": posture_str,
            "adapter_details": {
                "function_name": eval.function_name,
                "is_control": eval.is_control,
                "is_broadcast": eval.is_broadcast,
                "critical_infrastructure_relevant": eval.critical_infrastructure_relevant,
            },
            "audit_ref": audit_ref,
        })),
    )
        .into_response()
}
