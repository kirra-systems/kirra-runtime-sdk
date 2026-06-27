// src/posture_engine_v2.rs
//
// Patches and extensions to posture_engine.rs for v2.3.0:
//
//   1. LockoutReason — structured stale/failure reason codes
//   2. Generation persistence — survive restarts with monotonic ordering
//   3. PostureEngineTask — serialized recalculation via mpsc coalescing
//
// Apply these changes to posture_engine.rs and verifier_store.rs as directed
// by the section headers below. Each section is self-contained and can be
// applied independently, though the recommended order is 1 → 2 → 3.

// ============================================================================
// SECTION 1: LockoutReason — structured stale/failure reason codes
// ============================================================================

use std::fmt;

/// Structured reason code for any fail-closed LockedOut condition.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LockoutReason {
    /// Gray/black DAG traversal produced LockedOut (cycle or depth exceeded).
    DagLockedOut,
    /// Posture cache entry has aged beyond POSTURE_CACHE_TTL_MS.
    PostureCacheStale,
    /// Posture cache contains None (cold start or operator reset).
    PostureCacheEmpty,
    /// Posture cache RwLock was poisoned. Requires process restart.
    PostureCachePoisoned,
    /// Posture engine failed to complete a recalculation cycle.
    PostureEngineFailure,
    /// Watchdog task determined a node's telemetry has timed out.
    WatchdogTimeout,
    /// An operator or administrative action explicitly locked out the fleet.
    ManualLockout,
    /// S-FI1d — frame/localization integrity was `Untrusted` for a sustained
    /// run (sensor failure / possible GNSS spoofing). Sticky human-reset lockout.
    FrameIntegrityUntrusted,
}

impl fmt::Display for LockoutReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::DagLockedOut          => "DAG_LOCKED_OUT",
            Self::PostureCacheStale     => "POSTURE_CACHE_STALE",
            Self::PostureCacheEmpty     => "POSTURE_CACHE_EMPTY",
            Self::PostureCachePoisoned  => "POSTURE_CACHE_POISONED",
            Self::PostureEngineFailure  => "POSTURE_ENGINE_FAILURE",
            Self::WatchdogTimeout       => "WATCHDOG_TIMEOUT",
            Self::ManualLockout         => "MANUAL_LOCKOUT",
            Self::FrameIntegrityUntrusted => "FRAME_INTEGRITY_UNTRUSTED",
        };
        write!(f, "{code}")
    }
}

use std::time::{SystemTime, UNIX_EPOCH};
use crate::posture_cache::SharedPostureCache;
use crate::verifier::FleetPosture;

pub fn now_ms_engine() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn resolve_posture_with_reason(
    cache: &SharedPostureCache,
    posture_cache_ttl_ms: u64,
) -> (FleetPosture, Option<LockoutReason>) {
    let ts = now_ms_engine();

    match cache.read() {
        Ok(guard) => match guard.as_ref() {
            Some(cached) => {
                let age_ms = ts.saturating_sub(cached.generated_at_ms);
                if age_ms >= posture_cache_ttl_ms {
                    tracing::warn!(
                        reason       = %LockoutReason::PostureCacheStale,
                        age_ms       = age_ms,
                        ttl_ms       = posture_cache_ttl_ms,
                        generation   = cached.generation,
                        last_posture = ?cached.posture,
                        "Posture cache stale — failing closed"
                    );
                    (FleetPosture::LockedOut, Some(LockoutReason::PostureCacheStale))
                } else {
                    (cached.posture, None)
                }
            }
            None => {
                tracing::warn!(
                    reason = %LockoutReason::PostureCacheEmpty,
                    "Posture cache empty (cold start or reset) — failing closed"
                );
                (FleetPosture::LockedOut, Some(LockoutReason::PostureCacheEmpty))
            }
        },
        Err(_) => {
            tracing::error!(
                reason = %LockoutReason::PostureCachePoisoned,
                "Posture cache RwLock poisoned — failing closed"
            );
            (FleetPosture::LockedOut, Some(LockoutReason::PostureCachePoisoned))
        }
    }
}

/// Post-promotion posture-freshness gate (#83).
///
/// A standby that has just promoted to Active MUST hold a non-stale posture
/// before it serves commands. This is a thin, named application of
/// [`resolve_posture_with_reason`] at `POSTURE_CACHE_TTL_MS` — the SINGLE TTL
/// authority — so the staleness window is never duplicated. `perform_promotion`
/// calls it AFTER its initial recalculation: a fresh cache resolves to the real
/// posture (the node serves normally); a still-stale / empty / poisoned cache
/// resolves to `LockedOut(<reason>)` and the node fails closed (the mutation
/// gate independently blocks on the same stale cache, so no command is served
/// against a stale posture). It does NOT change `resolve_posture_with_reason`
/// semantics — it only fixes the TTL to the production constant.
pub fn resolve_post_promotion_posture(
    cache: &SharedPostureCache,
) -> (FleetPosture, Option<LockoutReason>) {
    resolve_posture_with_reason(cache, crate::posture_cache::POSTURE_CACHE_TTL_MS)
}

// ============================================================================
// SECTION 2: Generation persistence across restarts
// ============================================================================

/*
pub fn load_last_generation(&self) -> rusqlite::Result<u64> {
    let result = self.conn.query_row(
        "SELECT value FROM posture_engine_state WHERE key = 'last_generation'",
        [],
        |row| row.get::<_, String>(0),
    );
    match result {
        Ok(s)  => Ok(s.parse::<u64>().unwrap_or(0)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(e) => Err(e),
    }
}

pub fn save_last_generation(&self, generation: u64) -> rusqlite::Result<()> {
    self.conn.execute(
        "INSERT OR REPLACE INTO posture_engine_state (key, value)
         VALUES ('last_generation', ?1)",
        rusqlite::params![generation.to_string()],
    )?;
    Ok(())
}
*/

// ============================================================================
// SECTION 3: PostureEngineTask — serialized recalculation with coalescing
// ============================================================================

use tokio::sync::mpsc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use crate::verifier::AppState;
use parko_core::RssState;
use kirra_core::frame_integrity::FrameTrust;
use crate::recovery_hysteresis::{AV_RECOVERY_STREAK_THRESHOLD, AV_RECOVERY_WINDOW_MS};

/// Trigger reason sent to the posture engine worker.
#[derive(Debug, Clone)]
pub enum PostureRecalcTrigger {
    /// A node's trust state was changed (fault or recovery).
    NodeTrustChanged { node_id: String, reason: String },
    /// A watchdog detected telemetry timeout on a node.
    WatchdogTimeout { node_id: String, timeout_ms: u64 },
    /// An operator action requires immediate re-evaluation.
    ManualTrigger { operator_id: String },
    /// A dependency graph edge was added or removed.
    DependencyGraphChanged,
    /// An RSS safe-distance evaluation result (violation or recovery tick).
    /// safe==false activates the violation flag; safe==true advances recovery streak.
    RssViolation(RssState),
    /// S-FI1d — a frame/localization-integrity verdict for this tick.
    /// `Degraded`/`Untrusted` activate the frame-degraded flag (→ Degraded);
    /// sustained `Untrusted` escalates to LockedOut; `Trusted` advances recovery.
    FrameIntegrityChanged { trust: FrameTrust },
    /// Periodic liveness refresh — recompute and re-stamp the cache so it
    /// never idles past POSTURE_CACHE_TTL_MS. Not tied to any state change.
    /// A no-change recompute produces no transition (no broadcast) but DOES
    /// re-stamp `generated_at_ms` under a new generation, which is the
    /// freshness signal `should_route_command` depends on.
    PeriodicRefresh,
}

impl fmt::Display for PostureRecalcTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NodeTrustChanged { node_id, reason } =>
                write!(f, "NodeTrustChanged({node_id}, {reason})"),
            Self::WatchdogTimeout { node_id, timeout_ms } =>
                write!(f, "WatchdogTimeout({node_id}, {timeout_ms}ms)"),
            Self::ManualTrigger { operator_id } =>
                write!(f, "ManualTrigger({operator_id})"),
            Self::DependencyGraphChanged =>
                write!(f, "DependencyGraphChanged"),
            Self::RssViolation(rss) =>
                write!(f, "RssViolation(safe={}, lon={:.2}, lat={:.2})",
                    rss.safe, rss.longitudinal_margin, rss.lateral_margin),
            Self::FrameIntegrityChanged { trust } =>
                write!(f, "FrameIntegrityChanged({trust:?})"),
            Self::PeriodicRefresh =>
                write!(f, "PeriodicRefresh"),
        }
    }
}

/// Channel sender for posture recalculation triggers.
pub type PostureEngineSender = mpsc::Sender<PostureRecalcTrigger>;

/// Applies an RSS state report to `AppState` violation/recovery fields.
///
/// Violation (safe==false): sets `rss_active_violation` and resets the streak.
/// Safe tick (safe==true) while violation is active: advances the streak.
/// Recovery is confirmed when the streak reaches `AV_RECOVERY_STREAK_THRESHOLD`
/// within `AV_RECOVERY_WINDOW_MS`; the violation flag is cleared on confirmation.
// SAFETY: SG1 | REQ: rss-violation-escalates-posture | TEST: test_rss_violation_degrades_nominal_posture,test_rss_recovery_requires_full_streak,test_rss_posture_lifecycle_violation_to_recovery
// (Occy SG1 enforcement coupling: when parko RSS reports `safe==false`,
//  this escalates fleet posture to Degraded via the streak/window
//  hysteresis defined in recovery_hysteresis.)
pub fn apply_rss_state(app: &Arc<AppState>, rss: &RssState, now_ms: u64) {
    if !rss.safe {
        app.rss_active_violation.store(true, Ordering::SeqCst);
        if let Ok(mut streak) = app.rss_recovery_streak.lock() {
            streak.count = 0;
            streak.start_ms = 0;
        }
        tracing::info!(
            lon_margin = rss.longitudinal_margin,
            lat_margin = rss.lateral_margin,
            "RSS violation active — fleet posture will be escalated to Degraded"
        );
    } else if app.rss_active_violation.load(Ordering::SeqCst) {
        if let Ok(mut streak) = app.rss_recovery_streak.lock() {
            // Window expiry: discard streak and start fresh from this tick.
            if streak.start_ms > 0
                && now_ms.saturating_sub(streak.start_ms) >= AV_RECOVERY_WINDOW_MS
            {
                streak.count = 0;
                streak.start_ms = 0;
            }
            if streak.start_ms == 0 {
                streak.start_ms = now_ms;
            }
            streak.count += 1;
            if streak.count >= AV_RECOVERY_STREAK_THRESHOLD {
                app.rss_active_violation.store(false, Ordering::SeqCst);
                streak.count = 0;
                streak.start_ms = 0;
                tracing::info!(
                    threshold = AV_RECOVERY_STREAK_THRESHOLD,
                    "RSS recovery confirmed — violation cleared"
                );
            } else {
                tracing::debug!(
                    count    = streak.count,
                    required = AV_RECOVERY_STREAK_THRESHOLD,
                    "RSS recovery streak advancing"
                );
            }
        }
    }
}

/// Applies a frame/localization-integrity verdict to `AppState` (S-FI1d).
///
/// Posture mapping (confirmed; see `docs/safety/STAGE_S-FI1_FRAME_INTEGRITY_GATE.md`):
///   - `Trusted`   → advance the recovery streak; clear `frame_degraded_active`
///     once `AV_RECOVERY_STREAK_THRESHOLD` consecutive trusted ticks land within
///     `AV_RECOVERY_WINDOW_MS`. Resets the untrusted-escalation streak.
///   - `Degraded`  → set `frame_degraded_active` IMMEDIATELY (→ Degraded). NOT a
///     sustained-fault signal, so the untrusted streak resets.
///   - `Untrusted` → set `frame_degraded_active` IMMEDIATELY (→ Degraded MRC, the
///     frame-trust-minimal maneuver); advance the inverted untrusted streak and,
///     on reaching the threshold within the window, set the STICKY
///     `frame_lockout_active` (→ LockedOut, human reset).
///
/// Fail-closed-immediately: the drop to Degraded happens on the FIRST sub-trusted
/// tick — hysteresis governs only the Degraded→LockedOut escalation and the
/// recovery earn-back, never a grace period on the initial response.
// SAFETY: SG2 | REQ: frame-integrity-escalates-posture | TEST: test_frame_degraded_escalates_immediately,test_frame_untrusted_sustained_locks_out,test_frame_trusted_recovery_clears_degraded,test_frame_lockout_is_sticky
pub fn apply_frame_integrity_state(app: &Arc<AppState>, trust: FrameTrust, now_ms: u64) {
    match trust {
        FrameTrust::Trusted => {
            // Recovery: only meaningful while a frame degradation is active.
            if app.frame_degraded_active.load(Ordering::SeqCst) {
                if let Ok(mut streak) = app.frame_recovery_streak.lock() {
                    if streak.start_ms > 0
                        && now_ms.saturating_sub(streak.start_ms) >= AV_RECOVERY_WINDOW_MS
                    {
                        streak.count = 0;
                        streak.start_ms = 0;
                    }
                    if streak.start_ms == 0 {
                        streak.start_ms = now_ms;
                    }
                    streak.count += 1;
                    if streak.count >= AV_RECOVERY_STREAK_THRESHOLD {
                        app.frame_degraded_active.store(false, Ordering::SeqCst);
                        streak.count = 0;
                        streak.start_ms = 0;
                        tracing::info!(
                            threshold = AV_RECOVERY_STREAK_THRESHOLD,
                            "Frame-integrity recovery confirmed — frame degradation cleared"
                        );
                    }
                }
            }
            // A trusted tick breaks any sustained-untrusted run.
            if let Ok(mut s) = app.frame_untrusted_streak.lock() {
                s.count = 0;
                s.start_ms = 0;
            }
            // NOTE: `frame_lockout_active` is sticky (human reset) and is NOT
            // cleared here — matching LockedOut semantics.
        }
        FrameTrust::Degraded => {
            // Immediate Degraded; reset both streaks (not a recovery, not a
            // sustained-untrusted tick).
            app.frame_degraded_active.store(true, Ordering::SeqCst);
            if let Ok(mut s) = app.frame_recovery_streak.lock() {
                s.count = 0;
                s.start_ms = 0;
            }
            if let Ok(mut s) = app.frame_untrusted_streak.lock() {
                s.count = 0;
                s.start_ms = 0;
            }
        }
        FrameTrust::Untrusted => {
            // Immediate Degraded MRC; a trusted recovery must restart from scratch.
            app.frame_degraded_active.store(true, Ordering::SeqCst);
            if let Ok(mut s) = app.frame_recovery_streak.lock() {
                s.count = 0;
                s.start_ms = 0;
            }
            // Inverted streak toward the sticky LockedOut escalation.
            if let Ok(mut streak) = app.frame_untrusted_streak.lock() {
                if streak.start_ms > 0
                    && now_ms.saturating_sub(streak.start_ms) >= AV_RECOVERY_WINDOW_MS
                {
                    streak.count = 0;
                    streak.start_ms = 0;
                }
                if streak.start_ms == 0 {
                    streak.start_ms = now_ms;
                }
                streak.count += 1;
                if streak.count >= AV_RECOVERY_STREAK_THRESHOLD {
                    app.frame_lockout_active.store(true, Ordering::SeqCst);
                    tracing::error!(
                        reason = %LockoutReason::FrameIntegrityUntrusted,
                        streak = streak.count,
                        "Sustained Untrusted frame integrity — escalating fleet to LockedOut (human reset)"
                    );
                }
            }
        }
    }
}

/// Starts the posture engine worker task.
pub fn start_posture_engine_worker(
    app: Arc<AppState>,
    cache: SharedPostureCache,
) -> PostureEngineSender {
    let (tx, rx) = mpsc::channel::<PostureRecalcTrigger>(128);
    // C2: the worker is supervised, so its receiver must survive a re-spawn. The
    // `mpsc::Receiver` is not `Clone`, so it lives behind an `Arc<Mutex<…>>` that
    // each (re)started future re-locks. Only ever one task holds the lock at a time.
    let rx = Arc::new(tokio::sync::Mutex::new(rx));

    // C2 escalation: a wedged posture worker means recalculation stops. That is
    // ALREADY fail-closed after `POSTURE_CACHE_TTL_MS` (the cache goes stale and the
    // gate denies), but we make it immediate and sticky: set the flag and force the
    // cache to LockedOut directly (the worker is the dead task, so we cannot rely on
    // a recalc to apply the flag).
    let escalate: crate::supervisor::Escalation = {
        let app = Arc::clone(&app);
        let cache = cache.clone();
        Arc::new(move || {
            app.supervisor_tripped
                .store(true, std::sync::atomic::Ordering::SeqCst);
            crate::posture_engine::force_lockout(&cache, now_ms_engine());
        })
    };

    crate::supervisor::spawn_supervised(
        "posture_engine_worker",
        /* critical   */ true,
        /* run-forever */ false, // a closed trigger channel is a legitimate shutdown exit
        Some(escalate),
        move || {
            let app = Arc::clone(&app);
            let cache = cache.clone();
            let rx = Arc::clone(&rx);
            async move {
                // B7: the supervisor (`spawn_supervised`) awaits the prior task
                // future to completion (`inner.await`) BEFORE re-invoking this
                // closure, so any earlier guard is always dropped before we reach
                // here — `try_lock` therefore succeeds on every legitimate
                // (re)start. A CONTENDED lock would mean two worker futures are
                // alive at once (a supervisor-contract violation); do NOT
                // `lock().await`, which would hang this worker forever and
                // silently stall recalculation. Fail closed instead: log and
                // return. The supervisor treats the return as a terminal state and
                // stops; the posture cache then goes stale and every gate denies
                // (POSTURE_CACHE_TTL_MS). This makes the single-owner invariant
                // explicit and fail-fast rather than an unstated drop-ordering
                // coupling that deadlocks if it is ever broken.
                let mut rx = match rx.try_lock() {
                    Ok(guard) => guard,
                    Err(_) => {
                        tracing::error!(
                            "posture engine worker: trigger receiver already held — \
                             concurrent worker (supervisor invariant violation); exiting \
                             fail-closed (cache will stale to LockedOut)"
                        );
                        return;
                    }
                };
                loop {
                    let first = match rx.recv().await {
                        Some(t) => t,
                        None => {
                            tracing::info!("Posture engine worker: trigger channel closed, exiting");
                            break;
                        }
                    };

                    let mut batch: Vec<PostureRecalcTrigger> = vec![first];
                    while let Ok(trigger) = rx.try_recv() {
                        batch.push(trigger);
                    }

                    let batch_size = batch.len();
                    let trigger_summary: Vec<String> =
                        batch.iter().map(|t| t.to_string()).collect();

                    if batch_size > 1 {
                        tracing::debug!(
                            batch_size = batch_size,
                            triggers   = ?trigger_summary,
                            "Posture engine: coalescing {batch_size} triggers into single recalculation"
                        );
                    }

                    let now = now_ms_engine();
                    for trigger in &batch {
                        match *trigger {
                            PostureRecalcTrigger::RssViolation(ref rss) => {
                                apply_rss_state(&app, rss, now);
                            }
                            PostureRecalcTrigger::FrameIntegrityChanged { trust } => {
                                apply_frame_integrity_state(&app, trust, now);
                            }
                            _ => {}
                        }
                    }

                    recalculate_and_broadcast_with_context(&app, &cache, &trigger_summary);
                }
            }
        },
    );

    tx
}

fn recalculate_and_broadcast_with_context(
    app: &Arc<AppState>,
    cache: &SharedPostureCache,
    triggers: &[String],
) {
    tracing::debug!(triggers = ?triggers, "Posture engine recalculation triggered");
    crate::posture_engine::recalculate_and_broadcast(app, cache);
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod posture_engine_v2_tests {
    use super::*;

    #[test]
    fn test_lockout_reason_display_strings_are_stable() {
        assert_eq!(LockoutReason::DagLockedOut.to_string(),         "DAG_LOCKED_OUT");
        assert_eq!(LockoutReason::PostureCacheStale.to_string(),    "POSTURE_CACHE_STALE");
        assert_eq!(LockoutReason::PostureCacheEmpty.to_string(),    "POSTURE_CACHE_EMPTY");
        assert_eq!(LockoutReason::PostureCachePoisoned.to_string(), "POSTURE_CACHE_POISONED");
        assert_eq!(LockoutReason::PostureEngineFailure.to_string(), "POSTURE_ENGINE_FAILURE");
        assert_eq!(LockoutReason::WatchdogTimeout.to_string(),      "WATCHDOG_TIMEOUT");
        assert_eq!(LockoutReason::ManualLockout.to_string(),        "MANUAL_LOCKOUT");
        assert_eq!(LockoutReason::FrameIntegrityUntrusted.to_string(), "FRAME_INTEGRITY_UNTRUSTED");
    }

    // --- S-FI1d: frame-integrity posture coupling ---------------------------

    fn frame_app() -> Arc<AppState> {
        use crate::verifier::VerifierOperationMode;
        use crate::verifier_store::VerifierStore;
        let store = VerifierStore::new(":memory:").unwrap();
        Arc::new(AppState::new(store, VerifierOperationMode::Active))
    }

    #[test]
    fn test_frame_degraded_escalates_immediately() {
        let app = frame_app();
        // A SINGLE Degraded tick sets the flag — no grace period.
        apply_frame_integrity_state(&app, FrameTrust::Degraded, 1_000);
        assert!(app.frame_degraded_active.load(Ordering::SeqCst));
        assert!(!app.frame_lockout_active.load(Ordering::SeqCst),
            "Degraded must not by itself lock out");
    }

    #[test]
    fn test_frame_untrusted_escalates_immediately_then_locks_out_when_sustained() {
        let app = frame_app();
        // First Untrusted tick → immediate Degraded, not yet LockedOut.
        apply_frame_integrity_state(&app, FrameTrust::Untrusted, 1_000);
        assert!(app.frame_degraded_active.load(Ordering::SeqCst));
        assert!(!app.frame_lockout_active.load(Ordering::SeqCst),
            "a single Untrusted tick is the transient decel-to-stop MRC, not LockedOut");
        // Sustained Untrusted within the window → sticky LockedOut.
        for i in 1..AV_RECOVERY_STREAK_THRESHOLD {
            apply_frame_integrity_state(&app, FrameTrust::Untrusted, 1_000 + i as u64 * 10);
        }
        assert!(app.frame_lockout_active.load(Ordering::SeqCst),
            "sustained Untrusted ({AV_RECOVERY_STREAK_THRESHOLD} ticks) must escalate to LockedOut");
    }

    #[test]
    fn test_frame_trusted_recovery_clears_degraded() {
        let app = frame_app();
        apply_frame_integrity_state(&app, FrameTrust::Degraded, 1_000);
        assert!(app.frame_degraded_active.load(Ordering::SeqCst));
        // A full streak of Trusted ticks within the window clears the degradation.
        for i in 0..AV_RECOVERY_STREAK_THRESHOLD {
            apply_frame_integrity_state(&app, FrameTrust::Trusted, 1_010 + i as u64 * 10);
        }
        assert!(!app.frame_degraded_active.load(Ordering::SeqCst),
            "a full Trusted recovery streak must clear frame_degraded_active");
    }

    #[test]
    fn test_frame_lockout_is_sticky_across_trusted() {
        let app = frame_app();
        for i in 0..AV_RECOVERY_STREAK_THRESHOLD {
            apply_frame_integrity_state(&app, FrameTrust::Untrusted, 1_000 + i as u64 * 10);
        }
        assert!(app.frame_lockout_active.load(Ordering::SeqCst), "precondition: locked out");
        // Trusted recovery does NOT clear a sustained-fault lockout (human reset only).
        for i in 0..AV_RECOVERY_STREAK_THRESHOLD * 2 {
            apply_frame_integrity_state(&app, FrameTrust::Trusted, 2_000 + i as u64 * 10);
        }
        assert!(app.frame_lockout_active.load(Ordering::SeqCst),
            "frame_lockout_active must be sticky — only a human/HA reset clears it");
    }

    #[test]
    fn test_frame_integrity_trigger_display() {
        let t = PostureRecalcTrigger::FrameIntegrityChanged { trust: FrameTrust::Untrusted };
        let s = t.to_string();
        assert!(s.contains("FrameIntegrityChanged"));
        assert!(s.contains("Untrusted"));
    }

    #[test]
    fn test_lockout_reason_is_serializable() {
        let reason = LockoutReason::PostureCacheStale;
        let json = serde_json::to_string(&reason).expect("serialize");
        let roundtrip: LockoutReason = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(reason, roundtrip);
    }

    #[test]
    fn test_empty_cache_returns_locked_out_with_empty_reason() {
        use std::sync::Arc;

        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
        let (posture, reason) = resolve_posture_with_reason(&cache, 10_000);
        assert_eq!(posture, FleetPosture::LockedOut);
        assert_eq!(reason, Some(LockoutReason::PostureCacheEmpty));
    }

    #[test]
    fn test_fresh_nominal_cache_returns_nominal_with_no_reason() {
        use std::sync::Arc;
        use crate::posture_cache::CachedFleetPosture;

        let cached = CachedFleetPosture {
            posture: FleetPosture::Nominal,
            generated_at_ms: now_ms_engine(),
            ttl_ms: 10_000,
            generation: 1,
        };
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(cached)));
        let (posture, reason) = resolve_posture_with_reason(&cache, 10_000);
        assert_eq!(posture, FleetPosture::Nominal);
        assert_eq!(reason, None, "fresh cache must not produce a lockout reason");
    }

    #[test]
    fn test_stale_cache_returns_locked_out_with_stale_reason() {
        use std::sync::Arc;
        use crate::posture_cache::CachedFleetPosture;

        let stale_ts = now_ms_engine().saturating_sub(20_000);
        let cached = CachedFleetPosture {
            posture: FleetPosture::Nominal,
            generated_at_ms: stale_ts,
            ttl_ms: 10_000,
            generation: 5,
        };
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(cached)));
        let (posture, reason) = resolve_posture_with_reason(&cache, 10_000);
        assert_eq!(posture, FleetPosture::LockedOut);
        assert_eq!(reason, Some(LockoutReason::PostureCacheStale));
    }

    /// Tight boundary: a cache entry exactly `ttl_ms + 1` old must fail closed
    /// to `LockedOut` with `PostureCacheStale` — proves the TTL is enforced
    /// at the boundary the bin sites would observe at runtime, using
    /// `POSTURE_CACHE_TTL_MS` as the TTL (same constant the bin passes).
    #[test]
    fn test_stale_boundary_cache_fails_closed_at_runtime_ttl() {
        use std::sync::Arc;
        use crate::posture_cache::{CachedFleetPosture, POSTURE_CACHE_TTL_MS};

        let stale_ts = now_ms_engine().saturating_sub(POSTURE_CACHE_TTL_MS + 1);
        let cached = CachedFleetPosture {
            posture: FleetPosture::Nominal,
            generated_at_ms: stale_ts,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 42,
        };
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(cached)));
        let (posture, reason) = resolve_posture_with_reason(&cache, POSTURE_CACHE_TTL_MS);
        assert_eq!(posture, FleetPosture::LockedOut,
            "an entry older than POSTURE_CACHE_TTL_MS must NOT be served as current");
        assert_eq!(reason, Some(LockoutReason::PostureCacheStale));
    }

    #[test]
    fn test_trigger_display_includes_node_id() {
        let t = PostureRecalcTrigger::NodeTrustChanged {
            node_id: "lidar_front".to_string(),
            reason: "SENSOR_FAULT".to_string(),
        };
        let s = t.to_string();
        assert!(s.contains("lidar_front"));
        assert!(s.contains("SENSOR_FAULT"));
    }

    #[test]
    fn test_watchdog_trigger_display_includes_timeout() {
        let t = PostureRecalcTrigger::WatchdogTimeout {
            node_id: "gps_primary".to_string(),
            timeout_ms: 5000,
        };
        let s = t.to_string();
        assert!(s.contains("gps_primary"));
        assert!(s.contains("5000"));
    }

    #[tokio::test]
    async fn test_worker_channel_accepts_multiple_triggers() {
        let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);
        for i in 0..10 {
            tx.send(PostureRecalcTrigger::NodeTrustChanged {
                node_id: format!("node_{i}"),
                reason: "TEST".to_string(),
            }).await.expect("channel must accept trigger");
        }
        let mut count = 0;
        while rx.try_recv().is_ok() { count += 1; }
        assert_eq!(count, 10, "all triggers must be buffered");
    }

    #[tokio::test]
    async fn test_full_channel_returns_error_not_panic() {
        let (tx, _rx) = mpsc::channel::<PostureRecalcTrigger>(1);
        let _ = tx.try_send(PostureRecalcTrigger::DependencyGraphChanged);
        let result = tx.try_send(PostureRecalcTrigger::DependencyGraphChanged);
        assert!(result.is_err(), "full channel must return error");
    }

    #[test]
    fn test_periodic_refresh_display_is_distinct() {
        let s = PostureRecalcTrigger::PeriodicRefresh.to_string();
        assert_eq!(s, "PeriodicRefresh",
            "PeriodicRefresh Display must be the bare variant name (no state to print)");
    }

    /// SG9 / GAP 13: every `PostureRecalcTrigger` variant must render a
    /// stable, non-empty Display string. Audit/log lines downstream key on
    /// these tokens; covers the variants not exercised by the targeted
    /// tests above (ManualTrigger, DependencyGraphChanged, RssViolation).
    #[test]
    fn test_posture_recalc_trigger_display_per_variant() {
        let mt = PostureRecalcTrigger::ManualTrigger {
            operator_id: "alice".to_string(),
        };
        let s = mt.to_string();
        assert!(s.starts_with("ManualTrigger"));
        assert!(s.contains("alice"));

        let dg = PostureRecalcTrigger::DependencyGraphChanged.to_string();
        assert_eq!(dg, "DependencyGraphChanged");

        let rss = PostureRecalcTrigger::RssViolation(RssState {
            safe: false,
            longitudinal_margin: 1.25,
            lateral_margin: 0.50,
        });
        let s = rss.to_string();
        assert!(s.contains("RssViolation"));
        assert!(s.contains("safe=false"));
        assert!(s.contains("1.25"));
        assert!(s.contains("0.50"));
    }

    /// SG9 / GAP 11: a poisoned posture-cache `RwLock` must fail closed.
    /// `resolve_posture_with_reason` returns `(LockedOut, PostureCachePoisoned)`
    /// on the `Err(_)` arm of `cache.read()` (l.96–102). We poison the lock
    /// the standard way: take a write guard in a thread and panic inside it.
    #[test]
    fn test_resolve_posture_with_reason_poisoned_lock_fails_closed() {
        use std::sync::{Arc, RwLock};
        use std::thread;

        let cache: SharedPostureCache = Arc::new(RwLock::new(None));
        let poisoner = Arc::clone(&cache);
        let handle = thread::spawn(move || {
            let _guard = poisoner.write().expect("acquire write before poison");
            panic!("poisoning the RwLock for the test");
        });
        let _ = handle.join();
        assert!(cache.is_poisoned(), "test setup: lock must be poisoned");

        let (posture, reason) = resolve_posture_with_reason(&cache, 10_000);
        assert_eq!(posture, FleetPosture::LockedOut,
            "a poisoned cache must fail closed to LockedOut");
        assert_eq!(reason, Some(LockoutReason::PostureCachePoisoned),
            "must surface the PostureCachePoisoned reason");
    }

    /// PeriodicRefresh on an Active instance must re-stamp the cache —
    /// strictly-increasing generation and updated `generated_at_ms` — but
    /// NOT cause a posture change when state is unchanged. The first call
    /// populates from `None`; the second call updates only the generation
    /// (posture stays Nominal). Proves the gate's staleness check will
    /// observe a fresh entry after each tick.
    #[tokio::test]
    async fn test_periodic_refresh_restamps_cache_without_changing_posture() {
        use std::sync::Arc;
        use crate::verifier::{AppState, FleetPosture, NodeTrustState, RegisteredNode, VerifierOperationMode};
        use crate::verifier_store::VerifierStore;
        use crate::posture_cache::SharedPostureCache;

        let store = VerifierStore::new(":memory:").unwrap();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        // A live, Trusted node gives a genuine Nominal baseline. (Without it the
        // M-9 empty-live-set guard fails closed to LockedOut — the re-stamp
        // semantics under test are about an UNCHANGED posture, so a real Nominal
        // fleet is the right fixture here.)
        app.persist_and_insert_node(RegisteredNode {
            node_id: "node-1".to_string(),
            status: NodeTrustState::Trusted,
            registered_at_ms: 1,
            last_trust_update_ms: 1,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
            site: None,
            firmware_version: None,
        })
        .unwrap();

        crate::posture_engine::recalculate_and_broadcast(&app, &cache);
        let first = cache.read().unwrap().as_ref().cloned()
            .expect("initial recalc must populate the cache");
        assert_eq!(first.posture, FleetPosture::Nominal);

        // Wait so generated_at_ms can advance; not strictly required since
        // the generation already increments, but proves the re-stamp.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;

        crate::posture_engine::recalculate_and_broadcast(&app, &cache);
        let second = cache.read().unwrap().as_ref().cloned()
            .expect("periodic refresh must keep the cache populated");

        assert!(second.generation > first.generation,
            "periodic refresh must produce a strictly-increasing generation \
             (was {} → {})", first.generation, second.generation);
        assert!(second.generated_at_ms >= first.generated_at_ms,
            "periodic refresh must re-stamp generated_at_ms (monotonic)");
        assert_eq!(second.posture, FleetPosture::Nominal,
            "PeriodicRefresh on unchanged state must NOT alter the cached posture");
    }
}
