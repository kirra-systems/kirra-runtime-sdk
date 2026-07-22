//! HA standby monitor — standby promotion monitor (de-monolith split of standby_monitor.rs).
//!
//! Behaviour unchanged; shared vocabulary/keys are `pub(crate)` in the parent.

use super::*;

/// Spawns the background promotion monitor task for a PassiveStandby instance.
///
/// Polls the primary heartbeat every `PROMOTION_POLL_MS`. If the heartbeat
/// age exceeds `PROMOTION_TIMEOUT_MS`, performs an atomic promotion:
///
///   1. CAS app.ha_fence.mode_active: false → true
///   2. Writes promotion record to audit chain (disk-first)
///   3. Calls recalculate_and_broadcast() — now runs as Active, populates cache
///   4. Task exits — promotion is complete and one-way
///
/// If the primary heartbeat is absent (key not found), the standby treats this
/// as stale immediately. A fresh deployment with no primary yet running will
/// NOT auto-promote — the key must have been written at least once and then
/// gone stale. This prevents both instances from starting as Active if SQLite
/// is freshly initialized.
///
/// Exception: if KIRRA_FORCE_PROMOTE=1 is set, promotes immediately regardless
/// of heartbeat state. Use for manual failover or testing.
/// `on_promote` (review H2): a hook fired ONCE on a successful promotion, after
/// the mode flip / durable epoch claim / initial recalc. The caller (the binary)
/// uses it to (re)start the Active-mode posture-freshness tasks — the serialized
/// posture-engine worker, the telemetry watchdog, the periodic refresh loop, and
/// the local-asset feed — on the freshly-promoted node. Without it the promoted
/// node's posture cache goes stale one `POSTURE_CACHE_TTL_MS` after promotion and
/// every gated route fail-closes until process restart. It is a hook rather than
/// inline wiring because those tasks need `ServiceState`/bin-local handles the
/// library promotion path does not hold. `Arc<dyn Fn>` so the supervised task
/// factory can clone it across a restart.
pub type OnPromote = Arc<dyn Fn() + Send + Sync + 'static>;

pub fn spawn_promotion_monitor(
    app: Arc<AppState>,
    cache: SharedPostureCache,
    on_promote: OnPromote,
    ha: HaTimings,
) {
    let id = instance_id();
    // C2: supervised so a panic doesn't permanently disable failover. NON-critical
    // (a standby cannot serve writes anyway, so a LockedOut escalation is moot);
    // run_forever=false because exiting after a successful promotion is legitimate.
    crate::supervisor::spawn_supervised(
        "ha_promotion_monitor",
        /* critical   */ false,
        /* run-forever */ false,
        None,
        move || {
            promotion_loop(
                Arc::clone(&app),
                cache.clone(),
                id.clone(),
                Arc::clone(&on_promote),
                ha,
            )
        },
    );
}

/// Actions taken exactly once on a SUCCESSFUL promotion (reviews H2 + H3):
///   1. `on_promote()` — (re)start the Active posture-freshness tasks on the
///      newly-Active node (worker / watchdog / refresh / feed). Fixes H2: a
///      promoted standby used to skip this and fail-close one TTL later.
///   2. `spawn_heartbeat_writer` — start heartbeating as the new Active so any
///      tertiary standby sees this node alive (no invisible-Active window /
///      spurious re-promotion; the H3 addition).
/// Extracted so the promotion→wiring seam is unit-testable without the env-driven
/// `promotion_loop`.
///
/// Fail-closed on a panicking hook (Copilot #817): this runs inside the
/// supervised `promotion_loop`, and the supervisor ALWAYS restarts a task that
/// panics (`supervisor.rs`: "a panic is a bug, never a legitimate exit"). If
/// `on_promote` unwound, the loop would restart and re-run `perform_promotion`
/// — which durably claims a NEW epoch each time — churning the HA epoch while
/// leaving the node Active-but-unwired (the heartbeat writer below is never
/// reached, so it never advertises liveness and the restarted loop just
/// re-promotes on the still-stale heartbeat). Convert the panic into a clean
/// process exit: this instance dies and another standby / a systemd restart
/// re-promotes cleanly with the freshness wiring intact. (A hook that itself
/// calls `std::process::exit` — the double-set guard in
/// `wire_active_posture_freshness` — already terminates without unwinding, so
/// `catch_unwind` is transparent to it.)
pub(crate) fn apply_post_promotion(
    app: &Arc<AppState>,
    on_promote: &(dyn Fn() + Send + Sync),
    ha: HaTimings,
) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(on_promote)).is_err() {
        tracing::error!(
            "on_promote hook panicked after a successful promotion — exiting \
             (fail-closed: a supervised re-promotion would churn the HA epoch)"
        );
        std::process::exit(1);
    }
    spawn_heartbeat_writer(Arc::clone(app), ha);
}

/// #1099 split-brain guard decision for the `KIRRA_FORCE_PROMOTE` path. Given the
/// peer heartbeat token observed BEFORE and AFTER a one-interval probe, returns true
/// iff the force-promote must be REFUSED because the peer is demonstrably LIVE (its
/// token advanced between the two reads). No token (cold start / dead peer) or an
/// unchanged token (silent / wedged peer) permits the force — the operator override
/// is only blocked when it would knowingly race an actively-heartbeating primary.
pub(crate) fn force_promote_refused_by_live_peer(
    before: Option<&str>,
    after: Option<&str>,
) -> bool {
    matches!((before, after), (Some(b), Some(a)) if a != b)
}

pub(crate) async fn promotion_loop(
    app: Arc<AppState>,
    cache: SharedPostureCache,
    id: String,
    on_promote: OnPromote,
    timings: HaTimings,
) {
    let poll_ms = timings.promotion_poll_ms;

    // #689: cross-check the configured timeout against the configured
    // heartbeat interval (the const assert only guards the defaults). Clamp
    // UP to the (MAX+1)×interval floor so there is at least one full
    // heartbeat interval between the primary's self-demote and the
    // standby's promotion (see `enforce_promotion_timeout_floor`). Both
    // values arrive boot-VALIDATED via `HaTimings` (EP-12) — a malformed
    // env value now refuses startup instead of silently defaulting here.
    let (timeout_ms, clamped) = enforce_promotion_timeout_floor(
        timings.promotion_timeout_ms,
        timings.heartbeat_interval_ms,
    );
    if clamped {
        tracing::error!(
            configured_timeout_ms = timings.promotion_timeout_ms,
            interval_ms = timings.heartbeat_interval_ms,
            resolved_timeout_ms = timeout_ms,
            max_consecutive_failures = MAX_CONSECUTIVE_HEARTBEAT_FAILURES,
            "KIRRA_PROMOTION_TIMEOUT below the safe floor for KIRRA_HEARTBEAT_INTERVAL \
                 (timeout < (MAX+1)×interval): clamped UP to keep ≥1 heartbeat interval between \
                 the primary's self-demote and the standby's promotion (#689 split-brain margin). \
                 Fix the env config to silence this."
        );
    }

    if timings.force_promote {
        // #1099: refuse a force-promote over a DEMONSTRABLY-LIVE peer. Read the
        // heartbeat token twice, one interval apart; an ADVANCING token means the
        // primary is actively heartbeating, and forcing over it is exactly the
        // split-brain the #689 margin exists to prevent (the durable epoch CAS is the
        // backstop, but we must not deliberately race a live holder). No token (cold
        // start / dead peer) or an unchanged token (silent / wedged peer) → the force
        // proceeds. The promotion itself stays audit-chained
        // (STANDBY_PROMOTED_TO_ACTIVE, reason FORCE_PROMOTE) via perform_promotion.
        // SAFETY: SG-HA-3 — durable reads run off the async runtime (call_read).
        let before = match app
            .store
            .call_read(|store| store.load_engine_state(HEARTBEAT_KEY))
            .await
        {
            Ok(Ok(tok)) => tok,
            _ => None,
        };
        let refuse = if before.is_some() {
            tokio::time::sleep(Duration::from_millis(timings.heartbeat_interval_ms.max(1))).await;
            let after = match app
                .store
                .call_read(|store| store.load_engine_state(HEARTBEAT_KEY))
                .await
            {
                Ok(Ok(tok)) => tok,
                _ => None,
            };
            force_promote_refused_by_live_peer(before.as_deref(), after.as_deref())
        } else {
            false
        };
        if refuse {
            // #793 F6: count the aborted force-promotion so an abort storm (a
            // misconfigured/contended failover) is observable on /metrics.
            app.observability.fleet_metrics.record_ha_promotion_abort();
            tracing::error!(
                instance_id = %id,
                "KIRRA_FORCE_PROMOTE REFUSED (#1099): a peer heartbeat is actively advancing — refusing to force-promote over a LIVE primary (split-brain guard). Stop the peer or clear its heartbeat key, then retry."
            );
            return;
        }
        tracing::warn!(
            instance_id = %id,
            "KIRRA_FORCE_PROMOTE=1: no live peer heartbeat observed — bypassing heartbeat timeout, promoting immediately"
        );
        if perform_promotion(&app, &cache, &id, "FORCE_PROMOTE").await {
            // reviews H2 + H3: re-wire posture freshness AND start
            // heartbeating on the newly-Active node (see apply_post_promotion).
            apply_post_promotion(&app, on_promote.as_ref(), timings);
        }
        return;
    }

    // EP-03: the lease gate. When armed, the promotion TRIGGER is the lease
    // rule (`promote_after_ms` = ttl + ttl/2 ≈ 4.5 s at the default TTL)
    // instead of the legacy heartbeat timeout (~10 s) — the ≤5 s failover
    // property. The durable epoch CAS in `perform_promotion` remains the
    // sole takeover authority either way.
    let lease = timings.lease;
    let poll_ms = match &lease {
        // The challenger must observe every renewal: clamp the poll DOWN to
        // half the renew cadence if the configured poll is too slow
        // (clamping down is the safe direction — never promotes late, never
        // misses a live holder's renewals).
        Some(params) if !crate::lease::poll_fast_enough(poll_ms, params) => {
            let clamped = (params.renew_interval_ms / 2).max(1);
            tracing::error!(
                poll_ms,
                clamped,
                renew_interval_ms = params.renew_interval_ms,
                "KIRRA_PROMOTION_POLL too slow to observe every lease renewal — clamped down"
            );
            clamped
        }
        _ => poll_ms,
    };

    let mut tick = interval(Duration::from_millis(poll_ms));

    // #80: monotonic heartbeat-freshness tracker. Staleness is measured as
    // "how long the heartbeat TOKEN has gone unchanged", on this standby's
    // own monotonic clock — NOT by differencing the primary's wall-clock
    // timestamp against ours (which is skew-vulnerable across machines).
    let mut freshness = HeartbeatFreshness::new(Instant::now());

    tracing::info!(
        instance_id = %id,
        poll_ms     = poll_ms,
        timeout_ms  = timeout_ms,
        lease_enabled = lease.is_some(),
        "Promotion monitor started"
    );

    // EP-03 (gate on): the lease path. Promote only when BOTH liveness
    // signals — the heartbeat TOKEN and the durable lease STAMP — have gone
    // unobserved-to-advance for `promote_after_ms`, each timed on THIS
    // standby's monotonic clock via the same change-token tracker (#80's
    // skew-immunity, never cross-machine wall-clock differencing). The
    // conjunction keeps a mixed-config fleet safe: a gate-OFF primary never
    // renews the lease (its stamp reads permanently stale) but its
    // heartbeat token keeps advancing, so a gate-ON standby will not
    // promote over it; a DEAD primary stops advancing both, so promotion
    // fires at promote_after (≈4.5 s), not the legacy ~10 s timeout.
    if let Some(params) = lease {
        let mut hb_fresh = HeartbeatFreshness::new(Instant::now());
        let mut lease_fresh = HeartbeatFreshness::new(Instant::now());
        loop {
            tick.tick().await;

            let read = app
                .store
                .call_read(|store| {
                    let token = store.load_engine_state(HEARTBEAT_KEY)?;
                    let ha = store.read_ha_lease()?;
                    Ok::<_, rusqlite::Error>((token, ha))
                })
                .await;
            let (token, ha) = match read {
                Ok(Ok(pair)) => pair,
                // SAFETY: SG-HA-4 — a store/offload error skips the tick
                // without disturbing either anchor (decide only on a read).
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "Lease monitor: store read failed");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Lease monitor: read offload failed");
                    continue;
                }
            };

            // Fresh deployment: no primary has EVER held the lease and no
            // heartbeat was ever written — nothing to take over (mirrors the
            // legacy absent-key rule: never auto-promote a cold fleet).
            if ha.holder.is_none() && token.is_none() {
                tracing::debug!("Lease monitor: no holder yet — waiting for a primary");
                continue;
            }

            let now = Instant::now();
            if let Some(t) = &token {
                let _ = hb_fresh.observe(t, now);
            }
            let _ = lease_fresh.observe(&ha.last_renew_ms.to_string(), now);

            let hb_elapsed = hb_fresh.elapsed(now).as_millis() as u64;
            let lease_elapsed = lease_fresh.elapsed(now).as_millis() as u64;
            let hb_stale = crate::lease::should_promote(hb_elapsed, &params);
            let lease_stale = crate::lease::should_promote(lease_elapsed, &params);

            if hb_stale && lease_stale {
                tracing::error!(
                    instance_id = %id,
                    hb_elapsed_ms = hb_elapsed,
                    lease_elapsed_ms = lease_elapsed,
                    promote_after_ms = params.promote_after_ms,
                    "Lease expired unobserved past the promote deadline — promoting to Active"
                );
                if perform_promotion(&app, &cache, &id, "LEASE_EXPIRED").await {
                    apply_post_promotion(&app, on_promote.as_ref(), timings);
                }
                return;
            }
            tracing::debug!(
                hb_elapsed_ms = hb_elapsed,
                lease_elapsed_ms = lease_elapsed,
                "Lease monitor: holder alive (a liveness signal is advancing)"
            );
        }
    }

    loop {
        tick.tick().await;

        // Read the heartbeat TOKEN. It is treated as an opaque change-detector
        // (the primary writes now_ms(), but we never interpret its value as a
        // clock). On any read failure we skip this tick without disturbing the
        // anchor — we only ever decide on a successful read.
        // Closure returns Some(token) on a successful read; None signals
        // "skip this tick" (no key yet / read error) — the outer loop
        // `continue`s on None (Rule 4: `continue` cannot cross the closure).
        // SAFETY: SG-HA-3 — durable writes/reads must never block the async runtime.
        let token = match app
            .store
            .call_read(|store| store.load_engine_state(HEARTBEAT_KEY))
            .await
        {
            Ok(Ok(Some(token))) => token,
            Ok(Ok(None)) => {
                tracing::debug!("Promotion monitor: no heartbeat key yet — waiting for primary");
                continue;
            }
            // SAFETY: SG-HA-4 — DB errors demote node to safe state (fail-closed).
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "Promotion monitor: failed to read heartbeat");
                continue;
            }
            // SAFETY: SG-HA-4 — DB actor/offload failure is fail-closed.
            Err(e) => {
                tracing::warn!(error = %e, "Promotion monitor: heartbeat read offload failed");
                continue;
            }
        };

        let now = Instant::now();

        // Token advanced (changed, or first ever seen) → primary is alive →
        // re-anchor and wait. A CHANGED token re-anchors even if its numeric
        // value moved backward (e.g. a primary NTP step), so a primary clock
        // skew can never trigger a spurious failover.
        if freshness.observe(&token, now) {
            tracing::debug!(
                instance_id = %id,
                "Promotion monitor: heartbeat token advanced — primary alive"
            );
            continue;
        }

        // Token unchanged: how long (monotonic) has it been stale?
        let elapsed = freshness.elapsed(now);

        // The promotion gate is `promotion_decision` (unit-tested in
        // `sg_009_promotion_act_tests`) — the loop just acts on its verdict.
        if promotion_decision(elapsed, timeout_ms) {
            tracing::error!(
                instance_id = %id,
                stale_ms    = elapsed.as_millis() as u64,
                timeout_ms  = timeout_ms,
                "Primary heartbeat token unchanged past timeout — promoting to Active"
            );
            if perform_promotion(&app, &cache, &id, "HEARTBEAT_TIMEOUT").await {
                // reviews H2 + H3: re-wire posture freshness AND start
                // heartbeating on the newly-Active node (see apply_post_promotion).
                apply_post_promotion(&app, on_promote.as_ref(), timings);
            }
            return;
        } else {
            tracing::debug!(
                stale_ms = elapsed.as_millis() as u64,
                timeout_ms = timeout_ms,
                "Promotion monitor: primary alive (heartbeat token fresh)"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Promotion execution — atomic, disk-first
// ---------------------------------------------------------------------------

pub(crate) async fn perform_promotion(
    app: &Arc<AppState>,
    cache: &SharedPostureCache,
    id: &str,
    reason: &str,
) -> bool {
    let ts = now_ms();

    // Step 1: DURABLE epoch claim (the real split-brain fence).
    //
    // SQLite serializes write transactions, so a conditional UPDATE on the
    // singleton `ha_state` row gives a real distributed CAS: two standbys
    // that both read the same `observed` epoch will serialize at commit
    // and only one will see rows_affected == 1 (Some(new_epoch)). The
    // loser sees None and MUST abort — its in-memory mode_active stays
    // false and no audit/cache state is written. The previous in-memory
    // `compare_exchange` did NOT provide this guarantee: it was per-process.
    // SAFETY: SG-HA-3 — durable writes/reads must never block the async runtime.
    let observed = match app.store.call_read(|store| store.current_epoch()).await {
        Ok(Ok(e)) => e,
        // SAFETY: SG-HA-4 — DB errors demote node to safe state (fail-closed).
        Ok(Err(e)) => {
            tracing::error!(error = %e, instance_id = %id,
                "promotion: cannot read epoch — aborting");
            return false;
        }
        // SAFETY: SG-HA-4 — DB actor/offload failure is fail-closed.
        Err(e) => {
            tracing::error!(error = %e, instance_id = %id,
                "promotion: epoch read offload failed — aborting");
            return false;
        }
    };

    let id_owned = id.to_string();
    let new_epoch = match app
        .store
        .call(move |store| store.try_claim_epoch(observed, &id_owned, ts))
        .await
    {
        Ok(Ok(Some(e))) => e,
        Ok(Ok(None)) => {
            // Fence held: another instance advanced the epoch between
            // our read and our write. We stay PassiveStandby.
            tracing::warn!(
                instance_id = %id,
                observed    = observed,
                reason      = %reason,
                "promotion ABORTED — epoch already advanced by another instance (durable fence held)"
            );
            return false;
        }
        // SAFETY: SG-HA-4 — DB errors demote node to safe state (fail-closed).
        Ok(Err(e)) => {
            tracing::error!(error = %e, instance_id = %id,
                "promotion: epoch claim execute failed — aborting");
            return false;
        }
        // SAFETY: SG-HA-4 — DB actor/offload failure is fail-closed.
        Err(e) => {
            tracing::error!(error = %e, instance_id = %id,
                "promotion: epoch claim offload failed — aborting");
            return false;
        }
    };

    // Step 1b (#78): ensure the hash-v2 audit-chain migration anchor BEFORE this
    // node writes any audit record as Active. The Step 4 promotion event is the
    // first such write, so the anchor must be durable first.
    //
    // FAIL-CLOSED — ABORT (not just log) on failure: leave `mode_active` false,
    // write no promotion record, stay PassiveStandby (same posture as a lost
    // epoch claim). This differs deliberately from the STARTUP anchor path
    // (kirra_verifier_service.rs), which logs-and-continues: startup runs the
    // anchor BEFORE the listener binds and before any Active write, so a
    // transient failure there is retried on the next boot with no Active writer
    // in between. Here we are mid-promotion — the only alternative to aborting is
    // becoming a NEW Active writer that appends to an UNANCHORED v2 chain, whose
    // v1→v2 migration boundary was never durably anchored. That is strictly worse
    // than staying PassiveStandby (another instance, or a later retry once the
    // store is healthy, promotes instead). A newly-Active writer on an unanchored
    // chain is the worse state, so we fail closed to standby.
    match app
        .store
        .call(move |store| store.ensure_hash_v2_migration_anchor(ts))
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(
                error = %e, instance_id = %id, epoch = new_epoch, reason = %reason,
                "promotion ABORTED — cannot ensure hash-v2 audit anchor; staying PassiveStandby (fail-closed, mode_active stays false, no promotion record)"
            );
            return false;
        }
        Err(e) => {
            tracing::error!(
                error = %e, instance_id = %id, epoch = new_epoch, reason = %reason,
                "promotion ABORTED — hash-v2 anchor offload failed; staying PassiveStandby (fail-closed, mode_active stays false, no promotion record)"
            );
            return false;
        }
    }

    // Step 2: Cache the won epoch in memory so the mutation gate can
    // compare it against the DB epoch on every write. If a later
    // promotion fences us, gate-level checks will detect held != db and
    // self-demote.
    app.ha_fence
        .held_epoch
        .store(new_epoch, std::sync::atomic::Ordering::SeqCst);
    // Pass B1 (S3 / #115): re-stamp the cached DB epoch atomically so the
    // gate sees this promotion without taking the store lock. Release pairs
    // with the gate's Acquire load at `policy_layer.rs::enforce_posture_routing`.
    app.ha_fence
        .cached_db_epoch
        .store(new_epoch, std::sync::atomic::Ordering::Release);

    // Step 3: Flip the local mode atomic. By this point the durable
    // claim already succeeded, so this is a per-process bookkeeping
    // step, not a split-brain guard. A racing local CAS failure here
    // (e.g. a force-promote already flipped us) is informational only.
    if app
        .ha_fence
        .mode_active
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_err()
    {
        tracing::warn!(instance_id = %id,
            "Local mode atomic was already Active at promotion (continuing — durable epoch already claimed)");
    }

    tracing::info!(
        instance_id = %id,
        reason      = %reason,
        ts          = ts,
        epoch       = new_epoch,
        "Promoted to Active (durable epoch claimed)"
    );

    // Step 4: Persist promotion record (disk-first).
    // save_engine_state takes &self; acquire lock once for that, then release
    // and re-acquire as mut for save_posture_event_chained (&mut self).
    // SAFETY: SG-HA-3 — durable promotion records are persisted via async store actor calls.
    let id_owned = id.to_string();
    match app
        .store
        .call(move |store| store.save_engine_state(PROMOTION_RECORD_KEY, &id_owned))
        .await
    {
        Ok(Ok(())) => {}
        // SAFETY: SG-HA-4 — DB errors demote node to safe state (fail-closed).
        Ok(Err(e)) => {
            tracing::error!(
                error = %e,
                instance_id = %id,
                epoch = new_epoch,
                "promotion ABORTED — failed to persist promotion record key"
            );
            // Fail-closed: we already claimed the durable epoch; if we can't persist
            // required promotion metadata, immediately self-demote so this instance
            // does not serve writes as a partially-promoted Active.
            app.ha_fence
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                instance_id = %id,
                epoch = new_epoch,
                "promotion ABORTED — failed to persist promotion record key"
            );
            app.ha_fence
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
    }

    let id_owned = id.to_string();
    let reason_owned = reason.to_string();
    match app
        .store
        .call(move |store| {
            // Tag the audit payload with `epoch` so a partitioned write is
            // identifiable after the fact (the audit chain itself is
            // monotonic; the epoch column adds the HA-generation linkage).
            let reason_ref = reason_owned.as_str();
            let audit = serde_json::json!({
                "event":          "STANDBY_PROMOTED_TO_ACTIVE",
                "instance_id":    id_owned,
                "reason":         reason_ref,
                "promoted_at_ms": ts,
                "epoch":          new_epoch,
            });
            store.save_posture_event_chained(
                "standby_monitor",
                "STANDBY_PROMOTED_TO_ACTIVE",
                &audit.to_string(),
                Some(reason_ref),
                ts,
            )
        })
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(
                error = %e,
                instance_id = %id,
                epoch = new_epoch,
                "promotion ABORTED — failed to persist promotion audit event"
            );
            // Fail-closed: do not remain Active if the required audit event cannot be persisted.
            app.ha_fence
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                instance_id = %id,
                epoch = new_epoch,
                "promotion ABORTED — failed to persist promotion audit event"
            );
            app.ha_fence
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
    }

    // #112: record the control-authority handoff provenance ALONGSIDE the
    // promotion event (complement, not a duplicate). A primary failure transfers
    // autonomous command authority to this standby controller. Observability
    // only — never blocks promotion (its own store lock + failure counter).
    crate::command_source::record_handoff(
        app,
        crate::command_source::CommandSource::AutonomousPlanner,
        crate::command_source::CommandSource::StandbyController,
        reason,
        ts,
    );

    // Step 4c (review F1 — HA trust-state coherence): RE-HYDRATE the in-memory
    // fleet (node trust states + the dependency graph) from the SHARED store before
    // the first Active recalc. A PassiveStandby's `fleet.nodes` was seeded once at
    // its own boot and never sees the primary's durable trust downgrades (it does not
    // process them while passive). Promoting and recalculating posture from that
    // stale cache can treat a durably-`Untrusted` node as `Trusted` → a falsely
    // `Nominal` posture that admits forbidden commands. Reloading here makes the first
    // serving posture reflect DURABLE trust. Ordered before the Step 4d generation
    // re-seed and the Step 5 recalc; the posture cache stays stale/empty (the mutation
    // gate + the Step 6 freshness gate block serving) until Step 5, so no command is
    // served against stale trust in the interim. Runs AFTER the epoch claim / promotion
    // records so a fenced loser (which returned early above) never touches it.
    // Fail-closed: a load error aborts promotion and self-demotes — a promoted Active
    // computing posture from stale trust is worse than staying PassiveStandby for a
    // retry / another instance.
    // SAFETY: SG-HA-3 — durable read offloaded off the async runtime.
    // SAFETY: SG-HA-4 — DB/offload failure fails closed (self-demote, return false).
    let app_for_hydrate = Arc::clone(app);
    match tokio::task::spawn_blocking(move || app_for_hydrate.hydrate_fleet_from_store()).await {
        Ok(Ok((node_count, dep_count))) => {
            tracing::info!(
                instance_id = %id,
                epoch = new_epoch,
                node_count,
                dep_count,
                "promotion: rehydrated fleet trust + dependency graph from shared store (review F1) — first Active recalc computes posture from DURABLE trust, not the boot snapshot"
            );
        }
        Ok(Err(e)) => {
            tracing::error!(
                error = %e, instance_id = %id, epoch = new_epoch,
                "promotion ABORTED — cannot rehydrate fleet trust from shared store; staying PassiveStandby (fail-closed) to avoid computing posture from stale in-memory trust"
            );
            app.ha_fence
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
        Err(e) => {
            tracing::error!(
                error = %e, instance_id = %id, epoch = new_epoch,
                "promotion ABORTED — fleet rehydrate offload failed; staying PassiveStandby (fail-closed)"
            );
            app.ha_fence
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
    }

    // Step 4d (#771 F1): RE-SEED the generation counter from the SHARED store
    // before the first Active recalc. HA pairs share one SQLite file; the dead
    // primary advanced the durable generation high-water for its ENTIRE uptime
    // (~34,560 generations/day at the recalc cadence), while THIS node's in-memory
    // `POSTURE_GENERATION` was seeded once — at its own boot — and is now far
    // behind. Without this re-seed the first post-promotion recalc (Step 5) emits
    // a generation BELOW the dead primary's last, time-reversing the sequence that
    // federation peers hard-reject (`GenerationRegress`, fail-closed) and SSE
    // consumers order by — roughly a day of cross-fleet trust blindness per day of
    // prior primary uptime, on the exact event (primary death) where fresh posture
    // matters most. `init_generation_from_store` is idempotent (`fetch_max` only
    // ever RAISES the counter), so re-running it at promotion cannot regress a
    // counter another racing recalc already advanced. Runs AFTER the epoch claim /
    // promotion records so a fenced loser (which returned early above) never
    // touches it. Fail-closed: a store read error aborts promotion and self-demotes
    // — a promoted Active that would time-reverse generations is worse than staying
    // PassiveStandby for another instance / a later retry to handle.
    // SAFETY: SG-HA-3 — durable read offloaded off the async runtime.
    // SAFETY: SG-HA-4 — DB/offload failure fails closed (self-demote, return false).
    let app_for_seed = Arc::clone(app);
    match tokio::task::spawn_blocking(move || init_generation_from_store(&app_for_seed)).await {
        Ok(Ok(seeded_high_water)) => {
            tracing::info!(
                instance_id = %id,
                epoch = new_epoch,
                seeded_high_water,
                "promotion: re-seeded generation counter from shared store (#771 F1) — first Active recalc will emit above the dead primary's last generation"
            );
        }
        Ok(Err(e)) => {
            tracing::error!(
                error = %e, instance_id = %id, epoch = new_epoch,
                "promotion ABORTED — cannot re-seed generation high-water from shared store; staying PassiveStandby (fail-closed) to avoid time-reversing generations"
            );
            app.ha_fence
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
        Err(e) => {
            tracing::error!(
                error = %e, instance_id = %id, epoch = new_epoch,
                "promotion ABORTED — generation re-seed offload failed; staying PassiveStandby (fail-closed)"
            );
            app.ha_fence
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
    }

    // Step 5: Initial recalculation as Active instance.
    // is_active() now returns true, so recalculate_and_broadcast will write
    // to the cache and emit broadcasts instead of returning early.
    // SAFETY: SG-HA-3 — durable writes in recalc must not pin tokio workers.
    let app_for_recalc = Arc::clone(app);
    let cache_for_recalc = cache.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || {
        recalculate_and_broadcast(&app_for_recalc, &cache_for_recalc);
    })
    .await
    {
        tracing::error!(
            error = %e,
            instance_id = %id,
            "promotion ABORTED — initial posture recompute task failed"
        );
        // Fail-closed: avoid remaining Active without a fresh posture cache.
        app.ha_fence
            .mode_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
        return false;
    }

    // Step 6 (#83): posture-freshness gate. A freshly-promoted Active node must
    // hold a NON-stale posture before it serves commands. Reuse the single TTL
    // authority (`resolve_post_promotion_posture` → `resolve_posture_with_reason`
    // at POSTURE_CACHE_TTL_MS); do NOT duplicate the staleness logic. If the
    // Step 5 recalc did not yield a fresh posture, the resolution is
    // LockedOut(<reason>) and the node fails closed — the mutation gate
    // independently blocks on the same stale cache, so no command is served
    // against a stale posture. (Promotion is one-way: we do not revert the
    // epoch claim here; the node simply does not serve until posture recovers.)
    match resolve_post_promotion_posture(cache) {
        (posture, None) => tracing::info!(
            instance_id = %id,
            epoch       = new_epoch,
            posture     = ?posture,
            "Promotion complete — posture cache fresh, serving (SSE broadcast active)"
        ),
        (_, Some(stale_reason)) => tracing::error!(
            instance_id = %id,
            epoch       = new_epoch,
            reason      = %stale_reason,
            "Promotion recalc did NOT yield a fresh posture — node FAIL-CLOSED (non-serving) until posture recovers; the mutation gate blocks commands on the stale cache"
        ),
    }

    // Promotion succeeded (durable epoch claimed, audit anchored, mode flipped).
    // WS-0.5 — count the completed failover for /metrics (aborted promotions
    // above returned false and are not counted). Observability only.
    app.observability.fleet_metrics.record_ha_promotion();
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod force_promote_guard_tests {
    use super::force_promote_refused_by_live_peer;

    #[test]
    fn refuses_only_when_a_present_token_advances() {
        // No token either read (cold start / never-heartbeated peer) → allow.
        assert!(!force_promote_refused_by_live_peer(None, None));
        // Token disappeared (peer stopped writing) → allow.
        assert!(!force_promote_refused_by_live_peer(Some("100.1"), None));
        // Token appeared but was absent before → not two comparable reads → allow
        // (the probe only sleeps+re-reads when `before` is Some, so this arm is
        // defensive; an appearing token is still not proof of a sustained peer).
        assert!(!force_promote_refused_by_live_peer(None, Some("100.1")));
        // Token unchanged (silent / wedged primary) → allow the override.
        assert!(!force_promote_refused_by_live_peer(
            Some("100.1"),
            Some("100.1")
        ));
        // Token ADVANCED between the two reads → a live primary → REFUSE.
        assert!(force_promote_refused_by_live_peer(
            Some("100.1"),
            Some("100.2")
        ));
    }
}
