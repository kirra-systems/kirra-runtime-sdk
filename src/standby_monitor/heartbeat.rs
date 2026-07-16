//! HA standby monitor — primary heartbeat writer (de-monolith split of standby_monitor.rs).
//!
//! Behaviour unchanged; shared vocabulary/keys are `pub(crate)` in the parent.

use super::*;

/// Spawns the background heartbeat writer task for an Active instance.
///
/// Writes the current timestamp and instance ID to the `posture_engine_state`
/// table every `ha.heartbeat_interval_ms`. A standby monitoring this table will
/// detect the primary as alive as long as writes are succeeding.
///
/// If the primary loses its store lock (mutex poisoned) or SQLite write fails
/// for an extended period, the standby will promote. This is intentional —
/// a primary that can't write to the store can't enforce posture either.
///
/// # Demote-on-takeover
/// After each write, reads the PROMOTION_RECORD_KEY. If a standby has
/// recorded itself as promoted, the primary logs a structured warning and
/// terminates its heartbeat. The primary should then be manually restarted
/// in PassiveStandby mode.
pub fn spawn_heartbeat_writer(app: Arc<AppState>, ha: HaTimings) {
    let id = instance_id();
    // C2: supervised so a panic doesn't silently stop heartbeats (which would
    // trigger a spurious standby promotion). NON-critical — a genuinely dead
    // heartbeat is the DESIGNED failover signal, so no LockedOut escalation;
    // run_forever=false because exiting on fence / promoted-over is legitimate.
    crate::supervisor::spawn_supervised(
        "ha_heartbeat_writer",
        /* critical   */ false,
        /* run-forever */ false,
        None,
        move || heartbeat_loop(Arc::clone(&app), id.clone(), ha),
    );
}

async fn heartbeat_loop(app: Arc<AppState>, id: String, ha: HaTimings) {
    // EP-03: the lease gate. When armed, the holder must renew at the lease
    // half-life — which is FASTER than the default heartbeat interval
    // (1.5 s vs 2 s at the default TTL) — so the loop runs at the tighter
    // of the two cadences (each tick both heartbeats AND renews).
    let lease = ha.lease;
    let interval_ms = match &lease {
        Some(p) => ha.heartbeat_interval_ms.min(p.renew_interval_ms.max(1)),
        None => ha.heartbeat_interval_ms,
    };

    let mut tick = interval(Duration::from_millis(interval_ms));

    tracing::info!(
        instance_id = %id,
        interval_ms = interval_ms,
        lease_enabled = lease.is_some(),
        "Heartbeat writer started"
    );

    // EP-03: monotonic anchor of the last SUCCESSFUL lease renewal. The
    // durable claim that made this instance Active stamped the lease
    // (`try_claim_epoch` sets `updated_at_ms`), so loop start counts as
    // freshly renewed.
    let mut last_renew = Instant::now();

    // Review item "1": consecutive failed heartbeat ticks (write or epoch
    // read). Reset to 0 on any healthy tick; at MAX_CONSECUTIVE_HEARTBEAT_
    // FAILURES the primary self-demotes (disk-wedge / fence-uncertainty).
    let mut consecutive_failures: u32 = 0;

    // The store work runs under one acquisition; the closure returns an
    // OUTCOME the outer loop acts on (the closure cannot `continue`/`break`
    // the outer loop directly — Rule 4).
    enum HeartbeatOutcome {
        /// Fenced or promoted-over: the closure already set mode_active=false;
        /// the loop breaks.
        SelfDemoted,
        /// The heartbeat write, the epoch read, or the lease renewal failed
        /// this tick.
        Failed,
        /// A clean tick: heartbeat written, epoch read and still owned, and —
        /// lease gate on — the durable lease renewed.
        Healthy,
    }

    loop {
        tick.tick().await;

        // EP-03 holder-side lease rule: past its own TTL this holder can no
        // longer PROVE it holds the lease — self-demote (fail-closed) before
        // touching the store, exactly like the disk-wedge path. The
        // challenger's promote deadline is ttl + ttl/2, so this demote
        // strictly precedes any lease-triggered promotion
        // (`demote_before_promote`, const-proved in `lease.rs`).
        if let Some(params) = &lease {
            let elapsed_ms = last_renew.elapsed().as_millis() as u64;
            if crate::lease::lease_expired(elapsed_ms, params) {
                app.ha_fence
                    .mode_active
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                tracing::error!(
                    instance_id = %id,
                    elapsed_ms,
                    ttl_ms = params.ttl_ms,
                    "LEASE EXPIRED — this holder cannot prove ownership; self-demoting and stopping heartbeat (fail-closed)"
                );
                break;
            }
        }

        // #80: this `now_ms()` is written as a freshness TOKEN, not a clock
        // the standby trusts. The standby treats it as an opaque
        // change-detector (it advances each tick) and times staleness on its
        // OWN monotonic clock — see `HeartbeatFreshness`. Any value that
        // strictly changes each tick would do; `now_ms()` is convenient.
        let ts = now_ms();
        // P1: run the heartbeat write + epoch read OFF the tokio worker pool
        // (`call` → spawn_blocking) so the ~2 s fsync write can't pin a shared
        // worker and head-of-line-block request handlers / the fast loop. The
        // whole closure still runs under ONE writer-lock acquisition on the
        // blocking thread, so the write-then-epoch-read group stays atomic. The
        // closure must own its captures; clone the Arc (for the atomics) and the
        // instance id (reused next tick) into it.
        let app_c = Arc::clone(&app);
        let id_c = id.clone();
        let lease_c = lease;
        let outcome = match app.store.call(move |store| {
                if let Err(e) = store.save_engine_state(HEARTBEAT_KEY, &ts.to_string()) {
                    tracing::warn!(
                        error       = %e,
                        instance_id = %id_c,
                        "Heartbeat write failed"
                    );
                    return HeartbeatOutcome::Failed;
                }

                let _ = store.save_engine_state(PRIMARY_INSTANCE_KEY, &id_c);

                // Proactive epoch-fence check: if the durable epoch has
                // advanced past our held value, another instance has
                // promoted and we have been fenced. Self-demote and stop
                // heartbeating. This fixes the pre-existing gap where
                // PROMOTION_RECORD_KEY detection only stopped the
                // heartbeat loop but left mode_active = true (the
                // mutation gate would still let writes through until
                // the next request-time epoch check).
                let held = app_c.ha_fence.held_epoch.load(std::sync::atomic::Ordering::SeqCst);
                let db_epoch = match store.current_epoch() {
                    Ok(e) => e,
                    // Review item "1": an unreadable epoch is a FAILED tick, not a
                    // silent pass. The old code logged and fell through to
                    // `Proceed`, so a primary whose disk wedged on the epoch read
                    // kept running Active with a FROZEN `cached_db_epoch` — never
                    // fenced. Count it; the consecutive-failure guard below demotes.
                    Err(e) => {
                        tracing::warn!(error = %e, instance_id = %id_c,
                            "Heartbeat writer: epoch read failed");
                        return HeartbeatOutcome::Failed;
                    }
                };
                // Pass B1 (S3 / #115): cache the freshly observed DB epoch so the
                // gate can read it lock-free. Release pairs with the gate's Acquire
                // load. This runs every HEARTBEAT_INTERVAL_MS (~2000 ms) and is the
                // cache repopulation path that bounds the gate's staleness.
                app_c.ha_fence.cached_db_epoch.store(db_epoch, std::sync::atomic::Ordering::Release);
                if held != 0 && db_epoch != held {
                    app_c.ha_fence.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
                    tracing::error!(
                        instance_id = %id_c,
                        held        = held,
                        db_epoch    = db_epoch,
                        "FENCED — durable epoch advanced past held value; self-demoting and stopping heartbeat"
                    );
                    return HeartbeatOutcome::SelfDemoted;
                }

                if let Ok(Some(promoted_by)) = store.load_engine_state(PROMOTION_RECORD_KEY) {
                    match takeover_record_verdict(&promoted_by, &id_c, held) {
                        TakeoverRecordVerdict::OwnRecord => {
                            // Our own promotion record — we ARE the promoted
                            // Active. (Before the EP-02 drill fix, this branch
                            // demoted: every promoted standby self-fenced on its
                            // first heartbeat tick.)
                        }
                        TakeoverRecordVerdict::Demote => {
                            // Mirror the epoch path: tear down the local Active
                            // flag too, not just the heartbeat loop.
                            app_c.ha_fence.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
                            tracing::error!(
                                promoted_by = %promoted_by,
                                instance_id = %id_c,
                                "Standby has promoted — primary self-demoting and stopping heartbeat. \
                                 Restart this instance in PassiveStandby mode."
                            );
                            return HeartbeatOutcome::SelfDemoted;
                        }
                        TakeoverRecordVerdict::StaleArtifact => {
                            // We verified THIS tick that the durable epoch still
                            // equals ours (the check above), and takeovers claim
                            // the epoch before writing this record — stale
                            // forensics from an older failover, not a live fence.
                            tracing::debug!(
                                promoted_by = %promoted_by,
                                instance_id = %id_c,
                                held_epoch  = held,
                                "Stale promotion record from a previous failover generation — epoch fence current; ignoring"
                            );
                        }
                    }
                }

                // EP-03: renew the durable lease under the SAME store
                // acquisition (epoch + holder guarded — a fenced or superseded
                // row refuses the renewal). A refused/failed renewal is a FAILED
                // tick: the consecutive-failure guard demotes on persistence,
                // and the holder-side expiry check above is the hard backstop.
                if let Some(params) = &lease_c {
                    let _ = params; // cadence already folded into the tick interval
                    match store.renew_lease(&id_c, held, ts) {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::warn!(
                                instance_id = %id_c,
                                held = held,
                                "Lease renewal REFUSED (epoch/holder no longer ours)"
                            );
                            return HeartbeatOutcome::Failed;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, instance_id = %id_c, "Lease renewal write failed");
                            return HeartbeatOutcome::Failed;
                        }
                    }
                }

                HeartbeatOutcome::Healthy
            }).await {
                Ok(o) => o,
                // The spawn_blocking task panicked/was cancelled — count it as a
                // failed tick (it produced no heartbeat write / epoch confirmation),
                // so a persistently broken writer trips the disk-wedge demotion.
                Err(e) => {
                    tracing::warn!(error = %e, instance_id = %id, "Heartbeat store task failed");
                    HeartbeatOutcome::Failed
                }
            };
        match outcome {
            HeartbeatOutcome::SelfDemoted => break,
            HeartbeatOutcome::Healthy => {
                consecutive_failures = 0;
                // EP-03: a healthy tick renewed the lease (gate on) — re-anchor.
                last_renew = Instant::now();
            }
            HeartbeatOutcome::Failed => {
                consecutive_failures += 1;
                // Review item "1": after N consecutive failures the primary can
                // no longer confirm it owns the epoch and the standby is about
                // to promote on heartbeat silence. Self-demote (fail closed) so
                // the old primary stops being a writer before the new one starts.
                if should_self_demote_on_heartbeat_failures(consecutive_failures) {
                    app.ha_fence
                        .mode_active
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    tracing::error!(
                        instance_id          = %id,
                        consecutive_failures = consecutive_failures,
                        max                  = MAX_CONSECUTIVE_HEARTBEAT_FAILURES,
                        "DISK-WEDGE — consecutive heartbeat/epoch failures exceeded; self-demoting and stopping heartbeat (fence-uncertainty → fail closed)"
                    );
                    break;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-poll promotion decision (pure) + monotonic heartbeat-freshness tracker
// ---------------------------------------------------------------------------

/// The per-poll promotion DECISION the monitor loop acts on: a standby promotes
/// when the primary's heartbeat token has gone UNCHANGED for at least
/// `timeout_ms`, where that elapsed time is measured on the standby's OWN
/// monotonic clock (`Instant`), not by differencing two wall-clock stamps.
///
/// `elapsed_since_heartbeat` comes from [`HeartbeatFreshness`] (an `Instant`
/// delta), so it is immune to wall-clock skew / NTP steps on EITHER machine
/// (#80). It can never be negative — that structurally subsumes the old
/// `saturating_sub` clock-skew guard (a future-dated heartbeat could previously
/// read as a huge or zero age). Boundary stays `>=` (exactly `timeout_ms`
/// elapsed promotes), matching the original inline check. Sole gate on
/// `perform_promotion`.
//
// Verifies: SG-009
pub(crate) fn promotion_decision(elapsed_since_heartbeat: Duration, timeout_ms: u64) -> bool {
    elapsed_since_heartbeat >= Duration::from_millis(timeout_ms)
}

/// Monotonic heartbeat-freshness tracker (#80).
///
/// THE CROSS-MACHINE TRAP: the primary writes its own `now_ms()` to the shared
/// heartbeat key; the standby cannot difference that against its own wall clock,
/// because the two machines' clocks are independent and may skew or NTP-step
/// relative to each other. A naive `Instant::now()` swap is also wrong —
/// monotonic clocks are per-machine and not comparable across machines.
///
/// CORRECT MODEL: treat the stored heartbeat as an opaque change-TOKEN, not a
/// comparable timestamp. The standby remembers the last token it saw and the
/// monotonic `Instant` at which it last CHANGED. Staleness is "how long has the
/// token been unchanged", timed entirely on the standby's own monotonic clock —
/// using only one machine's `Instant`, so wall-clock skew/steps on either
/// machine cannot affect it.
pub(crate) struct HeartbeatFreshness {
    /// The last heartbeat token observed (opaque string; the primary writes
    /// `now_ms().to_string()`, but its NUMERIC value is never interpreted — only
    /// equality/change matters).
    last_token: Option<String>,
    /// Monotonic instant at which `last_token` last changed (the freshness
    /// anchor). Elapsed time since this point is the heartbeat staleness.
    anchor: Instant,
}

impl HeartbeatFreshness {
    /// Starts a tracker anchored at `now` with no token observed yet.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_token: None,
            anchor: now,
        }
    }

    /// Records an observation of `token` at monotonic instant `now`. Returns
    /// `true` if the token ADVANCED (changed, or was seen for the first time) —
    /// i.e. the primary is alive — in which case the anchor is reset to `now`.
    /// Returns `false` if the token is unchanged (the primary may be stalled);
    /// the anchor is left where it was so [`Self::elapsed`] keeps growing.
    pub(crate) fn observe(&mut self, token: &str, now: Instant) -> bool {
        if self.last_token.as_deref() != Some(token) {
            self.last_token = Some(token.to_string());
            self.anchor = now;
            true
        } else {
            false
        }
    }

    /// Monotonic elapsed time since the token last changed.
    pub(crate) fn elapsed(&self, now: Instant) -> Duration {
        now.duration_since(self.anchor)
    }
}

// ---------------------------------------------------------------------------
// Promotion monitor (Standby / PassiveStandby)
// ---------------------------------------------------------------------------
