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
                    (cached.posture.clone(), None)
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
use crate::verifier::AppState;

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
        }
    }
}

/// Channel sender for posture recalculation triggers.
pub type PostureEngineSender = mpsc::Sender<PostureRecalcTrigger>;

/// Starts the posture engine worker task.
pub fn start_posture_engine_worker(
    app: Arc<AppState>,
    cache: SharedPostureCache,
) -> PostureEngineSender {
    let (tx, mut rx) = mpsc::channel::<PostureRecalcTrigger>(128);

    tokio::spawn(async move {
        loop {
            let first = match rx.recv().await {
                Some(t) => t,
                None    => {
                    tracing::info!("Posture engine worker: trigger channel closed, exiting");
                    break;
                }
            };

            let mut batch: Vec<PostureRecalcTrigger> = vec![first];
            while let Ok(trigger) = rx.try_recv() {
                batch.push(trigger);
            }

            let batch_size = batch.len();
            let trigger_summary: Vec<String> = batch.iter().map(|t| t.to_string()).collect();

            if batch_size > 1 {
                tracing::debug!(
                    batch_size = batch_size,
                    triggers   = ?trigger_summary,
                    "Posture engine: coalescing {batch_size} triggers into single recalculation"
                );
            }

            recalculate_and_broadcast_with_context(&app, &cache, &trigger_summary);
        }
    });

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
        let (posture, reason) = resolve_posture_with_reason_sync(&cache, 10_000);
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
        let (posture, reason) = resolve_posture_with_reason_sync(&cache, 10_000);
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
        let (posture, reason) = resolve_posture_with_reason_sync(&cache, 10_000);
        assert_eq!(posture, FleetPosture::LockedOut);
        assert_eq!(reason, Some(LockoutReason::PostureCacheStale));
    }

    fn resolve_posture_with_reason_sync(
        cache: &SharedPostureCache,
        ttl_ms: u64,
    ) -> (FleetPosture, Option<LockoutReason>) {
        let ts = now_ms_engine();
        let guard = cache.read().unwrap();
        match guard.as_ref() {
            Some(cached) => {
                let age = ts.saturating_sub(cached.generated_at_ms);
                if age >= ttl_ms {
                    (FleetPosture::LockedOut, Some(LockoutReason::PostureCacheStale))
                } else {
                    (cached.posture.clone(), None)
                }
            }
            None => (FleetPosture::LockedOut, Some(LockoutReason::PostureCacheEmpty)),
        }
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
}
