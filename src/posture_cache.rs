// src/posture_cache.rs — CachedFleetPosture definition
use ed25519_dalek::VerifyingKey;
use crate::fabric::router::FabricRouter;
use crate::fabric::telemetry::FabricTelemetry;
use crate::fabric::causal_log::FabricCausalLog;
//
// v2.2.2 — Temporal hardening patch
//
// CORRECTIONS vs. milestone doc:
//
//   1. Field name is `generated_at_ms`, NOT `updated_at_ms`.
//      Every existing patch (policy_layer.rs, posture_engine.rs) reads
//      `cached.generated_at_ms`. Renaming it breaks all those call sites.
//      We keep the field name consistent with what the engine writes.
//
//   2. `ttl_ms` field is RETAINED. The milestone doc dropped it entirely.
//      policy_layer.rs reads `cached.ttl_ms` to evaluate staleness.
//      The TTL is owned by the entry (set by the engine), not hardcoded
//      in the middleware. Dropping it would re-centralize TTL knowledge
//      in the wrong layer.
//
//   3. `CachedFleetPosture::new()` signature kept compatible with existing
//      test infrastructure. A `new_with_generation(posture, generation, now_ms)`
//      constructor is added for engine use without breaking existing callers.
//
//   4. `should_route_command` retains `OperationalCommand` parameter type.
//      The milestone doc replaced it with `required_class: &str`, bypassing
//      the Unknown early-return and losing type safety (invariant #9 violation).
//
// This file is the single definition of CachedFleetPosture.
// SharedPostureCache = Arc<RwLock<Option<CachedFleetPosture>>> (unchanged).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::verifier::{FleetPosture, AppState};
use crate::gateway::policy::OperationalCommand;

/// Staleness TTL for the posture cache (milliseconds).
/// After `generated_at_ms + POSTURE_CACHE_TTL_MS < now`, the cache entry is
/// considered stale and resolve_posture fails closed.
pub const POSTURE_CACHE_TTL_MS: u64 = 5_000;

/// Cadence at which the Active primary's liveness loop sends a
/// `PostureRecalcTrigger::PeriodicRefresh` to re-stamp the cache. MUST be
/// strictly less than POSTURE_CACHE_TTL_MS — set to ~half the TTL so the
/// gate's staleness check has comfortable headroom for jitter or a single
/// missed tick. Without this refresh the gate fail-closes one TTL after the
/// last event-driven recalc and the service goes dark (503 fleet-wide).
pub const POSTURE_REFRESH_INTERVAL_MS: u64 = POSTURE_CACHE_TTL_MS / 2;

const _: () = assert!(
    POSTURE_REFRESH_INTERVAL_MS < POSTURE_CACHE_TTL_MS,
    "POSTURE_REFRESH_INTERVAL_MS must be strictly less than POSTURE_CACHE_TTL_MS"
);

// ---------------------------------------------------------------------------
// CachedFleetPosture
// ---------------------------------------------------------------------------

/// A complete, immutable snapshot of the fleet posture at a point in time.
///
/// This struct is atomically replaced (never field-mutated) by
/// `recalculate_and_broadcast`. Readers always observe a consistent snapshot.
///
/// Field ownership:
///   - `posture`          — derived by `derive_fleet_posture` in posture_engine.rs
///   - `generated_at_ms`  — timestamp set by the engine at write time
///   - `ttl_ms`           — staleness window set by the engine (POSTURE_CACHE_TTL_MS)
///   - `generation`       — monotonic counter from `next_generation()` in posture_engine.rs
///
/// The middleware (`resolve_posture`) reads `generated_at_ms` and `ttl_ms`
/// to evaluate staleness. It does not own either value.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CachedFleetPosture {
    /// The current aggregated system-wide posture derived from the DAG.
    pub posture: FleetPosture,

    /// Absolute timestamp (ms since UNIX epoch) when this snapshot was computed.
    /// Named `generated_at_ms` to distinguish from any external "update" event.
    /// This is when the *engine* computed it, not when a sensor last reported.
    pub generated_at_ms: u64,

    /// Staleness TTL in milliseconds. After `generated_at_ms + ttl_ms < now`,
    /// `resolve_posture` treats this entry as stale and fails closed.
    /// Set by the engine from `POSTURE_CACHE_TTL_MS` — not by the middleware.
    pub ttl_ms: u64,

    /// Monotonically increasing generation counter. Strictly increasing within
    /// a process lifetime; persisted across restarts (see posture_engine_v2.rs).
    /// Useful for ordering guarantees, stale-cache debugging, and federation
    /// reconciliation.
    pub generation: u64,
}

impl CachedFleetPosture {
    /// Constructs a new cache entry with engine-assigned fields.
    ///
    /// Used by `recalculate_and_broadcast` — callers supply the generation
    /// (from `next_generation()`) and timestamp (from `now_ms()`).
    pub fn new_with_generation(posture: FleetPosture, generation: u64, now_ms: u64) -> Self {
        Self {
            posture,
            generated_at_ms: now_ms,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation,
        }
    }

    /// Convenience constructor for tests and cold-start initialization.
    /// Uses generation=0 (sentinel for "no engine recalculation has landed
    /// yet") and the current system time. `next_generation()` always returns
    /// >= 1, so a generation=0 seed is guaranteed to be superseded by the
    /// > first real engine write — required for the monotonic-replace check
    /// > in `replace_cache_if_newer` to accept the first recalc result.
    /// > For production engine writes, use `new_with_generation` instead.
    pub fn new(posture: FleetPosture) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            posture,
            generated_at_ms: now,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 0,
        }
    }


    /// Returns true if this entry has exceeded its TTL relative to `now_ms`.
    // SAFETY: SG9 | REQ: ttl-staleness-detection | TEST: test_entry_beyond_ttl_is_stale,test_entry_exactly_at_ttl_boundary_is_stale,test_backward_clock_step_is_stale_not_fresh_b11
    // (≅ AEGIS SG-005. Comparison threshold is the entry's owned ttl_ms.)
    pub fn is_stale(&self, now_ms: u64) -> bool {
        // B11: a BACKWARD clock step (`now_ms < generated_at_ms` — e.g. an NTP step
        // back) makes the entry's age indeterminate. The prior `saturating_sub`
        // clamped that to age 0 and read the entry as FRESH — a fail-OPEN that
        // serves a stale posture as current until the wall clock catches back up.
        // Fail closed: an un-ageable entry is stale (→ LockedOut at the gate), and
        // the next posture refresh re-stamps it to clear the condition. A FORWARD
        // jump only inflates the age → stale → already fail-closed.
        match now_ms.checked_sub(self.generated_at_ms) {
            Some(age) => age >= self.ttl_ms,
            None => true,
        }
    }
}

/// The shared posture cache type.
///
/// `Arc<RwLock<Option<CachedFleetPosture>>>`:
///   - `Arc` — shared ownership across ServiceState, handlers, middleware
///   - `RwLock` — concurrent reads, exclusive writes
///   - `Option` — `None` = cold start / cache cleared (fail-closed in middleware)
///   - `CachedFleetPosture` — complete atomic snapshot (never partially updated)
pub type SharedPostureCache = std::sync::Arc<std::sync::RwLock<Option<CachedFleetPosture>>>;

/// Shared service state threaded through all axum handlers.
pub struct ServiceState {
    pub app: Arc<AppState>,
    pub posture_cache: SharedPostureCache,
    /// #395 console runtime — boot timestamp (ms since epoch) captured once at
    /// `main` startup. Read-only; powers the live console `uptime_ms`.
    pub started_at_ms: u64,
    pub audit_verifying_key: Option<VerifyingKey>,
    pub fabric_router: Arc<FabricRouter>,
    pub fabric_telemetry: Arc<FabricTelemetry>,
    pub fabric_causal_log: Arc<FabricCausalLog>,
    /// Channel to the serialized posture-engine worker. Populated by the
    /// Active startup path via `OnceLock::set` AFTER the `ServiceState` is
    /// already wrapped in `Arc` (the worker spawn needs Arc<AppState>).
    /// `get() == None` on PassiveStandby (no worker is spawned until the
    /// standby promotes). Handlers that mutate trust state, dependency
    /// graph, or other recalc-relevant state call `.get()` and `try_send`
    /// a `PostureRecalcTrigger` so the cache stays in sync with the DAG
    /// truth. A `try_send` failure means the worker is gone or its
    /// channel is full; the gate fail-closes on the resulting stale cache.
    pub posture_engine_tx:
        std::sync::OnceLock<crate::posture_engine_v2::PostureEngineSender>,

    /// Track-C perception-derate cap cache (KIRRA-OCCY-PMON-002). The
    /// perception-monitor worker publishes a speed cap here at perception-tick
    /// rate; the actuator verdict surfaces read it O(1) and compose it into the
    /// Nominal-arm contract via `apply_perception_cap`. Present even when the
    /// monitor is disabled (the enabled flag gates use, not allocation).
    pub perception_cap: crate::gateway::perception_monitor::SharedPerceptionCap,

    /// Whether the perception monitor is deployed/enabled. **Defaults false** —
    /// when false, `resolve_perception_cap` returns `None` (state 1: no-op), so
    /// the composition is a pure no-op until a real perception ingest (#126)
    /// wires and enables the monitor. A disabled monitor's absence is NOT a
    /// fault; only a *configured* monitor going silent fails closed (state 3).
    pub perception_monitor_enabled: bool,
}

/// Returns current time as milliseconds since UNIX epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Command routing gate
// ---------------------------------------------------------------------------

/// Routes or blocks an operational command based on fleet posture and cache freshness.
///
/// Invariants preserved:
///   - OperationalCommand::Unknown → false BEFORE posture check (invariant #9)
///   - Stale cache → false (fail-closed, uses entry.is_stale() from CachedFleetPosture)
///   - LockedOut → false for all commands
///   - Degraded → true for ReadTelemetry and the inner-gated ActuatorMotion
///                command only (Option A / ADR-0011); every other write denied
///   - Nominal → true for all except Unknown
///
/// The `now_ms` parameter accepts an injected clock value. In tests, pass
/// `virtual_clock.now_ms()`; in production, pass `crate::clock::SystemClock.now_ms()`.
/// This keeps the function testable without syscall access.
#[must_use]
pub fn should_route_command(
    cache: &Option<CachedFleetPosture>,
    now_ms: u64,
    command: OperationalCommand,
) -> bool {
    // SAFETY: SG9 | REQ: unknown-command-denied | TEST: test_unknown_command_denied_before_posture_check,test_safety_goal_sg_006_unknown_command_denial
    // (≅ AEGIS SG-006.)
    // Invariant #9: Unknown is denied before any posture evaluation.
    // This early return must never be removed.
    if command == OperationalCommand::Unknown {
        return false;
    }

    let Some(entry) = cache.as_ref() else {
        // No cache entry — fail-closed
        return false;
    };

    // SAFETY: SG8 SG9 | REQ: posture-cache-stale-fails-closed | TEST: test_stale_cache_denies_all_non_unknown_commands,test_entry_beyond_ttl_is_stale,test_stale_cache_fails_closed_after_virtual_clock_advance
    // (≅ AEGIS SG-005.)
    // Staleness check uses entry.is_stale() — the TTL is owned by the entry,
    // not hardcoded here. This aligns with policy_layer.rs resolve_posture.
    if entry.is_stale(now_ms) {
        tracing::warn!(
            generated_at_ms = entry.generated_at_ms,
            ttl_ms          = entry.ttl_ms,
            now_ms          = now_ms,
            generation      = entry.generation,
            "should_route_command: cache stale — blocking command"
        );
        return false;
    }

    match entry.posture {
        FleetPosture::Nominal   => true,
        // Degraded admits safe reads AND the inner-gated actuator-motion command
        // (Option A / ADR-0011): `ActuatorMotion` is the ONE write path mounted
        // behind `enforce_actuator_safety_envelope`, whose Degraded branch runs
        // `enforce_degraded_decel_to_stop` (decel-to-stop-and-HOLD, MRC 5.0 m/s).
        // The outer gate defers that command to the inner kinematic gate rather
        // than 503-ing it, so a Degraded vehicle bleeds speed to a controlled
        // stop instead of holding its pre-Degraded speed. Every OTHER WriteState
        // / SystemMutation stays denied here. LockedOut still denies even
        // ActuatorMotion below (deny-all preserved at both gates).
        FleetPosture::Degraded  => matches!(
            command,
            OperationalCommand::ReadTelemetry | OperationalCommand::ActuatorMotion
        ),
        FleetPosture::LockedOut => false,
    }
}

#[cfg(test)]
mod posture_cache_tests {
    use super::*;
    use crate::verifier::FleetPosture;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    #[test]
    fn test_new_entry_is_not_stale() {
        let entry = CachedFleetPosture::new(FleetPosture::Nominal);
        assert!(!entry.is_stale(now_ms()), "brand-new entry must not be stale");
    }

    #[test]
    fn test_entry_beyond_ttl_is_stale() {
        let old_ts = now_ms().saturating_sub(POSTURE_CACHE_TTL_MS + 1);
        let entry = CachedFleetPosture {
            posture: FleetPosture::Nominal,
            generated_at_ms: old_ts,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 1,
        };
        assert!(entry.is_stale(now_ms()), "entry older than TTL must be stale");
    }

    #[test]
    fn test_entry_exactly_at_ttl_boundary_is_stale() {
        // At exactly TTL age the entry is stale (>=, not >).
        let boundary_ts = now_ms().saturating_sub(POSTURE_CACHE_TTL_MS);
        let entry = CachedFleetPosture {
            posture: FleetPosture::Nominal,
            generated_at_ms: boundary_ts,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 1,
        };
        assert!(entry.is_stale(now_ms()));
    }

    #[test]
    fn test_backward_clock_step_is_stale_not_fresh_b11() {
        // B11: an entry stamped at t=10_000 but read at now=5_000 (the wall clock
        // stepped BACKWARD between write and read) is un-ageable. The prior
        // `saturating_sub` read it as age 0 → FRESH (fail-open). It must now be
        // treated as STALE (fail-closed → LockedOut at the gate).
        let entry = CachedFleetPosture {
            posture: FleetPosture::Nominal,
            generated_at_ms: 10_000,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 1,
        };
        assert!(entry.is_stale(5_000),
            "an entry stamped in the future (backward clock step) must be stale, not fresh");
        // Sanity: a normal forward read of the same entry within TTL is still fresh.
        assert!(!entry.is_stale(10_000 + POSTURE_CACHE_TTL_MS - 1),
            "within-TTL forward read must remain fresh");
    }

    #[test]
    fn test_new_with_generation_sets_all_fields() {
        let ts = now_ms();
        let entry = CachedFleetPosture::new_with_generation(FleetPosture::Degraded, 42, ts);
        assert_eq!(entry.posture, FleetPosture::Degraded);
        assert_eq!(entry.generation, 42);
        assert_eq!(entry.generated_at_ms, ts);
        assert_eq!(entry.ttl_ms, POSTURE_CACHE_TTL_MS);
    }

    #[test]
    fn test_new_convenience_constructor_uses_generation_0_sentinel() {
        // generation=0 is the "no engine write yet" sentinel — the first
        // recalculate_and_broadcast (which calls next_generation() returning
        // >= 1) must be able to supersede a seed entry. A non-zero default
        // would collide with the first real generation and the monotonic
        // replace would reject the first recalc — breaking cold-start.
        let entry = CachedFleetPosture::new(FleetPosture::Nominal);
        assert_eq!(entry.generation, 0);
        assert_eq!(entry.ttl_ms, POSTURE_CACHE_TTL_MS);
    }

    #[test]
    fn test_cached_posture_is_serializable() {
        let entry = CachedFleetPosture::new(FleetPosture::Nominal);
        let json = serde_json::to_string(&entry).expect("must serialize");
        let rt: CachedFleetPosture = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(entry.posture, rt.posture);
        assert_eq!(entry.generation, rt.generation);
    }

    // -----------------------------------------------------------------------
    // should_route_command tests
    // -----------------------------------------------------------------------

    fn fresh_cache(posture: FleetPosture) -> Option<CachedFleetPosture> {
        Some(CachedFleetPosture::new(posture))
    }

    fn stale_cache(posture: FleetPosture) -> Option<CachedFleetPosture> {
        let ts = now_ms().saturating_sub(POSTURE_CACHE_TTL_MS + 1);
        Some(CachedFleetPosture {
            posture,
            generated_at_ms: ts,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 1,
        })
    }

    #[test]
    fn test_unknown_command_denied_before_posture_check() {
        // Invariant #9: Unknown blocked even in Nominal posture.
        let cache = fresh_cache(FleetPosture::Nominal);
        assert!(!should_route_command(&cache, now_ms(), OperationalCommand::Unknown));
    }

    #[test]
    fn test_none_cache_denies_all_commands() {
        let ts = now_ms();
        assert!(!should_route_command(&None, ts, OperationalCommand::ReadTelemetry));
        assert!(!should_route_command(&None, ts, OperationalCommand::Unknown));
    }

    #[test]
    fn test_stale_cache_denies_all_non_unknown_commands() {
        let cache = stale_cache(FleetPosture::Nominal);
        let ts = now_ms();
        assert!(!should_route_command(&cache, ts, OperationalCommand::ReadTelemetry),
            "stale Nominal must deny — fail-closed");
    }

    #[test]
    fn test_nominal_posture_allows_read_telemetry() {
        let cache = fresh_cache(FleetPosture::Nominal);
        assert!(should_route_command(&cache, now_ms(), OperationalCommand::ReadTelemetry));
    }

    #[test]
    fn test_degraded_posture_allows_read_and_actuator_motion_only() {
        // Option A / ADR-0011: Degraded admits ReadTelemetry AND the inner-gated
        // ActuatorMotion (deferred to `enforce_degraded_decel_to_stop`), but
        // still denies every other write (WriteState / SystemMutation) and the
        // fail-closed Unknown.
        let cache = fresh_cache(FleetPosture::Degraded);
        let ts = now_ms();
        assert!(should_route_command(&cache, ts, OperationalCommand::ReadTelemetry),
            "Degraded must allow ReadTelemetry");
        assert!(should_route_command(&cache, ts, OperationalCommand::ActuatorMotion),
            "Degraded must defer ActuatorMotion to the inner kinematic gate (Option A)");
        assert!(!should_route_command(&cache, ts, OperationalCommand::WriteState),
            "Degraded must still deny generic WriteState (no inner gate)");
        assert!(!should_route_command(&cache, ts, OperationalCommand::SystemMutation),
            "Degraded must still deny SystemMutation");
        assert!(!should_route_command(&cache, ts, OperationalCommand::Unknown),
            "Degraded must deny Unknown (fail-closed)");
    }

    #[test]
    fn test_lockedout_posture_denies_all_commands() {
        // Deny-all is preserved at the OUTER gate even for ActuatorMotion: under
        // LockedOut the command never reaches the inner envelope (Option A relaxes
        // Degraded ONLY, never LockedOut).
        let cache = fresh_cache(FleetPosture::LockedOut);
        let ts = now_ms();
        assert!(!should_route_command(&cache, ts, OperationalCommand::ReadTelemetry));
        assert!(!should_route_command(&cache, ts, OperationalCommand::ActuatorMotion),
            "LockedOut must deny ActuatorMotion at the outer gate (deny-all preserved)");
        assert!(!should_route_command(&cache, ts, OperationalCommand::WriteState));
    }

    #[test]
    fn test_actuator_motion_fails_closed_on_stale_and_cold_cache() {
        // The Degraded relaxation must NOT weaken the stale/cold fail-closed
        // rule: ActuatorMotion is denied with no fresh posture, exactly like any
        // other command.
        let ts = now_ms();
        assert!(!should_route_command(&None, ts, OperationalCommand::ActuatorMotion),
            "cold cache (None) must deny ActuatorMotion — fail-closed");
        let stale = stale_cache(FleetPosture::Degraded);
        assert!(!should_route_command(&stale, ts, OperationalCommand::ActuatorMotion),
            "stale Degraded cache must deny ActuatorMotion — fail-closed");
    }
}
