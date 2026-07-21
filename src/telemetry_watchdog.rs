// src/telemetry_watchdog.rs
//
// Asynchronous telemetry watchdog task for AV sensor node health monitoring.
//
// CORRECTIONS vs. milestone doc:
//
//   1. Watchdog sends to PostureEngineSender, NOT direct recalculate_and_broadcast.
//      The serialized recalculation worker (posture_engine_v2.rs) exists precisely
//      to prevent concurrent recalculation bursts. The watchdog must route through
//      it, not bypass it. A burst of N watchdog timeouts firing simultaneously
//      produces 1 recalculation, not N.
//
//   2. SQLite not hit on every tick. The node list is cached in memory and
//      refreshed at a configurable interval (AV_WATCHDOG_NODE_REFRESH_MS),
//      not on every 100ms sweep. Per-node last_telemetry_ms is read once per
//      sweep from memory (DashMap), not from disk on each iteration.
//
//   3. AV_TELEMETRY_TIMEOUT_MS = 2_000 (2 seconds), not 500ms.
//      500ms is too aggressive for real sensor pipelines with CPU load jitter.
//      A warning threshold (AV_TELEMETRY_WARN_MS) is introduced at 1s to give
//      operators advance visibility before the node is dropped.
//
//   4. Watchdog persists trust state on timeout (disk-first invariant).
//      A silent sensor is written to SQLite as
//      `Untrusted("TELEMETRY_TIMEOUT")` before the in-memory registry is updated,
//      so a verifier restart cannot resurrect the node as Trusted.
//
//   5. Watchdog does NOT fabricate telemetry timestamps. The telemetry timestamp
//      remains the last observed report; timeout handling records trust state and
//      triggers recalculation.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{interval, Duration};

use tokio::sync::mpsc::error::TrySendError;

use crate::clock::{Clock, SystemClock};
use crate::posture_cache::SharedPostureCache;
use crate::posture_engine_v2::{PostureEngineSender, PostureRecalcTrigger};
use crate::verifier::{AppState, NodeTrustState};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// How often the watchdog sweeps for stale nodes (milliseconds).
/// Kept short to minimize detection latency, but long enough that SQLite
/// reads during node list refresh don't create I/O pressure.
pub const AV_WATCHDOG_SWEEP_MS: u64 = 100;

/// Warn threshold: log a warning when a node hasn't reported for this long.
/// Does not trigger trust mutation — operators can investigate before action.
pub const AV_TELEMETRY_WARN_MS: u64 = 1_000; // 1 second

/// Timeout threshold: mark a node Untrusted("TELEMETRY_TIMEOUT") when it has
/// been silent for this long. Triggers posture recalculation.
///
/// Set to 2 seconds to tolerate:
///   - Normal sensor pipeline latency (~50ms)
///   - CPU load spikes causing processing delays (~200ms)
///   - OS scheduling jitter on edge hardware (~100ms)
///   - Network retransmission on wireless sensor links (~500ms)
///
/// For tightly controlled wired lab environments this can be lowered.
/// For real road deployment with wireless sensor links, consider 3-5 seconds.
pub const AV_TELEMETRY_TIMEOUT_MS: u64 = 2_000; // 2 seconds

/// How often to refresh the node list from SQLite (milliseconds).
/// The list of registered AV nodes changes rarely (only on registration),
/// so we don't re-query it on every sweep.
pub const AV_WATCHDOG_NODE_REFRESH_MS: u64 = 30_000; // 30 seconds

// ---------------------------------------------------------------------------
// Node health record — in-memory state for the watchdog
// ---------------------------------------------------------------------------

/// In-memory health tracking entry maintained by the watchdog.
/// Derived from av_subsystem_meta at startup and on each refresh cycle.
/// Not persisted — the watchdog reconstructs it from SQLite on restart.
#[derive(Debug, Clone)]
pub(crate) struct WatchdogNodeEntry {
    pub(crate) node_id: String,
    /// Last telemetry timestamp from av_subsystem_meta.last_telemetry_ms.
    /// Updated in memory when the watchdog observes a fresh telemetry report.
    pub(crate) last_seen_ms: u64,
    /// Baseline for nodes that have never reported (`last_seen_ms == 0`).
    /// Prefer the node registration timestamp so a restart cannot grant an
    /// unbounded fresh grace window; fall back to first watchdog observation
    /// only if the node registry row is missing.
    pub(crate) monitoring_started_ms: u64,
    /// Whether a timeout warning has already been logged for this sweep cycle.
    /// Prevents repeated WARN logs for the same ongoing silence.
    pub(crate) warn_logged: bool,
}

// ---------------------------------------------------------------------------
// Watchdog task
// ---------------------------------------------------------------------------

/// Spawns the background telemetry watchdog task.
///
/// The watchdog:
///   1. Loads all registered AV node IDs from SQLite at startup
///   2. Every `AV_WATCHDOG_SWEEP_MS`, checks each node's last telemetry timestamp
///   3. At `AV_TELEMETRY_WARN_MS` silence: logs a structured warning
///   4. At `AV_TELEMETRY_TIMEOUT_MS` silence:
///      a. Persists node `Untrusted("TELEMETRY_TIMEOUT")` to SQLite
///      b. Updates AppState.fleet.nodes via the same disk-first helper
///      c. Logs a structured error
///      d. Sends `PostureRecalcTrigger::WatchdogTimeout` to the posture engine channel
///      (NOT calling recalculate_and_broadcast directly — routes through the
///      serialized worker to prevent burst recalculations)
///   5. Every `AV_WATCHDOG_NODE_REFRESH_MS`, refreshes the node list from SQLite
///      to pick up newly registered nodes — OR immediately on the next sweep when
///      a registration set `AppState::av_registry_dirty` (H-3), so a fresh node is
///      monitored within one sweep rather than after up to ~28 s.
///
/// # Disk-first ordering
/// Trust state mutation flows through `AppState::mark_node_untrusted`, which
/// persists to SQLite before replacing the in-memory node. The engine's
/// `recalculate_and_broadcast` persists the posture event before updating the
/// cache. The watchdog does not write to the audit chain directly — the posture
/// engine's recalculation produces the audit entry.
///
/// # Why PostureEngineSender and not direct recalculate_and_broadcast?
/// Multiple nodes can time out simultaneously (e.g., a network partition drops
/// all sensors at once). Sending N triggers to the channel produces 1
/// recalculation after coalescing. Calling recalculate_and_broadcast N times
/// directly would produce N DAG traversals, N audit entries, and N broadcasts
/// for the same logical event.
pub fn spawn_telemetry_watchdog(
    app: Arc<AppState>,
    posture_engine_tx: PostureEngineSender,
    posture_cache: SharedPostureCache,
) {
    // DI seam (S3 / #115): production callers get the real SystemClock. The
    // `_with_clock` form below is the test-only entry point — see
    // `watchdog_di_tests` for the deterministic VirtualClock wiring. The body is
    // identical except `now_ms()` becomes `clock.now_ms()`.
    spawn_telemetry_watchdog_with_clock(
        app,
        posture_engine_tx,
        posture_cache,
        Arc::new(SystemClock),
    );
}

/// Same as [`spawn_telemetry_watchdog`] but takes an injected clock.
///
/// Production must pass `Arc::new(SystemClock)` — which is exactly what
/// [`spawn_telemetry_watchdog`] does — so the production code path through
/// this function is byte-for-byte equivalent to the previous direct
/// `now_ms()` implementation. The seam exists solely to let tests pass a
/// `VirtualClock` to exercise the dead-man's switch deterministically.
pub fn spawn_telemetry_watchdog_with_clock(
    app: Arc<AppState>,
    posture_engine_tx: PostureEngineSender,
    posture_cache: SharedPostureCache,
    clock: Arc<dyn Clock>,
) {
    // C2: a wedged watchdog is fail-OPEN (a silent sensor would never be marked
    // Untrusted), so it is supervised as CRITICAL. On repeated panic the
    // escalation sets the sticky `supervisor_tripped` flag and nudges the
    // (surviving) posture engine to recompute — which then reads the flag and
    // forces LockedOut. The timeout path also holds the posture cache handle so
    // a recalc-channel failure can force-write LockedOut immediately instead of
    // serving the pre-timeout cache until TTL expiry.
    let escalate: crate::supervisor::Escalation = {
        let app = Arc::clone(&app);
        let tx = posture_engine_tx.clone();
        Arc::new(move || {
            app.escalation
                .supervisor_tripped
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = tx.try_send(PostureRecalcTrigger::PeriodicRefresh);
        })
    };

    crate::supervisor::spawn_supervised(
        "telemetry_watchdog",
        /* critical   */ true,
        /* run-forever */ true,
        Some(escalate),
        move || {
            let app = Arc::clone(&app);
            let posture_engine_tx = posture_engine_tx.clone();
            let posture_cache = posture_cache.clone();
            let clock = Arc::clone(&clock);
            async move {
                let mut sweep_interval = interval(Duration::from_millis(AV_WATCHDOG_SWEEP_MS));
                // If the runtime starves this task and several sweep windows are
                // missed, do NOT fire a back-to-back burst of catch-up sweeps
                // (the default Burst behavior): a watchdog wants evenly-spaced
                // sweeps, and a burst neither shortens the detection-latency bound
                // nor helps — it just thrashes. Delay re-paces from the actual
                // wake time. Each sweep is idempotent and uses the clock for
                // staleness, so skipping missed ticks is correct.
                sweep_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut last_node_refresh_ms: u64 = 0;
                let mut node_health: HashMap<String, WatchdogNodeEntry> = HashMap::new();
                // EP-11: sustained-miss hysteresis over the sweep deadline. Loop-owned
                // (single consumer, no lock); rebuilt fresh on a supervisor restart.
                let mut sustained_misses = crate::execution_manager::SustainedMissTracker::new();
                loop {
                    sweep_interval.tick().await;

                    // WP-20 s2c — time this sweep against the `telemetry_watchdog`
                    // deadline budget (`execution_manager` manifest) for /metrics
                    // observability. Measured on the CLOCK seam (real wall-ms under
                    // `SystemClock`; deterministic under a test `VirtualClock`), and
                    // captured HERE before the per-sweep clones below shadow `app`/`clock`.
                    let deadline_app = Arc::clone(&app);
                    let deadline_clock = Arc::clone(&clock);
                    // EP-11: escalation handle captured BEFORE the per-sweep clones
                    // below shadow-and-move `posture_engine_tx` into the blocking task.
                    let escalation_tx = posture_engine_tx.clone();
                    let sweep_start_ms = deadline_clock.now_ms();

                    // The sweep is SYNCHRONOUS and does blocking work — `std::sync::Mutex`
                    // acquisition plus SQLite reads/writes. Run it OFF the async worker via
                    // `spawn_blocking` so a slow disk or a writer-held store lock cannot pin
                    // a tokio runtime thread (which would stall every other async task,
                    // including the posture-engine channel this watchdog feeds). The
                    // loop-owned sweep state (`node_health`, `last_node_refresh_ms`) is moved
                    // into the blocking task and handed back — the sweep's cross-tick memory.
                    let app = Arc::clone(&app);
                    let posture_engine_tx = posture_engine_tx.clone();
                    let posture_cache = posture_cache.clone();
                    let clock = Arc::clone(&clock);
                    let mut nh = std::mem::take(&mut node_health);
                    let mut last_refresh = last_node_refresh_ms;

                    match tokio::task::spawn_blocking(move || {
                        watchdog_sweep_once_inner(
                            &app,
                            &posture_engine_tx,
                            Some(&posture_cache),
                            clock.as_ref(),
                            &mut nh,
                            &mut last_refresh,
                        );
                        (nh, last_refresh)
                    })
                    .await
                    {
                        Ok((nh, last_refresh)) => {
                            node_health = nh;
                            last_node_refresh_ms = last_refresh;
                        }
                        // A panic inside the sweep must NOT be swallowed: the watchdog is
                        // the ASIL-D dead-man's switch, supervised as CRITICAL. Re-raise it
                        // so this task's supervisor observes the failure and escalates (the
                        // C2 fail-closed path forces LockedOut) instead of the loop silently
                        // spinning on lost sweep state.
                        Err(join_err) if join_err.is_panic() => {
                            std::panic::resume_unwind(join_err.into_panic())
                        }
                        Err(join_err) => panic!("telemetry watchdog sweep task failed: {join_err}"),
                    }

                    // Record the completed sweep's elapsed against the budget (a slow
                    // sweep past `deadline_ms` increments the miss counter exported on
                    // /metrics). Reached only on the Ok path — a panicking sweep
                    // re-raises above and the loop dies (the CRITICAL supervisor escalates).
                    let elapsed_ms = deadline_clock.now_ms().saturating_sub(sweep_start_ms);
                    let missed = deadline_app
                        .observability
                        .deadline_registry
                        .record("telemetry_watchdog", elapsed_ms);
                    // EP-11: a SUSTAINED pattern of slow sweeps (threshold misses
                    // inside the rolling window) means this dead-man's switch can no
                    // longer hold its detection-latency bound — escalate fail-closed.
                    // An isolated slow sweep (nominal jitter) never trips this.
                    escalate_on_sustained_overrun(
                        &deadline_app,
                        &escalation_tx,
                        &mut sustained_misses,
                        deadline_clock.now_ms(),
                        missed,
                    );
                }
            }
        },
    );
}

/// One sweep cycle of the watchdog: refresh the node list (rate-limited
/// by `AV_WATCHDOG_NODE_REFRESH_MS`), check each node's last-telemetry
/// timestamp against the current clock, and emit warn/timeout actions.
///
/// Extracted into a sync function (no `.await`) so it can be exercised
/// deterministically from tests without driving the tokio interval timer.
/// `spawn_telemetry_watchdog_with_clock` calls this once per
/// `sweep_interval.tick()` — production behavior is byte-for-byte the
/// same as the previous in-loop body.
///
// Verifies: SG-003 — Sensor Timeout Fault Detection. A node silent for
// ≥ AV_TELEMETRY_TIMEOUT_MS is marked Untrusted(TELEMETRY_TIMEOUT) and a
// PostureRecalcTrigger::WatchdogTimeout is sent within one sweep
// (sg_003_cert_tests + watchdog_di_tests).
#[cfg(test)]
pub(crate) fn watchdog_sweep_once(
    app: &Arc<AppState>,
    posture_engine_tx: &PostureEngineSender,
    clock: &dyn Clock,
    node_health: &mut HashMap<String, WatchdogNodeEntry>,
    last_node_refresh_ms: &mut u64,
) {
    watchdog_sweep_once_inner(
        app,
        posture_engine_tx,
        None,
        clock,
        node_health,
        last_node_refresh_ms,
    );
}

fn watchdog_sweep_once_inner(
    app: &Arc<AppState>,
    posture_engine_tx: &PostureEngineSender,
    posture_cache: Option<&SharedPostureCache>,
    clock: &dyn Clock,
    node_health: &mut HashMap<String, WatchdogNodeEntry>,
    last_node_refresh_ms: &mut u64,
) {
    let now = clock.now_ms();

    // ----------------------------------------------------------------------
    // Periodically refresh the registered node list from SQLite.
    // This picks up nodes registered after the watchdog started.
    //
    // LOCK SCOPING (S3 / #115 — fixed self-deadlock):
    // `app.store` is a non-reentrant `std::sync::Mutex`. Previously the load
    // call was the match scrutinee, which held the guard for the entire
    // Ok arm — including the `or_insert_with` closure that tries to re-lock
    // the same mutex to read each node's last-telemetry timestamp.
    // Non-reentrant + nested lock = self-deadlock. We now acquire the lock
    // only long enough to collect the node IDs into a local, drop the
    // guard, then iterate. Each `get_last_telemetry_timestamp` re-locks
    // briefly (released at the end of its own expression). Same operations
    // in the same order — only the lock scope changes.
    // ----------------------------------------------------------------------
    // H-3: a registration/deregistration sets `av_registry_dirty`, forcing a
    // refresh on the NEXT sweep instead of waiting up to AV_WATCHDOG_NODE_REFRESH_MS
    // (30 s) — otherwise a freshly-registered node is unmonitored for ~28 s, a
    // fail-OPEN window (a silent fresh sensor would never be marked Untrusted until
    // the periodic refresh finally adds it). Swap-and-clear BEFORE the load: a
    // registration that lands after this swap but before the load is still picked
    // up by this load (it reads the committed DB), and if it lands after the load
    // the flag is set again → caught on the next sweep (≤ one AV_WATCHDOG_SWEEP_MS).
    let registry_dirty = app
        .escalation
        .av_registry_dirty
        .swap(false, std::sync::atomic::Ordering::AcqRel);
    if registry_dirty
        || now.saturating_sub(*last_node_refresh_ms) >= AV_WATCHDOG_NODE_REFRESH_MS
        || *last_node_refresh_ms == 0
    {
        // SG-003 fail-CLOSED: recover a poisoned store lock rather than
        // `.unwrap()`-panicking. The watchdog is the ASIL-D dead-man's
        // switch — if a sibling task panics while holding a store lock,
        // this sweep must keep running, because a dead watchdog is
        // fail-OPEN (a silent sensor would never be marked Untrusted and
        // posture would never recalculate). `with_read` recovers a poisoned
        // lock internally. This is a READ, so it goes through the read-replica
        // path (off the writer mutex, no contention with a slow write).
        let load_result = app
            .store
            .with_read(|store| store.load_all_registered_av_node_ids());

        match load_result {
            Ok(node_ids) => {
                let live: std::collections::HashSet<String> = node_ids.into_iter().collect();
                for node_id in &live {
                    // Insert new nodes; don't overwrite existing entries
                    // (that would reset their last_seen_ms).
                    node_health.entry(node_id.clone()).or_insert_with(|| {
                        // Load initial last_seen from persistent store (read-replica).
                        let last_seen = app
                            .store
                            .with_read(|store| store.get_last_telemetry_timestamp(node_id))
                            .unwrap_or(0);
                        let monitoring_started_ms = if last_seen == 0 {
                            app.fleet
                                .nodes
                                .get(node_id)
                                .map(|n| n.registered_at_ms)
                                .unwrap_or(now)
                        } else {
                            last_seen
                        };
                        WatchdogNodeEntry {
                            node_id: node_id.clone(),
                            last_seen_ms: last_seen,
                            monitoring_started_ms,
                            warn_logged: false,
                        }
                    });
                }
                // Prune entries for nodes no longer registered (review M1): the map
                // previously only ever grew, so deregistered nodes left stale entries
                // that kept being swept (wasted per-tick store lookups) and leaked
                // memory under fleet churn. Drop anything absent from the fresh set.
                let before = node_health.len();
                node_health.retain(|id, _| live.contains(id));
                let pruned = before.saturating_sub(node_health.len());
                *last_node_refresh_ms = now;
                tracing::debug!(
                    node_count = node_health.len(),
                    pruned = pruned,
                    "Watchdog: node list refreshed from store"
                );
            }
            Err(e) => {
                // The dirty flag was already cleared by the swap above, but this
                // refresh did NOT happen — re-arm it so the NEXT sweep retries the
                // prompt refresh instead of dropping a freshly-registered node back
                // into the ~30 s periodic window (which would re-open the H-3
                // fail-open precisely while the store is unhealthy). Idempotent: a
                // periodic refresh is unaffected (it re-fires on the 30 s timer),
                // and a redundant set just triggers one extra refresh attempt.
                if registry_dirty {
                    app.escalation
                        .av_registry_dirty
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                tracing::error!(
                    error = %e,
                    "Watchdog: failed to refresh node list — using cached list, re-armed prompt refresh"
                );
            }
        }
    }

    // ----------------------------------------------------------------------
    // Sweep: check each node's last telemetry timestamp.
    // We read last_telemetry_ms from memory (WatchdogNodeEntry), not from
    // SQLite on every tick. The fault handler updates av_subsystem_meta
    // on disk; the watchdog snapshot is refreshed at node list refresh time.
    // ----------------------------------------------------------------------
    for entry in node_health.values_mut() {
        // Sync last_seen_ms from the store on each sweep. This per-node read goes
        // through the read-replica path (`with_read`) so the 100 ms sweep never
        // serializes behind a writer holding the store mutex.
        if let Ok(ts) = app
            .store
            .with_read(|store| store.get_last_telemetry_timestamp(&entry.node_id))
        {
            if ts > entry.last_seen_ms {
                // Fresh telemetry received since last sweep — reset warn flag.
                entry.last_seen_ms = ts;
                entry.warn_logged = false;
            }
        }

        // If no telemetry has ever arrived, silence is measured from the
        // registration/monitoring baseline, not skipped forever. A silent fresh
        // node gets at most one timeout window; a restarted verifier does not
        // reset the window when the node's durable registration time is present.
        let silence_start_ms = if entry.last_seen_ms == 0 {
            entry.monitoring_started_ms
        } else {
            entry.last_seen_ms
        };
        let silence_ms = now.saturating_sub(silence_start_ms);

        if silence_ms >= AV_TELEMETRY_TIMEOUT_MS {
            // Already-timed-out check avoids repeated triggers for the
            // same ongoing silence.
            let already_timed_out = app
                .fleet
                .nodes
                .get(&entry.node_id)
                .map(|n| n.status == NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string()))
                .unwrap_or(false);

            if !already_timed_out {
                tracing::error!(
                    node_id      = %entry.node_id,
                    silence_ms   = silence_ms,
                    timeout_ms   = AV_TELEMETRY_TIMEOUT_MS,
                    "Watchdog: sensor node silent beyond timeout — marking Untrusted"
                );

                // Q4: a timed-out node must earn a FULL recovery-streak window
                // again before it can be re-trusted — otherwise a node that had
                // accumulated (threshold - 1) healthy reports before going silent
                // would re-trust on its very first report after recovery, bypassing
                // the hysteresis. Disk-first (store write before the trust mutation
                // below), and `*_preserving_telemetry` so the timeout does NOT
                // fabricate a fresh `last_telemetry_ms` (no report actually arrived).
                let _ = app
                    .store
                    .with(|store| store.reset_recovery_streak_preserving_telemetry(&entry.node_id));

                match app.mark_node_untrusted(&entry.node_id, "TELEMETRY_TIMEOUT", now) {
                    Ok(true) => {
                        let trigger = PostureRecalcTrigger::WatchdogTimeout {
                            node_id: entry.node_id.clone(),
                            timeout_ms: silence_ms,
                        };
                        if let Err(e) = posture_engine_tx.try_send(trigger) {
                            handle_recalc_send_failure(
                                app,
                                posture_cache,
                                now,
                                &entry.node_id,
                                "watchdog timeout trigger",
                                e,
                            );
                        }
                    }
                    Ok(false) => {
                        tracing::error!(
                            node_id = %entry.node_id,
                            "Watchdog: timed-out node missing from AppState.fleet.nodes — \
                             enqueueing graph recalc (fail-closed)"
                        );
                        if let Err(e) =
                            posture_engine_tx.try_send(PostureRecalcTrigger::DependencyGraphChanged)
                        {
                            handle_recalc_send_failure(
                                app,
                                posture_cache,
                                now,
                                &entry.node_id,
                                "missing-node graph recalc trigger",
                                e,
                            );
                        }
                    }
                    Err(()) => {
                        tracing::error!(
                            node_id = %entry.node_id,
                            "Watchdog: failed to persist telemetry-timeout trust state — \
                             tripping supervisor lockout"
                        );
                        // A durable trust-state write failure is a genuine fault
                        // (not a transient backlog): trip the sticky supervisor
                        // lockout so recovery is an explicit human/HA action.
                        force_watchdog_lockout(
                            app,
                            posture_cache,
                            now,
                            &entry.node_id,
                            /* sticky */ true,
                            "failed to persist telemetry timeout trust state",
                        );
                        if let Err(e) =
                            posture_engine_tx.try_send(PostureRecalcTrigger::PeriodicRefresh)
                        {
                            tracing::error!(
                                error   = %e,
                                node_id = %entry.node_id,
                                "Watchdog: failed to send supervisor lockout recalc trigger"
                            );
                        }
                    }
                }
            }
        } else if silence_ms >= AV_TELEMETRY_WARN_MS && !entry.warn_logged {
            // Warn threshold crossed — log once per silence episode.
            tracing::warn!(
                node_id    = %entry.node_id,
                silence_ms = silence_ms,
                warn_ms    = AV_TELEMETRY_WARN_MS,
                timeout_ms = AV_TELEMETRY_TIMEOUT_MS,
                "Watchdog: sensor node approaching telemetry timeout"
            );
            entry.warn_logged = true;
        }
    }
}

/// Route a posture-engine trigger-send failure to a PROPORTIONATE fail-closed
/// response. The node is already durably `Untrusted` at this point; the only
/// question is how to force posture to reflect that when the recalc could not be
/// enqueued.
///
///   * `Full`   — the engine is backlogged but ALIVE. This is potentially
///     transient, so force the posture cache to `LockedOut` IMMEDIATELY
///     (fail-closed now) but do NOT trip the sticky supervisor flag — the next
///     successful recalc (periodic refresh, or the engine draining) recovers to
///     the correct posture WITHOUT a human reset.
///   * `Closed` — the posture engine receiver is gone (a dead/panicked worker).
///     That is a fatal safety-loop failure: trip the sticky supervisor lockout
///     (human/HA reset, matching the C2 escalation semantics).
fn handle_recalc_send_failure(
    app: &Arc<AppState>,
    posture_cache: Option<&SharedPostureCache>,
    now_ms: u64,
    node_id: &str,
    trigger_label: &'static str,
    err: TrySendError<PostureRecalcTrigger>,
) {
    match err {
        TrySendError::Full(_) => {
            tracing::error!(
                node_id = %node_id,
                trigger = trigger_label,
                "Watchdog: recalc trigger channel FULL — forcing immediate LockedOut \
                 (transient; auto-recovers on the next successful recalc, no sticky trip)"
            );
            force_watchdog_lockout(
                app,
                posture_cache,
                now_ms,
                node_id,
                /* sticky */ false,
                "recalc trigger channel full",
            );
        }
        TrySendError::Closed(_) => {
            tracing::error!(
                node_id = %node_id,
                trigger = trigger_label,
                "Watchdog: recalc trigger channel CLOSED — posture engine is gone; \
                 tripping sticky supervisor lockout (human/HA reset required)"
            );
            force_watchdog_lockout(
                app,
                posture_cache,
                now_ms,
                node_id,
                /* sticky */ true,
                "recalc trigger channel closed",
            );
        }
    }
}

/// Force the fleet posture to `LockedOut` from the watchdog.
///
/// Always force-writes the posture cache to `LockedOut` immediately (the
/// fail-closed safety floor). `sticky` controls whether this is ALSO a
/// human-reset condition: when `true`, the sticky `supervisor_tripped` flag is
/// set so every subsequent recalc keeps producing `LockedOut` until an explicit
/// human/HA reset; when `false`, the lockout is a transient floor that the next
/// successful recalc can lift.
fn force_watchdog_lockout(
    app: &Arc<AppState>,
    posture_cache: Option<&SharedPostureCache>,
    now_ms: u64,
    node_id: &str,
    sticky: bool,
    reason: &'static str,
) {
    if sticky {
        app.escalation
            .supervisor_tripped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    if let Some(cache) = posture_cache {
        crate::posture_engine::force_lockout(cache, now_ms);
    } else {
        tracing::warn!(
            node_id = %node_id,
            reason = reason,
            sticky = sticky,
            "Watchdog: lockout requested but no posture cache handle was available to force immediate lockout"
        );
    }
}

// ---------------------------------------------------------------------------
// EP-11 — sustained sweep-deadline overrun → C2 supervisor escalation
// ---------------------------------------------------------------------------

/// Fold one recorded sweep-deadline outcome into the sustained-miss hysteresis;
/// on a breach, run the C2 escalation: set the sticky `supervisor_tripped` flag
/// and nudge the posture engine, which reads the flag and forces the fleet to a
/// fail-closed `LockedOut` (the same path the supervisor's restart-budget
/// escalation takes — see `spawn_supervised` + `posture_engine::force_lockout`).
///
/// Rationale: the watchdog is the SG-003 dead-man's switch. A sweep that
/// PERSISTENTLY overruns its `AV_WATCHDOG_SWEEP_MS` budget is a switch whose
/// detection latency is no longer bounded — silence could go unnoticed past the
/// telemetry timeout. That is fail-OPEN drift, so a sustained overrun fails the
/// fleet CLOSED instead. Hysteresis-guarded (`SustainedMissTracker`): an
/// isolated slow sweep — scheduling jitter, a cold cache — never escalates.
pub(crate) fn escalate_on_sustained_overrun(
    app: &Arc<AppState>,
    posture_engine_tx: &PostureEngineSender,
    tracker: &mut crate::execution_manager::SustainedMissTracker,
    now_ms: u64,
    missed: bool,
) {
    if tracker.observe(now_ms, missed) {
        tracing::error!(
            threshold = crate::execution_manager::DEADLINE_SUSTAINED_MISS_THRESHOLD,
            window_ms = crate::execution_manager::DEADLINE_SUSTAINED_WINDOW_MS,
            "telemetry watchdog SUSTAINED deadline overrun — the dead-man's switch cannot \
             hold its detection-latency bound; escalating fleet to LockedOut (C2 fail-closed)"
        );
        app.escalation
            .supervisor_tripped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = posture_engine_tx.try_send(PostureRecalcTrigger::PeriodicRefresh);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod watchdog_tests {
    // These tests assert COMPILE-TIME-CONSTANT invariants between config
    // constants (e.g. TIMEOUT > WARN) — that they are constant is the point.
    #![allow(clippy::assertions_on_constants)]
    use super::*;

    #[test]
    fn test_timeout_threshold_exceeds_warn_threshold() {
        // A node must warn before it times out — never time out without warning.
        assert!(
            AV_TELEMETRY_TIMEOUT_MS > AV_TELEMETRY_WARN_MS,
            "timeout threshold must be strictly greater than warn threshold"
        );
    }

    #[test]
    fn test_sweep_interval_is_shorter_than_warn_threshold() {
        // The sweep must run frequently enough to detect warn-threshold crossings.
        // If sweep_ms >= warn_ms, the warn log could be missed entirely.
        assert!(
            AV_WATCHDOG_SWEEP_MS < AV_TELEMETRY_WARN_MS,
            "sweep interval must be shorter than warn threshold"
        );
    }

    #[test]
    fn test_sweep_interval_is_shorter_than_timeout_threshold() {
        assert!(
            AV_WATCHDOG_SWEEP_MS < AV_TELEMETRY_TIMEOUT_MS,
            "sweep interval must be shorter than timeout threshold"
        );
    }

    #[test]
    fn test_node_refresh_interval_is_longer_than_timeout() {
        // Node list refresh is infrequent — it must not reset last_seen_ms
        // of nodes that are actively timing out.
        assert!(
            AV_WATCHDOG_NODE_REFRESH_MS > AV_TELEMETRY_TIMEOUT_MS,
            "node refresh must be less frequent than timeout detection"
        );
    }

    #[test]
    fn test_silence_duration_calculation_saturates_on_underflow() {
        // saturating_sub must be used — verify behavior with edge timestamps.
        let now: u64 = 1_000;
        let last_seen: u64 = 2_000; // Future timestamp (clock skew)
        let silence = now.saturating_sub(last_seen);
        // Must not panic or wrap — must produce 0 (no silence detected)
        assert_eq!(
            silence, 0,
            "future last_seen must produce zero silence duration"
        );
    }

    #[test]
    fn test_already_timed_out_check_matches_exact_reason_string() {
        // The already_timed_out check compares the exact string "TELEMETRY_TIMEOUT".
        // Verify it matches the string used in the Untrusted variant.
        let trust = NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
        let matches = trust == NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
        assert!(
            matches,
            "trust state comparison must match exact reason string"
        );
    }

    #[test]
    fn test_non_matching_reason_does_not_suppress_timeout_trigger() {
        // A node Untrusted for a different reason (e.g. SENSOR_FAULT) must NOT
        // be suppressed by the already_timed_out check — it uses a different reason.
        let trust = NodeTrustState::Untrusted("SENSOR_FAULT".to_string());
        let is_timeout = trust == NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
        assert!(
            !is_timeout,
            "different untrusted reason must not suppress watchdog"
        );
    }
}

// ---------------------------------------------------------------------------
// DI seam tests — GAP 17 (S3 / #115)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod watchdog_di_tests {
    //! GAP 17 — telemetry-watchdog dead-man's switch tests + cold-refresh
    //! deadlock regression.
    //!
    //! The cold-refresh path used to hold `app.store.lock()` open across
    //! the match scrutinee while the `or_insert_with` closure re-locked
    //! the same non-reentrant `std::sync::Mutex` → self-deadlock. That
    //! bug was fixed by tightening the lock scope (collect node_ids
    //! under the first lock, drop the guard, then iterate). The
    //! `test_watchdog_cold_refresh_completes_without_deadlock` test in
    //! this module is the regression test for that fix — it drives the
    //! cold-refresh arm directly under a 5-second timeout so any
    //! reintroduction fails LOUDLY instead of hanging CI.
    //!
    //! The other tests (fire/no-fire/idempotency) target the sweep loop
    //! directly via `watchdog_sweep_once` to keep them fast and
    //! independent of the SQLite refresh arm.

    use super::*;
    use crate::clock::VirtualClock;
    use crate::posture_engine_v2::PostureRecalcTrigger;
    use crate::verifier::{AppState, RegisteredNode, VerifierOperationMode};
    use kirra_persistence::VerifierStore;
    use tokio::sync::mpsc;

    fn insert_trusted_node(app: &Arc<AppState>, node_id: &str, registered_at_ms: u64) {
        app.fleet.nodes.insert(
            node_id.to_string(),
            RegisteredNode {
                node_id: node_id.to_string(),
                status: NodeTrustState::Trusted,
                registered_at_ms,
                last_trust_update_ms: registered_at_ms,
                ak_public_pem: None,
                expected_pcr16_digest_hex: None,
                site: None,
                firmware_version: None,
            },
        );
    }

    fn prepopulated_node_health(
        node_id: &str,
        last_seen_ms: u64,
    ) -> HashMap<String, WatchdogNodeEntry> {
        let mut m = HashMap::new();
        m.insert(
            node_id.to_string(),
            WatchdogNodeEntry {
                node_id: node_id.to_string(),
                last_seen_ms,
                monitoring_started_ms: last_seen_ms,
                warn_logged: false,
            },
        );
        m
    }

    /// SG9 / GAP 17: telemetry watchdog dead-man's switch — the fire arm.
    ///
    /// Pre-populates `node_health` and sets `last_node_refresh_ms = now`
    /// so the refresh arm is bypassed; the test then exercises only the
    /// sweep-loop logic that the dead-man's switch rides on. Anchored at
    /// a virtual `now` past the timeout boundary; one sweep must:
    ///   (a) mutate the node to `Untrusted("TELEMETRY_TIMEOUT")`, and
    ///   (b) send a `PostureRecalcTrigger::WatchdogTimeout` on the engine
    ///       channel with `timeout_ms == observed silence`.
    #[test]
    fn test_watchdog_dead_mans_switch_fires_after_telemetry_timeout() {
        let anchor: u64 = 1_000_000;
        let now = anchor + AV_TELEMETRY_TIMEOUT_MS + 250;
        let clock = VirtualClock::starting_at(now);

        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        insert_trusted_node(&app, "lidar_front", anchor);

        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut node_health = prepopulated_node_health("lidar_front", anchor);
        // Bypass the refresh arm by claiming a recent refresh.
        let mut last_node_refresh_ms: u64 = now;

        watchdog_sweep_once(
            &app,
            &tx,
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );

        // (a) node was mutated to Untrusted("TELEMETRY_TIMEOUT").
        let status = app.fleet.nodes.get("lidar_front").unwrap().status.clone();
        assert_eq!(
            status,
            NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string()),
            "watchdog must mark a silent node Untrusted(TELEMETRY_TIMEOUT); got {status:?}"
        );

        // B1 regression: the timeout trust state must be durable before memory is
        // trusted. A verifier restart hydrates from SQLite, so DashMap-only
        // mutation would resurrect this node as Trusted.
        let persisted = app
            .store
            .with(|store| store.load_node("lidar_front"))
            .expect("load persisted node")
            .expect("watchdog timeout must persist the node record");
        assert_eq!(
            persisted.status,
            NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string()),
            "watchdog timeout must persist Untrusted(TELEMETRY_TIMEOUT) to SQLite"
        );
        assert_eq!(
            persisted.last_trust_update_ms, now,
            "persisted watchdog trust update must carry the timeout observation timestamp"
        );

        // (b) a WatchdogTimeout trigger landed on the engine channel.
        let trigger = rx
            .try_recv()
            .expect("watchdog must send PostureRecalcTrigger::WatchdogTimeout");
        match trigger {
            PostureRecalcTrigger::WatchdogTimeout {
                node_id,
                timeout_ms,
            } => {
                assert_eq!(node_id, "lidar_front");
                assert_eq!(
                    timeout_ms,
                    now - anchor,
                    "trigger.timeout_ms must report observed silence (now - last_seen)"
                );
            }
            other => panic!("expected WatchdogTimeout, got {other:?}"),
        }
    }

    /// SG-003 fail-CLOSED regression (C1): a POISONED `app.store` mutex must NOT
    /// take the watchdog down. Before the fix, `app.store.lock().unwrap()` in the
    /// sweep panicked on a poisoned lock — silently killing the dead-man's switch
    /// (fail-OPEN: a silent sensor would never be marked Untrusted). After the fix
    /// the sweep recovers the poisoned guard (`unwrap_or_else(into_inner)`) and
    /// still fires the timeout. We poison the mutex by panicking a thread while it
    /// holds the lock, then assert one sweep still marks the silent node Untrusted.
    #[test]
    fn test_watchdog_survives_poisoned_store_lock() {
        let anchor: u64 = 1_000_000;
        let now = anchor + AV_TELEMETRY_TIMEOUT_MS + 250;
        let clock = VirtualClock::starting_at(now);

        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        insert_trusted_node(&app, "lidar_front", anchor);

        // Poison app.store: a thread panics while holding the lock (inside a
        // `StoreHandle::with` closure, which is where the lock is taken now).
        {
            let app_poison = Arc::clone(&app);
            let _ = std::thread::spawn(move || {
                app_poison.store.with(|_store| {
                    panic!("intentional poison for regression test");
                });
            })
            .join(); // Err — the thread panicked; the underlying mutex is now poisoned.
                     // The handle recovers the poison internally, so a subsequent `.with`
                     // must still run (this is the property the watchdog relies on).
        }

        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut node_health = prepopulated_node_health("lidar_front", anchor);
        let mut last_node_refresh_ms: u64 = now; // bypass refresh arm

        // Must NOT panic despite the poisoned lock — the sweep re-locks
        // `app.store` at the per-node telemetry read (the old `.unwrap()` site).
        watchdog_sweep_once(
            &app,
            &tx,
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );

        // And the dead-man's switch must still fire.
        assert_eq!(
            app.fleet.nodes.get("lidar_front").unwrap().status,
            NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string()),
            "watchdog must still mark a silent node Untrusted despite a poisoned store lock"
        );
        assert!(
            rx.try_recv().is_ok(),
            "watchdog must still send a WatchdogTimeout trigger after recovering the poisoned lock"
        );
    }

    /// Companion: in the warn band (silence > WARN, < TIMEOUT) the
    /// watchdog must NOT fire — node stays Trusted and warn_logged flips.
    #[test]
    fn test_watchdog_does_not_fire_before_timeout() {
        let anchor: u64 = 5_000_000;
        let now = anchor + AV_TELEMETRY_WARN_MS + 200;
        let clock = VirtualClock::starting_at(now);

        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        insert_trusted_node(&app, "imu_main", anchor);

        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut node_health = prepopulated_node_health("imu_main", anchor);
        let mut last_node_refresh_ms: u64 = now;

        watchdog_sweep_once(
            &app,
            &tx,
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );

        assert!(
            matches!(
                app.fleet.nodes.get("imu_main").map(|n| n.status.clone()),
                Some(NodeTrustState::Trusted)
            ),
            "node must remain Trusted while in warn band (silence < TIMEOUT)"
        );
        assert!(
            rx.try_recv().is_err(),
            "no WatchdogTimeout trigger may be sent before the timeout fires"
        );
        assert!(
            node_health.get("imu_main").unwrap().warn_logged,
            "warn_logged must flip true so the WARN log fires once per silence episode"
        );
    }

    /// Idempotency: a second sweep after the timeout has already fired
    /// must NOT emit another trigger for the same ongoing silence.
    /// Exercises the `already_timed_out` short-circuit.
    #[test]
    fn test_watchdog_does_not_double_fire_on_repeated_sweeps() {
        let anchor: u64 = 9_000_000;
        let now = anchor + AV_TELEMETRY_TIMEOUT_MS + 500;
        let clock = VirtualClock::starting_at(now);

        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        insert_trusted_node(&app, "radar_left", anchor);

        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut node_health = prepopulated_node_health("radar_left", anchor);
        let mut last_node_refresh_ms: u64 = now;

        watchdog_sweep_once(
            &app,
            &tx,
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );
        assert!(rx.try_recv().is_ok(), "first sweep past timeout must fire");

        // Advance the virtual clock further past the timeout but keep
        // the node silent — already-timed-out must suppress re-fire.
        clock.advance_ms(500);
        watchdog_sweep_once(
            &app,
            &tx,
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );
        assert!(
            rx.try_recv().is_err(),
            "second sweep on the same ongoing silence must NOT re-fire"
        );
    }

    /// B6 regression: `last_seen_ms == 0` means "no telemetry report has ever
    /// arrived", not "immune to timeout forever". A registered AV node that
    /// stays silent must time out from its registration/monitoring baseline.
    #[test]
    fn test_never_reported_registered_node_times_out_from_registration_b6() {
        let anchor: u64 = 10_000_000;
        let now = anchor + AV_TELEMETRY_TIMEOUT_MS + 250;
        let clock = VirtualClock::starting_at(now);

        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        insert_trusted_node(&app, "camera_front", anchor);

        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut node_health = HashMap::new();
        node_health.insert(
            "camera_front".to_string(),
            WatchdogNodeEntry {
                node_id: "camera_front".to_string(),
                last_seen_ms: 0,
                monitoring_started_ms: anchor,
                warn_logged: false,
            },
        );
        let mut last_node_refresh_ms: u64 = now;

        watchdog_sweep_once(
            &app,
            &tx,
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );

        assert_eq!(
            app.fleet.nodes.get("camera_front").unwrap().status,
            NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string()),
            "a registered node that never reports must not be skipped forever"
        );
        match rx
            .try_recv()
            .expect("never-reported node timeout must enqueue WatchdogTimeout")
        {
            PostureRecalcTrigger::WatchdogTimeout {
                node_id,
                timeout_ms,
            } => {
                assert_eq!(node_id, "camera_front");
                assert_eq!(timeout_ms, now - anchor);
            }
            other => panic!("expected WatchdogTimeout, got {other:?}"),
        }
    }

    /// Q4 regression: a watchdog timeout must RESET the node's recovery streak
    /// (so a node that had accumulated nearly a full streak before going silent
    /// cannot re-trust on its first post-recovery report), WITHOUT fabricating a
    /// fresh `last_telemetry_ms` (no report arrived).
    #[test]
    fn test_watchdog_timeout_resets_recovery_streak_preserving_telemetry_q4() {
        let anchor: u64 = 12_000_000;
        let now = anchor + AV_TELEMETRY_TIMEOUT_MS + 250;
        let clock = VirtualClock::starting_at(now);

        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        // Register the AV row (last_telemetry_ms = anchor) and drive the recovery
        // streak up to threshold-1 = 4 (each increment re-stamps last_telemetry_ms
        // to `anchor`, the same value).
        app.store.with(|s| {
            s.register_av_subsystem_meta("lidar_front", "Perception", "LDR-1", 0.70, anchor)
                .expect("register av meta");
            for _ in 0..(crate::recovery_hysteresis::AV_RECOVERY_STREAK_THRESHOLD - 1) {
                s.increment_recovery_streak("lidar_front", anchor)
                    .expect("increment streak");
            }
        });
        assert_eq!(
            app.store
                .with(|s| s.load_recovery_streak("lidar_front").unwrap())
                .0,
            crate::recovery_hysteresis::AV_RECOVERY_STREAK_THRESHOLD - 1,
            "precondition: streak is threshold-1"
        );
        insert_trusted_node(&app, "lidar_front", anchor);

        let (tx, _rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut node_health = prepopulated_node_health("lidar_front", anchor);
        let mut last_node_refresh_ms: u64 = now;

        watchdog_sweep_once(
            &app,
            &tx,
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );

        // Node is untrusted, streak is cleared, and the telemetry timestamp is
        // PRESERVED (the timeout did not invent a fresh last-seen).
        assert_eq!(
            app.fleet.nodes.get("lidar_front").unwrap().status,
            NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string()),
        );
        let (count, start) = app
            .store
            .with(|s| s.load_recovery_streak("lidar_front").unwrap());
        assert_eq!(count, 0, "watchdog timeout must reset the recovery streak");
        assert_eq!(
            start, 0,
            "watchdog timeout must clear the streak-start stamp"
        );
        assert_eq!(
            app.store
                .with(|s| s.get_last_telemetry_timestamp("lidar_front").unwrap()),
            anchor,
            "watchdog timeout must NOT fabricate a fresh last_telemetry_ms"
        );
    }

    /// B4 regression (transient): a FULL posture-engine channel after a watchdog
    /// timeout must force the shared cache to LockedOut IMMEDIATELY (fail-closed,
    /// no stale-serve until TTL) WITHOUT tripping the sticky supervisor flag —
    /// the engine is alive, just backlogged, so the next successful recalc must
    /// be able to recover without a human reset.
    #[test]
    fn test_watchdog_recalc_channel_full_forces_transient_lockout_b4() {
        use crate::posture_cache::{CachedFleetPosture, SharedPostureCache};
        use crate::verifier::FleetPosture;
        use std::sync::atomic::Ordering;

        let anchor: u64 = 10_000_000;
        let now = anchor + AV_TELEMETRY_TIMEOUT_MS + 500;
        let clock = VirtualClock::starting_at(now);

        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        insert_trusted_node(&app, "camera_front", anchor);

        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
            CachedFleetPosture::new_with_generation(FleetPosture::Nominal, 0, now),
        )));

        // Receiver kept ALIVE (channel open) but filled to capacity → Full.
        let (tx, _rx) = mpsc::channel::<PostureRecalcTrigger>(1);
        tx.try_send(PostureRecalcTrigger::ManualTrigger {
            operator_id: "fill-channel".to_string(),
        })
        .expect("test setup: channel has capacity for one item");

        let mut node_health = prepopulated_node_health("camera_front", anchor);
        let mut last_node_refresh_ms: u64 = now;

        watchdog_sweep_once_inner(
            &app,
            &tx,
            Some(&cache),
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );

        let cached = cache.read().unwrap().as_ref().cloned().unwrap();
        assert_eq!(
            cached.posture,
            FleetPosture::LockedOut,
            "a FULL channel must force the shared cache to LockedOut immediately"
        );
        assert!(
            !app.escalation.supervisor_tripped.load(Ordering::SeqCst),
            "a transient FULL channel must NOT trip the sticky supervisor lockout (auto-recoverable)"
        );
    }

    /// B4 regression (fatal): a CLOSED posture-engine channel means the engine
    /// receiver is gone. That is a fatal safety-loop failure → force LockedOut
    /// AND trip the sticky supervisor flag (human/HA reset required).
    #[test]
    fn test_watchdog_recalc_channel_closed_trips_sticky_lockout_b4() {
        use crate::posture_cache::{CachedFleetPosture, SharedPostureCache};
        use crate::verifier::FleetPosture;
        use std::sync::atomic::Ordering;

        let anchor: u64 = 11_000_000;
        let now = anchor + AV_TELEMETRY_TIMEOUT_MS + 500;
        let clock = VirtualClock::starting_at(now);

        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        insert_trusted_node(&app, "camera_front", anchor);

        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
            CachedFleetPosture::new_with_generation(FleetPosture::Nominal, 0, now),
        )));

        // Drop the receiver → channel CLOSED (the engine is gone).
        let (tx, rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        drop(rx);

        let mut node_health = prepopulated_node_health("camera_front", anchor);
        let mut last_node_refresh_ms: u64 = now;

        watchdog_sweep_once_inner(
            &app,
            &tx,
            Some(&cache),
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );

        let cached = cache.read().unwrap().as_ref().cloned().unwrap();
        assert_eq!(
            cached.posture,
            FleetPosture::LockedOut,
            "a CLOSED channel must force the shared cache to LockedOut immediately"
        );
        assert!(
            app.escalation.supervisor_tripped.load(Ordering::SeqCst),
            "a CLOSED channel (dead engine) must trip the sticky supervisor lockout"
        );
    }

    /// SG9 / regression — cold-refresh path COMPLETES without deadlock.
    ///
    /// Drives the cold-refresh arm (`last_node_refresh_ms == 0` + at
    /// least one registered av_subsystem_meta row not yet in
    /// `node_health`) under a 5 s tokio timeout so any regression of
    /// the previous self-deadlock fails LOUDLY instead of hanging CI.
    /// Asserts the refresh completes its three observable contracts:
    ///   (1) `node_health` is populated from the registered rows,
    ///   (2) each entry's `last_seen_ms` is loaded from
    ///        `av_subsystem_meta.last_telemetry_ms`, and
    ///   (3) `*last_node_refresh_ms` is advanced to `now`.
    ///
    /// Replaces the prior `KIRRA_RUN_WATCHDOG_DEADLOCK_REPRO`-gated
    /// diagnostic. This test now runs unconditionally.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_watchdog_cold_refresh_completes_without_deadlock() {
        use std::time::Duration;

        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        app.store.with(|store| {
            store
                .register_av_subsystem_meta("lidar_front", "LIDAR", "hw-0001", 0.7, 1_000_000)
                .expect("register av subsystem");
            store
                .register_av_subsystem_meta("imu_main", "IMU", "hw-0002", 0.7, 1_000_500)
                .expect("register av subsystem");
        });
        insert_trusted_node(&app, "lidar_front", 1_000_000);
        insert_trusted_node(&app, "imu_main", 1_000_500);

        // Choose a `now` in the warn band for lidar_front but NOT yet
        // past the timeout — so no fire arm is taken; we're only
        // measuring that the refresh path itself terminates.
        let now: u64 = 1_001_500;
        let clock = VirtualClock::starting_at(now);

        let (tx, _rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut node_health: HashMap<String, WatchdogNodeEntry> = HashMap::new();
        let mut last_node_refresh_ms: u64 = 0; // <- cold refresh

        // Run on a blocking worker so a regression in the std::sync::Mutex
        // scoping deadlocks the *worker*, not the test runtime. The
        // 5-second timeout then fires and we get a clean failure instead
        // of a 60-second cargo "running for over N seconds" hang.
        let app_for_task = Arc::clone(&app);
        let clock_for_task: Arc<dyn Clock> = clock.clone();
        let handle = tokio::task::spawn_blocking(move || {
            watchdog_sweep_once(
                &app_for_task,
                &tx,
                clock_for_task.as_ref(),
                &mut node_health,
                &mut last_node_refresh_ms,
            );
            (node_health, last_node_refresh_ms)
        });

        let (node_health, observed_last_refresh) =
            tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .expect("cold-refresh path must complete within 5s (no deadlock)")
                .expect("watchdog_sweep_once must not panic");

        // (1) node_health populated from the registered rows.
        assert_eq!(
            node_health.len(),
            2,
            "cold refresh must seed node_health from av_subsystem_meta"
        );
        assert!(node_health.contains_key("lidar_front"));
        assert!(node_health.contains_key("imu_main"));

        // (2) last_seen_ms loaded from each row's last_telemetry_ms.
        assert_eq!(
            node_health.get("lidar_front").unwrap().last_seen_ms,
            1_000_000
        );
        assert_eq!(node_health.get("imu_main").unwrap().last_seen_ms, 1_000_500);

        // (3) refresh stamp advanced to `now`.
        assert_eq!(
            observed_last_refresh, now,
            "*last_node_refresh_ms must be set to clock.now_ms() after a refresh"
        );
    }

    /// M3: exercise the PRODUCTION spawned watchdog loop end to end. The loop
    /// offloads each sweep to `spawn_blocking` and hands the cross-tick state
    /// (`node_health`, `last_node_refresh_ms`) into the blocking task and back out;
    /// this test drives the real task against a VirtualClock parked past the timeout
    /// and asserts the observable end-to-end result — a registered-but-silent node
    /// marked Untrusted and a `WatchdogTimeout` trigger fired. It does not directly
    /// assert the sweep ran on a blocking thread, but a broken offload or a dropped
    /// state hand-off would fail to fire here, so it guards that production path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_spawned_watchdog_loop_fires_end_to_end() {
        use crate::posture_cache::{CachedFleetPosture, SharedPostureCache};
        use crate::verifier::FleetPosture;
        use std::time::Duration;

        let anchor: u64 = 1_000_000;
        let now = anchor + AV_TELEMETRY_TIMEOUT_MS + 500;
        let clock = VirtualClock::starting_at(now);

        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        app.store.with(|s| {
            s.register_av_subsystem_meta("lidar_front", "LIDAR", "hw-1", 0.7, anchor)
                .expect("register av meta");
        });
        insert_trusted_node(&app, "lidar_front", anchor);

        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
            CachedFleetPosture::new_with_generation(FleetPosture::Nominal, 0, now),
        )));
        let clock_dyn: Arc<dyn Clock> = clock.clone();

        spawn_telemetry_watchdog_with_clock(Arc::clone(&app), tx, cache, clock_dyn);

        // Wait (real time; sweeps run every 100 ms) for the offloaded sweep to fire.
        let trigger = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("spawned watchdog must fire within a few real sweeps");
        assert!(
            matches!(trigger, Some(PostureRecalcTrigger::WatchdogTimeout { .. })),
            "the offloaded sweep must enqueue a WatchdogTimeout trigger; got {trigger:?}"
        );
        assert_eq!(
            app.fleet.nodes.get("lidar_front").unwrap().status,
            NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string()),
            "the offloaded sweep must mark the silent node Untrusted"
        );
    }

    /// H-3: a node registered just AFTER a periodic refresh must be picked up on
    /// the next sweep (via `av_registry_dirty`), not after up to ~28 s. Drives the
    /// fail-open as a control (within the refresh window + not dirty → not yet
    /// monitored) and then proves the dirty flag forces a prompt refresh.
    #[test]
    fn test_av_registry_dirty_forces_prompt_refresh_within_window() {
        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        // Register an AV subsystem AFTER construction (the realistic case).
        app.store.with(|store| {
            store
                .register_av_subsystem_meta("lidar_front", "LIDAR", "hw-0001", 0.7, 1_000_000)
                .expect("register av subsystem");
        });

        let now: u64 = 1_000_500;
        let clock = VirtualClock::starting_at(now);
        let (tx, _rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut node_health: HashMap<String, WatchdogNodeEntry> = HashMap::new();
        // A RECENT refresh stamp: both the 30 s periodic arm and the cold-start
        // arm are NOT due, so without the dirty flag this sweep does NOT refresh.
        let mut last_node_refresh_ms: u64 = now;

        // Control — the ~28 s fail-open: within the window and not dirty, the
        // freshly-registered node is NOT yet monitored.
        watchdog_sweep_once(
            &app,
            &tx,
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );
        assert!(
            node_health.is_empty(),
            "control: within the refresh window and not dirty, a fresh node is unmonitored"
        );

        // H-3: registration sets the dirty flag → the next sweep refreshes promptly.
        app.escalation
            .av_registry_dirty
            .store(true, std::sync::atomic::Ordering::Release);
        watchdog_sweep_once(
            &app,
            &tx,
            clock.as_ref(),
            &mut node_health,
            &mut last_node_refresh_ms,
        );
        assert!(
            node_health.contains_key("lidar_front"),
            "H-3: a dirty registry must force a prompt refresh so the fresh node is monitored within one sweep"
        );
        assert!(
            !app.escalation
                .av_registry_dirty
                .load(std::sync::atomic::Ordering::Acquire),
            "the watchdog must clear av_registry_dirty after refreshing"
        );
    }
}

// ---------------------------------------------------------------------------
// CERT-003 — SG-003 RTM-named coverage (ASIL D)
//
// Verifies: SG-003 — Sensor Timeout Fault Detection. The behaviour is also
// exercised by `watchdog_di_tests` (the dead-man's switch); these tests carry
// the RTM-named function names so `docs/safety/REQUIREMENTS_TRACEABILITY.md`
// reconciles against the source tree and `grep -rn "SG-0"` returns real hits
// (closes the SG-003 zero-coverage finding in RTM_GAP_REPORT.md B1). They drive
// the deterministic `watchdog_sweep_once` (pub(crate)) under a VirtualClock —
// no real time, no spawned-task timing.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod sg_003_cert_tests {
    use super::*;
    use crate::clock::VirtualClock;
    use crate::posture_engine_v2::PostureRecalcTrigger;
    use crate::verifier::{AppState, RegisteredNode, VerifierOperationMode};
    use kirra_persistence::VerifierStore;
    use tokio::sync::mpsc;

    fn app_with_trusted_node(node_id: &str, last_seen_ms: u64) -> Arc<AppState> {
        let store = VerifierStore::new(":memory:").expect("memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        app.fleet.nodes.insert(
            node_id.to_string(),
            RegisteredNode {
                node_id: node_id.to_string(),
                status: NodeTrustState::Trusted,
                registered_at_ms: last_seen_ms,
                last_trust_update_ms: last_seen_ms,
                ak_public_pem: None,
                expected_pcr16_digest_hex: None,
                site: None,
                firmware_version: None,
            },
        );
        app
    }

    fn health(node_id: &str, last_seen_ms: u64) -> HashMap<String, WatchdogNodeEntry> {
        let mut m = HashMap::new();
        m.insert(
            node_id.to_string(),
            WatchdogNodeEntry {
                node_id: node_id.to_string(),
                last_seen_ms,
                monitoring_started_ms: last_seen_ms,
                warn_logged: false,
            },
        );
        m
    }

    /// Verifies: SG-003 — a node silent for ≥ `AV_TELEMETRY_TIMEOUT_MS` is
    /// marked `Untrusted("TELEMETRY_TIMEOUT")` on the sweep that observes it.
    #[test]
    fn test_watchdog_marks_node_untrusted_after_timeout() {
        let anchor: u64 = 1_000_000;
        // Exactly at the timeout boundary (silence == AV_TELEMETRY_TIMEOUT_MS).
        let now = anchor + AV_TELEMETRY_TIMEOUT_MS;
        let clock = VirtualClock::starting_at(now);
        let app = app_with_trusted_node("lidar_front", anchor);
        let (tx, _rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut nh = health("lidar_front", anchor);
        let mut last_refresh = now; // bypass the SQLite refresh arm

        watchdog_sweep_once(&app, &tx, clock.as_ref(), &mut nh, &mut last_refresh);

        assert_eq!(
            app.fleet.nodes.get("lidar_front").unwrap().status.clone(),
            NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string()),
            "SG-003: a node silent ≥ AV_TELEMETRY_TIMEOUT_MS must be marked Untrusted(TELEMETRY_TIMEOUT)"
        );
    }

    /// Verifies: SG-003 — detection happens within `AV_TELEMETRY_TIMEOUT_MS +
    /// AV_WATCHDOG_SWEEP_MS`: nothing fires just below the timeout, and the
    /// first sweep at/after the boundary fires.
    #[test]
    fn test_watchdog_detection_latency_within_bound() {
        let anchor: u64 = 2_000_000;
        let app = app_with_trusted_node("imu_main", anchor);
        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut nh = health("imu_main", anchor);
        // Keep a recent (non-zero) refresh stamp so the SQLite refresh arm is
        // skipped on both sweeps below.
        let mut last_refresh = anchor + AV_TELEMETRY_TIMEOUT_MS;

        // Just below the timeout → no detection.
        let below = VirtualClock::starting_at(anchor + AV_TELEMETRY_TIMEOUT_MS - 1);
        watchdog_sweep_once(&app, &tx, below.as_ref(), &mut nh, &mut last_refresh);
        assert!(
            matches!(
                app.fleet.nodes.get("imu_main").map(|n| n.status.clone()),
                Some(NodeTrustState::Trusted)
            ),
            "SG-003: must NOT detect a timeout before AV_TELEMETRY_TIMEOUT_MS of silence"
        );
        assert!(
            rx.try_recv().is_err(),
            "SG-003: no trigger may fire before the timeout"
        );

        // Within one sweep after the boundary → detected.
        let detect_now = anchor + AV_TELEMETRY_TIMEOUT_MS + AV_WATCHDOG_SWEEP_MS;
        let at = VirtualClock::starting_at(detect_now);
        watchdog_sweep_once(&app, &tx, at.as_ref(), &mut nh, &mut last_refresh);

        let detection_latency_ms = detect_now - anchor;
        assert!(
            detection_latency_ms <= AV_TELEMETRY_TIMEOUT_MS + AV_WATCHDOG_SWEEP_MS,
            "SG-003: worst-case detection latency must be ≤ TIMEOUT + one SWEEP"
        );
        assert_eq!(
            app.fleet.nodes.get("imu_main").unwrap().status.clone(),
            NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string()),
            "SG-003: node must be detected within TIMEOUT + one sweep"
        );
    }

    /// Verifies: SG-003 — a detected timeout sends a
    /// `PostureRecalcTrigger::WatchdogTimeout` on the engine channel (so the
    /// loss of a sensor drives a posture recalculation).
    #[test]
    fn test_watchdog_triggers_posture_recalculation() {
        let anchor: u64 = 3_000_000;
        let now = anchor + AV_TELEMETRY_TIMEOUT_MS + 250;
        let clock = VirtualClock::starting_at(now);
        let app = app_with_trusted_node("radar_left", anchor);
        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut nh = health("radar_left", anchor);
        let mut last_refresh = now;

        watchdog_sweep_once(&app, &tx, clock.as_ref(), &mut nh, &mut last_refresh);

        match rx
            .try_recv()
            .expect("SG-003: watchdog must send PostureRecalcTrigger::WatchdogTimeout")
        {
            PostureRecalcTrigger::WatchdogTimeout { node_id, .. } => {
                assert_eq!(
                    node_id, "radar_left",
                    "SG-003: trigger must name the silent node"
                );
            }
            other => panic!("SG-003: expected WatchdogTimeout, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// EP-11 tests — sustained sweep-overrun → C2 escalation (the rig proof)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod sustained_overrun_tests {
    use super::*;
    use crate::execution_manager::{SustainedMissTracker, DEADLINE_SUSTAINED_MISS_THRESHOLD};
    use crate::verifier::{AppState, VerifierOperationMode};
    use kirra_persistence::VerifierStore;
    use tokio::sync::mpsc;

    fn app() -> Arc<AppState> {
        let store = VerifierStore::new(":memory:").expect("memory store");
        Arc::new(AppState::new(store, VerifierOperationMode::Active))
    }

    /// THE DoD TEST: a sustained watchdog overrun — the threshold count of
    /// deadline misses inside the rolling window, exactly what the sweep loop
    /// records under persistent overload — sets the sticky `supervisor_tripped`
    /// flag AND nudges the posture engine. The posture engine's handling of the
    /// flag (recalc → forced sticky LockedOut) is pinned by its own #688 tests;
    /// this proves the deadline path REACHES that escalation seam.
    #[test]
    fn sustained_overrun_trips_the_supervisor_and_nudges_the_engine() {
        let app = app();
        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut tracker = SustainedMissTracker::new();

        // Threshold misses at sweep cadence (100 ms apart) — a wedged store /
        // saturated node, not jitter.
        for i in 0..u64::from(DEADLINE_SUSTAINED_MISS_THRESHOLD) {
            escalate_on_sustained_overrun(&app, &tx, &mut tracker, 1_000 + i * 100, true);
        }

        assert!(
            app.escalation
                .supervisor_tripped
                .load(std::sync::atomic::Ordering::SeqCst),
            "a sustained sweep overrun must set the sticky supervisor_tripped flag (C2)"
        );
        assert!(
            matches!(rx.try_recv(), Ok(PostureRecalcTrigger::PeriodicRefresh)),
            "the escalation must nudge the posture engine so the flag takes effect NOW, \
             not at the next periodic refresh"
        );
    }

    /// The control (DoD's second half): nominal jitter — isolated slow sweeps
    /// spread wider than the window — never escalates. The dead-man's switch
    /// stays armed without ever crying wolf on scheduling noise.
    #[test]
    fn nominal_jitter_does_not_escalate() {
        let app = app();
        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut tracker = SustainedMissTracker::new();

        // One slow sweep every 15 s (window is 10 s) for a long stretch, with
        // on-time sweeps between — realistic jitter, never a sustained pattern.
        let mut now = 0u64;
        for _ in 0..50 {
            now += 15_000;
            escalate_on_sustained_overrun(&app, &tx, &mut tracker, now, true);
            for _ in 0..10 {
                now += 100;
                escalate_on_sustained_overrun(&app, &tx, &mut tracker, now, false);
            }
        }

        assert!(
            !app.escalation
                .supervisor_tripped
                .load(std::sync::atomic::Ordering::SeqCst),
            "isolated slow sweeps (nominal jitter) must never trip the supervisor"
        );
        assert!(
            rx.try_recv().is_err(),
            "no engine nudge without a sustained breach"
        );
    }

    /// Below-threshold misses inside one window do not escalate — the breach
    /// needs the FULL threshold, not merely "some misses recently".
    #[test]
    fn below_threshold_misses_do_not_escalate() {
        let app = app();
        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        let mut tracker = SustainedMissTracker::new();
        for i in 0..u64::from(DEADLINE_SUSTAINED_MISS_THRESHOLD) - 1 {
            escalate_on_sustained_overrun(&app, &tx, &mut tracker, 1_000 + i * 100, true);
        }
        assert!(!app
            .escalation
            .supervisor_tripped
            .load(std::sync::atomic::Ordering::SeqCst));
        assert!(rx.try_recv().is_err());
    }
}
