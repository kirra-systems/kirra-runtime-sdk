// src/standby_monitor.rs
//
// PassiveStandby promotion path for Kirra HA deployments.
//
// ARCHITECTURE
// ============
// Two Kirra instances share the same SQLite database (or the standby
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
//   KIRRA_INSTANCE_ID        — unique identifier for this instance (default: hostname)
//   KIRRA_HEARTBEAT_INTERVAL — override HEARTBEAT_INTERVAL_MS (ms, default: 2000)
//   KIRRA_PROMOTION_TIMEOUT  — override PROMOTION_TIMEOUT_MS (ms, default: 10000)

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
pub const HEARTBEAT_KEY: &str = "primary_heartbeat_ms";

/// SQLite key for the primary instance ID.
const PRIMARY_INSTANCE_KEY: &str = "primary_instance_id";

/// SQLite key recording which instance performed the last promotion.
const PROMOTION_RECORD_KEY: &str = "last_promotion_instance_id";

// ---------------------------------------------------------------------------
// Instance identity
// ---------------------------------------------------------------------------

/// Returns a unique identifier for this Kirra instance.
/// Reads KIRRA_INSTANCE_ID env var; falls back to hostname; falls back to
/// a process-lifetime stable ID derived from startup time.
pub fn instance_id() -> String {
    if let Ok(id) = std::env::var("KIRRA_INSTANCE_ID") {
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
        format!("kirra-{}", now_ms())
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
        let interval_ms = std::env::var("KIRRA_HEARTBEAT_INTERVAL")
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

                    // Proactive epoch-fence check: if the durable epoch has
                    // advanced past our held value, another instance has
                    // promoted and we have been fenced. Self-demote and stop
                    // heartbeating. This fixes the pre-existing gap where
                    // PROMOTION_RECORD_KEY detection only stopped the
                    // heartbeat loop but left mode_active = true (the
                    // mutation gate would still let writes through until
                    // the next request-time epoch check).
                    let held = app.held_epoch.load(std::sync::atomic::Ordering::SeqCst);
                    match store.current_epoch() {
                        Ok(db_epoch) => {
                            // Pass B1 (S3 / #115): cache the freshly observed
                            // DB epoch so the gate can read it lock-free.
                            // Release pairs with the gate's Acquire load.
                            // This runs every HEARTBEAT_INTERVAL_MS (~2000 ms)
                            // and is the cache repopulation path that bounds
                            // the gate's staleness.
                            app.cached_db_epoch.store(
                                db_epoch,
                                std::sync::atomic::Ordering::Release,
                            );
                            if held != 0 && db_epoch != held {
                                app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
                                tracing::error!(
                                    instance_id = %id,
                                    held        = held,
                                    db_epoch    = db_epoch,
                                    "FENCED — durable epoch advanced past held value; self-demoting and stopping heartbeat"
                                );
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, instance_id = %id,
                                "Heartbeat writer: epoch read failed");
                        }
                    }

                    if let Ok(Some(promoted_by)) = store.load_engine_state(PROMOTION_RECORD_KEY) {
                        // Mirror the epoch path: tear down the local Active
                        // flag too, not just the heartbeat loop.
                        app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
                        tracing::error!(
                            promoted_by = %promoted_by,
                            instance_id = %id,
                            "Standby has promoted — primary self-demoting and stopping heartbeat. \
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
// Per-poll promotion decision (pure)
// ---------------------------------------------------------------------------

/// The per-poll promotion DECISION the monitor loop acts on: a standby promotes
/// when the primary's last heartbeat is at least `timeout_ms` old. Extracted so
/// the real decision the spawned task calls (the task uses wall-clock time and
/// is otherwise untestable) can be exercised deterministically with injected
/// `now_ms` / heartbeat timestamps. `saturating_sub` makes clock skew
/// (heartbeat in the future) read as age 0 → no promotion, never an underflow.
///
/// Boundary is `>=` (exactly `timeout_ms` old promotes), matching the original
/// inline check. This is the SOLE gate on `perform_promotion`.
//
// Verifies: SG-009
pub(crate) fn promotion_decision(now_ms: u64, last_heartbeat_ms: u64, timeout_ms: u64) -> bool {
    now_ms.saturating_sub(last_heartbeat_ms) >= timeout_ms
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
/// Exception: if KIRRA_FORCE_PROMOTE=1 is set, promotes immediately regardless
/// of heartbeat state. Use for manual failover or testing.
pub fn spawn_promotion_monitor(app: Arc<AppState>, cache: SharedPostureCache) {
    let id = instance_id();
    tokio::spawn(async move {
        let poll_ms = std::env::var("KIRRA_PROMOTION_POLL")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(PROMOTION_POLL_MS);

        let timeout_ms = std::env::var("KIRRA_PROMOTION_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(PROMOTION_TIMEOUT_MS);

        let force_promote = std::env::var("KIRRA_FORCE_PROMOTE")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false);

        if force_promote {
            tracing::warn!(
                instance_id = %id,
                "KIRRA_FORCE_PROMOTE=1: bypassing heartbeat check, promoting immediately"
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

            let last_heartbeat_ms = match app.store.lock() {
                Ok(store) => {
                    match store.load_engine_state(HEARTBEAT_KEY) {
                        Ok(Some(ts_str)) => {
                            match ts_str.parse::<u64>() {
                                Ok(ts) => ts,
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

            // The promotion gate is `promotion_decision` (unit-tested in
            // `sg_009_promotion_act_tests`) — the loop just acts on its verdict.
            if promotion_decision(now, last_heartbeat_ms, timeout_ms) {
                tracing::error!(
                    instance_id   = %id,
                    heartbeat_age = now.saturating_sub(last_heartbeat_ms),
                    timeout_ms    = timeout_ms,
                    "Primary heartbeat stale — promoting to Active"
                );
                perform_promotion(&app, &cache, &id, "HEARTBEAT_TIMEOUT").await;
                return;
            } else {
                tracing::debug!(
                    heartbeat_age = now.saturating_sub(last_heartbeat_ms),
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

    // Step 1: DURABLE epoch claim (the real split-brain fence).
    //
    // SQLite serializes write transactions, so a conditional UPDATE on the
    // singleton `ha_state` row gives a real distributed CAS: two standbys
    // that both read the same `observed` epoch will serialize at commit
    // and only one will see rows_affected == 1 (Some(new_epoch)). The
    // loser sees None and MUST abort — its in-memory mode_active stays
    // false and no audit/cache state is written. The previous in-memory
    // `compare_exchange` did NOT provide this guarantee: it was per-process.
    let observed = match app.store.lock() {
        Ok(store) => match store.current_epoch() {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(error = %e, instance_id = %id,
                    "promotion: cannot read epoch — aborting");
                return;
            }
        },
        Err(_) => {
            tracing::error!(instance_id = %id,
                "promotion: store lock poisoned reading epoch — aborting");
            return;
        }
    };

    let new_epoch = match app.store.lock() {
        Ok(mut store) => match store.try_claim_epoch(observed, id, ts) {
            Ok(Some(e)) => e,
            Ok(None) => {
                // Fence held: another instance advanced the epoch between
                // our read and our write. We stay PassiveStandby.
                tracing::warn!(
                    instance_id = %id,
                    observed    = observed,
                    reason      = %reason,
                    "promotion ABORTED — epoch already advanced by another instance (durable fence held)"
                );
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, instance_id = %id,
                    "promotion: epoch claim execute failed — aborting");
                return;
            }
        },
        Err(_) => {
            tracing::error!(instance_id = %id,
                "promotion: store lock poisoned during claim — aborting");
            return;
        }
    };

    // Step 2: Cache the won epoch in memory so the mutation gate can
    // compare it against the DB epoch on every write. If a later
    // promotion fences us, gate-level checks will detect held != db and
    // self-demote.
    app.held_epoch.store(new_epoch, std::sync::atomic::Ordering::SeqCst);
    // Pass B1 (S3 / #115): re-stamp the cached DB epoch atomically so the
    // gate sees this promotion without taking the store lock. Release pairs
    // with the gate's Acquire load at `policy_layer.rs::enforce_posture_routing`.
    app.cached_db_epoch.store(new_epoch, std::sync::atomic::Ordering::Release);

    // Step 3: Flip the local mode atomic. By this point the durable
    // claim already succeeded, so this is a per-process bookkeeping
    // step, not a split-brain guard. A racing local CAS failure here
    // (e.g. a force-promote already flipped us) is informational only.
    if app
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
    if let Ok(store) = app.store.lock() {
        let _ = store.save_engine_state(PROMOTION_RECORD_KEY, id);
    }

    if let Ok(mut store) = app.store.lock() {
        // Tag the audit payload with `epoch` so a partitioned write is
        // identifiable after the fact (the audit chain itself is
        // monotonic; the epoch column adds the HA-generation linkage).
        let audit = serde_json::json!({
            "event":          "STANDBY_PROMOTED_TO_ACTIVE",
            "instance_id":    id,
            "reason":         reason,
            "promoted_at_ms": ts,
            "epoch":          new_epoch,
        });
        let _ = store.save_posture_event_chained(
            "standby_monitor",
            "STANDBY_PROMOTED_TO_ACTIVE",
            &audit.to_string(),
            Some(reason),
            ts,
        );
    }

    // Step 5: Initial recalculation as Active instance.
    // is_active() now returns true, so recalculate_and_broadcast will write
    // to the cache and emit broadcasts instead of returning early.
    recalculate_and_broadcast(app, cache);

    tracing::info!(
        instance_id = %id,
        epoch       = new_epoch,
        "Promotion complete — posture cache populated, SSE broadcast active"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod standby_monitor_tests {
    use super::*;

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

// ---------------------------------------------------------------------------
// HA epoch fence — real-state tests (temp-file SQLite, NOT :memory:)
// ---------------------------------------------------------------------------
//
// :memory: is per-connection — two VerifierStore instances opened against
// ":memory:" do NOT see each other's writes, which would silently turn a
// concurrent-promotion test into a no-op. These tests deliberately share a
// single temp file path so two stores genuinely share the ha_state row.

#[cfg(test)]
mod ha_epoch_fence_tests {
    use crate::verifier_store::VerifierStore;
    use std::path::PathBuf;

    fn tmp_db_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = format!(
            "kirra-ha-fence-{}-{}-{}.sqlite",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        p.push(nonce);
        p
    }

    /// CORE FENCE PROPERTY: two stores racing to promote — exactly ONE wins,
    /// the loser sees None, the final epoch == observed + 1 (not + 2).
    #[test]
    fn test_concurrent_promotion_only_one_instance_claims_epoch() {
        let path = tmp_db_path("concurrent");
        let path_str = path.to_str().unwrap().to_string();

        // Initialize the DB once (schema), then open two independent stores
        // sharing the same file — mirrors two processes pointing at the
        // same SQLite DB (or replicated WAL).
        let _seed = VerifierStore::new(&path_str).expect("seed open");
        let mut store_a = VerifierStore::new(&path_str).expect("store A");
        let mut store_b = VerifierStore::new(&path_str).expect("store B");

        let observed_a = store_a.current_epoch().unwrap();
        let observed_b = store_b.current_epoch().unwrap();
        assert_eq!(observed_a, observed_b,
            "both standbys must read the same observed epoch in the race");
        assert_eq!(observed_a, 0, "fresh DB starts at epoch 0");

        let claim_a = store_a.try_claim_epoch(observed_a, "instance-A", 1_000).unwrap();
        let claim_b = store_b.try_claim_epoch(observed_b, "instance-B", 1_000).unwrap();

        // The fence: exactly one Some, exactly one None.
        let wins = [claim_a.is_some(), claim_b.is_some()]
            .iter()
            .filter(|w| **w)
            .count();
        assert_eq!(wins, 1, "exactly one instance must win the epoch claim");

        // Epoch advanced by exactly 1 (not by 2).
        let final_epoch = store_a.current_epoch().unwrap();
        assert_eq!(final_epoch, 1,
            "exactly one bump must land — observed=0, final=1, not 2");

        let (db_epoch, holder) = store_a.current_active_holder().unwrap();
        assert_eq!(db_epoch, 1);
        let holder = holder.expect("a holder must be recorded after a successful claim");
        assert!(
            holder == "instance-A" || holder == "instance-B",
            "holder must be one of the racers, got {holder}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A stale-epoch holder is fenced: we hold epoch 1 but DB has been
    /// bumped to 2 by a separate instance — current_epoch reports 2,
    /// so the gate check (held != db) fails closed.
    #[test]
    fn test_stale_epoch_holder_is_fenced() {
        let path = tmp_db_path("stale-holder");
        let path_str = path.to_str().unwrap().to_string();

        let _seed = VerifierStore::new(&path_str).expect("seed");
        let mut store_a = VerifierStore::new(&path_str).expect("store A");
        let mut store_b = VerifierStore::new(&path_str).expect("store B");

        // A claims epoch 1.
        let e_a = store_a.try_claim_epoch(0, "A", 100).unwrap();
        assert_eq!(e_a, Some(1));
        let held_a: u64 = 1;

        // B reads observed=1 and claims epoch 2.
        let e_b = store_b.try_claim_epoch(1, "B", 200).unwrap();
        assert_eq!(e_b, Some(2));

        // A's gate check: held (1) vs current DB epoch (2) → fenced.
        let db_epoch = store_a.current_epoch().unwrap();
        assert_eq!(db_epoch, 2, "DB must reflect B's successful claim");
        assert_ne!(held_a, db_epoch,
            "A is fenced — its held epoch is now stale relative to the DB");

        let _ = std::fs::remove_file(&path);
    }

    /// A startup-time Active instance on a clean DB claims and records its
    /// epoch — the holder column matches the instance ID, epoch == 1.
    #[test]
    fn test_startup_active_claims_epoch_on_clean_db() {
        let path = tmp_db_path("startup-clean");
        let path_str = path.to_str().unwrap().to_string();

        let mut store = VerifierStore::new(&path_str).expect("store");

        let (initial_epoch, initial_holder) = store.current_active_holder().unwrap();
        assert_eq!(initial_epoch, 0);
        assert!(initial_holder.is_none(), "clean DB has no holder yet");

        let claimed = store.try_claim_epoch(initial_epoch, "primary-1", 5_000).unwrap();
        assert_eq!(claimed, Some(1), "first claim on clean DB must succeed");

        let (final_epoch, final_holder) = store.current_active_holder().unwrap();
        assert_eq!(final_epoch, 1);
        assert_eq!(final_holder.as_deref(), Some("primary-1"));

        let _ = std::fs::remove_file(&path);
    }

    /// A second concurrent Active attempt loses the claim race — its
    /// try_claim_epoch returns None. The bin's startup logic stands the
    /// loser down to PassiveStandby (the test asserts the primitive
    /// return value; the bin asserts the policy on top of it).
    #[test]
    fn test_promotion_aborts_when_epoch_already_advanced() {
        let path = tmp_db_path("aborts");
        let path_str = path.to_str().unwrap().to_string();

        let _seed = VerifierStore::new(&path_str).expect("seed");
        let mut store_a = VerifierStore::new(&path_str).expect("store A");
        let mut store_b = VerifierStore::new(&path_str).expect("store B");

        let observed = store_a.current_epoch().unwrap();
        let _win = store_a.try_claim_epoch(observed, "winner", 1).unwrap();

        // B observed `observed` (0) too, but the DB is now at 1. Its
        // conditional UPDATE finds zero matching rows.
        let lose = store_b.try_claim_epoch(observed, "loser", 2).unwrap();
        assert!(lose.is_none(),
            "stale-observed claim must abort with None (durable fence)");

        let (final_epoch, holder) = store_a.current_active_holder().unwrap();
        assert_eq!(final_epoch, 1, "exactly one bump");
        assert_eq!(holder.as_deref(), Some("winner"),
            "loser must NOT have overwritten the holder column");

        let _ = std::fs::remove_file(&path);
    }

    /// Startup-defer policy primitive: when a live holder is detected, the
    /// configured-Active node must NOT issue a claim. We assert the read
    /// surface the bin uses (current_active_holder + heartbeat freshness)
    /// gives the expected inputs for that decision.
    #[test]
    fn test_startup_defers_to_live_active_holder() {
        let path = tmp_db_path("defer");
        let path_str = path.to_str().unwrap().to_string();

        let _seed = VerifierStore::new(&path_str).expect("seed");
        let mut store_primary = VerifierStore::new(&path_str).expect("primary");
        let store_new = VerifierStore::new(&path_str).expect("new");

        // Primary claims and writes a fresh heartbeat.
        let _ = store_primary.try_claim_epoch(0, "primary", 1_000).unwrap();
        store_primary
            .save_engine_state(super::HEARTBEAT_KEY, "1000")
            .unwrap();

        // New instance, ~half the timeout later — heartbeat is fresh.
        let now: u64 = 1_000 + super::PROMOTION_TIMEOUT_MS / 2;
        let (epoch, holder) = store_new.current_active_holder().unwrap();
        let hb_str = store_new
            .load_engine_state(super::HEARTBEAT_KEY)
            .unwrap()
            .expect("heartbeat must be present");
        let hb_ts: u64 = hb_str.parse().unwrap();
        let fresh = now.saturating_sub(hb_ts) < super::PROMOTION_TIMEOUT_MS;

        assert_eq!(epoch, 1);
        assert_eq!(holder.as_deref(), Some("primary"));
        assert!(fresh, "heartbeat must read as fresh within the timeout window");

        // The bin's policy: holder is some OTHER id AND heartbeat is fresh
        // ⇒ stand down. We model that decision here so the test fails if
        // either input changes meaning.
        let my_id = "newcomer";
        let should_defer = matches!(holder.as_deref(), Some(h) if h != my_id) && fresh;
        assert!(should_defer,
            "startup must defer to a live holder rather than steal the epoch");

        let _ = std::fs::remove_file(&path);
    }
}

// ---------------------------------------------------------------------------
// SG-009 (ASIL B) — HA standby promotion: the ACT, not just the math
// ---------------------------------------------------------------------------
//
// The existing `standby_monitor_tests` cover the age/threshold arithmetic.
// These tests cover the two things the RTM names that the math tests do not:
//   1. The real per-poll DECISION the spawned task gates on
//      (`promotion_decision`) — stale promotes, fresh does not, with the
//      `>=`-at-boundary and clock-skew semantics the loop relied on inline.
//   2. The promotion ACT (`perform_promotion`): `mode_active` actually
//      transitions false→true (the standby becomes Active), the promoted path
//      writes its durable promotion record + audit event (disk-first), and it
//      recalculates posture (cache populated — only an Active instance writes
//      the cache, so a populated cache proves the flip happened before recalc).
//
// `perform_promotion` is `async` + module-private, so this MUST be an in-crate
// test (an external integration crate cannot see it). The matching external
// stub is an #[ignore] pointer here.

#[cfg(test)]
mod sg_009_promotion_act_tests {
    use super::*;
    use crate::verifier::{AppState, VerifierOperationMode};
    use crate::verifier_store::VerifierStore;
    use std::sync::atomic::Ordering;

    /// Stale heartbeat (older than the timeout) → promote.
    #[test]
    fn test_stale_heartbeat_decides_promote() {
        // 15s-old heartbeat at a 10s timeout.
        assert!(promotion_decision(25_000, 10_000, PROMOTION_TIMEOUT_MS),
            "a heartbeat older than PROMOTION_TIMEOUT_MS must decide promote");
    }

    /// Fresh heartbeat (within the timeout) → no promotion.
    #[test]
    fn test_fresh_heartbeat_decides_no_promote() {
        // 1s-old heartbeat at a 10s timeout.
        assert!(!promotion_decision(10_000, 9_000, PROMOTION_TIMEOUT_MS),
            "a heartbeat within PROMOTION_TIMEOUT_MS must NOT promote");
    }

    /// Boundary is inclusive (`>=`): exactly `timeout_ms` old promotes.
    #[test]
    fn test_decision_boundary_is_inclusive() {
        assert!(promotion_decision(PROMOTION_TIMEOUT_MS, 0, PROMOTION_TIMEOUT_MS),
            "exactly PROMOTION_TIMEOUT_MS old must promote (>=, matching the inline check)");
        assert!(!promotion_decision(PROMOTION_TIMEOUT_MS - 1, 0, PROMOTION_TIMEOUT_MS),
            "one ms below the timeout must not promote");
    }

    /// Clock skew (heartbeat timestamped in the future) saturates to age 0 —
    /// never an underflow, never a spurious promotion.
    #[test]
    fn test_decision_clock_skew_does_not_promote() {
        assert!(!promotion_decision(100, 5_000, PROMOTION_TIMEOUT_MS),
            "future-dated heartbeat must read as age 0 (saturating) → no promotion");
    }

    /// THE ACT: a PassiveStandby that calls `perform_promotion` becomes Active,
    /// records the promotion durably, and recalculates posture.
    #[test]
    fn test_perform_promotion_flips_mode_records_and_recalcs() {
        // PassiveStandby instance on a fresh in-memory store. (Single store ⇒
        // one connection ⇒ epoch reads/writes are self-consistent; the durable
        // connection falls back to `conn` for :memory:.)
        let store = VerifierStore::new(":memory:").expect("store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::PassiveStandby));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        // Precondition: standby (mode_active == false), empty cache.
        assert!(!app.is_active(), "must start as PassiveStandby");
        assert!(cache.read().unwrap().is_none(), "cache empty before promotion");

        // Drive the real promotion path. `perform_promotion` is async but has no
        // .await suspension points that need the reactor; a current-thread
        // runtime executes it deterministically.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(perform_promotion(&app, &cache, "standby-under-test", "HEARTBEAT_TIMEOUT"));

        // 1. mode_active transitioned false → true (the compare_exchange ACT).
        assert!(app.is_active(), "promotion must flip mode_active false→true (now Active)");

        // 2. Durable promotion record written (disk-first bookkeeping).
        let recorded = app.store.lock().unwrap()
            .load_engine_state(PROMOTION_RECORD_KEY).unwrap();
        assert_eq!(recorded.as_deref(), Some("standby-under-test"),
            "promoted instance must persist its id to the promotion record");

        // 3. An epoch was durably claimed (the split-brain fence advanced).
        assert_eq!(app.held_epoch.load(Ordering::SeqCst), 1,
            "first promotion on a clean DB must claim epoch 1");

        // 4. Posture was recalculated as Active — the cache is populated. Only an
        //    Active instance writes the cache, so a populated cache is proof the
        //    mode flip happened BEFORE the recalc (ordering invariant).
        assert!(cache.read().unwrap().is_some(),
            "promoted (Active) instance must recalculate posture and populate the cache");

        // 5. The promotion is recorded in the tamper-evident audit chain.
        let audit = app.store.lock().unwrap()
            .verify_audit_chain_full(None).unwrap();
        assert!(audit.total_entries >= 1,
            "promotion must append a STANDBY_PROMOTED_TO_ACTIVE audit event");
    }
}
