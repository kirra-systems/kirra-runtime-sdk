// src/supervisor.rs
//
// Background-task supervisor (review finding C2).
//
// The verifier's safety loops run as detached tokio tasks: the telemetry
// watchdog (SG-003 dead-man's switch), the HA heartbeat writer and promotion
// monitor, and the posture-engine recalculation worker. Before this, each was
// `tokio::spawn`-ed with its `JoinHandle` DROPPED — so a panic killed the task
// SILENTLY with no restart. For the watchdog that is fail-OPEN: a silent sensor
// would never be marked Untrusted, posture would never recalculate, and
// actuators would stay live.
//
// `spawn_supervised` re-spawns a dead task with bounded backoff. If a CRITICAL
// task crashes repeatedly within a rolling window (a deterministic panic, not a
// transient blip), the supervisor runs the ESCALATION — which forces the whole
// fleet to a fail-closed `FleetPosture::LockedOut` — and then keeps retrying at a
// slower cadence. It NEVER silently gives up.
//
// Escalation policy (operator-selected): "Force LockedOut + keep retrying" — the
// node stays observable (`/health` still answers, HA can still see it) while every
// actuator gate fails closed. See `posture_engine::force_lockout` +
// `AppState::supervisor_tripped` (the sticky flag the posture engine honors so the
// forced LockedOut survives subsequent recalcs).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Restarts permitted within [`SUPERVISOR_RESTART_WINDOW`] before a CRITICAL task
/// is declared wedged and the supervisor escalates the fleet to LockedOut.
pub const SUPERVISOR_RESTART_BUDGET: u32 = 5;

/// Rolling window over which the restart budget is counted. A task that runs
/// cleanly for longer than this resets its budget (a fresh transient blip later
/// is not held against an earlier, unrelated one).
pub const SUPERVISOR_RESTART_WINDOW: Duration = Duration::from_secs(60);

/// Backoff between ordinary restarts. The loops are cheap to respawn; this only
/// prevents a hot busy-restart loop on an immediate panic.
pub const SUPERVISOR_RESTART_BACKOFF: Duration = Duration::from_millis(500);

/// Retry cadence AFTER a critical task has tripped escalation. Slower, because the
/// fleet is already failed closed; a transient resource exhaustion may still clear
/// and let the task come back (though the LockedOut trip itself is sticky until a
/// human/HA reset, matching LockedOut semantics).
pub const SUPERVISOR_ESCALATED_RETRY: Duration = Duration::from_secs(5);

/// The escalation invoked when a CRITICAL supervised task exhausts its restart
/// budget. Constructed by the caller (it captures the `AppState` flag + posture
/// cache); see [`crate::posture_engine::force_lockout`].
pub type Escalation = Arc<dyn Fn() + Send + Sync>;

/// Spawn `make_future` under supervision and return the supervisor's own handle.
///
/// `make_future` is called once per (re)start to build a FRESH task future from
/// owned/cloned state, so loop-local state (intervals, in-memory maps) is rebuilt
/// on each restart.
///
/// - On a **panic**, the task is always restarted (a panic is a bug, never a
///   legitimate exit).
/// - On a **normal return**, behavior depends on `restart_on_return`: tasks that
///   must run forever (the watchdog) set it `true` (an unexpected return is a
///   failure); tasks with a legitimate terminal state (the heartbeat writer
///   exiting on fence, the promotion monitor exiting after it promotes) set it
///   `false`, and the supervisor stops cleanly.
/// - If `critical` and the restart budget is exhausted, `escalate` runs once and
///   the supervisor keeps retrying at [`SUPERVISOR_ESCALATED_RETRY`].
pub fn spawn_supervised<F, Fut>(
    name: &'static str,
    critical: bool,
    restart_on_return: bool,
    escalate: Option<Escalation>,
    make_future: F,
) -> JoinHandle<()>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let mut window_start = Instant::now();
        let mut restarts_in_window: u32 = 0;
        let mut escalated = false;

        loop {
            let inner: JoinHandle<()> = tokio::spawn(make_future());
            match inner.await {
                Ok(()) if !restart_on_return => {
                    tracing::info!(
                        task = name,
                        "supervised task returned normally (legitimate terminal state) — supervisor stopping"
                    );
                    return;
                }
                Ok(()) => {
                    tracing::error!(
                        task = name,
                        "supervised task returned unexpectedly (must run forever) — restarting"
                    );
                }
                Err(e) if e.is_panic() => {
                    tracing::error!(task = name, "supervised task PANICKED — restarting");
                }
                Err(e) => {
                    // Cancelled (only at runtime shutdown). Do not hot-loop.
                    tracing::warn!(task = name, error = %e, "supervised task cancelled — supervisor stopping");
                    return;
                }
            }

            // Rolling-window restart accounting.
            let now = Instant::now();
            if now.duration_since(window_start) > SUPERVISOR_RESTART_WINDOW {
                window_start = now;
                restarts_in_window = 0;
                // NOTE: we deliberately do NOT clear `escalated`. A forced fleet
                // LockedOut is sticky (human/HA reset), matching LockedOut
                // semantics — a task that later recovers must not silently
                // un-lock the fleet.
            }
            restarts_in_window += 1;

            if critical && restarts_in_window > SUPERVISOR_RESTART_BUDGET {
                if !escalated {
                    tracing::error!(
                        task = name,
                        restarts = restarts_in_window,
                        window_s = SUPERVISOR_RESTART_WINDOW.as_secs(),
                        "CRITICAL supervised task exceeded restart budget — escalating fleet to LockedOut (fail-closed) and retrying slowly"
                    );
                    if let Some(ref esc) = escalate {
                        esc();
                    }
                    escalated = true;
                }
                tokio::time::sleep(SUPERVISOR_ESCALATED_RETRY).await;
            } else {
                tokio::time::sleep(SUPERVISOR_RESTART_BACKOFF).await;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A panicking task is restarted; after the budget is exhausted the
    /// escalation fires exactly once. We use a tiny accelerated config by
    /// driving many quick panics inside the 60 s window.
    #[tokio::test(start_paused = true)]
    async fn panicking_critical_task_escalates_once_after_budget() {
        let runs = Arc::new(AtomicU32::new(0));
        let escalations = Arc::new(AtomicU32::new(0));

        let runs_c = Arc::clone(&runs);
        let esc_c = Arc::clone(&escalations);
        let escalate: Escalation = Arc::new(move || {
            esc_c.fetch_add(1, Ordering::SeqCst);
        });

        let _sup = spawn_supervised("test-critical", true, true, Some(escalate), move || {
            let runs_inner = Arc::clone(&runs_c);
            async move {
                runs_inner.fetch_add(1, Ordering::SeqCst);
                panic!("deterministic test panic");
            }
        });

        // Advance virtual time through enough backoff cycles to blow the budget.
        // Budget is 5 within 60s; each ordinary restart waits 500ms, then the
        // escalated cadence is 5s. Advance generously but stay inside the window.
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(600)).await;
            tokio::task::yield_now().await;
        }

        assert!(
            runs.load(Ordering::SeqCst) > SUPERVISOR_RESTART_BUDGET,
            "task should have been restarted past the budget"
        );
        assert_eq!(
            escalations.load(Ordering::SeqCst),
            1,
            "escalation must fire exactly once, not on every subsequent failure"
        );
    }

    /// A task that returns normally with `restart_on_return = false` stops the
    /// supervisor (legitimate terminal state — e.g. the promotion monitor after
    /// it promotes) and never escalates.
    #[tokio::test(start_paused = true)]
    async fn normal_return_without_restart_stops_supervisor() {
        let runs = Arc::new(AtomicU32::new(0));
        let escalations = Arc::new(AtomicU32::new(0));

        let runs_c = Arc::clone(&runs);
        let esc_c = Arc::clone(&escalations);
        let escalate: Escalation = Arc::new(move || {
            esc_c.fetch_add(1, Ordering::SeqCst);
        });

        let _sup = spawn_supervised("test-completes", true, false, Some(escalate), move || {
            let runs_inner = Arc::clone(&runs_c);
            async move {
                runs_inner.fetch_add(1, Ordering::SeqCst);
                // returns normally
            }
        });

        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(600)).await;
            tokio::task::yield_now().await;
        }

        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "task must run once and not be restarted"
        );
        assert_eq!(
            escalations.load(Ordering::SeqCst),
            0,
            "no escalation on a legitimate exit"
        );
    }
}
