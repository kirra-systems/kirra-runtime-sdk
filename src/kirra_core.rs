// src/kirra_core.rs

use crate::{SafetyContract, SafetyGovernor, GovernorInterceptResult, MitigationCode, TrustMode};
use crate::security::constant_time_compare;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum GlobalSystemState { Normal, Degraded, Failsafe }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BehavioralAction { ExecuteUnrestricted, ApplyVelocityCap, ForceStationaryHold, ExecutePassiveFailsafeLock }

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct JournalEntry {
    pub timestamp_ms: u64,
    pub actor: String,
    pub token: String,
    pub action: String,
    pub resolution: String,
    pub score: u32,
    pub system_state: GlobalSystemState,
    pub trust_mode: TrustMode,
    pub operator_narrative: String,
}

fn default_angular_ceiling() -> f64 { 1.5 }

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy)]
pub struct ContractProfile {
    #[serde(alias = "asset_identifier")]
    pub asset_register_offset: u16,
    pub min_permissible_ceiling: f64,
    pub max_permissible_ceiling: f64,
    #[serde(default = "default_angular_ceiling")]
    pub max_angular_velocity_ceiling: f64,
    pub max_rate_of_change_dt: f64,
    pub fallback_safe_setpoint: f64,
    pub constraint_cap_min: f64,
    pub constraint_cap_max: f64,
    pub engineering_scale_factor: f64,
}

impl SafetyContract for ContractProfile {
    #[inline] fn min_bound(&self) -> f64 { self.min_permissible_ceiling }
    #[inline] fn max_bound(&self) -> f64 { self.max_permissible_ceiling }
    #[inline] fn max_angular_rate(&self) -> f64 { self.max_angular_velocity_ceiling }
    #[inline] fn max_rate(&self) -> f64 { self.max_rate_of_change_dt }
    #[inline] fn fallback(&self) -> f64 { self.fallback_safe_setpoint }
    #[inline] fn scale_factor(&self) -> f64 { self.engineering_scale_factor }
}

/// Consecutive failed supervisor-reset attempts that trip the brute-force cooldown.
pub const RESET_MAX_FAILED_ATTEMPTS: u32 = 5;
/// Brute-force cooldown window (ms) armed once the failed-attempt threshold is hit.
pub const RESET_BRUTE_FORCE_COOLDOWN_MS: u64 = 60_000;

pub struct RuntimeTrustEngine {
    pub current_score: u32,
    pub mode: TrustMode,
    pub consecutive_safe_packets: u32,
    pub recovery_threshold: u32,
    pub failed_reset_attempts: u32,
    pub reset_cooldown_end_ms: u64,
}

impl Default for RuntimeTrustEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeTrustEngine {
    pub fn new() -> Self {
        Self {
            current_score: 100,
            mode: TrustMode::FullAutonomy,
            consecutive_safe_packets: 0,
            recovery_threshold: 50,
            failed_reset_attempts: 0,
            reset_cooldown_end_ms: 0,
        }
    }

    pub fn decay_trust(&mut self, penalty: u32) {
        self.current_score = self.current_score.saturating_sub(penalty);
        self.consecutive_safe_packets = 0;
        self.update_mode();
    }

    pub fn register_safe_tick(&mut self) {
        if self.current_score < 100 {
            self.consecutive_safe_packets = self.consecutive_safe_packets.saturating_add(1);
            if self.consecutive_safe_packets >= self.recovery_threshold {
                let recovery_step = if self.mode == TrustMode::LockedOut { 5 } else { 10 };
                self.current_score = (self.current_score + recovery_step).min(100);
                self.consecutive_safe_packets = 0;
                self.update_mode();
            }
        }
    }

    fn update_mode(&mut self) {
        self.mode = match self.current_score {
            86..=100 => TrustMode::FullAutonomy,
            56..=85  => TrustMode::ConstrainedAdvisory,
            45..=55  => TrustMode::ShadowMode,
            _        => TrustMode::LockedOut,
        };
    }

    pub fn authenticated_manual_reset(&mut self, raw_token: &[u8], system_auth_key: &[u8], current_time_ms: u64) -> Result<(), &'static str> {
        // An armed cooldown window blocks every attempt outright.
        if current_time_ms < self.reset_cooldown_end_ms {
            return Err("RESET_REJECTED_COOLDOWN_ACTIVE");
        }
        // At the threshold, the counter is cleared ONLY once a cooldown has been
        // armed and SERVED. Clearing it unconditionally would let a legitimate
        // supervisor recover (the counter is cleared only on a successful compare
        // below, which the `>= threshold` guard returns before ever reaching — the
        // permanent-lockout bug), BUT it would also let a restart bypass the
        // throttle: the gateway persists `failed_reset_attempts` and NOT the
        // (in-memory) `reset_cooldown_end_ms`, so after a reboot the counter is at
        // threshold with `reset_cooldown_end_ms == 0` and no wait has been served.
        if self.failed_reset_attempts >= RESET_MAX_FAILED_ATTEMPTS {
            if self.reset_cooldown_end_ms == 0 {
                // Threshold reached with NO cooldown on record — the cross-restart /
                // pre-fix-persisted signature. Arm a cooldown and reject so a
                // restart cannot skip the wait; it becomes SERVED (and recoverable)
                // once this window elapses.
                self.reset_cooldown_end_ms = current_time_ms + RESET_BRUTE_FORCE_COOLDOWN_MS;
                return Err("RESET_DISABLED_BRUTE_FORCE_SUSPECTED");
            }
            // A cooldown was armed and (per the guard above) has now elapsed —
            // reopen a fresh attempt window so the correct token recovers.
            self.failed_reset_attempts = 0;
            self.reset_cooldown_end_ms = 0;
        }
        if !constant_time_compare(raw_token, system_auth_key) {
            self.failed_reset_attempts = self.failed_reset_attempts.saturating_add(1);
            // Hitting the threshold arms the cooldown; the window is re-openable
            // (served → fresh attempts above), so this throttles brute force
            // without becoming a permanent lockout.
            if self.failed_reset_attempts >= RESET_MAX_FAILED_ATTEMPTS {
                self.reset_cooldown_end_ms = current_time_ms + RESET_BRUTE_FORCE_COOLDOWN_MS;
                return Err("RESET_DISABLED_BRUTE_FORCE_SUSPECTED");
            }
            return Err("RESET_REJECTED_INVALID_TOKEN");
        }
        self.current_score = 100;
        self.consecutive_safe_packets = 0;
        self.failed_reset_attempts = 0;
        self.reset_cooldown_end_ms = 0;
        self.mode = TrustMode::FullAutonomy;
        Ok(())
    }
}

pub struct KirraKernelGovernor<C: SafetyContract> {
    pub contract: C,
    pub trust_engine: RuntimeTrustEngine,
    pub last_validated_scalar: f64,
    pub continuous_rate_breach_ticks: u32,
    pub constraint_cap_min: f64,
    pub constraint_cap_max: f64,
}

/// Order- and NaN-tolerant clamp (B5). `f64::clamp` PANICS when `lo > hi` or
/// either bound is NaN — and under release `panic = "abort"` that panic is a
/// governor process kill. This normalizes the bound order and ignores a single
/// NaN bound, so a misconfigured/inverted cap or contract bound degrades to a
/// valid clamp instead of aborting the safety governor. (With both bounds NaN
/// the value passes through unclamped, but the demand is already envelope-bounded
/// upstream and the construction-time normalization keeps the stored cap finite.)
#[inline]
fn safe_clamp(value: f64, lo: f64, hi: f64) -> f64 {
    value.max(lo.min(hi)).min(lo.max(hi))
}

impl<C: SafetyContract> KirraKernelGovernor<C> {
    pub fn new(contract: C, initial_scalar: f64, cap_min: f64, cap_max: f64) -> Self {
        // B5: a misconfigured cap — an inverted pair (`cap_min > cap_max`, e.g. a
        // sign typo) or a non-finite bound — would make the Degraded
        // `ApplyVelocityCap` clamp in `evaluate()` panic, and under release
        // `panic = "abort"` that kills the governor process. Normalize at
        // construction so the STORED cap is always a valid ordered range: swap an
        // inverted pair (recovers the likely intent), and let a single NaN collapse
        // to its finite partner via `min`/`max`. `safe_clamp` at the use sites is
        // the runtime backstop for any residual (e.g. both-NaN) case. A startup
        // sentinel remains the right place to FAIL-FAST on such a config.
        let constraint_cap_min = cap_min.min(cap_max);
        let constraint_cap_max = cap_min.max(cap_max);
        // A non-finite `initial_scalar` would poison `last_validated_scalar`: the
        // Degraded decel-to-stop bound reads `current.signum()` / `current.abs()`
        // (#410), so a NaN current emits a NaN setpoint. Normal flow washes it out
        // on the first FullAutonomy tick before Degraded is reachable, but make the
        // stored value finite-by-construction rather than rely on that ordering.
        // Fall back to the contract's safe setpoint (then 0.0 if that is itself
        // non-finite — a deeper config error the startup sentinel should reject).
        let initial_scalar = if initial_scalar.is_finite() {
            initial_scalar
        } else {
            let fb = contract.fallback();
            if fb.is_finite() { fb } else { 0.0 }
        };
        Self {
            contract,
            trust_engine: RuntimeTrustEngine::new(),
            last_validated_scalar: initial_scalar,
            continuous_rate_breach_ticks: 0,
            constraint_cap_min,
            constraint_cap_max,
        }
    }
}

impl<C: SafetyContract> SafetyGovernor for KirraKernelGovernor<C> {
    fn evaluate(&mut self, proposed_demand: f64, dt: f64) -> GovernorInterceptResult {
        // Priority 0 (fail-closed): non-finite input. IEEE-754 `NaN` compares
        // `false` against every envelope/rate threshold, so an unguarded `NaN`
        // would fall straight through the `ExecuteUnrestricted` passthrough as a
        // "safe" command (and a `NaN`/`Inf` `dt` would poison the rate check).
        // Reject explicitly: command the contract fallback, decay trust, and report
        // an UNSAFE control state. `last_validated_scalar` is intentionally NOT
        // advanced to a tainted value. (#404 — this scalar/FFI ingress was the one
        // path that treated `NaN` as nominal; the vehicle path already guards.)
        if !crate::governor_guard::all_finite(&[proposed_demand, dt]) {
            self.trust_engine.decay_trust(30);
            return GovernorInterceptResult {
                sanitized_scalar: self.contract.fallback(),
                asset_in_safe_control_state: false,
                mitigation: MitigationCode::NonfiniteInputRejectedFailsafe,
                was_unsafe_attempt: true,
                was_rate_breached: false,
            };
        }
        // Priority 0b (fail-closed, Gov-M2): a non-positive timestep is not a
        // physical sample. Previously this substituted a fabricated 0.050 s, which
        // SILENTLY under-counted the rate-of-change (an instantaneous large step
        // scored against a 50 ms window could slip past the rate limit). Reject it,
        // mirroring the AV path's `DenyCode::InvalidTimeDelta`. A genuinely small
        // positive dt is kept as-is — a large step over it correctly trips the rate
        // clamp (conservative), so only `dt <= 0.0` fails closed.
        if dt <= 0.0 {
            self.trust_engine.decay_trust(30);
            return GovernorInterceptResult {
                sanitized_scalar: self.contract.fallback(),
                asset_in_safe_control_state: false,
                mitigation: MitigationCode::InvalidTimeDeltaRejectedFailsafe,
                was_unsafe_attempt: true,
                was_rate_breached: false,
            };
        }

        let prior_trust_mode = self.trust_mode();
        let prior_system_state = match prior_trust_mode {
            TrustMode::FullAutonomy => GlobalSystemState::Normal,
            TrustMode::ConstrainedAdvisory | TrustMode::ShadowMode => GlobalSystemState::Degraded,
            TrustMode::LockedOut => GlobalSystemState::Failsafe,
        };
        let active_action = match (prior_system_state, prior_trust_mode) {
            (GlobalSystemState::Normal, TrustMode::FullAutonomy) => BehavioralAction::ExecuteUnrestricted,
            (GlobalSystemState::Degraded, TrustMode::ConstrainedAdvisory) => BehavioralAction::ApplyVelocityCap,
            (GlobalSystemState::Degraded, TrustMode::ShadowMode) => BehavioralAction::ForceStationaryHold,
            _ => BehavioralAction::ExecutePassiveFailsafeLock,
        };

        let is_out_of_envelope = proposed_demand < self.contract.min_bound() || proposed_demand > self.contract.max_bound();
        let delta_step = (proposed_demand - self.last_validated_scalar).abs();
        let rate_change_per_sec = delta_step / dt;
        let is_rate_breached = rate_change_per_sec > self.contract.max_rate();

        if is_rate_breached {
            self.continuous_rate_breach_ticks = self.continuous_rate_breach_ticks.saturating_add(1);
        } else {
            self.continuous_rate_breach_ticks = self.continuous_rate_breach_ticks.saturating_sub(1);
        }

        let mut cumulative_penalty = 0;
        if is_out_of_envelope { cumulative_penalty += 30; }
        if self.continuous_rate_breach_ticks > 5 { cumulative_penalty += 15; }

        let core_bounded_demand = safe_clamp(proposed_demand, self.contract.min_bound(), self.contract.max_bound());

        let (sanitized_scalar, mitigation): (f64, MitigationCode) = match active_action {
            BehavioralAction::ExecuteUnrestricted => {
                if is_out_of_envelope {
                    (core_bounded_demand, MitigationCode::EnvelopeClampTakesPriority)
                } else if is_rate_breached {
                    let step_direction = (proposed_demand - self.last_validated_scalar).signum();
                    // Gov-M1 (invariant #8 — envelope ALWAYS wins over rate): re-clamp
                    // the rate-limited result to the hard envelope. This branch anchors
                    // on `last_validated_scalar`, which a prior Degraded
                    // (`ApplyVelocityCap`) tick may have left outside the contract
                    // envelope when the constructor caps are wider than the bounds; the
                    // unconditional clamp guarantees the emitted scalar is in-envelope,
                    // matching the AV path's `validate_vehicle_command`.
                    let rate_clamped_value = safe_clamp(
                        self.last_validated_scalar + (self.contract.max_rate() * dt * step_direction),
                        self.contract.min_bound(),
                        self.contract.max_bound(),
                    );
                    (rate_clamped_value, MitigationCode::RateClampEnforced { max_rate: self.contract.max_rate() })
                } else {
                    (proposed_demand, MitigationCode::PassthroughUnrestrictedNormal)
                }
            }
            BehavioralAction::ApplyVelocityCap => {
                // #410 residual / issue #70: Degraded is decel-to-stop-and-HOLD, NOT a
                // sustained crawl. The prior pure clamp to [cap_min, cap_max] admitted a
                // speed INCREASE up to the cap, a re-initiation from a stop, and a
                // reversal through zero — the post-stop pullover-drag failure mode the
                // vehicle path's `enforce_degraded_decel_to_stop` (#70) refuses. The
                // scalar kernel governor (the FFI/scalar ingress, #404) was the one
                // enforcement point left as a pure clamp.
                //
                // Enforce the same non-increasing / no-reinit / no-reversal rule on the
                // scalar. Cap/envelope first (defense-in-depth, unchanged), THEN the
                // decel-to-stop bound applied LAST so it is authoritative over any
                // positive `cap_min` floor: emit `sign(current)·min(|capped|,|current|)`.
                // This is epsilon-free yet complete — magnitude never exceeds the current
                // (non-increasing), the sign is forced to the current direction (no
                // reversal), and at a stop (`|current|≈0`) the magnitude term collapses to
                // ~0 (no re-initiation) — while a genuine decelerating-toward-zero command
                // still passes. Unlike the vehicle path there is no separate MRC channel,
                // so a refused increase/reinit/reversal becomes a non-increasing HOLD in
                // the current direction (itself a valid decel trajectory). Recovery is
                // automatic on return to FullAutonomy. The Nominal WCET path is UNCHANGED.
                let current = self.last_validated_scalar;
                let capped = safe_clamp(core_bounded_demand, self.constraint_cap_min, self.constraint_cap_max);
                let held = current.signum() * capped.abs().min(current.abs());
                if (held - capped).abs() > 1e-9 {
                    // The decel-to-stop bound actively overrode the (capped) demand.
                    (held, MitigationCode::DegradedDecelToStopHold { held })
                } else {
                    (held, MitigationCode::DegradedPostureClamp { cap_min: self.constraint_cap_min, cap_max: self.constraint_cap_max })
                }
            }
            BehavioralAction::ForceStationaryHold => {
                (self.last_validated_scalar, MitigationCode::ShadowModeHoldEnforced { retained: self.last_validated_scalar })
            }
            BehavioralAction::ExecutePassiveFailsafeLock => {
                (self.contract.fallback(), MitigationCode::CriticalLockoutFallback)
            }
        };

        if active_action != BehavioralAction::ExecutePassiveFailsafeLock {
            self.last_validated_scalar = sanitized_scalar;
        }

        if cumulative_penalty > 0 {
            self.trust_engine.decay_trust(cumulative_penalty);
        } else {
            self.trust_engine.register_safe_tick();
        }

        let asset_in_safe_control_state = match prior_trust_mode {
            TrustMode::FullAutonomy | TrustMode::ConstrainedAdvisory => !is_out_of_envelope && !is_rate_breached,
            TrustMode::ShadowMode | TrustMode::LockedOut => false,
        };

        GovernorInterceptResult {
            sanitized_scalar,
            asset_in_safe_control_state,
            mitigation,
            was_unsafe_attempt: is_out_of_envelope || self.continuous_rate_breach_ticks > 5,
            was_rate_breached: is_rate_breached,
        }
    }

    #[inline] fn trust_mode(&self) -> TrustMode { self.trust_engine.mode }
    #[inline] fn last_output(&self) -> f64 { self.last_validated_scalar }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct CausalFlightRecorder { pub journal: std::collections::VecDeque<JournalEntry> }

impl Default for CausalFlightRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// Bundled parameters for [`CausalFlightRecorder::log`].
#[derive(Debug, Clone)]
pub struct JournalLogEntry<'a> {
    pub ts: u64,
    pub actor: &'a str,
    pub token: &'a str,
    pub action: &'a str,
    pub res: &'a str,
    pub state: GlobalSystemState,
    pub mode: TrustMode,
    pub score: u32,
    pub narrative: String,
}

impl CausalFlightRecorder {
    pub fn new() -> Self { Self { journal: std::collections::VecDeque::new() } }
    pub fn log(&mut self, entry: JournalLogEntry<'_>) {
        self.journal.push_back(JournalEntry {
            timestamp_ms: entry.ts, actor: entry.actor.to_string(), token: entry.token.to_string(), action: entry.action.to_string(), resolution: entry.res.to_string(),
            score: entry.score, system_state: entry.state, trust_mode: entry.mode, operator_narrative: entry.narrative,
        });
        if self.journal.len() > 100 { self.journal.pop_front(); }
    }
}

#[cfg(test)]
mod governor_nonfinite_tests {
    // #404 — the scalar KirraKernelGovernor must fail-closed on non-finite input
    // (NaN compares false against every threshold, so an unguarded NaN/Inf would
    // pass through as a "safe" command). kirra_core.rs previously had no tests.
    use super::KirraKernelGovernor;
    use crate::kinematics_contract::KinematicContract;
    use crate::{SafetyContract, SafetyGovernor};

    fn gov() -> KirraKernelGovernor<KinematicContract> {
        let contract = KinematicContract {
            max_linear_velocity: 2.0,
            max_angular_velocity: 1.0,
            max_linear_acceleration: 10.0,
            fallback_linear_speed: 0.0,
        };
        KirraKernelGovernor::new(contract, 0.0, -2.0, 2.0)
    }

    #[test]
    fn nan_demand_is_rejected_failsafe_not_passed_through() {
        let mut g = gov();
        let out = g.evaluate(f64::NAN, 0.05);
        assert!(out.sanitized_scalar.is_finite(), "must never return NaN to an actuator");
        assert_eq!(out.sanitized_scalar, g.contract.fallback());
        assert!(!out.asset_in_safe_control_state, "NaN must not read as a safe control state");
        assert!(out.was_unsafe_attempt);
    }

    #[test]
    fn inf_demand_is_rejected_failsafe() {
        let mut g = gov();
        for demand in [f64::INFINITY, f64::NEG_INFINITY] {
            let out = g.evaluate(demand, 0.05);
            assert!(out.sanitized_scalar.is_finite());
            assert_eq!(out.sanitized_scalar, g.contract.fallback());
            assert!(!out.asset_in_safe_control_state);
        }
    }

    #[test]
    fn nan_dt_is_rejected_failsafe() {
        let mut g = gov();
        let out = g.evaluate(1.0, f64::NAN);
        assert!(out.sanitized_scalar.is_finite());
        assert!(!out.asset_in_safe_control_state);
    }

    #[test]
    fn nonfinite_does_not_advance_last_validated_scalar() {
        let mut g = gov();
        let before = g.last_validated_scalar;
        let _ = g.evaluate(f64::NAN, 0.05);
        assert_eq!(g.last_validated_scalar, before, "tainted value must not be retained");
    }

    #[test]
    fn finite_in_envelope_still_passes() {
        // A genuinely nominal command: in-envelope (|0.4| < 2.0) AND within the
        // rate limit (0.4 / 0.05 = 8 m/s^2 < the 10 m/s^2 accel cap), so it is a
        // clean passthrough — distinct from the rate-clamped or fail-closed paths.
        let mut g = gov();
        let out = g.evaluate(0.4, 0.05);
        assert!(out.asset_in_safe_control_state, "a nominal in-envelope, in-rate command must read safe");
        assert!(!out.was_unsafe_attempt);
        assert!((out.sanitized_scalar - 0.4).abs() < 1e-9, "got {}", out.sanitized_scalar);
    }

    #[test]
    fn nonpositive_dt_is_rejected_failsafe_not_substituted() {
        // Gov-M2: dt <= 0 must fail closed (like the AV path's InvalidTimeDelta),
        // NOT be replaced by a fabricated timestep that under-counts the rate.
        let mut g = gov();
        for dt in [0.0, -0.01] {
            let out = g.evaluate(1.0, dt);
            assert_eq!(out.sanitized_scalar, g.contract.fallback(),
                "dt={dt} must command the fallback");
            assert!(!out.asset_in_safe_control_state, "dt={dt} must not read safe");
            assert!(out.was_unsafe_attempt, "dt={dt} must flag an unsafe attempt");
        }
    }

    #[test]
    fn rate_clamp_result_is_reclamped_to_hard_envelope() {
        // Gov-M1: the rate-clamp branch anchors on last_validated_scalar, which a
        // prior wide-cap Degraded tick can leave OUTSIDE the contract envelope.
        // Construct exactly that: contract bounds are ±2.0, but the governor's
        // last_validated_scalar starts at 5.0 (constructor caps deliberately wide).
        // An in-envelope proposed command (0.0) is rate-breached (|0-5|/0.05 ≫ cap),
        // so the rate branch runs; its output MUST be re-clamped into [-2, 2].
        let contract = KinematicContract {
            max_linear_velocity: 2.0,
            max_angular_velocity: 1.0,
            max_linear_acceleration: 10.0,
            fallback_linear_speed: 0.0,
        };
        let mut g = KirraKernelGovernor::new(contract, 5.0, -10.0, 10.0);
        let out = g.evaluate(0.0, 0.05);
        assert!(
            out.sanitized_scalar <= 2.0 + 1e-9 && out.sanitized_scalar >= -2.0 - 1e-9,
            "rate-clamped output {} must be inside the hard envelope [-2, 2] (invariant #8)",
            out.sanitized_scalar
        );
    }

    fn contract() -> KinematicContract {
        KinematicContract {
            max_linear_velocity: 2.0,
            max_angular_velocity: 1.0,
            max_linear_acceleration: 10.0,
            fallback_linear_speed: 0.0,
        }
    }

    #[test]
    fn inverted_cap_is_normalized_not_paniced_b5() {
        // B5: a sign-typo'd cap (cap_min > cap_max) previously made the Degraded
        // `ApplyVelocityCap` `f64::clamp` panic — and under release `panic =
        // "abort"` that kills the governor. Construction must normalize the pair to
        // a valid ordered range instead of aborting.
        let g = KirraKernelGovernor::new(contract(), 0.0, 2.0, -2.0);
        assert!(
            g.constraint_cap_min <= g.constraint_cap_max,
            "an inverted cap must be normalized to an ordered range"
        );
        assert_eq!((g.constraint_cap_min, g.constraint_cap_max), (-2.0, 2.0));
    }

    #[test]
    fn safe_clamp_is_order_and_nan_tolerant_b5() {
        use super::safe_clamp;
        // Inverted bounds behave as a valid clamp; never panics.
        assert_eq!(safe_clamp(5.0, 2.0, -2.0), 2.0);
        assert_eq!(safe_clamp(-5.0, 2.0, -2.0), -2.0);
        assert_eq!(safe_clamp(0.5, -2.0, 2.0), 0.5);
        // A single NaN bound collapses to the finite partner (no panic).
        assert_eq!(safe_clamp(5.0, f64::NAN, 2.0), 2.0);
        assert_eq!(safe_clamp(-5.0, -2.0, f64::NAN), -2.0);
        // Both-NaN passes the (already envelope-bounded) value through, no panic.
        assert!(safe_clamp(1.5, f64::NAN, f64::NAN).is_finite());
    }

    #[test]
    fn evaluate_with_inverted_cap_never_panics_b5() {
        // End-to-end: a governor built with an inverted cap must `evaluate()`
        // without aborting and still return a finite scalar.
        let mut g = KirraKernelGovernor::new(contract(), 0.0, 2.0, -2.0);
        let out = g.evaluate(1.0, 0.05);
        assert!(out.sanitized_scalar.is_finite());
    }
}
