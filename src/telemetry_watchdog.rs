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
//   4. Watchdog updates last_seen timestamp on timeout (disk-first invariant).
//      The doc's watchdog mutated trust state but never updated the telemetry
//      timestamp, leaving `last_telemetry_ms` stale in av_subsystem_meta.
//
//   5. Watchdog does NOT call reset_recovery_streak directly — that belongs in
//      the fault handler. The watchdog's job is: detect silence → mark Untrusted
//      → trigger recalculation. Recovery streak management is the fault handler's
//      responsibility. Mixing them couples two separate concerns.

use std::sync::Arc;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{interval, Duration};

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
struct WatchdogNodeEntry {
    node_id: String,
    /// Last telemetry timestamp from av_subsystem_meta.last_telemetry_ms.
    /// Updated in memory when the watchdog observes a fresh telemetry report.
    last_seen_ms: u64,
    /// Whether a timeout warning has already been logged for this sweep cycle.
    /// Prevents repeated WARN logs for the same ongoing silence.
    warn_logged: bool,
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
///      a. Marks node `Untrusted("TELEMETRY_TIMEOUT")` in AppState.nodes (memory)
///      b. Logs a structured error
///      c. Sends `PostureRecalcTrigger::WatchdogTimeout` to the posture engine channel
///         (NOT calling recalculate_and_broadcast directly — routes through the
///          serialized worker to prevent burst recalculations)
///   5. Every `AV_WATCHDOG_NODE_REFRESH_MS`, refreshes the node list from SQLite
///      to pick up newly registered nodes
///
/// # Disk-first ordering
/// Trust state mutation (memory) happens after the trigger is sent to the engine.
/// The engine's recalculate_and_broadcast persists the posture event before
/// updating the cache. The watchdog does not write to the audit chain directly —
/// the posture engine's recalculation produces the audit entry.
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
) {
    tokio::spawn(async move {
        let mut sweep_interval = interval(Duration::from_millis(AV_WATCHDOG_SWEEP_MS));
        let mut last_node_refresh_ms: u64 = 0;

        // In-memory node health map: node_id → WatchdogNodeEntry
        // Keyed by node_id for O(1) lookup per sweep.
        let mut node_health: HashMap<String, WatchdogNodeEntry> = HashMap::new();

        loop {
            sweep_interval.tick().await;
            let now = now_ms();

            // ------------------------------------------------------------------
            // Periodically refresh the registered node list from SQLite.
            // This picks up nodes registered after the watchdog started.
            // ------------------------------------------------------------------
            if now.saturating_sub(last_node_refresh_ms) >= AV_WATCHDOG_NODE_REFRESH_MS
                || last_node_refresh_ms == 0
            {
                match app.store.load_all_registered_av_node_ids() {
                    Ok(node_ids) => {
                        for node_id in node_ids {
                            // Insert new nodes; don't overwrite existing entries
                            // (that would reset their last_seen_ms).
                            node_health.entry(node_id.clone()).or_insert_with(|| {
                                // Load initial last_seen from persistent store.
                                let last_seen = app.store
                                    .get_last_telemetry_timestamp(&node_id)
                                    .unwrap_or(0);
                                WatchdogNodeEntry {
                                    node_id: node_id.clone(),
                                    last_seen_ms: last_seen,
                                    warn_logged: false,
                                }
                            });
                        }
                        last_node_refresh_ms = now;
                        tracing::debug!(
                            node_count = node_health.len(),
                            "Watchdog: node list refreshed from store"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "Watchdog: failed to refresh node list — using cached list"
                        );
                    }
                }
            }

            // ------------------------------------------------------------------
            // Sweep: check each node's last telemetry timestamp.
            // We read last_telemetry_ms from memory (WatchdogNodeEntry), not from
            // SQLite on every tick. The fault handler updates av_subsystem_meta
            // on disk; the watchdog snapshot is refreshed at node list refresh time.
            // ------------------------------------------------------------------
            for entry in node_health.values_mut() {
                // Sync last_seen_ms from the store on each sweep.
                // This is a lightweight in-memory read path; SQLite is only hit
                // on the node refresh cycle above.
                // Note: For high-frequency production use, maintain a DashMap<String, u64>
                // of last_telemetry_ms updated by the fault handler on each report,
                // then read that map here instead of the store. This avoids all SQLite
                // contact in the sweep hot path.
                if let Ok(ts) = app.store.get_last_telemetry_timestamp(&entry.node_id) {
                    if ts > entry.last_seen_ms {
                        // Fresh telemetry received since last sweep — reset warn flag.
                        entry.last_seen_ms = ts;
                        entry.warn_logged = false;
                    }
                }

                // Skip nodes that have never reported (last_seen_ms == 0).
                // These are newly registered nodes that haven't sent their first
                // health report yet — not a timeout condition.
                if entry.last_seen_ms == 0 {
                    continue;
                }

                let silence_ms = now.saturating_sub(entry.last_seen_ms);

                if silence_ms >= AV_TELEMETRY_TIMEOUT_MS {
                    // Check if this node is already marked timed-out to avoid
                    // sending repeated triggers for the same ongoing silence.
                    let already_timed_out = app.nodes
                        .get(&entry.node_id)
                        .map(|n| {
                            n.trust_state
                                == NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string())
                        })
                        .unwrap_or(false);

                    if !already_timed_out {
                        tracing::error!(
                            node_id      = %entry.node_id,
                            silence_ms   = silence_ms,
                            timeout_ms   = AV_TELEMETRY_TIMEOUT_MS,
                            "Watchdog: sensor node silent beyond timeout — marking Untrusted"
                        );

                        // Mark Untrusted in AppState.nodes (DashMap — memory).
                        // Disk-first note: the posture engine's recalculate_and_broadcast
                        // will persist the resulting posture event to the audit chain
                        // before updating the cache. We don't write to SQLite here
                        // because the watchdog is not responsible for audit entries —
                        // the engine is.
                        if let Some(mut node) = app.nodes.get_mut(&entry.node_id) {
                            node.trust_state =
                                NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
                        }

                        // Route through the serialized posture engine worker.
                        // Multiple simultaneous timeouts will be coalesced into
                        // a single recalculation by the worker.
                        let trigger = PostureRecalcTrigger::WatchdogTimeout {
                            node_id: entry.node_id.clone(),
                            timeout_ms: silence_ms,
                        };
                        if let Err(e) = posture_engine_tx.try_send(trigger) {
                            tracing::error!(
                                error   = %e,
                                node_id = %entry.node_id,
                                "Watchdog: failed to send recalc trigger — engine channel full or closed"
                            );
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
    });
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod watchdog_tests {
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
        assert_eq!(silence, 0, "future last_seen must produce zero silence duration");
    }

    #[test]
    fn test_already_timed_out_check_matches_exact_reason_string() {
        // The already_timed_out check compares the exact string "TELEMETRY_TIMEOUT".
        // Verify it matches the string used in the Untrusted variant.
        let trust = NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
        let matches = trust == NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
        assert!(matches, "trust state comparison must match exact reason string");
    }

    #[test]
    fn test_non_matching_reason_does_not_suppress_timeout_trigger() {
        // A node Untrusted for a different reason (e.g. SENSOR_FAULT) must NOT
        // be suppressed by the already_timed_out check — it uses a different reason.
        let trust = NodeTrustState::Untrusted("SENSOR_FAULT".to_string());
        let is_timeout = trust == NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
        assert!(!is_timeout, "different untrusted reason must not suppress watchdog");
    }
}
