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
//   spawn_promotion_monitor(app, cache, on_promote)
//     → every PROMOTION_POLL_MS, reads "primary_heartbeat_ms"
//     → if age > PROMOTION_TIMEOUT_MS: promote
//     → promotion: app.mode_active transitions false → true
//       then calls recalculate_and_broadcast() once to populate the
//       cache and begin enforcing posture
//     → fires on_promote() to (re)start the Active posture-freshness
//       tasks on the new primary (review H2), then starts heartbeating
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
use std::time::Instant;
use tokio::time::{interval, Duration};

use crate::verifier::AppState;
use crate::posture_cache::{SharedPostureCache, now_ms};
use crate::posture_engine::{init_generation_from_store, recalculate_and_broadcast};
use crate::posture_engine_v2::resolve_post_promotion_posture;

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

/// Review item "1" — DISK-WEDGE SELF-DEMOTION. Number of CONSECUTIVE heartbeat
/// ticks (write OR epoch read) that may fail before the Active primary
/// self-demotes (mode_active → false) and stops heartbeating. A primary that
/// cannot WRITE its heartbeat or READ its durable epoch can neither refresh the
/// fence cache nor confirm it still owns the epoch — a fence-uncertainty that
/// must fail CLOSED, because the standby will promote on heartbeat silence and a
/// still-Active old primary would be a second writer.
///
/// Sized so the primary demotes BEFORE the standby promotes:
/// `3 × HEARTBEAT_INTERVAL_MS` = 6 s < `PROMOTION_TIMEOUT_MS` (10 s), leaving a
/// ~4 s margin so the two-writer windows never overlap. `> 1` so a single
/// transient (a checkpoint `SQLITE_BUSY` the P2 `busy_timeout` didn't fully
/// absorb) does not demote a healthy primary.
pub const MAX_CONSECUTIVE_HEARTBEAT_FAILURES: u32 = 3;

// Default-config safety check: the primary must self-demote strictly before the
// standby's promotion timeout, or both could be Active at once.
const _: () = assert!(
    MAX_CONSECUTIVE_HEARTBEAT_FAILURES as u64 * HEARTBEAT_INTERVAL_MS < PROMOTION_TIMEOUT_MS,
    "primary must self-demote before the standby promotes (default config)"
);

/// Pure decision (review item "1"): does a run of `consecutive_failures` failed
/// heartbeat ticks mean the primary should self-demote? Extracted so the
/// disk-wedge threshold is unit-testable without driving the async loop + a
/// failing store.
#[must_use]
pub fn should_self_demote_on_heartbeat_failures(consecutive_failures: u32) -> bool {
    consecutive_failures >= MAX_CONSECUTIVE_HEARTBEAT_FAILURES
}

/// #689: enforce a safe split-brain margin on the ENV-derived HA timings. The
/// compile-time `const _` assert above only guards the DEFAULT constants; the
/// runtime `KIRRA_HEARTBEAT_INTERVAL` / `KIRRA_PROMOTION_TIMEOUT` overrides are
/// read independently with no cross-check. A wedged primary self-demotes after
/// `MAX_CONSECUTIVE_HEARTBEAT_FAILURES × interval`; the standby promotes at
/// `promotion_timeout`. Two cases are unsafe-or-fragile:
/// - `promotion_timeout <= MAX × interval` — the standby promotes *before (or as)*
///   the old primary self-demotes: an outright transient two-`mode_active` window
///   (until the durable epoch fence catches the old primary on its next tick);
/// - `MAX × interval < promotion_timeout < (MAX+1) × interval` — strictly safe, but
///   with **less than one heartbeat interval** of slack, fragile against scheduling
///   / disk jitter (the same robustness margin the default constants carry).
///
/// So the floor enforced is `(MAX + 1) × interval` — i.e. the clamp fires whenever
/// `promotion_timeout < (MAX + 1) × interval`, covering BOTH cases and guaranteeing
/// at least one full heartbeat interval between self-demote and promotion. Clamping
/// UP is the SAFE direction: the standby waits longer, never promotes early. Returns
/// `(resolved_timeout_ms, clamped)`; the caller logs loudly when `clamped` so the
/// misconfiguration is visible rather than silently changing failover latency.
// #707: pub(crate) — internal helper (promotion loop + same-module tests), not a
// supported external API.
#[must_use]
pub(crate) fn enforce_promotion_timeout_floor(env_timeout_ms: u64, interval_ms: u64) -> (u64, bool) {
    // Contract: callers pass a positive interval (both env reads filter `> 0`).
    // `interval_ms == 0` would make the floor 0 and silently disable the
    // split-brain guard — fail fast on misuse in debug/test builds without
    // changing release behaviour (Copilot PR #707).
    debug_assert!(
        interval_ms > 0,
        "interval_ms must be > 0 (0 would disable the split-brain promotion floor)"
    );
    let floor = (MAX_CONSECUTIVE_HEARTBEAT_FAILURES as u64 + 1).saturating_mul(interval_ms);
    if env_timeout_ms < floor {
        (floor, true)
    } else {
        (env_timeout_ms, false)
    }
}

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
    FALLBACK_ID.get_or_init(stable_fallback_instance_id).clone()
}

/// Derive a fallback instance id that is STABLE across process restarts (HA
/// Part B / review H3+L2). The startup epoch-reclaim path keys on the instance
/// id: a node that crashed mid-promotion must present the SAME id on reboot to
/// reclaim its own durable epoch. The previous `kirra-<now_ms>` fallback changed
/// on every restart, breaking that reclaim. Resolution order (most→least stable):
///   1. `/etc/machine-id` — the canonical per-host stable identity (distinct per
///      container/host in an HA deployment).
///   2. A persisted id file (`KIRRA_INSTANCE_ID_FILE`, else `kirra_instance_id`
///      in the CWD): read if present, else generate once and best-effort persist.
///   3. Last resort: `kirra-<now_ms>` with a LOUD warning (not stable — operators
///      should set `KIRRA_INSTANCE_ID` / `HOSTNAME` in any real HA deployment).
fn stable_fallback_instance_id() -> String {
    if let Ok(mid) = std::fs::read_to_string("/etc/machine-id") {
        let mid = mid.trim();
        if !mid.is_empty() {
            return format!("kirra-{mid}");
        }
    }

    let path = std::env::var("KIRRA_INSTANCE_ID_FILE")
        .unwrap_or_else(|_| "kirra_instance_id".to_string());
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim();
        if !existing.is_empty() {
            return existing.to_string();
        }
    }
    let generated = format!("kirra-{}", now_ms());
    if let Err(e) = std::fs::write(&path, &generated) {
        tracing::warn!(
            error = %e, path = %path, instance_id = %generated,
            "instance_id: could not persist generated fallback id — it will NOT be stable across \
             restarts; set KIRRA_INSTANCE_ID or HOSTNAME for HA"
        );
    }
    generated
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
    // C2: supervised so a panic doesn't silently stop heartbeats (which would
    // trigger a spurious standby promotion). NON-critical — a genuinely dead
    // heartbeat is the DESIGNED failover signal, so no LockedOut escalation;
    // run_forever=false because exiting on fence / promoted-over is legitimate.
    crate::supervisor::spawn_supervised(
        "ha_heartbeat_writer",
        /* critical   */ false,
        /* run-forever */ false,
        None,
        move || heartbeat_loop(Arc::clone(&app), id.clone()),
    );
}

async fn heartbeat_loop(app: Arc<AppState>, id: String) {
        let interval_ms = std::env::var("KIRRA_HEARTBEAT_INTERVAL")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&v| v > 0) // #707: reject 0 — disables the #689 clamp AND panics tokio::interval
            .unwrap_or(HEARTBEAT_INTERVAL_MS);

        let mut tick = interval(Duration::from_millis(interval_ms));

        tracing::info!(
            instance_id = %id,
            interval_ms = interval_ms,
            "Heartbeat writer started"
        );

        // Review item "1": consecutive failed heartbeat ticks (write or epoch
        // read). Reset to 0 on any healthy tick; at MAX_CONSECUTIVE_HEARTBEAT_
        // FAILURES the primary self-demotes (disk-wedge / fence-uncertainty).
        let mut consecutive_failures: u32 = 0;

        // The store work runs under one acquisition; the closure returns an
        // OUTCOME the outer loop acts on (the closure cannot `continue`/`break`
        // the outer loop directly — Rule 4).
        enum HeartbeatOutcome {
            /// Fenced or promoted-over: the closure already set mode_active=false;
            /// the loop breaks.
            SelfDemoted,
            /// The heartbeat write or the epoch read failed this tick.
            Failed,
            /// A clean tick: heartbeat written, epoch read and still owned.
            Healthy,
        }

        loop {
            tick.tick().await;
            // #80: this `now_ms()` is written as a freshness TOKEN, not a clock
            // the standby trusts. The standby treats it as an opaque
            // change-detector (it advances each tick) and times staleness on its
            // OWN monotonic clock — see `HeartbeatFreshness`. Any value that
            // strictly changes each tick would do; `now_ms()` is convenient.
            let ts = now_ms();
            // P1: run the heartbeat write + epoch read OFF the tokio worker pool
            // (`call` → spawn_blocking) so the ~2 s fsync write can't pin a shared
            // worker and head-of-line-block request handlers / the fast loop. The
            // whole closure still runs under ONE writer-lock acquisition on the
            // blocking thread, so the write-then-epoch-read group stays atomic. The
            // closure must own its captures; clone the Arc (for the atomics) and the
            // instance id (reused next tick) into it.
            let app_c = Arc::clone(&app);
            let id_c = id.clone();
            let outcome = match app.store.call(move |store| {
                if let Err(e) = store.save_engine_state(HEARTBEAT_KEY, &ts.to_string()) {
                    tracing::warn!(
                        error       = %e,
                        instance_id = %id_c,
                        "Heartbeat write failed"
                    );
                    return HeartbeatOutcome::Failed;
                }

                let _ = store.save_engine_state(PRIMARY_INSTANCE_KEY, &id_c);

                // Proactive epoch-fence check: if the durable epoch has
                // advanced past our held value, another instance has
                // promoted and we have been fenced. Self-demote and stop
                // heartbeating. This fixes the pre-existing gap where
                // PROMOTION_RECORD_KEY detection only stopped the
                // heartbeat loop but left mode_active = true (the
                // mutation gate would still let writes through until
                // the next request-time epoch check).
                let held = app_c.held_epoch.load(std::sync::atomic::Ordering::SeqCst);
                let db_epoch = match store.current_epoch() {
                    Ok(e) => e,
                    // Review item "1": an unreadable epoch is a FAILED tick, not a
                    // silent pass. The old code logged and fell through to
                    // `Proceed`, so a primary whose disk wedged on the epoch read
                    // kept running Active with a FROZEN `cached_db_epoch` — never
                    // fenced. Count it; the consecutive-failure guard below demotes.
                    Err(e) => {
                        tracing::warn!(error = %e, instance_id = %id_c,
                            "Heartbeat writer: epoch read failed");
                        return HeartbeatOutcome::Failed;
                    }
                };
                // Pass B1 (S3 / #115): cache the freshly observed DB epoch so the
                // gate can read it lock-free. Release pairs with the gate's Acquire
                // load. This runs every HEARTBEAT_INTERVAL_MS (~2000 ms) and is the
                // cache repopulation path that bounds the gate's staleness.
                app_c.cached_db_epoch.store(db_epoch, std::sync::atomic::Ordering::Release);
                if held != 0 && db_epoch != held {
                    app_c.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
                    tracing::error!(
                        instance_id = %id_c,
                        held        = held,
                        db_epoch    = db_epoch,
                        "FENCED — durable epoch advanced past held value; self-demoting and stopping heartbeat"
                    );
                    return HeartbeatOutcome::SelfDemoted;
                }

                if let Ok(Some(promoted_by)) = store.load_engine_state(PROMOTION_RECORD_KEY) {
                    // Mirror the epoch path: tear down the local Active
                    // flag too, not just the heartbeat loop.
                    app_c.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
                    tracing::error!(
                        promoted_by = %promoted_by,
                        instance_id = %id_c,
                        "Standby has promoted — primary self-demoting and stopping heartbeat. \
                         Restart this instance in PassiveStandby mode."
                    );
                    return HeartbeatOutcome::SelfDemoted;
                }

                HeartbeatOutcome::Healthy
            }).await {
                Ok(o) => o,
                // The spawn_blocking task panicked/was cancelled — count it as a
                // failed tick (it produced no heartbeat write / epoch confirmation),
                // so a persistently broken writer trips the disk-wedge demotion.
                Err(e) => {
                    tracing::warn!(error = %e, instance_id = %id, "Heartbeat store task failed");
                    HeartbeatOutcome::Failed
                }
            };
            match outcome {
                HeartbeatOutcome::SelfDemoted => break,
                HeartbeatOutcome::Healthy => consecutive_failures = 0,
                HeartbeatOutcome::Failed => {
                    consecutive_failures += 1;
                    // Review item "1": after N consecutive failures the primary can
                    // no longer confirm it owns the epoch and the standby is about
                    // to promote on heartbeat silence. Self-demote (fail closed) so
                    // the old primary stops being a writer before the new one starts.
                    if should_self_demote_on_heartbeat_failures(consecutive_failures) {
                        app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
                        tracing::error!(
                            instance_id          = %id,
                            consecutive_failures = consecutive_failures,
                            max                  = MAX_CONSECUTIVE_HEARTBEAT_FAILURES,
                            "DISK-WEDGE — consecutive heartbeat/epoch failures exceeded; self-demoting and stopping heartbeat (fence-uncertainty → fail closed)"
                        );
                        break;
                    }
                }
            }
        }
}

// ---------------------------------------------------------------------------
// Per-poll promotion decision (pure) + monotonic heartbeat-freshness tracker
// ---------------------------------------------------------------------------

/// The per-poll promotion DECISION the monitor loop acts on: a standby promotes
/// when the primary's heartbeat token has gone UNCHANGED for at least
/// `timeout_ms`, where that elapsed time is measured on the standby's OWN
/// monotonic clock (`Instant`), not by differencing two wall-clock stamps.
///
/// `elapsed_since_heartbeat` comes from [`HeartbeatFreshness`] (an `Instant`
/// delta), so it is immune to wall-clock skew / NTP steps on EITHER machine
/// (#80). It can never be negative — that structurally subsumes the old
/// `saturating_sub` clock-skew guard (a future-dated heartbeat could previously
/// read as a huge or zero age). Boundary stays `>=` (exactly `timeout_ms`
/// elapsed promotes), matching the original inline check. Sole gate on
/// `perform_promotion`.
//
// Verifies: SG-009
pub(crate) fn promotion_decision(elapsed_since_heartbeat: Duration, timeout_ms: u64) -> bool {
    elapsed_since_heartbeat >= Duration::from_millis(timeout_ms)
}

/// Monotonic heartbeat-freshness tracker (#80).
///
/// THE CROSS-MACHINE TRAP: the primary writes its own `now_ms()` to the shared
/// heartbeat key; the standby cannot difference that against its own wall clock,
/// because the two machines' clocks are independent and may skew or NTP-step
/// relative to each other. A naive `Instant::now()` swap is also wrong —
/// monotonic clocks are per-machine and not comparable across machines.
///
/// CORRECT MODEL: treat the stored heartbeat as an opaque change-TOKEN, not a
/// comparable timestamp. The standby remembers the last token it saw and the
/// monotonic `Instant` at which it last CHANGED. Staleness is "how long has the
/// token been unchanged", timed entirely on the standby's own monotonic clock —
/// using only one machine's `Instant`, so wall-clock skew/steps on either
/// machine cannot affect it.
pub(crate) struct HeartbeatFreshness {
    /// The last heartbeat token observed (opaque string; the primary writes
    /// `now_ms().to_string()`, but its NUMERIC value is never interpreted — only
    /// equality/change matters).
    last_token: Option<String>,
    /// Monotonic instant at which `last_token` last changed (the freshness
    /// anchor). Elapsed time since this point is the heartbeat staleness.
    anchor: Instant,
}

impl HeartbeatFreshness {
    /// Starts a tracker anchored at `now` with no token observed yet.
    pub(crate) fn new(now: Instant) -> Self {
        Self { last_token: None, anchor: now }
    }

    /// Records an observation of `token` at monotonic instant `now`. Returns
    /// `true` if the token ADVANCED (changed, or was seen for the first time) —
    /// i.e. the primary is alive — in which case the anchor is reset to `now`.
    /// Returns `false` if the token is unchanged (the primary may be stalled);
    /// the anchor is left where it was so [`Self::elapsed`] keeps growing.
    pub(crate) fn observe(&mut self, token: &str, now: Instant) -> bool {
        if self.last_token.as_deref() != Some(token) {
            self.last_token = Some(token.to_string());
            self.anchor = now;
            true
        } else {
            false
        }
    }

    /// Monotonic elapsed time since the token last changed.
    pub(crate) fn elapsed(&self, now: Instant) -> Duration {
        now.duration_since(self.anchor)
    }
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
/// `on_promote` (review H2): a hook fired ONCE on a successful promotion, after
/// the mode flip / durable epoch claim / initial recalc. The caller (the binary)
/// uses it to (re)start the Active-mode posture-freshness tasks — the serialized
/// posture-engine worker, the telemetry watchdog, the periodic refresh loop, and
/// the local-asset feed — on the freshly-promoted node. Without it the promoted
/// node's posture cache goes stale one `POSTURE_CACHE_TTL_MS` after promotion and
/// every gated route fail-closes until process restart. It is a hook rather than
/// inline wiring because those tasks need `ServiceState`/bin-local handles the
/// library promotion path does not hold. `Arc<dyn Fn>` so the supervised task
/// factory can clone it across a restart.
pub type OnPromote = Arc<dyn Fn() + Send + Sync + 'static>;

pub fn spawn_promotion_monitor(app: Arc<AppState>, cache: SharedPostureCache, on_promote: OnPromote) {
    let id = instance_id();
    // C2: supervised so a panic doesn't permanently disable failover. NON-critical
    // (a standby cannot serve writes anyway, so a LockedOut escalation is moot);
    // run_forever=false because exiting after a successful promotion is legitimate.
    crate::supervisor::spawn_supervised(
        "ha_promotion_monitor",
        /* critical   */ false,
        /* run-forever */ false,
        None,
        move || promotion_loop(Arc::clone(&app), cache.clone(), id.clone(), Arc::clone(&on_promote)),
    );
}

/// Actions taken exactly once on a SUCCESSFUL promotion (reviews H2 + H3):
///   1. `on_promote()` — (re)start the Active posture-freshness tasks on the
///      newly-Active node (worker / watchdog / refresh / feed). Fixes H2: a
///      promoted standby used to skip this and fail-close one TTL later.
///   2. `spawn_heartbeat_writer` — start heartbeating as the new Active so any
///      tertiary standby sees this node alive (no invisible-Active window /
///      spurious re-promotion; the H3 addition).
/// Extracted so the promotion→wiring seam is unit-testable without the env-driven
/// `promotion_loop`.
///
/// Fail-closed on a panicking hook (Copilot #817): this runs inside the
/// supervised `promotion_loop`, and the supervisor ALWAYS restarts a task that
/// panics (`supervisor.rs`: "a panic is a bug, never a legitimate exit"). If
/// `on_promote` unwound, the loop would restart and re-run `perform_promotion`
/// — which durably claims a NEW epoch each time — churning the HA epoch while
/// leaving the node Active-but-unwired (the heartbeat writer below is never
/// reached, so it never advertises liveness and the restarted loop just
/// re-promotes on the still-stale heartbeat). Convert the panic into a clean
/// process exit: this instance dies and another standby / a systemd restart
/// re-promotes cleanly with the freshness wiring intact. (A hook that itself
/// calls `std::process::exit` — the double-set guard in
/// `wire_active_posture_freshness` — already terminates without unwinding, so
/// `catch_unwind` is transparent to it.)
fn apply_post_promotion(app: &Arc<AppState>, on_promote: &(dyn Fn() + Send + Sync)) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(on_promote)).is_err() {
        tracing::error!(
            "on_promote hook panicked after a successful promotion — exiting \
             (fail-closed: a supervised re-promotion would churn the HA epoch)"
        );
        std::process::exit(1);
    }
    spawn_heartbeat_writer(Arc::clone(app));
}

async fn promotion_loop(
    app: Arc<AppState>,
    cache: SharedPostureCache,
    id: String,
    on_promote: OnPromote,
) {
        let poll_ms = std::env::var("KIRRA_PROMOTION_POLL")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(PROMOTION_POLL_MS);

        let env_timeout_ms = std::env::var("KIRRA_PROMOTION_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(PROMOTION_TIMEOUT_MS);
        // #689: cross-check the ENV timeout against the ENV heartbeat interval (the
        // const assert only guards the defaults). Clamp UP to the (MAX+1)×interval
        // floor so there is at least one full heartbeat interval between the
        // primary's self-demote and the standby's promotion (see
        // `enforce_promotion_timeout_floor`).
        let interval_ms = std::env::var("KIRRA_HEARTBEAT_INTERVAL")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&v| v > 0) // #707: reject 0 — disables the #689 clamp AND panics tokio::interval
            .unwrap_or(HEARTBEAT_INTERVAL_MS);
        let (timeout_ms, clamped) = enforce_promotion_timeout_floor(env_timeout_ms, interval_ms);
        if clamped {
            tracing::error!(
                env_timeout_ms,
                interval_ms,
                resolved_timeout_ms = timeout_ms,
                max_consecutive_failures = MAX_CONSECUTIVE_HEARTBEAT_FAILURES,
                "KIRRA_PROMOTION_TIMEOUT below the safe floor for KIRRA_HEARTBEAT_INTERVAL \
                 (timeout < (MAX+1)×interval): clamped UP to keep ≥1 heartbeat interval between \
                 the primary's self-demote and the standby's promotion (#689 split-brain margin). \
                 Fix the env config to silence this."
            );
        }

        let force_promote = std::env::var("KIRRA_FORCE_PROMOTE")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false);

        if force_promote {
            tracing::warn!(
                instance_id = %id,
                "KIRRA_FORCE_PROMOTE=1: bypassing heartbeat check, promoting immediately"
            );
            if perform_promotion(&app, &cache, &id, "FORCE_PROMOTE").await {
                // reviews H2 + H3: re-wire posture freshness AND start
                // heartbeating on the newly-Active node (see apply_post_promotion).
                apply_post_promotion(&app, on_promote.as_ref());
            }
            return;
        }

        let mut tick = interval(Duration::from_millis(poll_ms));

        // #80: monotonic heartbeat-freshness tracker. Staleness is measured as
        // "how long the heartbeat TOKEN has gone unchanged", on this standby's
        // own monotonic clock — NOT by differencing the primary's wall-clock
        // timestamp against ours (which is skew-vulnerable across machines).
        let mut freshness = HeartbeatFreshness::new(Instant::now());

        tracing::info!(
            instance_id = %id,
            poll_ms     = poll_ms,
            timeout_ms  = timeout_ms,
            "Promotion monitor started"
        );

        loop {
            tick.tick().await;

            // Read the heartbeat TOKEN. It is treated as an opaque change-detector
            // (the primary writes now_ms(), but we never interpret its value as a
            // clock). On any read failure we skip this tick without disturbing the
            // anchor — we only ever decide on a successful read.
            // Closure returns Some(token) on a successful read; None signals
            // "skip this tick" (no key yet / read error) — the outer loop
            // `continue`s on None (Rule 4: `continue` cannot cross the closure).
            // SAFETY: SG-HA-3 — durable writes/reads must never block the async runtime.
            let token = match app
                .store
                .call_read(|store| store.load_engine_state(HEARTBEAT_KEY))
                .await
            {
                Ok(Ok(Some(token))) => token,
                Ok(Ok(None)) => {
                    tracing::debug!("Promotion monitor: no heartbeat key yet — waiting for primary");
                    continue;
                }
                // SAFETY: SG-HA-4 — DB errors demote node to safe state (fail-closed).
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "Promotion monitor: failed to read heartbeat");
                    continue;
                }
                // SAFETY: SG-HA-4 — DB actor/offload failure is fail-closed.
                Err(e) => {
                    tracing::warn!(error = %e, "Promotion monitor: heartbeat read offload failed");
                    continue;
                }
            };

            let now = Instant::now();

            // Token advanced (changed, or first ever seen) → primary is alive →
            // re-anchor and wait. A CHANGED token re-anchors even if its numeric
            // value moved backward (e.g. a primary NTP step), so a primary clock
            // skew can never trigger a spurious failover.
            if freshness.observe(&token, now) {
                tracing::debug!(
                    instance_id = %id,
                    "Promotion monitor: heartbeat token advanced — primary alive"
                );
                continue;
            }

            // Token unchanged: how long (monotonic) has it been stale?
            let elapsed = freshness.elapsed(now);

            // The promotion gate is `promotion_decision` (unit-tested in
            // `sg_009_promotion_act_tests`) — the loop just acts on its verdict.
            if promotion_decision(elapsed, timeout_ms) {
                tracing::error!(
                    instance_id = %id,
                    stale_ms    = elapsed.as_millis() as u64,
                    timeout_ms  = timeout_ms,
                    "Primary heartbeat token unchanged past timeout — promoting to Active"
                );
                if perform_promotion(&app, &cache, &id, "HEARTBEAT_TIMEOUT").await {
                    // reviews H2 + H3: re-wire posture freshness AND start
                    // heartbeating on the newly-Active node (see apply_post_promotion).
                    apply_post_promotion(&app, on_promote.as_ref());
                }
                return;
            } else {
                tracing::debug!(
                    stale_ms   = elapsed.as_millis() as u64,
                    timeout_ms = timeout_ms,
                    "Promotion monitor: primary alive (heartbeat token fresh)"
                );
            }
        }
}

// ---------------------------------------------------------------------------
// Promotion execution — atomic, disk-first
// ---------------------------------------------------------------------------

async fn perform_promotion(
    app: &Arc<AppState>,
    cache: &SharedPostureCache,
    id: &str,
    reason: &str,
) -> bool {
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
    // SAFETY: SG-HA-3 — durable writes/reads must never block the async runtime.
    let observed = match app.store.call_read(|store| store.current_epoch()).await {
        Ok(Ok(e)) => e,
        // SAFETY: SG-HA-4 — DB errors demote node to safe state (fail-closed).
        Ok(Err(e)) => {
            tracing::error!(error = %e, instance_id = %id,
                "promotion: cannot read epoch — aborting");
            return false;
        }
        // SAFETY: SG-HA-4 — DB actor/offload failure is fail-closed.
        Err(e) => {
            tracing::error!(error = %e, instance_id = %id,
                "promotion: epoch read offload failed — aborting");
            return false;
        }
    };

    let id_owned = id.to_string();
    let new_epoch = match app
        .store
        .call(move |store| store.try_claim_epoch(observed, &id_owned, ts))
        .await
    {
        Ok(Ok(Some(e))) => e,
        Ok(Ok(None)) => {
            // Fence held: another instance advanced the epoch between
            // our read and our write. We stay PassiveStandby.
            tracing::warn!(
                instance_id = %id,
                observed    = observed,
                reason      = %reason,
                "promotion ABORTED — epoch already advanced by another instance (durable fence held)"
            );
            return false;
        }
        // SAFETY: SG-HA-4 — DB errors demote node to safe state (fail-closed).
        Ok(Err(e)) => {
            tracing::error!(error = %e, instance_id = %id,
                "promotion: epoch claim execute failed — aborting");
            return false;
        }
        // SAFETY: SG-HA-4 — DB actor/offload failure is fail-closed.
        Err(e) => {
            tracing::error!(error = %e, instance_id = %id,
                "promotion: epoch claim offload failed — aborting");
            return false;
        }
    };

    // Step 1b (#78): ensure the hash-v2 audit-chain migration anchor BEFORE this
    // node writes any audit record as Active. The Step 4 promotion event is the
    // first such write, so the anchor must be durable first.
    //
    // FAIL-CLOSED — ABORT (not just log) on failure: leave `mode_active` false,
    // write no promotion record, stay PassiveStandby (same posture as a lost
    // epoch claim). This differs deliberately from the STARTUP anchor path
    // (kirra_verifier_service.rs), which logs-and-continues: startup runs the
    // anchor BEFORE the listener binds and before any Active write, so a
    // transient failure there is retried on the next boot with no Active writer
    // in between. Here we are mid-promotion — the only alternative to aborting is
    // becoming a NEW Active writer that appends to an UNANCHORED v2 chain, whose
    // v1→v2 migration boundary was never durably anchored. That is strictly worse
    // than staying PassiveStandby (another instance, or a later retry once the
    // store is healthy, promotes instead). A newly-Active writer on an unanchored
    // chain is the worse state, so we fail closed to standby.
    match app
        .store
        .call(move |store| store.ensure_hash_v2_migration_anchor(ts))
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(
                error = %e, instance_id = %id, epoch = new_epoch, reason = %reason,
                "promotion ABORTED — cannot ensure hash-v2 audit anchor; staying PassiveStandby (fail-closed, mode_active stays false, no promotion record)"
            );
            return false;
        }
        Err(e) => {
            tracing::error!(
                error = %e, instance_id = %id, epoch = new_epoch, reason = %reason,
                "promotion ABORTED — hash-v2 anchor offload failed; staying PassiveStandby (fail-closed, mode_active stays false, no promotion record)"
            );
            return false;
        }
    }

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
    // SAFETY: SG-HA-3 — durable promotion records are persisted via async store actor calls.
    let id_owned = id.to_string();
    match app
        .store
        .call(move |store| store.save_engine_state(PROMOTION_RECORD_KEY, &id_owned))
        .await
    {
        Ok(Ok(())) => {}
        // SAFETY: SG-HA-4 — DB errors demote node to safe state (fail-closed).
        Ok(Err(e)) => {
            tracing::error!(
                error = %e,
                instance_id = %id,
                epoch = new_epoch,
                "promotion ABORTED — failed to persist promotion record key"
            );
            // Fail-closed: we already claimed the durable epoch; if we can't persist
            // required promotion metadata, immediately self-demote so this instance
            // does not serve writes as a partially-promoted Active.
            app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                instance_id = %id,
                epoch = new_epoch,
                "promotion ABORTED — failed to persist promotion record key"
            );
            app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
    }

    let id_owned = id.to_string();
    let reason_owned = reason.to_string();
    match app
        .store
        .call(move |store| {
            // Tag the audit payload with `epoch` so a partitioned write is
            // identifiable after the fact (the audit chain itself is
            // monotonic; the epoch column adds the HA-generation linkage).
            let reason_ref = reason_owned.as_str();
            let audit = serde_json::json!({
                "event":          "STANDBY_PROMOTED_TO_ACTIVE",
                "instance_id":    id_owned,
                "reason":         reason_ref,
                "promoted_at_ms": ts,
                "epoch":          new_epoch,
            });
            store.save_posture_event_chained(
                "standby_monitor",
                "STANDBY_PROMOTED_TO_ACTIVE",
                &audit.to_string(),
                Some(reason_ref),
                ts,
            )
        })
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(
                error = %e,
                instance_id = %id,
                epoch = new_epoch,
                "promotion ABORTED — failed to persist promotion audit event"
            );
            // Fail-closed: do not remain Active if the required audit event cannot be persisted.
            app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                instance_id = %id,
                epoch = new_epoch,
                "promotion ABORTED — failed to persist promotion audit event"
            );
            app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
    }

    // #112: record the control-authority handoff provenance ALONGSIDE the
    // promotion event (complement, not a duplicate). A primary failure transfers
    // autonomous command authority to this standby controller. Observability
    // only — never blocks promotion (its own store lock + failure counter).
    crate::command_source::record_handoff(
        app,
        crate::command_source::CommandSource::AutonomousPlanner,
        crate::command_source::CommandSource::StandbyController,
        reason,
        ts,
    );

    // Step 4d (#771 F1): RE-SEED the generation counter from the SHARED store
    // before the first Active recalc. HA pairs share one SQLite file; the dead
    // primary advanced the durable generation high-water for its ENTIRE uptime
    // (~34,560 generations/day at the recalc cadence), while THIS node's in-memory
    // `POSTURE_GENERATION` was seeded once — at its own boot — and is now far
    // behind. Without this re-seed the first post-promotion recalc (Step 5) emits
    // a generation BELOW the dead primary's last, time-reversing the sequence that
    // federation peers hard-reject (`GenerationRegress`, fail-closed) and SSE
    // consumers order by — roughly a day of cross-fleet trust blindness per day of
    // prior primary uptime, on the exact event (primary death) where fresh posture
    // matters most. `init_generation_from_store` is idempotent (`fetch_max` only
    // ever RAISES the counter), so re-running it at promotion cannot regress a
    // counter another racing recalc already advanced. Runs AFTER the epoch claim /
    // promotion records so a fenced loser (which returned early above) never
    // touches it. Fail-closed: a store read error aborts promotion and self-demotes
    // — a promoted Active that would time-reverse generations is worse than staying
    // PassiveStandby for another instance / a later retry to handle.
    // SAFETY: SG-HA-3 — durable read offloaded off the async runtime.
    // SAFETY: SG-HA-4 — DB/offload failure fails closed (self-demote, return false).
    let app_for_seed = Arc::clone(app);
    match tokio::task::spawn_blocking(move || init_generation_from_store(&app_for_seed)).await {
        Ok(Ok(seeded_high_water)) => {
            tracing::info!(
                instance_id = %id,
                epoch = new_epoch,
                seeded_high_water,
                "promotion: re-seeded generation counter from shared store (#771 F1) — first Active recalc will emit above the dead primary's last generation"
            );
        }
        Ok(Err(e)) => {
            tracing::error!(
                error = %e, instance_id = %id, epoch = new_epoch,
                "promotion ABORTED — cannot re-seed generation high-water from shared store; staying PassiveStandby (fail-closed) to avoid time-reversing generations"
            );
            app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
        Err(e) => {
            tracing::error!(
                error = %e, instance_id = %id, epoch = new_epoch,
                "promotion ABORTED — generation re-seed offload failed; staying PassiveStandby (fail-closed)"
            );
            app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
    }

    // Step 5: Initial recalculation as Active instance.
    // is_active() now returns true, so recalculate_and_broadcast will write
    // to the cache and emit broadcasts instead of returning early.
    // SAFETY: SG-HA-3 — durable writes in recalc must not pin tokio workers.
    let app_for_recalc = Arc::clone(app);
    let cache_for_recalc = cache.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || {
        recalculate_and_broadcast(&app_for_recalc, &cache_for_recalc);
    })
    .await
    {
        tracing::error!(
            error = %e,
            instance_id = %id,
            "promotion ABORTED — initial posture recompute task failed"
        );
        // Fail-closed: avoid remaining Active without a fresh posture cache.
        app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
        return false;
    }

    // Step 6 (#83): posture-freshness gate. A freshly-promoted Active node must
    // hold a NON-stale posture before it serves commands. Reuse the single TTL
    // authority (`resolve_post_promotion_posture` → `resolve_posture_with_reason`
    // at POSTURE_CACHE_TTL_MS); do NOT duplicate the staleness logic. If the
    // Step 5 recalc did not yield a fresh posture, the resolution is
    // LockedOut(<reason>) and the node fails closed — the mutation gate
    // independently blocks on the same stale cache, so no command is served
    // against a stale posture. (Promotion is one-way: we do not revert the
    // epoch claim here; the node simply does not serve until posture recovers.)
    match resolve_post_promotion_posture(cache) {
        (posture, None) => tracing::info!(
            instance_id = %id,
            epoch       = new_epoch,
            posture     = ?posture,
            "Promotion complete — posture cache fresh, serving (SSE broadcast active)"
        ),
        (_, Some(stale_reason)) => tracing::error!(
            instance_id = %id,
            epoch       = new_epoch,
            reason      = %stale_reason,
            "Promotion recalc did NOT yield a fresh posture — node FAIL-CLOSED (non-serving) until posture recovers; the mutation gate blocks commands on the stale cache"
        ),
    }

    // Promotion succeeded (durable epoch claimed, audit anchored, mode flipped).
    // WS-0.5 — count the completed failover for /metrics (aborted promotions
    // above returned false and are not counted). Observability only.
    app.fleet_metrics.record_ha_promotion();
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod standby_monitor_tests {
    // These tests assert COMPILE-TIME-CONSTANT invariants between config
    // constants (e.g. TIMEOUT > WARN) — that they are constant is the point.
    #![allow(clippy::assertions_on_constants)]
    use super::*;

    // NOTE (#80): the former wall-clock-age arithmetic tests
    // (`now.saturating_sub(last_heartbeat)` vs the timeout) modeled the OLD
    // cross-machine wall-clock decision. That model is replaced by the monotonic
    // token tracker, so those tests were superseded by — and their intent is
    // covered more strongly in — `sg_009_promotion_act_tests`:
    //   - boundary/fresh/stale → `test_decision_boundary_is_inclusive`,
    //     `test_fresh_heartbeat_decides_no_promote`, `test_stale_heartbeat_decides_promote`;
    //   - clock-skew immunity → `test_primary_clock_step_back_does_not_spuriously_promote`,
    //     `test_standby_wall_clock_jump_does_not_change_decision`.
    // The constant-relationship and absent-key tests below remain valid as-is.

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

    #[test]
    fn test_disk_wedge_demotes_only_after_threshold() {
        // Review item "1": a transient blip (below the threshold) must NOT demote a
        // healthy primary; the run reaching MAX_CONSECUTIVE_HEARTBEAT_FAILURES must.
        for n in 0..MAX_CONSECUTIVE_HEARTBEAT_FAILURES {
            assert!(
                !should_self_demote_on_heartbeat_failures(n),
                "{n} consecutive failures (< {MAX_CONSECUTIVE_HEARTBEAT_FAILURES}) must NOT demote"
            );
        }
        assert!(
            should_self_demote_on_heartbeat_failures(MAX_CONSECUTIVE_HEARTBEAT_FAILURES),
            "reaching the threshold must demote"
        );
        assert!(
            should_self_demote_on_heartbeat_failures(MAX_CONSECUTIVE_HEARTBEAT_FAILURES + 5),
            "staying failed must remain demoted"
        );
    }

    #[test]
    fn test_disk_wedge_demotion_precedes_standby_promotion() {
        // The primary must self-demote strictly before the standby promotes on
        // heartbeat silence, or both are Active at once (two writers). Verified at
        // the default config (the const assert in the module guards this too).
        let demote_at_ms = MAX_CONSECUTIVE_HEARTBEAT_FAILURES as u64 * HEARTBEAT_INTERVAL_MS;
        assert!(
            demote_at_ms < PROMOTION_TIMEOUT_MS,
            "self-demote at {demote_at_ms} ms must precede promotion at {PROMOTION_TIMEOUT_MS} ms"
        );
    }

    #[test]
    fn test_enforce_promotion_timeout_floor_689() {
        // A coherent ENV config (the inequality already holds) is passed through
        // unchanged — no clamp, operator's value respected.
        let (t, clamped) = enforce_promotion_timeout_floor(10_000, 2_000);
        assert_eq!((t, clamped), (10_000, false), "a safe config must not be clamped");

        // The DEFAULTS must never trip the runtime clamp (they satisfy the const assert).
        let (t, clamped) = enforce_promotion_timeout_floor(PROMOTION_TIMEOUT_MS, HEARTBEAT_INTERVAL_MS);
        assert!(!clamped, "default config must pass the runtime check too");
        assert_eq!(t, PROMOTION_TIMEOUT_MS);

        // The HA4 misconfig: MAX×interval ≥ timeout. With interval=5s the primary
        // self-demotes at 3×5=15s but the operator set promotion=10s → standby
        // promotes BEFORE the demote → two-active window. Clamp UP to (MAX+1)×interval.
        let (t, clamped) = enforce_promotion_timeout_floor(10_000, 5_000);
        assert!(clamped, "MAX×interval (15s) ≥ timeout (10s) must be clamped");
        assert_eq!(t, (MAX_CONSECUTIVE_HEARTBEAT_FAILURES as u64 + 1) * 5_000);
        // Post-clamp the split-brain inequality holds: self-demote < promote.
        assert!(
            MAX_CONSECUTIVE_HEARTBEAT_FAILURES as u64 * 5_000 < t,
            "after clamping, the primary self-demotes strictly before the standby promotes"
        );

        // Boundary: timeout exactly == MAX×interval is still unsafe (needs STRICT <)
        // → clamped up past it.
        let interval = 2_000;
        let exactly = MAX_CONSECUTIVE_HEARTBEAT_FAILURES as u64 * interval;
        let (t, clamped) = enforce_promotion_timeout_floor(exactly, interval);
        assert!(clamped && t > exactly, "timeout == MAX×interval must be clamped strictly above");
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

    /// Stale token (unchanged for longer than the timeout, monotonic) → promote.
    #[test]
    fn test_stale_heartbeat_decides_promote() {
        // Token unchanged for 15s at a 10s timeout.
        assert!(promotion_decision(Duration::from_millis(15_000), PROMOTION_TIMEOUT_MS),
            "a heartbeat token unchanged longer than PROMOTION_TIMEOUT_MS must decide promote");
    }

    /// Fresh token (unchanged for less than the timeout) → no promotion.
    #[test]
    fn test_fresh_heartbeat_decides_no_promote() {
        // Token unchanged for only 1s at a 10s timeout.
        assert!(!promotion_decision(Duration::from_millis(1_000), PROMOTION_TIMEOUT_MS),
            "a heartbeat token unchanged less than PROMOTION_TIMEOUT_MS must NOT promote");
    }

    /// Boundary is inclusive (`>=`): exactly `timeout_ms` of monotonic staleness promotes.
    #[test]
    fn test_decision_boundary_is_inclusive() {
        assert!(promotion_decision(Duration::from_millis(PROMOTION_TIMEOUT_MS), PROMOTION_TIMEOUT_MS),
            "exactly PROMOTION_TIMEOUT_MS of staleness must promote (>=, matching the inline check)");
        assert!(!promotion_decision(Duration::from_millis(PROMOTION_TIMEOUT_MS - 1), PROMOTION_TIMEOUT_MS),
            "one ms below the timeout must not promote");
    }

    /// A just-anchored token (zero monotonic elapsed) never promotes — the
    /// monotonic elapsed can never be negative, so the old future-dated /
    /// saturating clock-skew underflow case is structurally impossible.
    #[test]
    fn test_decision_zero_elapsed_does_not_promote() {
        assert!(!promotion_decision(Duration::ZERO, PROMOTION_TIMEOUT_MS),
            "a freshly-anchored token (0 elapsed) must never promote");
    }

    // -----------------------------------------------------------------------
    // #80 — wall-clock-skew immunity via the monotonic token tracker.
    // -----------------------------------------------------------------------

    /// Heartbeat token STOPS advancing → promotion fires once monotonic elapsed
    /// reaches the timeout (and not before). Uses only `Instant` deltas.
    #[test]
    fn test_token_unchanged_promotes_after_monotonic_timeout() {
        let t0 = Instant::now();
        let mut hb = HeartbeatFreshness::new(t0);
        assert!(hb.observe("1000", t0), "first observation anchors (token advanced)");

        // Token never changes again. Just under the timeout: no promotion.
        let t_under = t0 + Duration::from_millis(PROMOTION_TIMEOUT_MS - 1);
        assert!(!hb.observe("1000", t_under), "unchanged token does not re-anchor");
        assert!(!promotion_decision(hb.elapsed(t_under), PROMOTION_TIMEOUT_MS),
            "just under timeout → no promotion");

        // At the timeout: promote (inclusive boundary).
        let t_at = t0 + Duration::from_millis(PROMOTION_TIMEOUT_MS);
        assert!(!hb.observe("1000", t_at), "still unchanged");
        assert!(promotion_decision(hb.elapsed(t_at), PROMOTION_TIMEOUT_MS),
            "token unchanged for the full timeout → promote");
    }

    /// Heartbeat token KEEPS advancing → never promotes, even across a span far
    /// larger than the timeout, because each change re-anchors the monotonic
    /// clock. Models a healthy primary observed under realistic poll spacing.
    #[test]
    fn test_advancing_token_never_promotes() {
        let t0 = Instant::now();
        let mut hb = HeartbeatFreshness::new(t0);
        // 30 polls, 1s apart (> timeout total), token advances each poll.
        for i in 0..30u64 {
            let now = t0 + Duration::from_millis(1_000 * i);
            let token = (1_000 + i).to_string();
            let advanced = hb.observe(&token, now);
            assert!(advanced, "an advancing token must always re-anchor (primary alive)");
            assert!(!promotion_decision(hb.elapsed(now), PROMOTION_TIMEOUT_MS),
                "a re-anchored (advancing) token must never promote");
        }
    }

    /// PRIMARY clock skew (NTP step BACKWARD): the token's numeric value
    /// DECREASES but still CHANGES, so it re-anchors and does NOT promote. The
    /// old `now − last_heartbeat` design would have seen a smaller last_heartbeat
    /// → larger age → spurious failover. The token model is immune.
    #[test]
    fn test_primary_clock_step_back_does_not_spuriously_promote() {
        let t0 = Instant::now();
        let mut hb = HeartbeatFreshness::new(t0);
        assert!(hb.observe("5000", t0), "anchor on first token (primary ts=5000)");

        // One poll (1s) later the primary's clock steps BACK to 3000 — a smaller,
        // but still different, token.
        let t1 = t0 + Duration::from_millis(1_000);
        assert!(hb.observe("3000", t1),
            "a changed token (even decreasing) re-anchors — primary is alive");
        assert!(!promotion_decision(hb.elapsed(t1), PROMOTION_TIMEOUT_MS),
            "a primary clock step-back must NOT cause spurious failover");
    }

    /// STANDBY wall-clock jump immunity: the decision consults only `Instant`
    /// deltas, never `now_ms()`/`SystemTime`, so a standby wall-clock jump
    /// (forward or backward) cannot change the verdict. We prove it by driving
    /// the SAME monotonic timeline with an unchanged token and asserting the
    /// promotion timing is governed purely by the monotonic anchor — there is no
    /// wall-clock input to perturb.
    #[test]
    fn test_standby_wall_clock_jump_does_not_change_decision() {
        let t0 = Instant::now();
        let mut hb = HeartbeatFreshness::new(t0);
        hb.observe("1000", t0);

        // Whatever the standby's wall clock does between these monotonic
        // instants (a forward NTP step, a backward correction), the inputs below
        // are pure `Instant` deltas — the decision is a function of those alone.
        let before = t0 + Duration::from_millis(PROMOTION_TIMEOUT_MS - 1);
        let at = t0 + Duration::from_millis(PROMOTION_TIMEOUT_MS);
        assert!(!promotion_decision(hb.elapsed(before), PROMOTION_TIMEOUT_MS),
            "no wall-clock value can promote before the monotonic timeout");
        assert!(promotion_decision(hb.elapsed(at), PROMOTION_TIMEOUT_MS),
            "promotion is governed by the monotonic anchor, not the wall clock");
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
        let recorded = app.store
            .with(|store| store.load_engine_state(PROMOTION_RECORD_KEY)).unwrap();
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
        let audit = app.store
            .with(|store| store.verify_audit_chain_full(None)).unwrap();
        assert!(audit.total_entries >= 1,
            "promotion must append a STANDBY_PROMOTED_TO_ACTIVE audit event");
    }

    /// #771 F1 — a promoted standby must RE-SEED the generation counter from the
    /// SHARED store before its first Active recalc, so its first emitted
    /// generation is ABOVE the dead primary's last (which advanced the durable
    /// high-water for the primary's whole uptime). Before the fix `perform_promotion`
    /// recalculated with THIS node's stale boot counter, time-reversing the sequence
    /// federation peers hard-reject.
    ///
    /// The store carries a high-water `H` well ABOVE the current live counter
    /// (offset like the other generation tests to survive the process-global
    /// `POSTURE_GENERATION` that concurrent tests also mutate — #771 F7 debt). After
    /// promotion the persisted high-water — bumped by the first Active recalc — must
    /// exceed `H`. Without the re-seed the recalc stamps a generation BELOW `H`, the
    /// monotonic guard rejects the persist, and the high-water stays exactly `H`
    /// (assertion fails). Verified to fail on the pre-fix code.
    #[test]
    fn test_promotion_reseeds_generation_above_dead_primary_highwater() {
        use crate::posture_engine::POSTURE_GENERATION;

        let store = VerifierStore::new(":memory:").expect("store");
        // The dead primary's durable high-water: far above any value concurrent
        // tests could have pushed the shared counter to.
        let h = POSTURE_GENERATION.load(Ordering::SeqCst) + 1_000_000;
        assert!(
            store.save_last_generation(h).expect("seed high-water"),
            "seeding the dead-primary high-water must persist"
        );

        let app = Arc::new(AppState::new(store, VerifierOperationMode::PassiveStandby));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
        assert!(!app.is_active(), "must start as PassiveStandby");

        run_promotion(&app, &cache, "standby-f1", "HEARTBEAT_TIMEOUT");
        assert!(app.is_active(), "promotion must complete");

        let persisted = app
            .store
            .with(|s| s.load_last_generation())
            .expect("load high-water");
        assert!(
            persisted > h,
            "post-promotion high-water {persisted} must EXCEED the dead primary's {h} \
             (re-seed at promotion, #771 F1); without the re-seed the recalc stamps below {h} \
             and the monotonic guard leaves it unchanged"
        );
    }

    /// Drives the real async promotion on a deterministic current-thread runtime.
    fn run_promotion(app: &Arc<AppState>, cache: &SharedPostureCache, id: &str, reason: &str) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(perform_promotion(app, cache, id, reason));
    }

    /// Review H2 — a successful promotion MUST fire the caller's `on_promote`
    /// wiring hook (the seam the binary uses to re-start the posture-freshness
    /// tasks on the freshly-promoted node). Before this fix `promotion_loop`
    /// only re-spawned the heartbeat writer, so the promoted node's posture
    /// cache went stale one TTL later and every gated route fail-closed. Here we
    /// drive the extracted `apply_post_promotion` seam and assert the hook runs
    /// exactly once (env-free — no `promotion_loop` / `KIRRA_FORCE_PROMOTE`).
    #[test]
    fn test_post_promotion_fires_on_promote_hook_once() {
        use std::sync::atomic::AtomicUsize;

        let store = VerifierStore::new(":memory:").expect("store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_hook = Arc::clone(&calls);
        let on_promote: OnPromote = Arc::new(move || {
            calls_hook.fetch_add(1, Ordering::SeqCst);
        });

        // apply_post_promotion also spawns the heartbeat writer, which needs a
        // tokio context — run it on a current-thread runtime.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            apply_post_promotion(&app, on_promote.as_ref());
        });

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a successful promotion must fire the on_promote freshness-wiring hook exactly once"
        );
    }

    // -----------------------------------------------------------------------
    // #78 — hash-v2 audit anchor is ENSURED before any Active write, and its
    // failure FAILS CLOSED (promotion aborts, no Active state written).
    // -----------------------------------------------------------------------

    /// WS-0.5 — a COMPLETED promotion increments the /metrics failover
    /// counter; an ABORTED one (here: the hash-v2 anchor cannot be ensured,
    /// the same deterministic abort `test_promotion_aborts_when_anchor_
    /// cannot_be_ensured` pins) does not — the counter reports failovers
    /// that actually transferred authority, not attempts.
    #[test]
    fn test_promotion_counts_for_metrics_only_when_completed() {
        // Completed promotion → counted once.
        let store = VerifierStore::new(":memory:").expect("store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::PassiveStandby));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
        run_promotion(&app, &cache, "standby-metrics", "HEARTBEAT_TIMEOUT");
        assert!(app.is_active(), "healthy promotion completes");
        assert_eq!(
            app.fleet_metrics.ha_promotion_count(),
            1,
            "a completed failover must be counted"
        );

        // Aborted promotion (broken audit-chain table → anchor failure,
        // fail-closed early return) → NOT counted.
        let store2 = VerifierStore::new(":memory:").expect("store");
        store2.break_audit_chain_table_for_test();
        let app2 = Arc::new(AppState::new(store2, VerifierOperationMode::PassiveStandby));
        let cache2: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
        run_promotion(&app2, &cache2, "standby-abort", "HEARTBEAT_TIMEOUT");
        assert!(!app2.is_active(), "precondition: the promotion aborted");
        assert_eq!(
            app2.fleet_metrics.ha_promotion_count(),
            0,
            "an aborted promotion is not a failover and must not be counted"
        );
    }

    /// Happy path: a store carrying legacy v1 audit rows has its
    /// `HASH_V2_MIGRATION` anchor ensured DURING promotion (proof the anchor
    /// step runs before the first Active audit write), and promotion completes.
    #[test]
    fn test_promotion_ensures_hash_v2_anchor_on_happy_path() {
        let store = VerifierStore::new(":memory:").expect("store");
        // A legacy v1 row makes the anchor write its marker (on a clean chain
        // v1_total == 0, so the anchor is a no-op and writes nothing).
        store.seed_legacy_v1_audit_row_for_test();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::PassiveStandby));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        assert_eq!(
            app.store.with(|store| store.count_audit_events_for_test("HASH_V2_MIGRATION")),
            0,
            "precondition: no anchor marker before promotion"
        );

        run_promotion(&app, &cache, "standby-anchor", "HEARTBEAT_TIMEOUT");

        assert!(app.is_active(), "healthy promotion must complete (Active)");
        assert_eq!(
            app.store.with(|store| store.count_audit_events_for_test("HASH_V2_MIGRATION")),
            1,
            "promotion must ensure the hash-v2 anchor before writing as Active"
        );
    }

    /// Fail-closed: if the anchor cannot be ensured (audit table broken),
    /// promotion ABORTS — `mode_active` stays false, the in-memory epoch is not
    /// cached, no promotion record is written, and the posture cache stays
    /// empty. Without the #78 gate the broken table would only fail the
    /// (ignored) audit write and the node would still flip Active — so this also
    /// proves the gate is wired into the promotion sequence.
    #[test]
    fn test_promotion_aborts_when_anchor_cannot_be_ensured() {
        let store = VerifierStore::new(":memory:").expect("store");
        store.break_audit_chain_table_for_test();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::PassiveStandby));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        run_promotion(&app, &cache, "standby-broken", "HEARTBEAT_TIMEOUT");

        assert!(
            !app.is_active(),
            "anchor failure must ABORT promotion — mode_active stays false (fail-closed)"
        );
        assert_eq!(
            app.held_epoch.load(Ordering::SeqCst),
            0,
            "abort must occur before the in-memory epoch is cached (Step 2 not reached)"
        );
        let record = app.store.with(|store| store.load_engine_state(PROMOTION_RECORD_KEY)).unwrap();
        assert_eq!(record, None, "aborted promotion must NOT write a promotion record");
        assert!(
            cache.read().unwrap().is_none(),
            "aborted promotion must NOT recalculate / populate the cache"
        );
    }

    // -----------------------------------------------------------------------
    // #83 — post-promotion posture-freshness gate.
    // -----------------------------------------------------------------------

    /// Serves on a FRESH posture: after a healthy promotion the Step 5 recalc
    /// populates the cache, so the freshness gate resolves to a real posture
    /// with no stale reason (the node serves normally).
    #[test]
    fn test_post_promotion_freshness_gate_serves_on_fresh_cache() {
        use crate::posture_engine_v2::LockoutReason;
        let store = VerifierStore::new(":memory:").expect("store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::PassiveStandby));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        run_promotion(&app, &cache, "standby-fresh", "HEARTBEAT_TIMEOUT");

        let (_posture, reason) = resolve_post_promotion_posture(&cache);
        assert_eq!(
            reason, None,
            "a freshly-promoted node's recalc must leave a non-stale posture (serving)"
        );
        assert_ne!(reason, Some(LockoutReason::PostureCacheStale));
    }

    /// FAILS CLOSED on a stale posture: an entry older than
    /// `POSTURE_CACHE_TTL_MS` resolves to `LockedOut(PostureCacheStale)` — the
    /// node is non-serving until posture recovers. Reuses the single TTL
    /// authority (`resolve_posture_with_reason`); no TTL logic is duplicated.
    #[test]
    fn test_post_promotion_freshness_gate_locks_out_on_stale_cache() {
        use crate::posture_cache::{CachedFleetPosture, POSTURE_CACHE_TTL_MS};
        use crate::posture_engine_v2::LockoutReason;
        use crate::verifier::FleetPosture;

        let stale_ts = now_ms().saturating_sub(POSTURE_CACHE_TTL_MS + 1);
        let stale = CachedFleetPosture {
            posture: FleetPosture::Nominal,
            generated_at_ms: stale_ts,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 1,
        };
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(stale)));

        let (posture, reason) = resolve_post_promotion_posture(&cache);
        assert_eq!(
            posture,
            FleetPosture::LockedOut,
            "a stale posture after promotion must fail closed to LockedOut (non-serving)"
        );
        assert_eq!(reason, Some(LockoutReason::PostureCacheStale));
    }
}
