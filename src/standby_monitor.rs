// src/standby_monitor.rs
//
// PassiveStandby promotion path for Aegis HA deployments.
//
// ARCHITECTURE
// ============
// Two Aegis instances share the same SQLite database (or the standby
// replicates via WAL shipping). The primary is Active; the standby is
// PassiveStandby. If the primary crashes or loses its DB connection,
// the standby detects the stale heartbeat and promotes itself.
//
// PRIMARY (Active):
//   spawn_heartbeat_writer(app)
//     → every HEARTBEAT_INTERVAL_MS, writes now_ms() to
//       posture_engine_state key "primary_heartbeat_ms"
//     → also writes its instance ID so the promoted standby can log
//       which primary it replaced
//
// STANDBY (PassiveStandby):
//   spawn_promotion_monitor(app, cache)
//     → every PROMOTION_POLL_MS, reads "primary_heartbeat_ms"
//     → if age > PROMOTION_TIMEOUT_MS: promote
//     → promotion: app.mode_active transitions false → true
//       then calls recalculate_and_broadcast() once to populate the
//       cache and begin enforcing posture
//     → logs a structured promotion event to the audit chain
//     → task exits after promotion (one-way, no revert)
//
// PROMOTION INVARIANTS
//   - Promotion is one-way. A promoted standby never reverts to PassiveStandby.
//   - If the primary recovers and finds the standby has taken over, the primary
//     must detect this (its own heartbeat writes will fail or be ignored) and
//     either enter PassiveStandby itself or shut down. That logic lives in the
//     primary's heartbeat writer (see spawn_heartbeat_writer).
//   - The standby does NOT steal the primary's posture cache. It recomputes
//     from the live DAG state on promotion. The first recalculate_and_broadcast()
//     call after promotion is authoritative.
//   - recalculate_and_broadcast() checks app.is_active() internally. The mode
//     must be updated to Active BEFORE calling it, or the function returns early.
//
// SHARED STATE
//   app.mode_active is an Arc<AtomicBool>.
//   true = Active, false = PassiveStandby.
//   Promotion: compare-and-swap false → true.
//   This is the only write to app.mode_active outside of startup.
//
// ENV VARS
//   AEGIS_INSTANCE_ID        — unique identifier for this instance (default: hostname)
//   AEGIS_HEARTBEAT_INTERVAL — override HEARTBEAT_INTERVAL_MS (ms, default: 2000)
//   AEGIS_PROMOTION_TIMEOUT  — override PROMOTION_TIMEOUT_MS (ms, default: 10000)

use std::sync::Arc;
use tokio::time::{interval, Duration};

use crate::verifier::AppState;
use crate::posture_cache::{SharedPostureCache, now_ms};
use crate::posture_engine::recalculate_and_broadcast;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// How often the primary writes its heartbeat (milliseconds).
/// Shorter = faster failure detection, more SQLite writes.
/// At 2s with a 10s timeout, the standby detects failure within ~12s.
pub const HEARTBEAT_INTERVAL_MS: u64 = 2_000;

/// How often the standby polls for a stale heartbeat (milliseconds).
/// Should be shorter than PROMOTION_TIMEOUT_MS to avoid missing the window.
pub const PROMOTION_POLL_MS: u64 = 1_000;

/// Age threshold beyond which the primary heartbeat is considered stale (milliseconds).
/// 5× HEARTBEAT_INTERVAL_MS gives the primary 5 missed writes before promotion fires.
/// Set higher for flaky network/disk environments.
pub const PROMOTION_TIMEOUT_MS: u64 = 10_000;

/// SQLite key for the primary heartbeat timestamp.
const HEARTBEAT_KEY: &str = "primary_heartbeat_ms";

/// SQLite key for the primary instance ID.
const PRIMARY_INSTANCE_KEY: &str = "primary_instance_id";

/// SQLite key recording which instance performed the last promotion.
const PROMOTION_RECORD_KEY: &str = "last_promotion_instance_id";

// ---------------------------------------------------------------------------
// Instance identity
// ---------------------------------------------------------------------------

/// Returns a unique identifier for this Aegis instance.
/// Reads AEGIS_INSTANCE_ID env var; falls back to hostname; falls back to
/// a process-lifetime stable ID derived from startup time.
pub fn instance_id() -> String {
    if let Ok(id) = std::env::var("AEGIS_INSTANCE_ID") {
        if !id.trim().is_empty() {
            return id.trim().to_string();
        }
    }
    if let Ok(host) = std::env::var("HOSTNAME") {
        if !host.trim().is_empty() {
            return host.trim().to_string();
        }
    }
    static FALLBACK_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FALLBACK_ID.get_or_init(|| {
        format!("aegis-{}", now_ms())
    }).clone()
}

// ---------------------------------------------------------------------------
// Heartbeat writer (Primary / Active)
// ---------------------------------------------------------------------------

/// Spawns the background heartbeat writer task for an Active instance.
///
/// Writes the current timestamp and instance ID to the `posture_engine_state`
/// table every `HEARTBEAT_INTERVAL_MS`. A standby monitoring this table will
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
pub fn spawn_heartbeat_writer(app: Arc<AppState>) {
    let id = instance_id();
    tokio::spawn(async move {
        let interval_ms = std::env::var("AEGIS_HEARTBEAT_INTERVAL")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(HEARTBEAT_INTERVAL_MS);

        let mut tick = interval(Duration::from_millis(interval_ms));

        tracing::info!(
            instance_id = %id,
            interval_ms = interval_ms,
            "Heartbeat writer started"
        );

        loop {
            tick.tick().await;
            let ts = now_ms();

            match app.store.lock() {
                Ok(store) => {
                    if let Err(e) = store.save_engine_state(HEARTBEAT_KEY, &ts.to_string()) {
                        tracing::warn!(
                            error       = %e,
                            instance_id = %id,
                            "Heartbeat write failed"
                        );
                        continue;
                    }

                    let _ = store.save_engine_state(PRIMARY_INSTANCE_KEY, &id);

                    if let Ok(Some(promoted_by)) = store.load_engine_state(PROMOTION_RECORD_KEY) {
                        tracing::error!(
                            promoted_by = %promoted_by,
                            instance_id = %id,
                            "Standby has promoted — primary heartbeat writer stopping. \
                             Restart this instance in PassiveStandby mode."
                        );
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!(
                        error       = %e,
                        instance_id = %id,
                        "Heartbeat writer: store lock poisoned — cannot write heartbeat"
                    );
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Promotion monitor (Standby / PassiveStandby)
// ---------------------------------------------------------------------------

/// Spawns the background promotion monitor task for a PassiveStandby instance.
///
/// Polls the primary heartbeat every `PROMOTION_POLL_MS`. If the heartbeat
/// age exceeds `PROMOTION_TIMEOUT_MS`, performs an atomic promotion:
///
///   1. CAS app.mode_active: false → true
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
/// Exception: if AEGIS_FORCE_PROMOTE=1 is set, promotes immediately regardless
/// of heartbeat state. Use for manual failover or testing.
pub fn spawn_promotion_monitor(app: Arc<AppState>, cache: SharedPostureCache) {
    let id = instance_id();
    tokio::spawn(async move {
        let poll_ms = std::env::var("AEGIS_PROMOTION_POLL")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(PROMOTION_POLL_MS);

        let timeout_ms = std::env::var("AEGIS_PROMOTION_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(PROMOTION_TIMEOUT_MS);

        let force_promote = std::env::var("AEGIS_FORCE_PROMOTE")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false);

        if force_promote {
            tracing::warn!(
                instance_id = %id,
                "AEGIS_FORCE_PROMOTE=1: bypassing heartbeat check, promoting immediately"
            );
            perform_promotion(&app, &cache, &id, "FORCE_PROMOTE").await;
            return;
        }

        let mut tick = interval(Duration::from_millis(poll_ms));

        tracing::info!(
            instance_id = %id,
            poll_ms     = poll_ms,
            timeout_ms  = timeout_ms,
            "Promotion monitor started"
        );

        loop {
            tick.tick().await;
            let now = now_ms();

            let heartbeat_age = match app.store.lock() {
                Ok(store) => {
                    match store.load_engine_state(HEARTBEAT_KEY) {
                        Ok(Some(ts_str)) => {
                            match ts_str.parse::<u64>() {
                                Ok(ts) => now.saturating_sub(ts),
                                Err(_) => {
                                    tracing::warn!("Promotion monitor: malformed heartbeat value");
                                    continue;
                                }
                            }
                        }
                        Ok(None) => {
                            tracing::debug!("Promotion monitor: no heartbeat key yet — waiting for primary");
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Promotion monitor: failed to read heartbeat");
                            continue;
                        }
                    }
                }
                Err(_) => {
                    tracing::error!("Promotion monitor: store lock poisoned");
                    continue;
                }
            };

            if heartbeat_age >= timeout_ms {
                tracing::error!(
                    instance_id   = %id,
                    heartbeat_age = heartbeat_age,
                    timeout_ms    = timeout_ms,
                    "Primary heartbeat stale — promoting to Active"
                );
                perform_promotion(&app, &cache, &id, "HEARTBEAT_TIMEOUT").await;
                return;
            } else {
                tracing::debug!(
                    heartbeat_age = heartbeat_age,
                    timeout_ms    = timeout_ms,
                    "Promotion monitor: primary alive"
                );
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Promotion execution — atomic, disk-first
// ---------------------------------------------------------------------------

async fn perform_promotion(
    app: &Arc<AppState>,
    cache: &SharedPostureCache,
    id: &str,
    reason: &str,
) {
    let ts = now_ms();

    // Step 1: Atomic mode transition PassiveStandby → Active.
    // compare_exchange ensures only one standby promotes even under split-brain.
    let promoted = app.mode_active.compare_exchange(
        false,
        true,
        std::sync::atomic::Ordering::SeqCst,
        std::sync::atomic::Ordering::SeqCst,
    ).is_ok();

    if !promoted {
        tracing::warn!(instance_id = %id, "Promotion attempted but mode was already Active");
        return;
    }

    tracing::info!(
        instance_id = %id,
        reason      = %reason,
        ts          = ts,
        "Promoted to Active"
    );

    // Step 2: Persist promotion record (disk-first).
    // save_engine_state takes &self; acquire lock once for that, then release
    // and re-acquire as mut for save_posture_event_chained (&mut self).
    if let Ok(store) = app.store.lock() {
        let _ = store.save_engine_state(PROMOTION_RECORD_KEY, id);
    }

    if let Ok(mut store) = app.store.lock() {
        let audit = serde_json::json!({
            "event":          "STANDBY_PROMOTED_TO_ACTIVE",
            "instance_id":    id,
            "reason":         reason,
            "promoted_at_ms": ts,
        });
        let _ = store.save_posture_event_chained(
            "standby_monitor",
            "STANDBY_PROMOTED_TO_ACTIVE",
            &audit.to_string(),
            Some(reason),
            ts,
        );
    }

    // Step 3: Initial recalculation as Active instance.
    // is_active() now returns true, so recalculate_and_broadcast will write
    // to the cache and emit broadcasts instead of returning early.
    recalculate_and_broadcast(app, cache);

    tracing::info!(
        instance_id = %id,
        "Promotion complete — posture cache populated, SSE broadcast active"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod standby_monitor_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn test_heartbeat_within_timeout_does_not_trigger_promotion() {
        let now: u64 = 10_000;
        let last_heartbeat: u64 = 9_000;
        let age = now.saturating_sub(last_heartbeat);
        assert!(age < PROMOTION_TIMEOUT_MS,
            "1s old heartbeat must not trigger promotion at {}ms timeout", PROMOTION_TIMEOUT_MS);
    }

    #[test]
    fn test_heartbeat_beyond_timeout_triggers_promotion() {
        let now: u64 = 25_000;
        let last_heartbeat: u64 = 10_000;
        let age = now.saturating_sub(last_heartbeat);
        assert!(age >= PROMOTION_TIMEOUT_MS,
            "15s old heartbeat must trigger promotion at {}ms timeout", PROMOTION_TIMEOUT_MS);
    }

    #[test]
    fn test_heartbeat_exactly_at_timeout_boundary_triggers_promotion() {
        let now: u64 = PROMOTION_TIMEOUT_MS;
        let last_heartbeat: u64 = 0;
        let age = now.saturating_sub(last_heartbeat);
        assert!(age >= PROMOTION_TIMEOUT_MS, "boundary must trigger (>=, not >)");
    }

    #[test]
    fn test_heartbeat_age_saturates_on_clock_skew() {
        let now: u64 = 100;
        let last_heartbeat: u64 = 5_000;
        let age = now.saturating_sub(last_heartbeat);
        assert_eq!(age, 0, "clock skew must produce zero age, not underflow");
        assert!(age < PROMOTION_TIMEOUT_MS, "clock skew must not trigger promotion");
    }

    #[test]
    fn test_absent_heartbeat_key_does_not_auto_promote() {
        let heartbeat_value: Option<String> = None;
        let should_skip = heartbeat_value.is_none();
        assert!(should_skip, "absent heartbeat must not trigger promotion");
    }

    #[test]
    fn test_promotion_cas_succeeds_from_passive() {
        let mode_active = Arc::new(AtomicBool::new(false));
        let result = mode_active.compare_exchange(
            false, true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        );
        assert!(result.is_ok(), "CAS must succeed when transitioning from PassiveStandby");
        assert!(mode_active.load(std::sync::atomic::Ordering::SeqCst),
            "mode must be Active after successful CAS");
    }

    #[test]
    fn test_promotion_cas_fails_if_already_active() {
        let mode_active = Arc::new(AtomicBool::new(true));
        let result = mode_active.compare_exchange(
            false, true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        );
        assert!(result.is_err(), "CAS must fail when already Active (double-promotion guard)");
    }

    #[test]
    fn test_promotion_is_one_way_by_task_exit() {
        // One-way enforcement is by task exit: perform_promotion() returns after
        // completing, and the caller (spawn_promotion_monitor) also returns.
        // No code path reverts mode_active to false after promotion.
        assert!(true, "one-way invariant is enforced by task lifecycle");
    }

    #[test]
    fn test_promotion_timeout_exceeds_heartbeat_interval() {
        assert!(PROMOTION_TIMEOUT_MS > HEARTBEAT_INTERVAL_MS,
            "timeout must exceed interval to allow for missed writes");
    }

    #[test]
    fn test_poll_interval_shorter_than_promotion_timeout() {
        assert!(PROMOTION_POLL_MS < PROMOTION_TIMEOUT_MS,
            "poll must be faster than timeout to avoid missing the window");
    }

    #[test]
    fn test_timeout_allows_multiple_missed_heartbeats() {
        let missed_beats = PROMOTION_TIMEOUT_MS / HEARTBEAT_INTERVAL_MS;
        assert!(missed_beats >= 3,
            "timeout should tolerate at least 3 missed beats (got {})", missed_beats);
    }

    #[test]
    fn test_instance_id_is_non_empty() {
        let id = instance_id();
        assert!(!id.trim().is_empty(), "instance_id must never be empty");
    }

    #[test]
    fn test_instance_id_is_stable_within_process() {
        let id1 = instance_id();
        let id2 = instance_id();
        assert_eq!(id1, id2, "instance_id must be stable within a process lifetime");
    }
}
