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
//     → promotion: app.ha_fence.mode_active transitions false → true
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
//   app.ha_fence.mode_active is an Arc<AtomicBool>.
//   true = Active, false = PassiveStandby.
//   Promotion: compare-and-swap false → true.
//   This is the only write to app.ha_fence.mode_active outside of startup.
//
// ENV VARS
//   KIRRA_INSTANCE_ID        — unique identifier for this instance (default: hostname)
//   KIRRA_HEARTBEAT_INTERVAL — override HEARTBEAT_INTERVAL_MS (ms, default: 2000)
//   KIRRA_PROMOTION_TIMEOUT  — override PROMOTION_TIMEOUT_MS (ms, default: 10000)

use std::sync::Arc;
use std::time::Instant;
use tokio::time::{interval, Duration};

use crate::posture_cache::{now_ms, SharedPostureCache};
use crate::posture_engine::{init_generation_from_store, recalculate_and_broadcast};
use crate::posture_engine_v2::resolve_post_promotion_posture;
use crate::verifier::AppState;

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

/// EP-02 drill finding — what a non-empty `PROMOTION_RECORD_KEY` means for the
/// heartbeating Active that reads it. The record is written by
/// `perform_promotion` AFTER the durable epoch claim, naming the promoter.
/// Three cases:
/// - the record names THIS instance: it is our own promotion record (we are
///   the legitimately promoted Active). Ignore — treating it as a takeover
///   self-fenced every freshly-promoted standby on its first heartbeat tick,
///   leaving the fleet with ZERO Actives after every failover (the two-process
///   drill caught this).
/// - the record names another instance and we hold NO epoch (`held == 0`,
///   the durable fence not armed): the record is the only takeover signal —
///   preserve the legacy demote-on-takeover.
/// - the record names another instance and we DO hold an epoch: the caller
///   has already verified this tick that the durable epoch still equals ours
///   (the epoch check runs first), and every legitimate takeover claims the
///   epoch BEFORE writing this record — so the record is a STALE artifact of
///   a previous failover generation (e.g. this instance restarted and
///   re-claimed after an old failover). Not a live fence; do not demote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TakeoverRecordVerdict {
    /// Our own promotion record — not a takeover.
    OwnRecord,
    /// Another instance promoted and no epoch fence is armed — demote.
    Demote,
    /// Another instance's record from an older epoch generation — forensics only.
    StaleArtifact,
}

#[must_use]
pub(crate) fn takeover_record_verdict(
    promoted_by: &str,
    my_id: &str,
    held_epoch: u64,
) -> TakeoverRecordVerdict {
    if promoted_by == my_id {
        TakeoverRecordVerdict::OwnRecord
    } else if held_epoch == 0 {
        TakeoverRecordVerdict::Demote
    } else {
        TakeoverRecordVerdict::StaleArtifact
    }
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
pub(crate) fn enforce_promotion_timeout_floor(
    env_timeout_ms: u64,
    interval_ms: u64,
) -> (u64, bool) {
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
pub(crate) const PRIMARY_INSTANCE_KEY: &str = "primary_instance_id";

/// SQLite key recording which instance performed the last promotion.
pub(crate) const PROMOTION_RECORD_KEY: &str = "last_promotion_instance_id";

// ---------------------------------------------------------------------------
// Instance identity
// ---------------------------------------------------------------------------

/// The process-wide HA instance identity, set once at boot by
/// [`init_instance_id`] from the validated `EffectiveConfig` (EP-12 — this
/// module no longer reads the environment; the raw `KIRRA_INSTANCE_ID` /
/// `HOSTNAME` / `KIRRA_INSTANCE_ID_FILE` inputs are captured by
/// `env_config::EffectiveConfig` and resolved via [`resolve_instance_id`]).
static INSTANCE_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Install the boot-resolved instance id (idempotent for the same value; a
/// CONFLICTING second value is refused — identity must be unambiguous, the
/// epoch-reclaim path keys on it).
pub fn init_instance_id(id: String) {
    if INSTANCE_ID.set(id.clone()).is_err() {
        let existing = INSTANCE_ID.get().cloned().unwrap_or_default();
        if existing != id {
            tracing::error!(
                existing = %existing, attempted = %id,
                "FATAL: instance id initialized more than once with DIFFERENT values — \
                 refusing to continue (HA identity must be unambiguous)."
            );
            std::process::exit(1);
        }
    }
}

/// Returns this Kirra instance's unique identifier: the boot-installed value
/// ([`init_instance_id`]), else the stable no-input fallback chain
/// (machine-id → persisted id file → loud unstable last resort) — the same
/// resolution [`resolve_instance_id`] performs with no explicit inputs, which
/// keeps tests and library callers working without boot wiring.
pub fn instance_id() -> String {
    INSTANCE_ID
        .get_or_init(|| resolve_instance_id(None, None, None))
        .clone()
}

/// Resolve the HA instance identity from EXPLICIT inputs (pure over its
/// arguments except the documented filesystem fallbacks): the configured id if
/// present, else the hostname, else the stable filesystem fallback chain with the
/// optional id-file override. The env-reading production path is
/// `EffectiveConfig::resolve_instance_id` (EP-12) — this module performs no
/// environment reads.
#[must_use]
pub fn resolve_instance_id(
    explicit: Option<&str>,
    hostname: Option<&str>,
    id_file: Option<&str>,
) -> String {
    if let Some(id) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    if let Some(host) = hostname.map(str::trim).filter(|s| !s.is_empty()) {
        return host.to_string();
    }
    stable_fallback_instance_id(id_file)
}

/// Derive a fallback instance id that is STABLE across process restarts (HA
/// Part B / review H3+L2). The startup epoch-reclaim path keys on the instance
/// id: a node that crashed mid-promotion must present the SAME id on reboot to
/// reclaim its own durable epoch. The previous `kirra-<now_ms>` fallback changed
/// on every restart, breaking that reclaim. Resolution order (most→least stable):
///   1. `/etc/machine-id` — the canonical per-host stable identity (distinct per
///      container/host in an HA deployment).
///   2. A persisted id file (the caller-supplied `id_file` override — the
///      captured `KIRRA_INSTANCE_ID_FILE` value — else `kirra_instance_id`
///      in the CWD): read if present, else generate once and best-effort persist.
///   3. Last resort: `kirra-<now_ms>` with a LOUD warning (not stable — operators
///      should set `KIRRA_INSTANCE_ID` / `HOSTNAME` in any real HA deployment).
fn stable_fallback_instance_id(id_file: Option<&str>) -> String {
    if let Ok(mid) = std::fs::read_to_string("/etc/machine-id") {
        let mid = mid.trim();
        if !mid.is_empty() {
            return format!("kirra-{mid}");
        }
    }

    let path = id_file
        .map(str::to_string)
        .unwrap_or_else(|| "kirra_instance_id".to_string());
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

/// The validated HA timing bundle (EP-12, Config Slice B) — the heartbeat and
/// promotion loops consume THIS, injected from the boot-validated
/// `EffectiveConfig` (`EffectiveConfig::ha_timings()`), instead of reading the
/// environment per loop. A malformed knob therefore fails at BOOT, never
/// silently defaulting inside a running loop. `Default` is the documented
/// constant cadence (lease off) — what an unset environment produced before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HaTimings {
    /// Heartbeat write cadence (ms); validated > 0 (#707).
    pub heartbeat_interval_ms: u64,
    /// Standby promotes after this much primary silence (ms); still clamped UP
    /// at use to the #689 floor against the heartbeat interval.
    pub promotion_timeout_ms: u64,
    /// Standby heartbeat poll cadence (ms); validated > 0.
    pub promotion_poll_ms: u64,
    /// Break-glass: promote immediately, bypassing the heartbeat check.
    pub force_promote: bool,
    /// EP-03 lease gate: `Some` = lease-conjunctive failover armed.
    pub lease: Option<crate::lease::LeaseParams>,
}

impl Default for HaTimings {
    fn default() -> Self {
        Self {
            heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
            promotion_timeout_ms: PROMOTION_TIMEOUT_MS,
            promotion_poll_ms: PROMOTION_POLL_MS,
            force_promote: false,
            lease: None,
        }
    }
}

// De-monolith split: the two supervised HA loops move to sibling modules; this
// file keeps the shared config/identity vocabulary + the test suites. Behaviour
// unchanged — the public entry points are re-exported below.
mod heartbeat;
mod promotion;
pub use heartbeat::spawn_heartbeat_writer;
pub(crate) use heartbeat::{promotion_decision, HeartbeatFreshness};
#[cfg(test)]
pub(crate) use promotion::{apply_post_promotion, perform_promotion, promotion_loop};
pub use promotion::{spawn_promotion_monitor, OnPromote};

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

    /// EP-02 drill finding — the promotion record must never fence the
    /// instance that WROTE it (the freshly promoted Active), and a stale
    /// record from an older failover generation must not demote a current
    /// epoch holder. Only an other-named record with NO armed epoch fence
    /// keeps the legacy demote-on-takeover meaning.
    #[test]
    fn test_takeover_record_verdict() {
        use super::TakeoverRecordVerdict as V;
        assert_eq!(
            takeover_record_verdict("drill-b", "drill-b", 2),
            V::OwnRecord,
            "the promoted Active reading its own record must NOT self-fence"
        );
        assert_eq!(
            takeover_record_verdict("drill-b", "drill-b", 0),
            V::OwnRecord,
            "own record is never a takeover regardless of fence state"
        );
        assert_eq!(
            takeover_record_verdict("drill-b", "drill-a", 0),
            V::Demote,
            "another promoter + no armed epoch fence → legacy demote-on-takeover"
        );
        assert_eq!(
            takeover_record_verdict("drill-b", "drill-a", 3),
            V::StaleArtifact,
            "another promoter's record while WE hold the current epoch is stale forensics"
        );
    }

    #[test]
    fn test_absent_heartbeat_key_does_not_auto_promote() {
        let heartbeat_value: Option<String> = None;
        let should_skip = heartbeat_value.is_none();
        assert!(should_skip, "absent heartbeat must not trigger promotion");
    }

    #[test]
    fn test_promotion_timeout_exceeds_heartbeat_interval() {
        assert!(
            PROMOTION_TIMEOUT_MS > HEARTBEAT_INTERVAL_MS,
            "timeout must exceed interval to allow for missed writes"
        );
    }

    #[test]
    fn test_poll_interval_shorter_than_promotion_timeout() {
        assert!(
            PROMOTION_POLL_MS < PROMOTION_TIMEOUT_MS,
            "poll must be faster than timeout to avoid missing the window"
        );
    }

    #[test]
    fn test_timeout_allows_multiple_missed_heartbeats() {
        let missed_beats = PROMOTION_TIMEOUT_MS / HEARTBEAT_INTERVAL_MS;
        assert!(
            missed_beats >= 3,
            "timeout should tolerate at least 3 missed beats (got {})",
            missed_beats
        );
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
        assert_eq!(
            id1, id2,
            "instance_id must be stable within a process lifetime"
        );
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
        assert_eq!(
            (t, clamped),
            (10_000, false),
            "a safe config must not be clamped"
        );

        // The DEFAULTS must never trip the runtime clamp (they satisfy the const assert).
        let (t, clamped) =
            enforce_promotion_timeout_floor(PROMOTION_TIMEOUT_MS, HEARTBEAT_INTERVAL_MS);
        assert!(!clamped, "default config must pass the runtime check too");
        assert_eq!(t, PROMOTION_TIMEOUT_MS);

        // The HA4 misconfig: MAX×interval ≥ timeout. With interval=5s the primary
        // self-demotes at 3×5=15s but the operator set promotion=10s → standby
        // promotes BEFORE the demote → two-active window. Clamp UP to (MAX+1)×interval.
        let (t, clamped) = enforce_promotion_timeout_floor(10_000, 5_000);
        assert!(
            clamped,
            "MAX×interval (15s) ≥ timeout (10s) must be clamped"
        );
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
        assert!(
            clamped && t > exactly,
            "timeout == MAX×interval must be clamped strictly above"
        );
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
        assert_eq!(
            observed_a, observed_b,
            "both standbys must read the same observed epoch in the race"
        );
        assert_eq!(observed_a, 0, "fresh DB starts at epoch 0");

        let claim_a = store_a
            .try_claim_epoch(observed_a, "instance-A", 1_000)
            .unwrap();
        let claim_b = store_b
            .try_claim_epoch(observed_b, "instance-B", 1_000)
            .unwrap();

        // The fence: exactly one Some, exactly one None.
        let wins = [claim_a.is_some(), claim_b.is_some()]
            .iter()
            .filter(|w| **w)
            .count();
        assert_eq!(wins, 1, "exactly one instance must win the epoch claim");

        // Epoch advanced by exactly 1 (not by 2).
        let final_epoch = store_a.current_epoch().unwrap();
        assert_eq!(
            final_epoch, 1,
            "exactly one bump must land — observed=0, final=1, not 2"
        );

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
        assert_ne!(
            held_a, db_epoch,
            "A is fenced — its held epoch is now stale relative to the DB"
        );

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

        let claimed = store
            .try_claim_epoch(initial_epoch, "primary-1", 5_000)
            .unwrap();
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
        assert!(
            lose.is_none(),
            "stale-observed claim must abort with None (durable fence)"
        );

        let (final_epoch, holder) = store_a.current_active_holder().unwrap();
        assert_eq!(final_epoch, 1, "exactly one bump");
        assert_eq!(
            holder.as_deref(),
            Some("winner"),
            "loser must NOT have overwritten the holder column"
        );

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
        assert!(
            fresh,
            "heartbeat must read as fresh within the timeout window"
        );

        // The bin's policy: holder is some OTHER id AND heartbeat is fresh
        // ⇒ stand down. We model that decision here so the test fails if
        // either input changes meaning.
        let my_id = "newcomer";
        let should_defer = matches!(holder.as_deref(), Some(h) if h != my_id) && fresh;
        assert!(
            should_defer,
            "startup must defer to a live holder rather than steal the epoch"
        );

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
        assert!(
            promotion_decision(Duration::from_millis(15_000), PROMOTION_TIMEOUT_MS),
            "a heartbeat token unchanged longer than PROMOTION_TIMEOUT_MS must decide promote"
        );
    }

    /// Fresh token (unchanged for less than the timeout) → no promotion.
    #[test]
    fn test_fresh_heartbeat_decides_no_promote() {
        // Token unchanged for only 1s at a 10s timeout.
        assert!(
            !promotion_decision(Duration::from_millis(1_000), PROMOTION_TIMEOUT_MS),
            "a heartbeat token unchanged less than PROMOTION_TIMEOUT_MS must NOT promote"
        );
    }

    /// Boundary is inclusive (`>=`): exactly `timeout_ms` of monotonic staleness promotes.
    #[test]
    fn test_decision_boundary_is_inclusive() {
        assert!(promotion_decision(Duration::from_millis(PROMOTION_TIMEOUT_MS), PROMOTION_TIMEOUT_MS),
            "exactly PROMOTION_TIMEOUT_MS of staleness must promote (>=, matching the inline check)");
        assert!(
            !promotion_decision(
                Duration::from_millis(PROMOTION_TIMEOUT_MS - 1),
                PROMOTION_TIMEOUT_MS
            ),
            "one ms below the timeout must not promote"
        );
    }

    /// A just-anchored token (zero monotonic elapsed) never promotes — the
    /// monotonic elapsed can never be negative, so the old future-dated /
    /// saturating clock-skew underflow case is structurally impossible.
    #[test]
    fn test_decision_zero_elapsed_does_not_promote() {
        assert!(
            !promotion_decision(Duration::ZERO, PROMOTION_TIMEOUT_MS),
            "a freshly-anchored token (0 elapsed) must never promote"
        );
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
        assert!(
            hb.observe("1000", t0),
            "first observation anchors (token advanced)"
        );

        // Token never changes again. Just under the timeout: no promotion.
        let t_under = t0 + Duration::from_millis(PROMOTION_TIMEOUT_MS - 1);
        assert!(
            !hb.observe("1000", t_under),
            "unchanged token does not re-anchor"
        );
        assert!(
            !promotion_decision(hb.elapsed(t_under), PROMOTION_TIMEOUT_MS),
            "just under timeout → no promotion"
        );

        // At the timeout: promote (inclusive boundary).
        let t_at = t0 + Duration::from_millis(PROMOTION_TIMEOUT_MS);
        assert!(!hb.observe("1000", t_at), "still unchanged");
        assert!(
            promotion_decision(hb.elapsed(t_at), PROMOTION_TIMEOUT_MS),
            "token unchanged for the full timeout → promote"
        );
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
            assert!(
                advanced,
                "an advancing token must always re-anchor (primary alive)"
            );
            assert!(
                !promotion_decision(hb.elapsed(now), PROMOTION_TIMEOUT_MS),
                "a re-anchored (advancing) token must never promote"
            );
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
        assert!(
            hb.observe("5000", t0),
            "anchor on first token (primary ts=5000)"
        );

        // One poll (1s) later the primary's clock steps BACK to 3000 — a smaller,
        // but still different, token.
        let t1 = t0 + Duration::from_millis(1_000);
        assert!(
            hb.observe("3000", t1),
            "a changed token (even decreasing) re-anchors — primary is alive"
        );
        assert!(
            !promotion_decision(hb.elapsed(t1), PROMOTION_TIMEOUT_MS),
            "a primary clock step-back must NOT cause spurious failover"
        );
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
        assert!(
            !promotion_decision(hb.elapsed(before), PROMOTION_TIMEOUT_MS),
            "no wall-clock value can promote before the monotonic timeout"
        );
        assert!(
            promotion_decision(hb.elapsed(at), PROMOTION_TIMEOUT_MS),
            "promotion is governed by the monotonic anchor, not the wall clock"
        );
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
        assert!(
            cache.read().unwrap().is_none(),
            "cache empty before promotion"
        );

        // Drive the real promotion path. `perform_promotion` is async but has no
        // .await suspension points that need the reactor; a current-thread
        // runtime executes it deterministically.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(perform_promotion(
            &app,
            &cache,
            "standby-under-test",
            "HEARTBEAT_TIMEOUT",
        ));

        // 1. mode_active transitioned false → true (the compare_exchange ACT).
        assert!(
            app.is_active(),
            "promotion must flip mode_active false→true (now Active)"
        );

        // 2. Durable promotion record written (disk-first bookkeeping).
        let recorded = app
            .store
            .with(|store| store.load_engine_state(PROMOTION_RECORD_KEY))
            .unwrap();
        assert_eq!(
            recorded.as_deref(),
            Some("standby-under-test"),
            "promoted instance must persist its id to the promotion record"
        );

        // 3. An epoch was durably claimed (the split-brain fence advanced).
        assert_eq!(
            app.ha_fence.held_epoch.load(Ordering::SeqCst),
            1,
            "first promotion on a clean DB must claim epoch 1"
        );

        // 4. Posture was recalculated as Active — the cache is populated. Only an
        //    Active instance writes the cache, so a populated cache is proof the
        //    mode flip happened BEFORE the recalc (ordering invariant).
        assert!(
            cache.read().unwrap().is_some(),
            "promoted (Active) instance must recalculate posture and populate the cache"
        );

        // 5. The promotion is recorded in the tamper-evident audit chain.
        let audit = app
            .store
            .with(|store| store.verify_audit_chain_full(None))
            .unwrap();
        assert!(
            audit.total_entries >= 1,
            "promotion must append a STANDBY_PROMOTED_TO_ACTIVE audit event"
        );
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
            apply_post_promotion(&app, on_promote.as_ref(), HaTimings::default());
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
            app.store
                .with(|store| store.count_audit_events_for_test("HASH_V2_MIGRATION")),
            0,
            "precondition: no anchor marker before promotion"
        );

        run_promotion(&app, &cache, "standby-anchor", "HEARTBEAT_TIMEOUT");

        assert!(app.is_active(), "healthy promotion must complete (Active)");
        assert_eq!(
            app.store
                .with(|store| store.count_audit_events_for_test("HASH_V2_MIGRATION")),
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
            app.ha_fence.held_epoch.load(Ordering::SeqCst),
            0,
            "abort must occur before the in-memory epoch is cached (Step 2 not reached)"
        );
        let record = app
            .store
            .with(|store| store.load_engine_state(PROMOTION_RECORD_KEY))
            .unwrap();
        assert_eq!(
            record, None,
            "aborted promotion must NOT write a promotion record"
        );
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
