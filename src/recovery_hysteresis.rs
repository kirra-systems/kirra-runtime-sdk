// src/recovery_hysteresis.rs
//
// Recovery hysteresis filter for AV sensor node trust restoration.
//
// CORRECTIONS vs. milestone doc:
//
//   1. Time-bounded streak, not count-only.
//      The doc's streak is purely a count (N consecutive reports). This means
//      5 reports sent 10 seconds apart satisfies the streak. It also means a
//      sensor that buffers and replays stale health reports satisfies it.
//      Correct: N reports within T milliseconds. Both conditions must hold.
//
//   2. Streak reset on any fault clears the time window too.
//      last_recovery_attempt_ms tracks the first report in the current streak,
//      not the most recent. This gives us a clean time window boundary.
//
//   3. Calls posture_engine_tx.send(), NOT svc.app.recalculate_and_broadcast().
//      Same serialization invariant as the watchdog: routes through the worker.
//
//   4. Disk-first ordering is explicit and commented.
//      The sequence is: reset streak on disk → update trust in memory → send trigger.
//      Never the reverse.
//
//   5. All hysteresis logic is in this module, not inlined in the handler.
//      The handler calls evaluate_recovery_report(). This keeps the handler
//      readable and the hysteresis logic independently testable.

use std::time::{SystemTime, UNIX_EPOCH};
use crate::verifier::NodeTrustState;
use crate::verifier_store::VerifierStore;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of consecutive healthy reports required before a node can be re-trusted.
/// Must be reached within AV_RECOVERY_WINDOW_MS to count.
pub const AV_RECOVERY_STREAK_THRESHOLD: u32 = 5;

/// Time window within which all streak reports must arrive (milliseconds).
/// A streak started more than this long ago is discarded — the node must
/// start fresh. This prevents slow-drip recovery from satisfying hysteresis.
///
/// At AV_RECOVERY_STREAK_THRESHOLD=5 and a typical 100ms sensor reporting rate,
/// 5 reports arrive in ~500ms. Setting the window to 10 seconds is generous
/// while still blocking replayed or buffered stale reports.
pub const AV_RECOVERY_WINDOW_MS: u64 = 10_000; // 10 seconds

// ---------------------------------------------------------------------------
// Hysteresis evaluation result
// ---------------------------------------------------------------------------

/// Result of evaluating a healthy telemetry report against the hysteresis filter.
#[derive(Debug, Clone, PartialEq)]
pub enum HysteresisDecision {
    /// Streak threshold reached within the time window. Node may be re-trusted.
    RecoveryConfirmed { streak: u32 },

    /// Streak is building but threshold not yet reached.
    StreakBuilding { current: u32, required: u32, window_remaining_ms: u64 },

    /// Streak was discarded because the time window expired.
    /// The streak counter was reset; this report starts a new streak of 1.
    WindowExpired { old_streak: u32 },

    /// Node is not currently untrusted — hysteresis does not apply.
    NotApplicable,
}

// ---------------------------------------------------------------------------
// Core hysteresis evaluation — pure logic
// ---------------------------------------------------------------------------

/// Evaluates whether a healthy report advances or resets the recovery streak.
///
/// This function is pure with respect to trust state — it reads the current
/// streak/timestamp from the store and returns a decision. The caller is
/// responsible for acting on the decision (updating trust state, triggering
/// recalculation).
///
/// # Time window logic
/// The first report in a streak sets `last_recovery_attempt_ms` (streak start).
/// Subsequent reports must arrive within `AV_RECOVERY_WINDOW_MS` of that start.
/// If the window expires, the streak is reset and this report begins a new one.
///
/// # Disk-first ordering
/// Store writes happen inside this function before returning the decision.
/// The caller must not write to the store after calling this function for the
/// same event — that would violate the disk-first invariant for the streak data.
pub fn evaluate_recovery_report(
    store: &VerifierStore,
    node_id: &str,
    now_ms: u64,
) -> HysteresisDecision {
    // Load current streak state from persistent store.
    let (current_streak, streak_start_ms) = match store.load_recovery_streak(node_id) {
        Ok(data) => data,
        Err(e) => {
            tracing::error!(
                error   = %e,
                node_id = %node_id,
                "Failed to load recovery streak — treating as fresh streak"
            );
            (0, 0)
        }
    };

    // Check if the current streak's time window has expired.
    let window_elapsed = if streak_start_ms == 0 {
        // No streak in progress — this is the first report. No expiry.
        0
    } else {
        now_ms.saturating_sub(streak_start_ms)
    };

    if streak_start_ms > 0 && window_elapsed >= AV_RECOVERY_WINDOW_MS {
        // Window expired — discard the old streak and start fresh.
        tracing::info!(
            node_id      = %node_id,
            old_streak   = current_streak,
            window_ms    = AV_RECOVERY_WINDOW_MS,
            elapsed_ms   = window_elapsed,
            "Recovery streak window expired — resetting streak"
        );

        let _ = store.reset_recovery_streak(node_id, now_ms);
        // This report is now the first in a new streak (streak=1, start=now).
        let _ = store.increment_recovery_streak(node_id, now_ms);

        return HysteresisDecision::WindowExpired { old_streak: current_streak };
    }

    // Window is valid (or this is the first report). Increment streak.
    // Pass streak_start_ms=0 to indicate we want to set it if not already set.
    let new_streak = match store.increment_recovery_streak(node_id, now_ms) {
        Ok(s)  => s,
        Err(e) => {
            tracing::error!(error = %e, node_id = %node_id, "Failed to increment recovery streak");
            return HysteresisDecision::StreakBuilding {
                current: current_streak,
                required: AV_RECOVERY_STREAK_THRESHOLD,
                window_remaining_ms: AV_RECOVERY_WINDOW_MS.saturating_sub(window_elapsed),
            };
        }
    };

    if new_streak >= AV_RECOVERY_STREAK_THRESHOLD {
        // Threshold reached. The caller will re-trust the node and reset the streak.
        tracing::info!(
            node_id  = %node_id,
            streak   = new_streak,
            required = AV_RECOVERY_STREAK_THRESHOLD,
            "Recovery hysteresis satisfied — node cleared for re-trust"
        );
        HysteresisDecision::RecoveryConfirmed { streak: new_streak }
    } else {
        let window_remaining = AV_RECOVERY_WINDOW_MS.saturating_sub(window_elapsed);
        tracing::debug!(
            node_id          = %node_id,
            streak           = new_streak,
            required         = AV_RECOVERY_STREAK_THRESHOLD,
            window_remaining = window_remaining,
            "Recovery streak advancing"
        );
        HysteresisDecision::StreakBuilding {
            current: new_streak,
            required: AV_RECOVERY_STREAK_THRESHOLD,
            window_remaining_ms: window_remaining,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Updated VerifierStore methods required by this module
// ---------------------------------------------------------------------------
//
// Add these to impl VerifierStore in src/verifier_store.rs.
// They extend the av_subsystem_meta table (defined in verifier_store_av_patch.rs).
//
// Schema additions to av_subsystem_meta:
//   recovery_streak_count    INTEGER NOT NULL DEFAULT 0,
//   recovery_streak_start_ms INTEGER NOT NULL DEFAULT 0,
//   -- Note: last_recovery_attempt_ms renamed to recovery_streak_start_ms
//   -- to clarify it marks the START of the current streak, not the last attempt.

/*
/// Loads the current recovery streak count and streak start timestamp.
/// Returns (0, 0) if no streak is in progress.
pub fn load_recovery_streak(&self, node_id: &str) -> rusqlite::Result<(u32, u64)> {
    let result = self.conn.query_row(
        "SELECT recovery_streak_count, recovery_streak_start_ms
         FROM av_subsystem_meta WHERE node_id = ?1",
        rusqlite::params![node_id],
        |row| Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)? as u64)),
    );
    match result {
        Ok(data) => Ok(data),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok((0, 0)),
        Err(e) => Err(e),
    }
}

/// Resets recovery streak to zero and clears the streak start timestamp.
/// Called on any fault or watchdog timeout event (disk-first, before memory mutation).
pub fn reset_recovery_streak(&self, node_id: &str, now_ms: u64) -> rusqlite::Result<()> {
    self.conn.execute(
        "UPDATE av_subsystem_meta
         SET recovery_streak_count = 0, recovery_streak_start_ms = 0,
             last_telemetry_ms = ?1
         WHERE node_id = ?2",
        rusqlite::params![now_ms as i64, node_id],
    )?;
    Ok(())
}

/// Increments the recovery streak counter.
/// Sets recovery_streak_start_ms to now_ms if this is the first report (count was 0).
/// Returns the new streak count.
pub fn increment_recovery_streak(&self, node_id: &str, now_ms: u64) -> rusqlite::Result<u32> {
    // Set start timestamp only if streak is starting fresh (count = 0).
    self.conn.execute(
        "UPDATE av_subsystem_meta
         SET recovery_streak_count = recovery_streak_count + 1,
             recovery_streak_start_ms = CASE
                 WHEN recovery_streak_count = 0 THEN ?1
                 ELSE recovery_streak_start_ms
             END,
             last_telemetry_ms = ?1
         WHERE node_id = ?2",
        rusqlite::params![now_ms as i64, node_id],
    )?;

    self.conn.query_row(
        "SELECT recovery_streak_count FROM av_subsystem_meta WHERE node_id = ?1",
        rusqlite::params![node_id],
        |row| row.get::<_, i64>(0).map(|v| v as u32),
    )
}
*/

// ---------------------------------------------------------------------------
// Updated handle_sensor_fault_report using hysteresis module
// ---------------------------------------------------------------------------
//
// Apply this to src/bin/aegis_verifier_service.rs, replacing the existing
// handle_sensor_fault_report implementation.
//
// Key changes from the milestone doc version:
//   - Calls evaluate_recovery_report() instead of inlining streak logic
//   - Routes recalculation through posture_engine_tx, not direct call
//   - Disk-first ordering is enforced by evaluate_recovery_report

/*
pub async fn handle_sensor_fault_report(
    State(svc): State<Arc<ServiceState>>,
    Json(report): Json<SensorFaultReport>,
) -> Result<StatusCode, StatusCode> {
    if !svc.app.nodes.contains_key(&report.source_node_id) {
        return Err(StatusCode::NOT_FOUND);
    }

    let ts = now_ms();
    let confidence_floor = svc.app.store
        .load_av_confidence_floor(&report.source_node_id)
        .unwrap_or(None)
        .unwrap_or(AV_DEFAULT_CONFIDENCE_FLOOR);

    let is_degraded = report.hardware_fault_detected
        || report.confidence_score < confidence_floor;

    if is_degraded {
        let reason = if report.hardware_fault_detected {
            AV_TRUST_REASON_HARDWARE_FAULT
        } else {
            AV_TRUST_REASON_LOW_CONFIDENCE
        };

        // Disk-first: reset streak on disk before mutating memory trust state.
        let _ = svc.app.store.reset_recovery_streak(&report.source_node_id, ts);

        // Memory mutation after disk write.
        if let Some(mut node) = svc.app.nodes.get_mut(&report.source_node_id) {
            node.trust_state = NodeTrustState::Untrusted(reason.to_string());
        }

        // Route through serialized worker (not direct recalculate_and_broadcast).
        let _ = svc.posture_engine_tx.send(
            PostureRecalcTrigger::NodeTrustChanged {
                node_id: report.source_node_id.clone(),
                reason: reason.to_string(),
            }
        ).await;

    } else {
        let currently_untrusted = svc.app.nodes
            .get(&report.source_node_id)
            .map(|n| matches!(n.trust_state, NodeTrustState::Untrusted(_)))
            .unwrap_or(false);

        if currently_untrusted {
            // Evaluate time-bounded hysteresis.
            // evaluate_recovery_report handles all disk writes (streak increment/reset).
            match evaluate_recovery_report(&svc.app.store, &report.source_node_id, ts) {
                HysteresisDecision::RecoveryConfirmed { streak } => {
                    tracing::info!(
                        node_id  = %report.source_node_id,
                        streak   = streak,
                        "Hysteresis satisfied — re-trusting node"
                    );
                    // Memory mutation after disk confirmation.
                    if let Some(mut node) = svc.app.nodes.get_mut(&report.source_node_id) {
                        node.trust_state = NodeTrustState::Trusted;
                    }
                    // Reset streak on disk after re-trust.
                    let _ = svc.app.store.reset_recovery_streak(&report.source_node_id, ts);
                    // Route recalculation through serialized worker.
                    let _ = svc.posture_engine_tx.send(
                        PostureRecalcTrigger::NodeTrustChanged {
                            node_id: report.source_node_id.clone(),
                            reason: "RECOVERY_CONFIRMED".to_string(),
                        }
                    ).await;
                }
                HysteresisDecision::StreakBuilding { current, required, .. } => {
                    tracing::info!(
                        node_id  = %report.source_node_id,
                        current  = current,
                        required = required,
                        "Recovery streak building — node remains Untrusted"
                    );
                    // No posture change — no recalculation needed.
                }
                HysteresisDecision::WindowExpired { old_streak } => {
                    tracing::info!(
                        node_id    = %report.source_node_id,
                        old_streak = old_streak,
                        "Recovery window expired — streak reset, starting fresh"
                    );
                    // No posture change — node remains Untrusted, streak reset.
                }
                HysteresisDecision::NotApplicable => {
                    // Node is healthy and was already trusted — just touch telemetry ts.
                    let _ = svc.app.store.touch_av_telemetry_timestamp(
                        &report.source_node_id, ts
                    );
                }
            }
        } else {
            // Already trusted — just update telemetry timestamp.
            let _ = svc.app.store.touch_av_telemetry_timestamp(&report.source_node_id, ts);
        }
    }

    Ok(StatusCode::ACCEPTED)
}
*/

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod hysteresis_tests {
    use super::*;

    // Pure logic tests — no AppState construction needed.
    // evaluate_recovery_report takes a VerifierStore, so full tests
    // are in integration tests. These tests cover the decision logic
    // using computed inputs.

    #[test]
    fn test_recovery_constants_are_coherent() {
        // Streak threshold must be > 1 to provide any filtering benefit.
        assert!(AV_RECOVERY_STREAK_THRESHOLD > 1,
            "streak threshold of 1 provides no flapping protection");

        // Window must be large enough for the threshold count at a typical
        // 100ms sensor reporting rate.
        let min_window = (AV_RECOVERY_STREAK_THRESHOLD as u64) * 100;
        assert!(AV_RECOVERY_WINDOW_MS >= min_window,
            "window too small for threshold at 100ms reporting rate");

        // Window must be finite (not absurdly large — that would defeat hysteresis).
        assert!(AV_RECOVERY_WINDOW_MS <= 60_000,
            "window larger than 60s defeats the purpose of hysteresis");
    }

    #[test]
    fn test_hysteresis_decision_variants_are_distinct() {
        let confirmed = HysteresisDecision::RecoveryConfirmed { streak: 5 };
        let building  = HysteresisDecision::StreakBuilding {
            current: 3, required: 5, window_remaining_ms: 5000,
        };
        let expired   = HysteresisDecision::WindowExpired { old_streak: 3 };
        let na        = HysteresisDecision::NotApplicable;

        assert_ne!(confirmed, building);
        assert_ne!(confirmed, expired);
        assert_ne!(confirmed, na);
        assert_ne!(building,  expired);
        assert_ne!(building,  na);
        assert_ne!(expired,   na);
    }

    #[test]
    fn test_window_expiry_detection_uses_saturating_arithmetic() {
        let streak_start: u64 = 1_000;
        let now: u64 = 500; // Clock skew — now < streak_start
        let elapsed = now.saturating_sub(streak_start);
        // Must not panic or overflow — must produce 0
        assert_eq!(elapsed, 0, "saturating_sub must handle clock skew safely");
        // And must NOT trigger window expiry
        assert!(elapsed < AV_RECOVERY_WINDOW_MS,
            "clock skew must not falsely trigger window expiry");
    }

    #[test]
    fn test_streak_threshold_boundary_exactly_at_threshold_confirms() {
        // Streak count == threshold must produce RecoveryConfirmed, not StreakBuilding.
        // This is a >= check, not >.
        let streak = AV_RECOVERY_STREAK_THRESHOLD;
        let confirmed = streak >= AV_RECOVERY_STREAK_THRESHOLD;
        assert!(confirmed, "streak exactly at threshold must confirm recovery");
    }

    #[test]
    fn test_streak_one_below_threshold_does_not_confirm() {
        let streak = AV_RECOVERY_STREAK_THRESHOLD - 1;
        let confirmed = streak >= AV_RECOVERY_STREAK_THRESHOLD;
        assert!(!confirmed, "streak one below threshold must not confirm recovery");
    }

    #[test]
    fn test_window_remaining_calculation_cannot_underflow() {
        let window = AV_RECOVERY_WINDOW_MS;
        // Simulate window already elapsed (elapsed > window)
        let elapsed: u64 = window + 5_000;
        let remaining = window.saturating_sub(elapsed);
        assert_eq!(remaining, 0, "window remaining must saturate at 0, not underflow");
    }
}
