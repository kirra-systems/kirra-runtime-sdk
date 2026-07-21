//! OTA campaign posture-sweep monitor (WS-4 / Track 3 — Fleet Plane).
//!
//! The [`crate::ota_campaign`] engine halts a rollout on regression only when
//! someone drives it — a `POST …/advance` reads the fleet posture and halts if it
//! is not `Nominal`. Between advances, a fleet that regresses would keep a
//! `Rolling` campaign live until the next manual advance. This background monitor
//! closes that gap: it periodically re-checks fleet posture and auto-halts any
//! active campaign the moment a regression is CONFIRMED — the continuous arm of
//! the same fail-closed halt-on-regression rule.
//!
//! **Confirmed-regression only.** The monitor halts a campaign ONLY on a
//! freshly-computed non-`Nominal` posture. An *unavailable* posture
//! (stale/empty/poisoned cache — the resolver's synthetic `LockedOut` carrying a
//! [`LockoutReason`](crate::posture_engine_v2::LockoutReason)) is NOT treated as a
//! regression: the actuator gate already
//! fail-closes on staleness, and terminally halting a slow rollout on a transient
//! cache gap (e.g. a startup window before the first recalc) would force an
//! operator to re-author the campaign. Unknown ≠ regressed — the tick is skipped.
//!
//! Availability, not safety: a wedged monitor cannot make anything UNSAFE (the
//! actuator posture gate is the independent safety spine; `advance` still halts
//! fail-closed on demand). So the worker is supervised **non-critical** — a panic
//! restarts it, but never forces `LockedOut`.

use std::sync::Arc;

use tokio::time::{interval, Duration};

use crate::clock::{Clock, SystemClock};
use crate::posture_cache::{SharedPostureCache, POSTURE_CACHE_TTL_MS};
use crate::posture_engine_v2::resolve_posture_snapshot_silent;
use crate::verifier::{AppState, FleetPosture};
use crate::verifier_store::VerifierStore;

/// Campaign posture-sweep interval. Campaigns are a slow control-plane concern, so
/// a 1 s cadence bounds the auto-halt latency after a confirmed regression without
/// thrashing SQLite. Comfortably shorter than `POSTURE_CACHE_TTL_MS` (5 s), so a
/// fresh posture is normally available every tick.
pub const CAMPAIGN_SWEEP_MS: u64 = 1_000;

/// Halt every active campaign iff a fleet regression is CONFIRMED. `confirmed` is
/// `Some(posture)` only when the caller resolved a fresh, authoritative posture;
/// `None` means posture is unavailable (stale/empty/poisoned) and the sweep is a
/// no-op (unknown ≠ regressed — see the module docs). Returns the number of
/// campaigns halted this sweep.
///
/// Pure over the store: loads the active set, applies the same
/// [`crate::ota_campaign::Campaign::check_regression`] rule the advance path uses,
/// and persists each halt (with its R156 audit entry) via `update_campaign`.
pub fn sweep_active_campaigns_once(
    store: &mut VerifierStore,
    confirmed: Option<FleetPosture>,
    now_ms: u64,
) -> rusqlite::Result<usize> {
    // Posture unavailable → do not halt. The actuator gate handles staleness; a
    // campaign is only ever terminally halted on a CONFIRMED regression.
    let Some(posture) = confirmed else {
        return Ok(0);
    };

    let active = store.load_active_campaigns()?;
    let mut halted = 0usize;
    for mut c in active {
        if let Some(reason) = c.check_regression(posture, now_ms) {
            store.update_campaign(&c, "OtaCampaignHalted")?;
            halted += 1;
            tracing::warn!(
                campaign_id = %c.campaign_id,
                reason = reason.as_str(),
                posture = ?posture,
                "OTA campaign auto-halted by the posture-sweep monitor (confirmed fleet regression)"
            );
        }
    }
    Ok(halted)
}

/// Resolve the fleet posture into the `Option<FleetPosture>` the sweep expects:
/// `Some(p)` for a fresh authoritative posture, `None` when it is unavailable
/// (the resolver returned a synthetic lockout with a reason). Pure — the seam the
/// worker and its tests share.
fn confirmed_posture(cache: &SharedPostureCache) -> Option<FleetPosture> {
    let (posture, reason, _generation) =
        resolve_posture_snapshot_silent(cache, POSTURE_CACHE_TTL_MS);
    // `reason.is_none()` ⇔ the posture is a real computed value, not a
    // staleness/empty/poison synthetic. Only then is it authoritative for a halt.
    match reason {
        None => Some(posture),
        Some(_) => None,
    }
}

/// Spawn the campaign posture-sweep monitor (production entry point — real
/// `SystemClock`). Supervised non-critical, run-forever.
pub fn spawn_campaign_monitor(app: Arc<AppState>, posture_cache: SharedPostureCache) {
    spawn_campaign_monitor_with_clock(app, posture_cache, Arc::new(SystemClock));
}

/// Same as [`spawn_campaign_monitor`] but with an injected clock, so tests can
/// drive `now_ms` deterministically. The production path passes `SystemClock`, so
/// it is byte-for-byte the direct implementation.
pub fn spawn_campaign_monitor_with_clock(
    app: Arc<AppState>,
    posture_cache: SharedPostureCache,
    clock: Arc<dyn Clock>,
) {
    crate::supervisor::spawn_supervised(
        "campaign_monitor",
        /* critical    */ false,
        /* run-forever  */ true,
        /* escalate     */ None,
        move || {
            let app = Arc::clone(&app);
            let posture_cache = posture_cache.clone();
            let clock = Arc::clone(&clock);
            async move {
                let mut sweep = interval(Duration::from_millis(CAMPAIGN_SWEEP_MS));
                // Evenly-paced sweeps; a starved runtime should not fire a catch-up
                // burst (each sweep is idempotent, so skipping missed ticks is fine).
                sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    sweep.tick().await;
                    let now = clock.now_ms();
                    let confirmed = confirmed_posture(&posture_cache);
                    // EP-11: time this sweep against the manifest's deadline budget
                    // (observability; NonCritical — never escalates).
                    let sweep_start_ms = now;
                    // Store I/O off the async thread (StoreHandle::call → spawn_blocking).
                    match app
                        .store
                        .call(move |s| sweep_active_campaigns_once(s, confirmed, now))
                        .await
                    {
                        Ok(Ok(n)) if n > 0 => {
                            tracing::info!(
                                halted = n,
                                "campaign monitor halted regressed campaigns"
                            )
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            tracing::error!(error = %e, "campaign monitor sweep DB error")
                        }
                        Err(_) => tracing::error!("campaign monitor sweep task failed"),
                    }
                    let elapsed_ms = clock.now_ms().saturating_sub(sweep_start_ms);
                    app.observability
                        .deadline_registry
                        .record("campaign_monitor", elapsed_ms);
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ota_campaign::{Campaign, CampaignState};

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn store() -> VerifierStore {
        VerifierStore::new(":memory:").expect("in-memory store")
    }

    /// Insert an armed, one-step-advanced (Rolling) campaign.
    fn rolling(s: &mut VerifierStore, id: &str) {
        let mut c =
            Campaign::new(id, DIGEST, "v1", vec!["a".into()], vec![50, 100], 1_000).unwrap();
        c.arm(1_100).unwrap();
        c.advance(FleetPosture::Nominal, 1_200).unwrap();
        s.insert_campaign(&c).unwrap();
    }

    #[test]
    fn nominal_sweep_halts_nothing() {
        let mut s = store();
        rolling(&mut s, "camp-1");
        let n = sweep_active_campaigns_once(&mut s, Some(FleetPosture::Nominal), 2_000).unwrap();
        assert_eq!(n, 0);
        assert_eq!(
            s.load_campaign("camp-1").unwrap().unwrap().state,
            CampaignState::Rolling
        );
    }

    #[test]
    fn confirmed_regression_halts_all_active() {
        let mut s = store();
        rolling(&mut s, "camp-1");
        rolling(&mut s, "camp-2");
        let n = sweep_active_campaigns_once(&mut s, Some(FleetPosture::LockedOut), 2_000).unwrap();
        assert_eq!(n, 2, "both active campaigns halt on a confirmed regression");
        assert_eq!(
            s.load_campaign("camp-1").unwrap().unwrap().state,
            CampaignState::Halted
        );
        assert_eq!(
            s.load_campaign("camp-2").unwrap().unwrap().state,
            CampaignState::Halted
        );
        // The halt is R156-audited like any other lifecycle mutation.
        assert_eq!(s.count_audit_events_for_test("OtaCampaignHalted"), 2);
    }

    #[test]
    fn degraded_is_a_regression_too() {
        let mut s = store();
        rolling(&mut s, "camp-1");
        let n = sweep_active_campaigns_once(&mut s, Some(FleetPosture::Degraded), 2_000).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn unavailable_posture_is_a_noop_not_a_halt() {
        // `None` = posture unavailable (stale/empty/poisoned). The sweep must NOT
        // halt — unknown is not a confirmed regression.
        let mut s = store();
        rolling(&mut s, "camp-1");
        let n = sweep_active_campaigns_once(&mut s, None, 2_000).unwrap();
        assert_eq!(n, 0, "unavailable posture must not halt a campaign");
        assert_eq!(
            s.load_campaign("camp-1").unwrap().unwrap().state,
            CampaignState::Rolling
        );
        assert_eq!(s.count_audit_events_for_test("OtaCampaignHalted"), 0);
    }

    #[test]
    fn terminal_and_draft_campaigns_are_untouched() {
        let mut s = store();
        // Draft (un-armed).
        s.insert_campaign(
            &Campaign::new(
                "camp-draft",
                DIGEST,
                "v1",
                vec!["a".into()],
                vec![100],
                1_000,
            )
            .unwrap(),
        )
        .unwrap();
        // Already Completed.
        let mut done = Campaign::new(
            "camp-done",
            DIGEST,
            "v1",
            vec!["a".into()],
            vec![100],
            1_000,
        )
        .unwrap();
        done.arm(1_100).unwrap();
        done.advance(FleetPosture::Nominal, 1_200).unwrap(); // single-stage → Completed
        assert_eq!(done.state, CampaignState::Completed);
        s.insert_campaign(&done).unwrap();

        let n = sweep_active_campaigns_once(&mut s, Some(FleetPosture::LockedOut), 2_000).unwrap();
        assert_eq!(
            n, 0,
            "a confirmed regression must not touch Draft/terminal campaigns"
        );
        assert_eq!(
            s.load_campaign("camp-draft").unwrap().unwrap().state,
            CampaignState::Draft
        );
        assert_eq!(
            s.load_campaign("camp-done").unwrap().unwrap().state,
            CampaignState::Completed
        );
    }

    #[test]
    fn confirmed_posture_helper_gates_on_reason() {
        use crate::posture_cache::CachedFleetPosture;
        // Fresh posture → Some(posture).
        let fresh: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
            CachedFleetPosture::new(FleetPosture::Degraded),
        )));
        assert_eq!(confirmed_posture(&fresh), Some(FleetPosture::Degraded));
        // Empty cache → None (unavailable, not a regression).
        let empty: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
        assert_eq!(confirmed_posture(&empty), None);
    }
}
