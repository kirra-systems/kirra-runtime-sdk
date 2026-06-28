// src/bin/kirra_verifier_service/fleet.rs
// fleet route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use fleet::*`) lets build_app/tests name them unqualified.

use super::*;

pub(crate) async fn system_posture_stream(
    State(svc): State<Arc<ServiceState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = svc.app.posture_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| {
        match msg {
            Ok(event) => serde_json::to_string(&event).ok().map(|data| {
                Ok(Event::default().data(data))
            }),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "posture stream subscriber lagged; frames dropped");
                None
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub(crate) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok".to_string() })
}

pub(crate) async fn ready(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    match svc.app.store.with(|store| store.health_check()) {
        Ok(()) => (StatusCode::OK, Json(HealthResponse { status: "ready".to_string() }))
            .into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE,
                   Json(HealthResponse { status: "db_unavailable".to_string() }))
            .into_response(),
    }
}

pub(crate) async fn export_backup(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    let exported_at_ms = now_ms();
    // Three full-table loads (nodes + dependencies + all posture events) — the
    // heaviest read. `call_read` runs it off the worker pool AND against a
    // read-only replica connection, so a large dump neither pins a tokio worker
    // nor contends the writer mutex (a concurrent write proceeds unblocked).
    let result = svc.app.store.call_read(move |store| {
        let nodes = store.load_nodes().ok()?;
        let dependencies = store.load_dependencies().ok()?;
        let posture_events = store.load_all_posture_events().ok()?;
        Some(BackupExport { exported_at_ms, nodes, dependencies, posture_events })
    })
    .await;
    match result {
        Ok(Some(export)) => Json(export).into_response(),
        Ok(None) => (StatusCode::INTERNAL_SERVER_ERROR,
                     Json(json!({ "error": "failed to export backup" }))).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

pub(crate) async fn get_fleet_posture(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    // M2: one shared-memo whole-fleet pass (O(N+E)) instead of O(N·(N+E))
    // per-node recomputation, and no `nodes.iter()` guard held across the
    // re-entrant DAG walk (the B1 hazard). Result is identical.
    let postures = svc.app.calculate_fleet_posture();
    Json(json!({ "fleet": postures }))
}

pub(crate) async fn get_node_posture(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    let posture = svc.app.calculate_posture(&node_id);
    Json(posture)
}

/// Read-only listing of registered AV subsystem diagnostics (confidence floor,
/// recovery streak, last telemetry). Admin-gated; no secrets returned. (#385)
pub(crate) async fn list_av_subsystems(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    match svc.app.store.with(|store| store.load_av_subsystems()) {
        Ok(rows) => {
            let subsystems: Vec<AvSubsystemView> = rows
                .into_iter()
                .map(|r| AvSubsystemView {
                    node_id: r.node_id,
                    subsystem_type: r.subsystem_type,
                    hardware_id: r.hardware_id,
                    confidence_floor: r.confidence_floor,
                    last_telemetry_ms: r.last_telemetry_ms,
                    recovery_streak_count: r.recovery_streak_count,
                    recovery_streak_start_ms: r.recovery_streak_start_ms,
                })
                .collect();
            let total = subsystems.len();
            Json(json!({ "subsystems": subsystems, "total": total })).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "failed to load av subsystems" }))).into_response(),
    }
}

/// Read-only listing of registered operators. Admin-gated. Exposes only the
/// public-key FINGERPRINT (never the PEM), matching the write-side convention. (#385)
pub(crate) async fn list_operators(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    match svc.app.store.with(|store| store.load_operators()) {
        Ok(rows) => {
            let operators: Vec<OperatorView> = rows
                .into_iter()
                .map(|r| {
                    let active = r.is_active();
                    OperatorView {
                        operator_key_fingerprint:
                            kirra_verifier::attestation::operator_key_fingerprint(&r.pubkey_pem)
                                .unwrap_or_else(|| "unparseable".to_string()),
                        operator_id: r.operator_id,
                        registered_at_ms: r.registered_at_ms,
                        revoked_at_ms: r.revoked_at_ms,
                        active,
                    }
                })
                .collect();
            let total = operators.len();
            Json(json!({ "operators": operators, "total": total })).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "failed to load operators" }))).into_response(),
    }
}

pub(crate) async fn register_dependencies(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterDependenciesRequest>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    if svc.app.persist_and_insert_deps(&req.node_id, req.depends_on).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to persist dependencies" }))).into_response();
    }

    let posture = svc.app.calculate_posture(&req.node_id);
    let now = now_ms();
    if let Ok(posture_json) = serde_json::to_string(&posture) {
        // P1: durable audit write off the worker pool. Own the node id (reused in
        // the response); the posture json is owned and moves in.
        let node_id_c = req.node_id.clone();
        let _ = svc.app.store.call(move |store| {
            if let Err(e) = store.save_posture_event_chained(
                &node_id_c, "DEPENDENCY_UPDATED", &posture_json, None, now,
            ) {
                tracing::error!(error=%e, node_id=%node_id_c,
                    "AUDIT-CHAIN WRITE FAILED for DEPENDENCY_UPDATED — event missing from tamper-evident log");
            }
        }).await;
    }
    emit_posture_event(&svc.app, "DEPENDENCY_GRAPH_MUTATED", Some(req.node_id.clone()));
    enqueue_recalc(&svc, kirra_verifier::posture_engine_v2::PostureRecalcTrigger::DependencyGraphChanged);

    (StatusCode::OK, Json(json!({ "node_id": req.node_id, "dependencies_registered": true }))).into_response()
}

pub(crate) async fn get_node_history(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    match svc.app.store.with_read(|store| store.load_node_history(&node_id)) {
        Ok(history) => Json(json!({ "node_id": node_id, "history": history })).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "failed to load history" }))).into_response(),
    }
}

pub(crate) async fn get_node_flap_status(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    let five_minutes_ago = now_ms().saturating_sub(300_000);
    match svc.app.store.with_read(|store| store.count_recent_posture_events(&node_id, five_minutes_ago)) {
        Ok(count) => {
            let status = FlapStatus {
                node_id: node_id.clone(),
                flapping: count >= 3,
                event_count_5m: count,
            };
            Json(status).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "failed to query events" }))).into_response(),
    }
}

pub(crate) async fn handle_sensor_fault_report(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<SensorFaultReportRequest>,
) -> impl IntoResponse {
    if req.source_node_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({ "error": "source_node_id is required" }))).into_response();
    }
    if !svc.app.nodes.contains_key(&req.source_node_id) {
        return (StatusCode::NOT_FOUND,
                Json(json!({ "error": "node not registered" }))).into_response();
    }

    let now = now_ms();

    let confidence_floor = svc.app.store.with(|store| {
        store.load_av_confidence_floor(&req.source_node_id)
            .unwrap_or(None)
            .unwrap_or(0.70)
    });

    let is_degraded = req.hardware_fault_detected || req.confidence_score < confidence_floor;

    if is_degraded {
        let reason = if req.hardware_fault_detected { "hardware_fault" } else { "low_confidence" };

        svc.app.store.with(|store| {
            let _ = store.reset_recovery_streak(&req.source_node_id, now);
        });

        let updated = match svc.app.nodes.get(&req.source_node_id) {
            Some(n) => RegisteredNode {
                node_id:              n.node_id.clone(),
                status:               NodeTrustState::Untrusted(reason.to_string()),
                registered_at_ms:     n.registered_at_ms,
                last_trust_update_ms: now,
                ak_public_pem:        n.ak_public_pem.clone(),
                expected_pcr16_digest_hex: n.expected_pcr16_digest_hex.clone(),
                site:                 n.site.clone(),
                firmware_version:     n.firmware_version.clone(),
            },
            None => return (StatusCode::NOT_FOUND,
                            Json(json!({ "error": "node not found" }))).into_response(),
        };

        if svc.app.persist_and_insert_node(updated).is_err() {
            return (StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "failed to persist node state" }))).into_response();
        }

        let event = json!({
            "source_node_id":          req.source_node_id,
            "confidence_score":        req.confidence_score,
            "hardware_fault_detected": req.hardware_fault_detected,
            "reason":                  reason,
        });
        // P1: durable audit write off the worker pool (own the node id + payload).
        let event_str = event.to_string();
        let node_id_c = req.source_node_id.clone();
        let _ = svc.app.store.call(move |store| {
            if let Err(e) = store.save_posture_event_chained(
                &node_id_c, "SENSOR_HEALTH_REPORT_FAULT",
                &event_str, None, now,
            ) {
                tracing::error!(error=%e, node_id=%node_id_c,
                    "AUDIT-CHAIN WRITE FAILED for SENSOR_HEALTH_REPORT_FAULT — event missing from tamper-evident log");
            }
        }).await;

        emit_posture_event(&svc.app, "NODE_STATUS_CHANGED", Some(req.source_node_id.clone()));
        enqueue_recalc(&svc, kirra_verifier::posture_engine_v2::PostureRecalcTrigger::NodeTrustChanged {
            node_id: req.source_node_id.clone(),
            reason:  format!("SENSOR_FAULT:{reason}"),
        });

        return (StatusCode::OK, Json(json!({
            "source_node_id": req.source_node_id,
            "accepted": true,
            "fault_recorded": true,
        }))).into_response();
    }

    let currently_untrusted = svc.app.nodes.get(&req.source_node_id)
        .map(|n| matches!(n.status, NodeTrustState::Untrusted(_)))
        .unwrap_or(false);

    if !currently_untrusted {
        svc.app.store.with(|store| {
            let _ = store.touch_av_telemetry_timestamp(&req.source_node_id, now);
        });
        return (StatusCode::OK, Json(json!({
            "source_node_id": req.source_node_id,
            "accepted": true,
            "fault_recorded": false,
        }))).into_response();
    }

    let decision = svc.app.store.with(|store| {
        // `&*store` dereferences to `&VerifierStore` so the generic
        // `S: RecoveryStreakStore` bound on `evaluate_recovery_report`
        // resolves correctly (S3 / #115 — trait seam, behavior unchanged:
        // the trait impl delegates verbatim).
        evaluate_recovery_report(&*store, &req.source_node_id, now)
    });

    match &decision {
        HysteresisDecision::RecoveryConfirmed { streak } => {
            let updated = match svc.app.nodes.get(&req.source_node_id) {
                Some(n) => RegisteredNode {
                    node_id:              n.node_id.clone(),
                    status:               NodeTrustState::Trusted,
                    registered_at_ms:     n.registered_at_ms,
                    last_trust_update_ms: now,
                    ak_public_pem:        n.ak_public_pem.clone(),
                    expected_pcr16_digest_hex: n.expected_pcr16_digest_hex.clone(),
                    site:                 n.site.clone(),
                    firmware_version:     n.firmware_version.clone(),
                },
                None => return (StatusCode::NOT_FOUND,
                                Json(json!({ "error": "node not found" }))).into_response(),
            };

            if svc.app.persist_and_insert_node(updated).is_err() {
                return (StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": "failed to persist node state" }))).into_response();
            }

            // P1: the recovery-streak reset + its audit write run as ONE off-worker
            // closure (same single-acquisition grouping as before, just off the pool).
            let node_id_c = req.source_node_id.clone();
            let streak_v = *streak;
            let _ = svc.app.store.call(move |store| {
                let _ = store.reset_recovery_streak(&node_id_c, now);
                let event = json!({
                    "source_node_id": node_id_c.as_str(),
                    "streak":         streak_v,
                });
                if let Err(e) = store.save_posture_event_chained(
                    &node_id_c, "SENSOR_RECOVERY_CONFIRMED",
                    &event.to_string(), None, now,
                ) {
                    tracing::error!(error=%e, node_id=%node_id_c,
                        "AUDIT-CHAIN WRITE FAILED for SENSOR_RECOVERY_CONFIRMED — event missing from tamper-evident log");
                }
            }).await;

            emit_posture_event(&svc.app, "NODE_STATUS_CHANGED", Some(req.source_node_id.clone()));
            enqueue_recalc(&svc, kirra_verifier::posture_engine_v2::PostureRecalcTrigger::NodeTrustChanged {
                node_id: req.source_node_id.clone(),
                reason:  "SENSOR_RECOVERY_CONFIRMED".to_string(),
            });
        }
        HysteresisDecision::StreakBuilding { .. } | HysteresisDecision::WindowExpired { .. } => {}
        HysteresisDecision::NotApplicable => {
            svc.app.store.with(|store| {
                let _ = store.touch_av_telemetry_timestamp(&req.source_node_id, now);
            });
        }
    }

    (StatusCode::OK, Json(json!({
        "source_node_id":      req.source_node_id,
        "accepted":            true,
        "fault_recorded":      false,
        "hysteresis_decision": format!("{:?}", decision),
    }))).into_response()
}

pub(crate) async fn handle_register_av_asset(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterAvAssetRequest>,
) -> impl IntoResponse {
    if req.node_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({ "error": "node_id is required" }))).into_response();
    }
    if !svc.app.nodes.contains_key(&req.node_id) {
        return (StatusCode::NOT_FOUND,
                Json(json!({ "error": "node not registered" }))).into_response();
    }

    let now = now_ms();
    let floor = req.confidence_floor.unwrap_or(0.70);

    // P1: registration + its audit write run as ONE off-worker closure (own the
    // request fields; node id is reused in the response).
    let node_id_c = req.node_id.clone();
    let subsystem_type_c = req.subsystem_type.clone();
    let hardware_id_c = req.hardware_id.clone();
    let _ = svc.app.store.call(move |store| {
        if let Err(e) = store.register_av_subsystem_meta(
            &node_id_c, &subsystem_type_c, &hardware_id_c, floor, now,
        ) {
            tracing::warn!(
                error   = %e,
                node_id = %node_id_c,
                "Failed to register av_subsystem_meta"
            );
        }
        let meta = json!({
            "subsystem_type":   subsystem_type_c.as_str(),
            "hardware_id":      hardware_id_c.as_str(),
            "confidence_floor": floor,
        });
        if let Err(e) = store.save_posture_event_chained(
            &node_id_c, "AV_ASSET_REGISTERED", &meta.to_string(), None, now,
        ) {
            tracing::error!(error=%e, node_id=%node_id_c,
                "AUDIT-CHAIN WRITE FAILED for AV_ASSET_REGISTERED — event missing from tamper-evident log");
        }
    }).await;

    // H-3: the av_subsystem_meta row is now committed (the `call` closure ran).
    // Signal the telemetry watchdog to refresh its watched-node list on its NEXT
    // sweep (~100 ms) rather than at the next 30 s periodic refresh — otherwise a
    // node registered just after a refresh is unmonitored for up to ~28 s, a
    // fail-OPEN window in which a silent/faulty fresh sensor would go undetected
    // (breaking the SG-003 detection-latency bound).
    svc.app
        .av_registry_dirty
        .store(true, std::sync::atomic::Ordering::Release);

    (StatusCode::OK, Json(json!({ "node_id": req.node_id, "registered": true }))).into_response()
}
