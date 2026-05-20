// src/aegis_core.rs

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

pub struct AegisKernelGovernor<C: SafetyContract> {
    pub contract: C,
    pub trust_engine: RuntimeTrustEngine,
    pub last_validated_scalar: f64,
    pub continuous_rate_breach_ticks: u32,
    pub constraint_cap_min: f64,
    pub constraint_cap_max: f64,
}

impl<C: SafetyContract> AegisKernelGovernor<C> {
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

impl<C: SafetyContract> SafetyGovernor for AegisKernelGovernor<C> {
    fn evaluate(&mut self, proposed_demand: f64, mut dt: f64) -> GovernorInterceptResult {
        if dt <= 0.001 { dt = 0.050; }

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
                    let rate_clamped_value = self.last_validated_scalar + (self.contract.max_rate() * dt * step_direction);
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

impl CausalFlightRecorder {
    pub fn new() -> Self { Self { journal: std::collections::VecDeque::new() } }
    pub fn log(&mut self, ts: u64, actor: &str, token: &str, action: &str, res: &str, state: GlobalSystemState, mode: TrustMode, score: u32, narrative: String) {
        self.journal.push_back(JournalEntry {
            timestamp_ms: ts, actor: actor.to_string(), token: token.to_string(), action: action.to_string(), resolution: res.to_string(),
            score, system_state: state, trust_mode: mode, operator_narrative: narrative,
        });
        if self.journal.len() > 100 { self.journal.pop_front(); }
    }
}
