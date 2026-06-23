// src/kirra_core.rs

use crate::{SafetyContract, SafetyGovernor, GovernorInterceptResult, TrustMode};
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
        if current_time_ms < self.reset_cooldown_end_ms { return Err("RESET_REJECTED_COOLDOWN_ACTIVE"); }
        if self.failed_reset_attempts >= 5 {
            self.reset_cooldown_end_ms = current_time_ms + 60000;
            return Err("RESET_DISABLED_BRUTE_FORCE_SUSPECTED");
        }
        if !constant_time_compare(raw_token, system_auth_key) {
            self.failed_reset_attempts = self.failed_reset_attempts.saturating_add(1);
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

impl<C: SafetyContract> KirraKernelGovernor<C> {
    pub fn new(contract: C, initial_scalar: f64, cap_min: f64, cap_max: f64) -> Self {
        Self {
            contract,
            trust_engine: RuntimeTrustEngine::new(),
            last_validated_scalar: initial_scalar,
            continuous_rate_breach_ticks: 0,
            constraint_cap_min: cap_min,
            constraint_cap_max: cap_max,
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
                mitigation_narrative: "NONFINITE_INPUT_REJECTED_FAILSAFE".to_string(),
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
                mitigation_narrative: "INVALID_TIME_DELTA_REJECTED_FAILSAFE".to_string(),
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

        let core_bounded_demand = proposed_demand.clamp(self.contract.min_bound(), self.contract.max_bound());

        let (sanitized_scalar, narrative) = match active_action {
            BehavioralAction::ExecuteUnrestricted => {
                if is_out_of_envelope {
                    (core_bounded_demand, "ENVELOPE_CLAMP_TAKES_PRIORITY".to_string())
                } else if is_rate_breached {
                    let step_direction = (proposed_demand - self.last_validated_scalar).signum();
                    // Gov-M1 (invariant #8 — envelope ALWAYS wins over rate): re-clamp
                    // the rate-limited result to the hard envelope. This branch anchors
                    // on `last_validated_scalar`, which a prior Degraded
                    // (`ApplyVelocityCap`) tick may have left outside the contract
                    // envelope when the constructor caps are wider than the bounds; the
                    // unconditional clamp guarantees the emitted scalar is in-envelope,
                    // matching the AV path's `validate_vehicle_command`.
                    let rate_clamped_value = (self.last_validated_scalar
                        + (self.contract.max_rate() * dt * step_direction))
                        .clamp(self.contract.min_bound(), self.contract.max_bound());
                    (rate_clamped_value, format!("RATE_CLAMP_ENFORCED: Max {} GPM/s", self.contract.max_rate()))
                } else {
                    (proposed_demand, "PASSTHROUGH_UNRESTRICTED_NORMAL".to_string())
                }
            }
            BehavioralAction::ApplyVelocityCap => {
                let clamped_value = core_bounded_demand.clamp(self.constraint_cap_min, self.constraint_cap_max);
                (clamped_value, format!("DEGRADED_POSTURE_CLAMP: Bounded inside [{} - {}]", self.constraint_cap_min, self.constraint_cap_max))
            }
            BehavioralAction::ForceStationaryHold => {
                (self.last_validated_scalar, format!("SHADOW_MODE_HOLD_ENFORCED: Fixed value retained: {:.1}", self.last_validated_scalar))
            }
            BehavioralAction::ExecutePassiveFailsafeLock => {
                (self.contract.fallback(), "CRITICAL_LOCKOUT: Active fallback state commanded".to_string())
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
            mitigation_narrative: narrative,
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
}
