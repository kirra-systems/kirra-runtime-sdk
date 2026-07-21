// src/bin/kirra_verifier_service/console.rs
// console route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use console::*`) lets build_app/tests name them unqualified.

use super::*;

/// GET /console — the operator console UI. One self-contained static file
/// embedded at build time (`include_str!`): inline CSS+JS, no CDN, no build
/// step — it works air-gapped.
pub(crate) async fn console_html() -> impl IntoResponse {
    Html(include_str!("../../../static/console.html"))
}

/// GET /console/fleet — per-node posture summary from the durable store
/// (`load_nodes`). QM read. `note` is the Untrusted reason carried on the node's
/// trust state (the latest trust note); `null` for Trusted/Unknown.
pub(crate) async fn console_fleet(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    // SAFETY: SG-HA-3 — heavy read off the worker pool via read replica.
    // load_nodes + the per-node clearance lookups run under ONE acquisition
    // (Rule 5). The closure returns a Result; the error response is built outside.
    let fleet = svc
        .app
        .store
        .call_read(|store| {
            let nodes = store.load_nodes().map_err(|_| "load_nodes failed")?;
            let fleet: Vec<_> = nodes
                .iter()
                .map(|n| {
                    let (posture, note) = match &n.status {
                        NodeTrustState::Trusted => ("Trusted", None),
                        NodeTrustState::Untrusted(reason) => ("Untrusted", Some(reason.clone())),
                        NodeTrustState::Unknown => ("Unknown", None),
                    };
                    // Phase B: the latest clearance grant's delivery state (or null). The
                    // UI derives the lifecycle label (pending / delivered:Cleared /
                    // delivery-rejected:reason) from these raw columns — no invented state.
                    let clearance = store.latest_clearance_grant(&n.node_id).ok().flatten();
                    json!({
                        "node_id": n.node_id,
                        "posture": posture,
                        "note": note,
                        "last_seen_ms": n.last_trust_update_ms,
                        "clearance": clearance,
                    })
                })
                .collect();
            Ok::<_, &'static str>(fleet)
        })
        .await;
    let fleet = match fleet {
        Ok(Ok(f)) => f,
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "load_nodes failed" })),
            )
                .into_response()
        }
    };
    Json(json!({ "fleet": fleet, "total": fleet.len() })).into_response()
}

/// GET /console/audit?limit=&offset= — passthrough to `load_audit_chain_page`.
///
/// GROUNDING NOTE: `load_audit_chain_page` is **offset-paginated** (DESC by id),
/// not before-seq cursored. The console pages by offset (the "load older" control
/// increments `offset` over the DESC feed — the same backward paging the original
/// `?before=<seq>` intent describes). Returns exactly what the page rows carry
/// (sequence, event_type, payload, signature key-id, per-row signature status,
/// and the page-level `chain_intact` flag) — no invented fields. QM read.
pub(crate) async fn console_audit(
    State(svc): State<Arc<ServiceState>>,
    Query(params): Query<ConsoleAuditQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50).min(500);
    let offset = params.offset.unwrap_or(0);
    // SAFETY: SG-HA-3 — paged read off the worker pool via read replica.
    let vk = svc.audit_verifying_key;
    match svc
        .app
        .store
        .call_read(move |store| store.load_audit_chain_page(limit, offset, vk.as_ref()))
        .await
    {
        Ok(Ok(page)) => Json(page).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "audit query failed" })),
        )
            .into_response(),
    }
}

/// GET /console/escalations — derived view of OPEN SG6 escalations.
///
/// CONSERVATIVE SUPERSET + AMBIGUITY NOTE: the SG6 impact lifecycle events
/// (`ImpactDetected` / `ImpactEscalationRaised` / `ImpactCleared`, parko-kirra
/// `audit_sink`, #102/#103) DO land in this audit chain — but their payloads
/// (`ImpactDetectedPayload`) carry **no node/asset id**, so per-node attribution
/// is NOT derivable from the chain alone. This view therefore returns the
/// fleet-level OPEN superset over a single timeline: scanning most-recent-first,
/// the detect/raise events seen before the first `ImpactCleared` are "open"
/// (events since the last clear). It passes through whatever each event row
/// carries — no fabricated node_id. Per-node attribution is a Phase-B / event-
/// taxonomy enhancement. QM read.
pub(crate) async fn console_escalations(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    // SAFETY: SG-HA-3 — paged read off the worker pool via read replica.
    let vk = svc.audit_verifying_key;
    let page = match svc
        .app
        .store
        .call_read(move |store| store.load_audit_chain_page(1000, 0, vk.as_ref()))
        .await
    {
        Ok(Ok(p)) => p,
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "audit query failed" })),
            )
                .into_response()
        }
    };
    let mut open = Vec::new();
    for e in &page.entries {
        let v = serde_json::to_value(e).unwrap_or(serde_json::Value::Null);
        let et = v.get("event_type").and_then(|x| x.as_str()).unwrap_or("");
        if et == "ImpactCleared" {
            break; // most-recent clear closes the single-timeline superset
        }
        if et == "ImpactDetected" || et == "ImpactEscalationRaised" {
            open.push(v);
        }
    }
    Json(json!({
        "open_escalations": open,
        "count": open.len(),
        "note": "fleet-level superset — impact events carry no node id (see handler doc); attribution is Phase B",
    }))
    .into_response()
}

/// GET /console/runtime (#395) — public read-only runtime snapshot.
///
/// Composes live in-memory state (mode, uptime, generation, cache freshness,
/// node/asset counts, broadcast fanout, HA heartbeat age) with two store reads
/// (audit chain depth + the persisted heartbeat ms). Fail-closed: store-lock
/// poison or query error → 500 json error (mirrors `console_audit`).
pub(crate) async fn console_runtime(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    let now = now_ms();

    // last_recalc_ms comes from the atomic posture-cache snapshot (0 if cold).
    // `SharedPostureCache` is a std `RwLock`; a poisoned lock fails closed → 500.
    let last_recalc_ms = match svc.posture_cache.read() {
        Ok(guard) => guard.as_ref().map(|c| c.generated_at_ms).unwrap_or(0),
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "posture cache lock poisoned" })),
            )
                .into_response()
        }
    };

    // Two store reads under one lock acquisition: audit depth + HA heartbeat.
    // The closure returns a Result; per-read error responses are built outside.
    // SAFETY: SG-HA-3 — two-read probe off the worker pool via read replica.
    let probe = svc
        .app
        .store
        .call_read(move |store| {
            let audit_entries = store.audit_chain_len().map_err(|_| "audit query failed")?;
            // Heartbeat absent → null (no primary has written yet).
            let hb = store
                .load_engine_state(HEARTBEAT_KEY)
                .map_err(|_| "engine state query failed")?
                .and_then(|v| v.parse::<u64>().ok())
                .map(|stored| now.saturating_sub(stored));
            Ok::<_, &'static str>((audit_entries, hb))
        })
        .await;
    let (audit_entries, ha_heartbeat_age_ms) = match probe {
        Ok(Ok(pair)) => pair,
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "store probe failed" })),
            )
                .into_response()
        }
    };

    let mode = if svc.app.is_active() {
        "Active"
    } else {
        "PassiveStandby"
    };

    Json(json!({
        "mode": mode,
        "uptime_ms": now.saturating_sub(svc.started_at_ms),
        "posture_generation": kirra_verifier::posture_engine::POSTURE_GENERATION
            .load(std::sync::atomic::Ordering::SeqCst),
        "last_recalc_ms": last_recalc_ms,
        "posture_cache_ttl_ms": POSTURE_CACHE_TTL_MS,
        "total_nodes": svc.app.fleet.nodes.len(),
        "fabric_assets": svc.fabric_router.fabric_state().total_assets,
        "fabric_denial_rate": svc.fabric_telemetry.summary().fabric_denial_rate,
        "audit_entries": audit_entries,
        "broadcast_subscribers": svc.app.posture_tx.receiver_count(),
        "ha_heartbeat_age_ms": ha_heartbeat_age_ms,
    }))
    .into_response()
}

/// GET /console/analytics?window_ms= (#396) — public read-only time-series view.
///
/// Buckets EXISTING `posture_events` rows over `[since_ms, now]` into 24 buckets
/// (no new data class). Fail-closed: store-lock poison / query error → 500.
///
/// DATA-AVAILABILITY NOTES (honest about what is and isn't stored):
///   - `posture_transitions`: real — bucketed from `posture_events.posture_json`
///     (the resulting `FleetPosture`).
///   - `denial_rate_series`: the store keeps NO per-bucket denial history. We
///     therefore emit a SINGLE current-value point (the live fabric denial rate)
///     rather than fabricate a fake series — the array shape is preserved.
///   - `interventions_by_asset`: fabric telemetry tracks a per-asset `denial_rate`
///     and `commands_per_minute`, NOT separate clamp/deny COUNTERS. `denies` is
///     derived (rate × cpm, rounded); `clamps` is 0 — clamp counts are not stored.
///   - `flapping_top`: real — per-node posture-event counts since `since_ms`.
pub(crate) async fn console_analytics(
    State(svc): State<Arc<ServiceState>>,
    Query(params): Query<ConsoleAnalyticsQuery>,
) -> impl IntoResponse {
    const BUCKETS: u64 = 24;
    const FLAPPING_TOP_N: usize = 10;

    let now = now_ms();
    let window_ms = params.window_ms.unwrap_or(86_400_000).max(1);
    let since_ms = now.saturating_sub(window_ms);
    let bucket_span = (window_ms / BUCKETS).max(1);

    // Both reads share ONE acquisition (Rule 5); the closure returns a Result and
    // the error response is produced outside (Rule 4).
    // SAFETY: SG-HA-3 — analytics reads off the worker pool via read replica.
    let loaded = svc
        .app
        .store
        .call_read(move |store| {
            let events = store
                .load_posture_events_since(since_ms)
                .map_err(|_| "posture event query failed")?;
            let by_node = store
                .count_posture_events_by_node_since(since_ms)
                .map_err(|_| "posture event query failed")?;
            Ok::<_, &'static str>((events, by_node))
        })
        .await;
    let (events, by_node) = match loaded {
        Ok(Ok(pair)) => pair,
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "posture event query failed" })),
            )
                .into_response()
        }
    };

    // Bucket posture transitions by resulting posture (parsed from posture_json).
    let mut to_degraded = vec![0u64; BUCKETS as usize];
    let mut to_lockedout = vec![0u64; BUCKETS as usize];
    let mut to_nominal = vec![0u64; BUCKETS as usize];
    for (created_at_ms, posture_json) in &events {
        let idx = (created_at_ms.saturating_sub(since_ms) / bucket_span).min(BUCKETS - 1) as usize;
        // posture_json serializes FleetPosture as "Nominal"/"Degraded"/"LockedOut"
        // (or {"Untrusted": "..."} style for node states). Match the variant name.
        let v: serde_json::Value =
            serde_json::from_str(posture_json).unwrap_or(serde_json::Value::Null);
        let label = v.as_str().map(|s| s.to_string()).unwrap_or_else(|| {
            // Object-tagged form: take the first key.
            v.as_object()
                .and_then(|o| o.keys().next().cloned())
                .unwrap_or_default()
        });
        match label.as_str() {
            "Degraded" => to_degraded[idx] += 1,
            "LockedOut" => to_lockedout[idx] += 1,
            "Nominal" => to_nominal[idx] += 1,
            _ => {}
        }
    }
    let posture_transitions: Vec<serde_json::Value> = (0..BUCKETS as usize)
        .map(|i| {
            json!({
                "bucket_start_ms": since_ms + (i as u64) * bucket_span,
                "to_degraded": to_degraded[i],
                "to_lockedout": to_lockedout[i],
                "to_nominal": to_nominal[i],
            })
        })
        .collect();

    // denial_rate_series: no stored per-bucket history → single current point.
    let denial_rate_series = json!([{
        "bucket_start_ms": now,
        "denial_rate": svc.fabric_telemetry.summary().fabric_denial_rate,
    }]);

    // interventions_by_asset: derive denies from rate × cpm; clamps not stored.
    let interventions_by_asset: Vec<serde_json::Value> = svc
        .fabric_telemetry
        .all_snapshots()
        .into_iter()
        .map(|s| {
            let denies = (s.denial_rate * s.commands_per_minute).round() as u64;
            json!({
                "asset_id": s.asset_id,
                "clamps": 0, // clamp counts are not separately tracked (see doc)
                "denies": denies,
            })
        })
        .collect();

    let flapping_top: Vec<serde_json::Value> = by_node
        .into_iter()
        .take(FLAPPING_TOP_N)
        .map(|(node_id, transitions)| {
            json!({
                "node_id": node_id,
                "transitions": transitions,
            })
        })
        .collect();

    Json(json!({
        "window_ms": window_ms,
        "posture_transitions": posture_transitions,
        "denial_rate_series": denial_rate_series,
        "interventions_by_asset": interventions_by_asset,
        "flapping_top": flapping_top,
    }))
    .into_response()
}

/// GET /console/sites (#397) — public read-only site rollup over in-memory nodes.
///
/// MAPPING CHOICE (documented): the rollup is by node TRUST STATUS, not DAG
/// fleet posture. `NodeTrustState::Trusted` → nominal; `Unknown` → degraded
/// bucket; `Untrusted(_)` → lockedout bucket. Nodes with a NULL `site` roll into
/// `unassigned`. Pure in-memory read (no store lock); cannot fail-closed.
pub(crate) async fn console_sites(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    use std::collections::BTreeMap;
    // (total, nominal, degraded, lockedout)
    let mut sites: BTreeMap<String, (u64, u64, u64, u64)> = BTreeMap::new();
    let mut unassigned: u64 = 0;

    for entry in svc.app.fleet.nodes.iter() {
        let node = entry.value();
        let bucket = match &node.status {
            NodeTrustState::Trusted => 0,
            NodeTrustState::Unknown => 1,
            NodeTrustState::Untrusted(_) => 2,
        };
        match &node.site {
            Some(site) => {
                let e = sites.entry(site.clone()).or_insert((0, 0, 0, 0));
                e.0 += 1;
                match bucket {
                    0 => e.1 += 1,
                    1 => e.2 += 1,
                    _ => e.3 += 1,
                }
            }
            None => unassigned += 1,
        }
    }

    let sites: Vec<serde_json::Value> = sites
        .into_iter()
        .map(|(site, (total, nominal, degraded, lockedout))| {
            json!({
                "site": site,
                "total": total,
                "nominal": nominal,
                "degraded": degraded,
                "lockedout": lockedout,
            })
        })
        .collect();

    Json(json!({ "sites": sites, "unassigned": unassigned })).into_response()
}

/// GET /console/versions (#398) — public read-only firmware-version rollup over
/// in-memory nodes. Nodes with a NULL `firmware_version` count toward `unknown`.
/// `pct` = count/total*100 (guarded against divide-by-zero). Pure in-memory read.
pub(crate) async fn console_versions(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    use std::collections::BTreeMap;
    let mut versions: BTreeMap<String, u64> = BTreeMap::new();
    let mut unknown: u64 = 0;
    let mut total: u64 = 0;

    for entry in svc.app.fleet.nodes.iter() {
        total += 1;
        match &entry.value().firmware_version {
            Some(v) => *versions.entry(v.clone()).or_insert(0) += 1,
            None => unknown += 1,
        }
    }

    let versions: Vec<serde_json::Value> = versions
        .into_iter()
        .map(|(version, count)| {
            let pct = if total > 0 {
                (count as f64) / (total as f64) * 100.0
            } else {
                0.0
            };
            json!({ "version": version, "count": count, "pct": pct })
        })
        .collect();

    Json(json!({ "versions": versions, "total": total, "unknown": unknown })).into_response()
}

/// GET /console/campaigns (WS-4 / Track 3) — public read-only OTA rollout view for
/// the operator console. Same `summarize_campaigns` projection the admin
/// `/system/campaigns/summary` returns (counts by state, active-campaign stage
/// progress, halted-with-reason, and the `applied_nodes` adoption numerator joined
/// from the node reports), but posture-exempt and unauthenticated like the rest of
/// the `/console/` plane. No secrets: artifact digests are public release identities.
/// Reads off the replica so a heavy fleet's console never contends the writer.
pub(crate) async fn console_campaigns(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    match svc
        .app
        .store
        .call_read(|store| {
            let campaigns = store.load_campaigns()?;
            let statuses = store.load_node_artifact_statuses()?;
            Ok::<_, rusqlite::Error>((campaigns, statuses))
        })
        .await
    {
        Ok(Ok((campaigns, statuses))) => {
            let summary = kirra_verifier::ota_campaign::summarize_campaigns(&campaigns, &statuses);
            Json(summary).into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "load campaigns failed" })),
        )
            .into_response(),
    }
}
