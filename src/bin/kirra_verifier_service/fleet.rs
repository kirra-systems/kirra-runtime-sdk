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
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(event) => serde_json::to_string(&event)
            .ok()
            .map(|data| Ok(Event::default().data(data))),
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                "posture stream subscriber lagged; frames dropped"
            );
            None
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub(crate) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}

pub(crate) async fn ready(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    // SAFETY: SG-HA-3 — lightweight health probe off the worker pool via read replica.
    match svc.app.store.call_read(|store| store.health_check()).await {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(HealthResponse {
                status: "ready".to_string(),
            }),
        )
            .into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "db_unavailable".to_string(),
            }),
        )
            .into_response(),
    }
}

/// WS-0.5 — `GET /metrics`: Prometheus text exposition (0.0.4) of the
/// fleet-safety series (posture gauge, committed transitions, gate denials
/// by reason, HA promotions, audit/capture drop counters). Public read-only
/// and posture-exempt (pre-allowlisted in `is_posture_exempt`): the scrape
/// must survive LockedOut — that is exactly when an operator needs it. No
/// secrets: counters and posture state only.
pub(crate) async fn metrics_endpoint(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    use std::sync::atomic::Ordering;

    // Effective fail-closed posture via the single TTL authority — the gauge
    // reports what the ROUTING GATE would enforce (cold/stale/poisoned →
    // LockedOut), not a possibly-stale cached optimism. #774 F1+F5: the SILENT
    // single-read resolver — no WARN-per-scrape on an empty standby cache / from
    // an unauthenticated peer, and posture + generation come from ONE cache read
    // (no torn snapshot).
    let (effective_posture, stale_reason, posture_generation) =
        resolve_posture_snapshot_silent(&svc.posture_cache, POSTURE_CACHE_TTL_MS);

    let snap = kirra_verifier::metrics::FleetMetricsSnapshot {
        effective_posture,
        posture_cache_stale: stale_reason.is_some(),
        posture_generation,
        mode_active: svc.app.is_active(),
        // Relaxed: monotonic observability counters — a best-effort snapshot
        // needs no ordering with other memory, and the scrape path should not
        // pay SeqCst fences.
        audit_write_drops: svc.app.audit_write_drops.load(Ordering::Relaxed),
        capture_drops: svc.app.capture_drops.load(Ordering::Relaxed),
        post_incident_write_failures: svc.app.post_incident_write_failures.load(Ordering::Relaxed),
        incident_durability_failures: svc.app.incident_durability_failures.load(Ordering::Relaxed),
        command_source_write_failures: svc
            .app
            .command_source_write_failures
            .load(Ordering::Relaxed),
    };
    let mut body = svc
        .app
        .fleet_metrics
        .format_prometheus(&kirra_verifier::standby_monitor::instance_id(), &snap);

    // WS-4: append the OTA fleet-rollout series (campaign counts by state + per
    // active-campaign rollout % and adoption). Store read off the REPLICA (never
    // contends the writer); posture-EXEMPT like the rest of `/metrics`, so a rollout
    // stays observable under LockedOut. Best-effort — a query hiccup omits the
    // campaign series this scrape but never fails the core fleet-safety series.
    if let Ok(Ok((campaigns, statuses))) = svc
        .app
        .store
        .call_read(|store| {
            Ok::<_, rusqlite::Error>((store.load_campaigns()?, store.load_node_artifact_statuses()?))
        })
        .await
    {
        let summary = kirra_verifier::ota_campaign::summarize_campaigns(&campaigns, &statuses);
        body.push_str(&kirra_verifier::ota_campaign::campaign_metrics_prometheus(&summary));
    }

    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        body,
    )
}

pub(crate) async fn export_backup(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    let exported_at_ms = now_ms();
    // Three full-table loads (nodes + dependencies + all posture events) — the
    // heaviest read. `call_read` runs it off the worker pool AND against a
    // read-only replica connection, so a large dump neither pins a tokio worker
    // nor contends the writer mutex (a concurrent write proceeds unblocked).
    let result = svc
        .app
        .store
        .call_read(move |store| {
            let nodes = store.load_nodes().ok()?;
            let dependencies = store.load_dependencies().ok()?;
            let posture_events = store.load_all_posture_events().ok()?;
            Some(BackupExport {
                exported_at_ms,
                nodes,
                dependencies,
                posture_events,
            })
        })
        .await;
    match result {
        Ok(Some(export)) => Json(export).into_response(),
        Ok(None) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "failed to export backup" })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "store lock poisoned" })),
        )
            .into_response(),
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

/// Query for the node campaign-assignment lookup. `cohorts` is a comma-separated
/// list of the cohort labels the node belongs to (the node declares them from its
/// deployment ring config); empty/absent → the node is in no cohort and is never
/// assigned an artifact.
#[derive(Deserialize, Default)]
pub(crate) struct NodeAssignmentQuery {
    #[serde(default)]
    cohorts: String,
}

/// GET /fleet/campaigns/assignment/{node_id}?cohorts=a,b — WS-4 / Track 3.
///
/// The node-facing seam the on-device installer consumes: given a node id and the
/// cohorts it belongs to, resolve which SIGNED governor artifact it should run from
/// the currently ACTIVE campaigns at their staged rollout percentages. Public
/// read-only (posture-gated like the other `/fleet/*` reads): under LockedOut the
/// gate denies it — and active campaigns are auto-halted then anyway — so no new
/// artifact is ever adopted while the fleet is locked out. The returned digest is
/// public (a signed release identity), never a secret.
pub(crate) async fn get_node_campaign_assignment(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
    Query(q): Query<NodeAssignmentQuery>,
) -> impl IntoResponse {
    let cohorts: Vec<String> = q
        .cohorts
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    // Read the active campaign set off the worker pool (read replica).
    match svc
        .app
        .store
        .call_read(|store| store.load_active_campaigns())
        .await
    {
        Ok(Ok(active)) => {
            let assignment =
                kirra_verifier::ota_campaign::resolve_node_assignment(&node_id, &cohorts, &active);
            (StatusCode::OK, Json(assignment)).into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "failed to load active campaigns" })),
        )
            .into_response(),
    }
}

/// Clock-skew allowance for a signed report's `reported_at_ms` (5 min). The upsert is
/// monotonic on this timestamp, so a far-future value would wedge future updates; a
/// time-synced fleet (AOU-TIMESYNC-001) never legitimately exceeds this.
const REPORT_MAX_FUTURE_SKEW_MS: u64 = 300_000;

/// Body of a node adoption report: the node id and the digest it is now running.
/// Optionally attestation-SIGNED for unforgeable attribution.
#[derive(Deserialize)]
pub(crate) struct NodeArtifactReport {
    node_id: String,
    /// 64-char lowercase hex SHA-256 of the artifact the node currently runs.
    applied_digest: String,
    #[serde(default)]
    campaign_id: Option<String>,
    #[serde(default)]
    artifact_version: Option<String>,
    /// Optional base64 Ed25519 signature over
    /// `attestation::adoption_report_signing_payload(node_id, applied_digest,
    /// reported_at_ms)`, verified against the node's registered `ak_public_pem`.
    #[serde(default)]
    signature: Option<String>,
    /// The node's claimed report timestamp. REQUIRED when `signature` is present (the
    /// signature covers it); ignored (server stamps `now_ms`) when unsigned.
    #[serde(default)]
    reported_at_ms: Option<u64>,
}

/// POST /fleet/campaigns/report — WS-4 / Track 3. A node reports the governor
/// artifact digest it is now RUNNING (its OTA agent calls this after a commit); the
/// fleet summary joins these to show real per-campaign adoption. IDENTITY-GATED (Tier
/// 1: `SCOPE_INTEGRATION_EVALUATE` + client-identity), like the other node-facing
/// writes — a report is a mutation, so it needs a credential (unlike the open,
/// read-only assignment GET). Validated fail-closed: a non-hex digest or empty
/// node_id is a 400 and nothing is written. Pure observability — the upsert is NOT
/// audit-chained and never gates any actuator/posture decision.
pub(crate) async fn report_node_artifact(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<NodeArtifactReport>,
) -> impl IntoResponse {
    let node_id = req.node_id.trim().to_string();
    let applied_digest = req.applied_digest.trim().to_ascii_lowercase();
    if !valid_identifier(&node_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "node_id must be non-empty and free of '|' or control characters" })),
        )
            .into_response();
    }
    if !is_sha256_hex_lower(&applied_digest) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "applied_digest must be a 64-char lowercase hex sha256" })),
        )
            .into_response();
    }

    // Optional attestation signature → unforgeable attribution. Present + valid →
    // attested (using the node-signed timestamp); present + invalid / no registered
    // AK / bad base64 → fail-closed reject; absent → an unattested (identity-gated
    // only) report stamped with the server clock.
    let (attested, reported_at_ms) = if let Some(sig_b64) = req.signature.as_deref() {
        let Some(ts) = req.reported_at_ms else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "a signed report requires reported_at_ms" })),
            )
                .into_response();
        };
        // Bounded future-skew: the upsert is MONOTONIC on the (signed) timestamp, so a
        // far-future `reported_at_ms` would permanently block every later legitimate
        // report for this node. Reject beyond a small clock-skew allowance (nodes are
        // time-synced per AOU-TIMESYNC-001); the signature covers `ts`, so this can't
        // be shifted after signing.
        if ts > now_ms().saturating_add(REPORT_MAX_FUTURE_SKEW_MS) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "reported_at_ms is too far in the future (clock skew)" })),
            )
                .into_response();
        }
        let ak = svc.app.nodes.get(&node_id).and_then(|n| n.ak_public_pem.clone());
        let Some(ak) = ak else {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "no registered attestation key for node" })),
            )
                .into_response();
        };
        use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
        let Ok(sig) = b64e.decode(sig_b64.trim()) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "signature is not valid base64" })),
            )
                .into_response();
        };
        let payload = kirra_verifier::attestation::adoption_report_signing_payload(
            &node_id,
            &applied_digest,
            ts,
        );
        if !kirra_verifier::attestation::verify_ed25519_pem_signature(&ak, &payload, &sig) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "adoption report signature invalid" })),
            )
                .into_response();
        }
        (true, ts)
    } else {
        (false, now_ms())
    };

    let status = kirra_verifier::ota_campaign::NodeArtifactStatus {
        node_id,
        applied_digest,
        campaign_id: req.campaign_id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        artifact_version: req
            .artifact_version
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        reported_at_ms,
        attested,
    };

    match svc
        .app
        .store
        .call(move |store| store.upsert_node_artifact_status(&status))
        .await
    {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({ "recorded": true }))).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "failed to record adoption report" })),
        )
            .into_response(),
    }
}

/// A 64-char lowercase hex SHA-256 (the artifact identity a node reports).
fn is_sha256_hex_lower(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Read-only listing of registered AV subsystem diagnostics (confidence floor,
/// recovery streak, last telemetry). Admin-gated; no secrets returned. (#385)
pub(crate) async fn list_av_subsystems(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    // SAFETY: SG-HA-3 — read off the worker pool via read replica.
    match svc
        .app
        .store
        .call_read(|store| store.load_av_subsystems())
        .await
    {
        Ok(Ok(rows)) => {
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
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "failed to load av subsystems" })),
        )
            .into_response(),
    }
}

/// Read-only listing of registered operators. Admin-gated. Exposes only the
/// public-key FINGERPRINT (never the PEM), matching the write-side convention. (#385)
pub(crate) async fn list_operators(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    // SAFETY: SG-HA-3 — read off the worker pool via read replica.
    match svc
        .app
        .store
        .call_read(|store| store.load_operators())
        .await
    {
        Ok(Ok(rows)) => {
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
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "failed to load operators" })),
        )
            .into_response(),
    }
}

pub(crate) async fn register_dependencies(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterDependenciesRequest>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "instance is in passive standby mode" })),
        )
            .into_response();
    }
    if svc
        .app
        .persist_and_insert_deps(&req.node_id, req.depends_on)
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "failed to persist dependencies" })),
        )
            .into_response();
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
    emit_posture_event(
        &svc.app,
        "DEPENDENCY_GRAPH_MUTATED",
        Some(req.node_id.clone()),
    );
    enqueue_recalc(
        &svc,
        kirra_verifier::posture_engine_v2::PostureRecalcTrigger::DependencyGraphChanged,
    );

    (
        StatusCode::OK,
        Json(json!({ "node_id": req.node_id, "dependencies_registered": true })),
    )
        .into_response()
}

pub(crate) async fn get_node_history(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    match svc
        .app
        .store
        .with_read(|store| store.load_node_history(&node_id))
    {
        Ok(history) => Json(json!({ "node_id": node_id, "history": history })).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "failed to load history" })),
        )
            .into_response(),
    }
}

pub(crate) async fn get_node_flap_status(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    let five_minutes_ago = now_ms().saturating_sub(300_000);
    match svc
        .app
        .store
        .with_read(|store| store.count_recent_posture_events(&node_id, five_minutes_ago))
    {
        Ok(count) => {
            let status = FlapStatus {
                node_id: node_id.clone(),
                flapping: count >= 3,
                event_count_5m: count,
            };
            Json(status).into_response()
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "failed to query events" })),
        )
            .into_response(),
    }
}

pub(crate) async fn handle_sensor_fault_report(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<SensorFaultReportRequest>,
) -> impl IntoResponse {
    if req.source_node_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "source_node_id is required" })),
        )
            .into_response();
    }
    if !svc.app.nodes.contains_key(&req.source_node_id) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "node not registered" })),
        )
            .into_response();
    }

    let now = now_ms();

    // SAFETY: SG-HA-3 — read off the worker pool via read replica.
    let node_id_cf = req.source_node_id.clone();
    let confidence_floor = svc
        .app
        .store
        .call_read(move |store| {
            store
                .load_av_confidence_floor(&node_id_cf)
                .unwrap_or(None)
                .unwrap_or(0.70)
        })
        .await
        .unwrap_or(0.70);

    let is_degraded = req.hardware_fault_detected || req.confidence_score < confidence_floor;

    if is_degraded {
        let reason = if req.hardware_fault_detected {
            "hardware_fault"
        } else {
            "low_confidence"
        };

        // SAFETY: SG-HA-3 — durable streak reset off the worker pool.
        let node_id_rs = req.source_node_id.clone();
        let _ = svc
            .app
            .store
            .call(move |store| {
                let _ = store.reset_recovery_streak(&node_id_rs, now);
            })
            .await;

        let updated = match svc.app.nodes.get(&req.source_node_id) {
            Some(n) => RegisteredNode {
                node_id: n.node_id.clone(),
                status: NodeTrustState::Untrusted(reason.to_string()),
                registered_at_ms: n.registered_at_ms,
                last_trust_update_ms: now,
                ak_public_pem: n.ak_public_pem.clone(),
                expected_pcr16_digest_hex: n.expected_pcr16_digest_hex.clone(),
                site: n.site.clone(),
                firmware_version: n.firmware_version.clone(),
            },
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "node not found" })),
                )
                    .into_response()
            }
        };

        if svc.app.persist_and_insert_node(updated).is_err() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to persist node state" })),
            )
                .into_response();
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

        emit_posture_event(
            &svc.app,
            "NODE_STATUS_CHANGED",
            Some(req.source_node_id.clone()),
        );
        enqueue_recalc(
            &svc,
            kirra_verifier::posture_engine_v2::PostureRecalcTrigger::NodeTrustChanged {
                node_id: req.source_node_id.clone(),
                reason: format!("SENSOR_FAULT:{reason}"),
            },
        );

        return (
            StatusCode::OK,
            Json(json!({
                "source_node_id": req.source_node_id,
                "accepted": true,
                "fault_recorded": true,
            })),
        )
            .into_response();
    }

    let currently_untrusted = svc
        .app
        .nodes
        .get(&req.source_node_id)
        .map(|n| matches!(n.status, NodeTrustState::Untrusted(_)))
        .unwrap_or(false);

    if !currently_untrusted {
        // SAFETY: SG-HA-3 — durable telemetry timestamp update off the worker pool.
        let node_id_tt = req.source_node_id.clone();
        let _ = svc
            .app
            .store
            .call(move |store| {
                let _ = store.touch_av_telemetry_timestamp(&node_id_tt, now);
            })
            .await;
        return (
            StatusCode::OK,
            Json(json!({
                "source_node_id": req.source_node_id,
                "accepted": true,
                "fault_recorded": false,
            })),
        )
            .into_response();
    }

    // SAFETY: SG-HA-3 — recovery-streak read+write off the worker pool.
    // `evaluate_recovery_report` internally reads and writes streak state;
    // it must run on the writer connection (via `call`) to keep the
    // read-then-write atomic under one lock acquisition.
    let node_id_rr = req.source_node_id.clone();
    let decision = match svc
        .app
        .store
        .call(move |store| {
            // `&*store` dereferences to `&VerifierStore` so the generic
            // `S: RecoveryStreakStore` bound on `evaluate_recovery_report`
            // resolves correctly (S3 / #115 — trait seam, behavior unchanged:
            // the trait impl delegates verbatim).
            evaluate_recovery_report(&*store, &node_id_rr, now)
        })
        .await
    {
        Ok(d) => d,
        // Task failure: treat as NotApplicable (fail-closed: no recovery confirmed).
        Err(_) => HysteresisDecision::NotApplicable,
    };

    match &decision {
        HysteresisDecision::RecoveryConfirmed { streak } => {
            let updated = match svc.app.nodes.get(&req.source_node_id) {
                Some(n) => RegisteredNode {
                    node_id: n.node_id.clone(),
                    status: NodeTrustState::Trusted,
                    registered_at_ms: n.registered_at_ms,
                    last_trust_update_ms: now,
                    ak_public_pem: n.ak_public_pem.clone(),
                    expected_pcr16_digest_hex: n.expected_pcr16_digest_hex.clone(),
                    site: n.site.clone(),
                    firmware_version: n.firmware_version.clone(),
                },
                None => {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(json!({ "error": "node not found" })),
                    )
                        .into_response()
                }
            };

            if svc.app.persist_and_insert_node(updated).is_err() {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "failed to persist node state" })),
                )
                    .into_response();
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

            emit_posture_event(
                &svc.app,
                "NODE_STATUS_CHANGED",
                Some(req.source_node_id.clone()),
            );
            enqueue_recalc(
                &svc,
                kirra_verifier::posture_engine_v2::PostureRecalcTrigger::NodeTrustChanged {
                    node_id: req.source_node_id.clone(),
                    reason: "SENSOR_RECOVERY_CONFIRMED".to_string(),
                },
            );
        }
        HysteresisDecision::StreakBuilding { .. } | HysteresisDecision::WindowExpired { .. } => {}
        HysteresisDecision::NotApplicable => {
            // SAFETY: SG-HA-3 — durable telemetry timestamp update off the worker pool.
            let node_id_tt = req.source_node_id.clone();
            let _ = svc
                .app
                .store
                .call(move |store| {
                    let _ = store.touch_av_telemetry_timestamp(&node_id_tt, now);
                })
                .await;
        }
    }

    (
        StatusCode::OK,
        Json(json!({
            "source_node_id":      req.source_node_id,
            "accepted":            true,
            "fault_recorded":      false,
            "hysteresis_decision": format!("{:?}", decision),
        })),
    )
        .into_response()
}

pub(crate) async fn handle_register_av_asset(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterAvAssetRequest>,
) -> impl IntoResponse {
    if req.node_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "node_id is required" })),
        )
            .into_response();
    }
    if !svc.app.nodes.contains_key(&req.node_id) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "node not registered" })),
        )
            .into_response();
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

    (
        StatusCode::OK,
        Json(json!({ "node_id": req.node_id, "registered": true })),
    )
        .into_response()
}
