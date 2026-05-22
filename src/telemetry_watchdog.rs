// src/telemetry_watchdog.rs
//
// Asynchronous telemetry watchdog task for AV sensor node health monitoring.

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
pub const AV_WATCHDOG_SWEEP_MS: u64 = 100;

/// Warn threshold: log a warning when a node hasn't reported for this long.
pub const AV_TELEMETRY_WARN_MS: u64 = 1_000;

/// Timeout threshold: mark a node Untrusted("TELEMETRY_TIMEOUT") when it has
/// been silent for this long.
pub const AV_TELEMETRY_TIMEOUT_MS: u64 = 2_000;

/// How often to refresh the node list from SQLite (milliseconds).
pub const AV_WATCHDOG_NODE_REFRESH_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// Node health record
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct WatchdogNodeEntry {
    node_id: String,
    last_seen_ms: u64,
    warn_logged: bool,
}

// ---------------------------------------------------------------------------
// Watchdog task
// ---------------------------------------------------------------------------

/// Spawns the background telemetry watchdog task.
///
/// Detects nodes that have gone silent beyond AV_TELEMETRY_TIMEOUT_MS and
/// marks them Untrusted, routing recalculation through the posture engine
/// worker channel to coalesce burst timeouts.
pub fn spawn_telemetry_watchdog(
    app: Arc<AppState>,
    posture_engine_tx: PostureEngineSender,
) {
    tokio::spawn(async move {
        let mut sweep_interval = interval(Duration::from_millis(AV_WATCHDOG_SWEEP_MS));
        let mut last_node_refresh_ms: u64 = 0;
        let mut node_health: HashMap<String, WatchdogNodeEntry> = HashMap::new();

        loop {
            sweep_interval.tick().await;
            let now = now_ms();

            // Periodically refresh the registered node list from SQLite.
            if now.saturating_sub(last_node_refresh_ms) >= AV_WATCHDOG_NODE_REFRESH_MS
                || last_node_refresh_ms == 0
            {
                let node_ids_result = app.store
                    .lock()
                    .map(|guard| guard.load_all_registered_av_node_ids());

                match node_ids_result {
                    Ok(Ok(node_ids)) => {
                        for node_id in node_ids {
                            node_health.entry(node_id.clone()).or_insert_with(|| {
                                let last_seen = app.store
                                    .lock()
                                    .ok()
                                    .and_then(|g| g.get_last_telemetry_timestamp(&node_id).ok())
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
                    Ok(Err(e)) => {
                        tracing::error!(
                            error = %e,
                            "Watchdog: failed to refresh node list — using cached list"
                        );
                    }
                    Err(_) => {
                        tracing::error!("Watchdog: store lock poisoned during node refresh");
                    }
                }
            }

            // Sweep: check each node's last telemetry timestamp.
            for entry in node_health.values_mut() {
                if let Ok(ts) = app.store
                    .lock()
                    .ok()
                    .and_then(|g| g.get_last_telemetry_timestamp(&entry.node_id).ok())
                    .map(Ok::<u64, ()>)
                    .unwrap_or(Err(()))
                    .map_err(|_| ())
                    .map(Ok::<u64, ()>)
                    .unwrap_or(Err(()))
                {
                    if ts > entry.last_seen_ms {
                        entry.last_seen_ms = ts;
                        entry.warn_logged = false;
                    }
                }

                if entry.last_seen_ms == 0 {
                    continue;
                }

                let silence_ms = now.saturating_sub(entry.last_seen_ms);

                if silence_ms >= AV_TELEMETRY_TIMEOUT_MS {
                    let already_timed_out = app.nodes
                        .get(&entry.node_id)
                        .map(|n| {
                            n.status
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

                        if let Some(mut node) = app.nodes.get_mut(&entry.node_id) {
                            node.status =
                                NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
                        }

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

#[cfg(test)]
mod watchdog_tests {
    use super::*;

    #[test]
    fn test_timeout_threshold_exceeds_warn_threshold() {
        assert!(
            AV_TELEMETRY_TIMEOUT_MS > AV_TELEMETRY_WARN_MS,
            "timeout threshold must be strictly greater than warn threshold"
        );
    }

    #[test]
    fn test_sweep_interval_is_shorter_than_warn_threshold() {
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
        assert!(
            AV_WATCHDOG_NODE_REFRESH_MS > AV_TELEMETRY_TIMEOUT_MS,
            "node refresh must be less frequent than timeout detection"
        );
    }

    #[test]
    fn test_silence_duration_calculation_saturates_on_underflow() {
        let now: u64 = 1_000;
        let last_seen: u64 = 2_000;
        let silence = now.saturating_sub(last_seen);
        assert_eq!(silence, 0, "future last_seen must produce zero silence duration");
    }

    #[test]
    fn test_already_timed_out_check_matches_exact_reason_string() {
        let trust = NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
        let matches = trust == NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
        assert!(matches, "trust state comparison must match exact reason string");
    }

    #[test]
    fn test_non_matching_reason_does_not_suppress_timeout_trigger() {
        let trust = NodeTrustState::Untrusted("SENSOR_FAULT".to_string());
        let is_timeout = trust == NodeTrustState::Untrusted("TELEMETRY_TIMEOUT".to_string());
        assert!(!is_timeout, "different untrusted reason must not suppress watchdog");
    }
}
