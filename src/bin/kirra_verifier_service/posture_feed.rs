// posture_feed — the verifier→fabric local-asset posture feed, recalc
// triggers, freshness wiring, and posture-event emission.
// Extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).

use super::*;

// --- Real-time posture stream -----------------------------------------------

/// Sends an event-driven posture recalc trigger to the worker if the
/// `posture_engine_tx` is initialized (Active path). On PassiveStandby the
/// OnceLock is unset and this is a no-op — correct, since a standby does
/// not maintain a posture cache. A `try_send` failure (channel full or
/// worker gone) is logged; the periodic-refresh loop will fail-close the
/// cache and gate on its own if the worker has truly died.
pub(crate) fn enqueue_recalc(
    svc: &ServiceState,
    trigger: kirra_verifier::posture_engine_v2::PostureRecalcTrigger,
) {
    if let Some(tx) = svc.posture_engine_tx.get() {
        if let Err(e) = tx.try_send(trigger) {
            tracing::warn!(error = %e,
                "posture recalc trigger: try_send failed (channel full or worker gone)");
        }
    }
}

/// Fail-closed posture read for action/actuator gating sites.
///
/// Delegates to `resolve_posture_with_reason` so the cache-staleness check
/// (age >= POSTURE_CACHE_TTL_MS), empty-cache check, and poisoned-lock check
/// all collapse into the same `(FleetPosture::LockedOut, Some(LockoutReason))`
/// answer — never serving a stale entry as if current. The returned
/// `LockoutReason` is threaded into the denial-audit payload so operators
/// can distinguish a DAG-derived LockedOut from a posture-cache-derived one.
pub(crate) fn gate_posture(svc: &ServiceState) -> (FleetPosture, Option<LockoutReason>) {
    resolve_posture_with_reason(&svc.posture_cache, POSTURE_CACHE_TTL_MS)
}

/// Verifier→fabric posture feed (#88, single-local-asset model).
///
/// Mirrors THIS controller's aggregate `FleetPosture` into the fabric
/// posture of the one locally governed asset named by `KIRRA_FABRIC_ASSET_ID`,
/// so fabric command enforcement for that asset reflects real verified trust
/// rather than the interim `Degraded` registration seed.
///
/// Seam: `recalculate_and_broadcast` lives in the lib and cannot see the
/// `FabricRouter` (it is on `ServiceState`, here in the binary). The posture
/// broadcast (`app.posture_tx`) fires on every fleet-posture transition,
/// including those produced by the lib-side posture-engine worker, so a
/// broadcast subscriber catches all transitions from one place.
///
/// Inert (logs once, no task spawned) when `KIRRA_FABRIC_ASSET_ID` is
/// unset/empty: the asset then keeps its registration seed. This is the
/// single-asset model — other registered assets are intentionally NOT fed
/// here, which is why the registration seed stays `Degraded` rather than
/// `LockedOut` (an unfed asset must not be bricked).
pub(crate) fn spawn_local_asset_posture_feed(svc: Arc<ServiceState>) {
    let asset_id = match std::env::var("KIRRA_FABRIC_ASSET_ID") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            tracing::info!(
                "verifier→fabric posture feed: KIRRA_FABRIC_ASSET_ID unset — \
                 feed inert (local fabric asset keeps its registration seed)"
            );
            return;
        }
    };

    tokio::spawn(async move {
        // Subscribe BEFORE the initial sync so a transition occurring in the
        // window between the initial cache read and entering recv() is
        // buffered by the broadcast channel rather than lost.
        let mut rx = svc.app.posture_tx.subscribe();
        tracing::info!(
            asset_id = %asset_id,
            "verifier→fabric posture feed: started (single-local-asset model)"
        );

        // Initial sync: the synchronous startup recalc already populated the
        // cache before this task subscribed, so reflect it once now.
        sync_local_asset_posture(&svc, &asset_id);

        loop {
            match rx.recv().await {
                Ok(_event) => sync_local_asset_posture(&svc, &asset_id),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // A lag only means we may have missed a transition; the
                    // cache is authoritative, so re-sync from it.
                    tracing::warn!(
                        skipped = n,
                        "verifier→fabric posture feed lagged; re-syncing from cache"
                    );
                    sync_local_asset_posture(&svc, &asset_id);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::warn!(
                        "verifier→fabric posture feed: broadcast channel closed; feed stopping"
                    );
                    break;
                }
            }
        }
    });
}

/// Wire the Active-mode posture-freshness background tasks onto `svc`: the
/// serialized posture-engine worker, the telemetry watchdog (SG9 sensor-
/// liveness), the periodic recompute-and-restamp loop, and the verifier→fabric
/// local-asset posture feed (#88).
///
/// Shared by TWO entry points (review H2): the Active startup path AND the
/// standby→Active promotion path. The bug this closes: a node promoted from
/// standby at runtime used to (re)start only the heartbeat writer, never these
/// four tasks — so `POSTURE_CACHE_TTL_MS` after promotion the cache went stale
/// and every gated route fail-closed (503) until process restart, negating the
/// HA availability guarantee. Calling this from the promotion path keeps the
/// freshly-promoted primary serving.
///
/// Does NOT perform the initial `recalculate_and_broadcast` — that is the
/// caller's job (startup runs it synchronously before `axum::serve`;
/// `perform_promotion` runs it as part of the promotion sequence, so the cache
/// is already populated before this wiring starts the ongoing-freshness tasks).
///
/// Must be called inside a tokio runtime context (it spawns tasks). Fail-closed:
/// a double-set of `posture_engine_tx` is an invariant breach — the cell must be
/// empty on both the pre-serve Active path and a never-Active promoted standby —
/// and aborts the process (a half-wired node is safer dead: another standby /
/// systemd restart re-promotes cleanly).
pub(crate) fn wire_active_posture_freshness(svc: &Arc<ServiceState>) {
    let posture_tx = kirra_verifier::posture_engine_v2::start_posture_engine_worker(
        Arc::clone(&svc.app),
        Arc::clone(&svc.posture_cache),
    );
    if svc.posture_engine_tx.set(posture_tx.clone()).is_err() {
        tracing::error!(
            "posture freshness wiring failed: posture_engine_tx already initialized (fail-closed)"
        );
        std::process::exit(1);
    }
    tracing::info!("posture: serialized worker started");

    // SAFETY: SG9 | REQ: sensor-liveness-watchdog | TEST: test_watchdog_dead_mans_switch_fires_after_telemetry_timeout
    // A node going silent past AV_TELEMETRY_TIMEOUT_MS produces a WatchdogTimeout
    // trigger, which the posture engine worker consumes and recomputes the
    // posture (typically collapsing the affected node to LockedOut, which fails
    // the actuator gate closed).
    kirra_verifier::telemetry_watchdog::spawn_telemetry_watchdog(
        Arc::clone(&svc.app),
        posture_tx.clone(),
        Arc::clone(&svc.posture_cache),
    );
    tracing::info!(
        timeout_ms = kirra_verifier::telemetry_watchdog::AV_TELEMETRY_TIMEOUT_MS,
        "telemetry watchdog spawned (SG9 sensor-liveness)"
    );

    // Periodic recompute-and-restamp loop at POSTURE_REFRESH_INTERVAL_MS (= TTL/2)
    // — load-bearing: without it the cache goes stale one TTL after the last
    // event and the gate fails closed fleet-wide. It is also the engine-liveness
    // signal (if the loop stops, the cache stales and the gate fail-closes).
    let refresh_tx = posture_tx;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(
            kirra_verifier::posture_cache::POSTURE_REFRESH_INTERVAL_MS,
        ));
        // Coalesce missed refresh windows instead of bursting catch-up recalcs
        // after runtime starvation (the trigger only re-stamps the cache; bursts
        // add no freshness and the posture worker already coalesces). Delay
        // re-paces from the actual wake time.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // First tick fires immediately; skip it (the caller's initial recalc
        // already covered the populated-cache precondition).
        tick.tick().await;
        loop {
            tick.tick().await;
            if refresh_tx
                .try_send(kirra_verifier::posture_engine_v2::PostureRecalcTrigger::PeriodicRefresh)
                .is_err()
            {
                tracing::error!(
                    "posture periodic refresh: worker channel unavailable — \
                     cache will go stale (gate will fail-close fleet-wide)"
                );
            }
        }
    });
    tracing::info!(
        interval_ms = kirra_verifier::posture_cache::POSTURE_REFRESH_INTERVAL_MS,
        ttl_ms = kirra_verifier::posture_cache::POSTURE_CACHE_TTL_MS,
        "posture: periodic refresh loop started"
    );

    // #88: verifier→fabric posture feed (single-local-asset model). Mirrors this
    // controller's aggregate FleetPosture into the fabric posture for the one
    // locally governed asset, so fabric command enforcement reflects real
    // verified trust instead of the interim registration seed.
    spawn_local_asset_posture_feed(Arc::clone(svc));

    // WP-20 s2b — the trailing INDEPENDENT supervised monitors are now dispatched
    // in the manifest's resolved dependency order via a name→spawn registry, with
    // fail-closed DRIFT protection: `dispatch_in_order` refuses to run unless the
    // registered spawners EXACTLY cover these manifest tasks (no unknown task, no
    // missing spawner, no duplicate), so a spawner can never silently diverge from
    // its manifest entry. For this cover the resolved order is
    // campaign_monitor → cert_expiry_monitor → audit_shipper — identical to the
    // prior hand-coded order, so this is behaviour-preserving. (The posture worker +
    // telemetry watchdog above stay hand-wired — they thread the posture_tx the
    // worker returns — and the HA heartbeat/promotion loops spawn in the
    // standby-monitor block; driving THOSE through the registry too is the recorded
    // WP-20 follow-up.) All monitors are Active-block (a promoted standby (H2) runs
    // them too) + supervised non-critical (a wedged monitor cannot make anything
    // unsafe — the actuator gate is the safety spine).
    let mut monitors: kirra_verifier::execution_manager::SpawnRegistry<Arc<ServiceState>> =
        kirra_verifier::execution_manager::SpawnRegistry::new();
    // WS-4/Track 3 — OTA campaign posture-sweep monitor: auto-halts an active
    // campaign the moment a fleet regression is CONFIRMED (a fresh non-Nominal
    // posture; an unavailable/stale posture is NOT a regression and is skipped).
    monitors.register("campaign_monitor", |svc: &Arc<ServiceState>| {
        kirra_verifier::campaign_monitor::spawn_campaign_monitor(
            Arc::clone(&svc.app),
            Arc::clone(&svc.posture_cache),
        );
        tracing::info!(
            sweep_ms = kirra_verifier::campaign_monitor::CAMPAIGN_SWEEP_MS,
            "OTA campaign posture-sweep monitor spawned (WS-4 halt-on-regression)"
        );
    });
    // WP-15 (MGA G-19) — mTLS cert-principal expiry monitor: WARNs (log + audit)
    // when a pinned client cert has lapsed / is about to. Observability only (the
    // auth path already fail-closes an expired cert).
    monitors.register("cert_expiry_monitor", |svc: &Arc<ServiceState>| {
        kirra_verifier::cert_expiry_monitor::spawn_cert_expiry_monitor(Arc::clone(&svc.app));
        tracing::info!(
            sweep_ms = kirra_verifier::cert_expiry_monitor::CERT_EXPIRY_SWEEP_MS,
            warn_window_ms = kirra_verifier::cert_expiry_monitor::CERT_EXPIRY_WARN_WINDOW_MS,
            "mTLS cert-principal expiry monitor spawned (WP-15 cert lifecycle)"
        );
    });
    // WS-4/Track 3 — WORM off-box audit shipper. Opt-in: runs only when
    // KIRRA_AUDIT_SHIP_PATH names an append-only sink file so the tamper-evidence
    // log survives loss of this box.
    monitors.register("audit_shipper", |svc: &Arc<ServiceState>| {
        // EP-12: the sink path comes from the boot-validated snapshot (the
        // module reads no env). Unset → shipping off (the opt-in default).
        let ship_path = EFFECTIVE_CONFIG
            .get()
            .and_then(|c| c.audit_ship_path.clone());
        if kirra_verifier::audit_shipper::spawn_audit_shipper(
            Arc::clone(&svc.app),
            ship_path.as_deref(),
        ) {
            tracing::info!(
                interval_ms = kirra_verifier::audit_shipper::AUDIT_SHIP_INTERVAL_MS,
                "WORM off-box audit shipper spawned (WS-4 audit survivability)"
            );
        }
    });
    const TRAILING_MONITORS: &[&str] =
        &["campaign_monitor", "cert_expiry_monitor", "audit_shipper"];
    if let Err(e) = kirra_verifier::execution_manager::dispatch_in_order(
        &monitors,
        kirra_verifier::execution_manager::TASK_MANIFEST,
        TRAILING_MONITORS,
        svc,
    ) {
        tracing::error!(
            error = %e,
            "execution manager: supervised-monitor dispatch failed (unresolvable manifest or registry/cover drift) — aborting (fail-closed)"
        );
        std::process::exit(1);
    }
}

/// One idempotent push of the cached fleet posture into the local asset.
///
/// Fail-closed: a poisoned OR stale cache yields NO push. The actuator gate
/// already fail-closes on a stale fleet posture, so leaving the asset's last
/// good posture in place is correct — we never write a stale or
/// not-yet-computed posture forward. Compare-before-write avoids churn (and a
/// generation bump / propagation pass) when the posture is unchanged.
/// #88 tightening: seed the LOCAL fabric asset fail-closed `LockedOut`.
///
/// `register_asset` seeds every asset `Degraded` (the documented interim) —
/// correct for PEERS, which have no lifting feed (cross-asset propagation only
/// degrades, never lifts, so a `LockedOut` peer would be bricked). But the ONE
/// locally governed asset named by `KIRRA_FABRIC_ASSET_ID` DOES have a lifting
/// feed (`sync_local_asset_posture`), so it can be fail-closed: it starts
/// `LockedOut` and the feed lifts it to a real posture on the first Active
/// recalc. On `PassiveStandby` (no recalc) it correctly stays `LockedOut` until
/// promotion. Call this right after each `register_asset` for the just-
/// registered asset id; it only acts when that id IS the configured local asset.
pub(crate) fn seed_local_asset_lockedout(svc: &ServiceState, registered_id: &str) {
    let local = std::env::var("KIRRA_FABRIC_ASSET_ID").ok();
    let local = local.as_deref().map(str::trim).filter(|s| !s.is_empty());
    seed_local_asset_lockedout_inner(svc, registered_id, local);
}

/// Env-free core of [`seed_local_asset_lockedout`] (testable). Overrides the
/// `Degraded` registration seed with fail-closed `LockedOut` IFF `registered_id`
/// is the configured local asset. A peer (or an unset `local_id`) is left at its
/// `Degraded` seed — peers rely on it.
pub(crate) fn seed_local_asset_lockedout_inner(
    svc: &ServiceState,
    registered_id: &str,
    local_id: Option<&str>,
) {
    let Some(local_id) = local_id else { return };
    if local_id != registered_id {
        return;
    }
    svc.fabric_router.update_asset_posture(
        local_id,
        AssetPosture {
            asset_id: local_id.to_string(),
            posture: FleetPosture::LockedOut,
            // generation 0 = never-computed sentinel; the feed's first push
            // (>= generation 1) supersedes it, exactly like the register seed.
            generation: 0,
            computed_at_ms: now_ms(),
            contributing_nodes: vec![],
            blocked_by: vec!["LOCAL_ASSET_FAILCLOSED_PENDING_FEED".to_string()],
        },
    );
    tracing::info!(
        asset_id = %local_id,
        "local fabric asset seeded fail-closed LockedOut; the verifier→fabric feed lifts it on the first Active recalc"
    );
}

pub(crate) fn sync_local_asset_posture(svc: &ServiceState, asset_id: &str) {
    let now = now_ms();
    let fleet = {
        let guard = match svc.posture_cache.read() {
            Ok(g) => g,
            Err(_) => {
                tracing::error!(
                    "verifier→fabric feed: posture cache poisoned — skipping push (fail-closed)"
                );
                return;
            }
        };
        match guard.as_ref() {
            Some(c) if !c.is_stale(now) => c.posture,
            Some(_) => return, // stale → do not propagate a stale posture
            None => return,    // not yet computed
        }
    };

    let current = svc.fabric_router.asset_posture(asset_id);
    if let Some(ref existing) = current {
        if existing.posture == fleet {
            return; // unchanged — nothing to do
        }
    }
    let next_gen = current
        .as_ref()
        .map(|p| p.generation.saturating_add(1))
        .unwrap_or(1);

    let blocked_by = match fleet {
        FleetPosture::Nominal => vec![],
        FleetPosture::Degraded => vec!["VERIFIER_FLEET_POSTURE_DEGRADED".to_string()],
        FleetPosture::LockedOut => vec!["VERIFIER_FLEET_POSTURE_LOCKED_OUT".to_string()],
    };

    let updated = AssetPosture {
        asset_id: asset_id.to_string(),
        posture: fleet,
        generation: next_gen,
        computed_at_ms: now,
        contributing_nodes: vec![],
        blocked_by,
    };
    // External-entry update: runs one bounded cross-asset propagation pass so
    // a LockedOut local asset degrades its dependents in the same fabric pass.
    svc.fabric_router
        .update_asset_posture_and_propagate(asset_id, updated);
    tracing::info!(
        asset_id = %asset_id,
        posture = ?fleet,
        generation = next_gen,
        "verifier→fabric posture feed: local asset posture updated"
    );
}

pub(crate) fn emit_posture_event(state: &AppState, event_type: &str, node_id: Option<String>) {
    let posture = node_id.as_ref().map(|id| state.calculate_posture(id));
    let _ = state.posture_tx.send(PostureStreamEvent {
        event_type: event_type.to_string(),
        node_id,
        emitted_at_ms: now_ms(),
        posture,
    });
}
