// src/aegis_core.rs
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum GlobalSystemState { Normal, Degraded, Failsafe }

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum TrustMode { FullAutonomy, ConstrainedAdvisory, ShadowMode, LockedOut }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BehavioralAction { ExecuteUnrestricted, ApplyVelocityCap, ForceStationaryHold, ExecutePassiveFailsafeLock }

pub struct LockFreeTelemetryBus {
    pub processed_traffic_count: AtomicU32,
    pub attempted_unsafe_actions: AtomicU32,
    pub policy_enforced_actions: AtomicU32,
    pub rate_limited_actions: AtomicU32,
}

impl LockFreeTelemetryBus {
    pub fn new() -> Self {
        Self {
            processed_traffic_count: AtomicU32::new(0),
            attempted_unsafe_actions: AtomicU32::new(0),
            policy_enforced_actions: AtomicU32::new(0),
            rate_limited_actions: AtomicU32::new(0),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct SafetyContractProfile {
    pub asset_identifier: u32,
    pub min_permissible_ceiling: f64,
    pub max_permissible_ceiling: f64,
    pub max_rate_of_change_dt: f64,
    pub fallback_safe_setpoint: f64,
    pub constraint_cap_min: f64,
    pub constraint_cap_max: f64,
    pub engineering_scale_factor: f64,
}

pub struct TrustEvaluator {
    pub current_score: u32,
    pub mode: TrustMode,
    pub consecutive_safe_packets: u32,
    pub recovery_threshold: u32,
    pub failed_reset_attempts: u32,
    pub reset_cooldown_end_ms: u64,
}

impl TrustEvaluator {
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

    pub fn constant_time_compare(a: &[u8], b: &[u8]) -> bool {
        let bitwise_accumulator = AtomicU8::new(0);
        let length_match = a.len() == b.len();
        let length_mask = if length_match { 0u8 } else { 0xFFu8 };

        for i in 0..64 {
            let byte_a = if i < a.len() { unsafe { std::ptr::read_volatile(&a[i]) } } else { 0u8 };
            let byte_b = if i < b.len() { unsafe { std::ptr::read_volatile(&b[i]) } } else { 0u8 };
            bitwise_accumulator.fetch_or(byte_a ^ byte_b, Ordering::SeqCst);
        }

        bitwise_accumulator.fetch_or(length_mask, Ordering::SeqCst);
        bitwise_accumulator.load(Ordering::SeqCst) == 0
    }

    pub fn authenticated_manual_reset(&mut self, raw_token: &[u8], system_auth_key: &[u8], current_time_ms: u64) -> Result<(), &'static str> {
        if current_time_ms < self.reset_cooldown_end_ms {
            return Err("RESET_REJECTED_COOLDOWN_ACTIVE");
        }
        if self.failed_reset_attempts >= 5 {
            self.reset_cooldown_end_ms = current_time_ms + 60000;
            return Err("RESET_DISABLED_BRUTE_FORCE_SUSPECTED");
        }

        if !Self::constant_time_compare(raw_token, system_auth_key) {
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

pub struct GovernancePostureEngine { pub current_action: BehavioralAction }

impl GovernancePostureEngine {
    pub fn new() -> Self { Self { current_action: BehavioralAction::ExecuteUnrestricted } }
    pub fn determine_operational_continuity(&mut self, trust_mode: TrustMode, system_state: GlobalSystemState) -> BehavioralAction {
        self.current_action = match (system_state, trust_mode) {
            (GlobalSystemState::Normal, TrustMode::FullAutonomy) => BehavioralAction::ExecuteUnrestricted,
            (GlobalSystemState::Degraded, TrustMode::ConstrainedAdvisory) => BehavioralAction::ApplyVelocityCap,
            (GlobalSystemState::Degraded, TrustMode::ShadowMode) => BehavioralAction::ForceStationaryHold,
            _ => BehavioralAction::ExecutePassiveFailsafeLock,
        };
        self.current_action
    }
}

pub struct AegisUnifiedGovernor {
    pub contract_config: SafetyContractProfile,
    pub posture_engine: GovernancePostureEngine,
    pub trust_evaluator: TrustEvaluator,
    pub last_validated_scalar: f64,
    pub continuous_rate_breach_ticks: u32,
}

pub struct GovernorInterceptResult {
    pub sanitized_scalar: f64,
    pub asset_in_safe_control_state: bool,
    pub mitigation_narrative: String,
    pub was_unsafe_attempt: bool,
    pub was_rate_breached: bool,
}

impl AegisUnifiedGovernor {
    pub fn new(config: SafetyContractProfile, initial_safe_scalar: f64) -> Self {
        Self {
            contract_config: config,
            posture_engine: GovernancePostureEngine::new(),
            trust_evaluator: TrustEvaluator::new(),
            last_validated_scalar: initial_safe_scalar,
            continuous_rate_breach_ticks: 0,
        }
    }

    pub fn evaluate_demand_scalar(&mut self, proposed_demand: f64, mut dt: f64) -> GovernorInterceptResult {
        if dt <= 0.001 { dt = 0.050; }

        let prior_trust_mode = self.trust_evaluator.mode;
        let prior_system_state = match prior_trust_mode {
            TrustMode::FullAutonomy => GlobalSystemState::Normal,
            TrustMode::ConstrainedAdvisory | TrustMode::ShadowMode => GlobalSystemState::Degraded,
            TrustMode::LockedOut => GlobalSystemState::Failsafe,
        };
        let active_posture_action = self.posture_engine.determine_operational_continuity(prior_trust_mode, prior_system_state);

        let is_out_of_envelope = proposed_demand < self.contract_config.min_permissible_ceiling
            || proposed_demand > self.contract_config.max_permissible_ceiling;

        let delta_step = (proposed_demand - self.last_validated_scalar).abs();
        let rate_change_per_sec = delta_step / dt;
        let is_rate_breached = rate_change_per_sec > self.contract_config.max_rate_of_change_dt;

        if is_rate_breached {
            self.continuous_rate_breach_ticks = self.continuous_rate_breach_ticks.saturating_add(1);
        } else {
            self.continuous_rate_breach_ticks = self.continuous_rate_breach_ticks.saturating_sub(1);
        }

        let mut cumulative_penalty = 0;
        if is_out_of_envelope { cumulative_penalty += 30; }
        if self.continuous_rate_breach_ticks > 5 { cumulative_penalty += 15; }

        let core_bounded_demand = proposed_demand.clamp(self.contract_config.min_permissible_ceiling, self.contract_config.max_permissible_ceiling);

        let (sanitized_scalar, narrative) = match active_posture_action {
            BehavioralAction::ExecuteUnrestricted => {
                if is_out_of_envelope {
                    (core_bounded_demand, "ENVELOPE_CLAMP_TAKES_PRIORITY".to_string())
                } else if is_rate_breached {
                    let step_direction = (proposed_demand - self.last_validated_scalar).signum();
                    let rate_clamped_value = self.last_validated_scalar + (self.contract_config.max_rate_of_change_dt * dt * step_direction);
                    (rate_clamped_value, format!("RATE_CLAMP_ENFORCED: Max {} GPM/s", self.contract_config.max_rate_of_change_dt))
                } else {
                    (proposed_demand, "PASSTHROUGH_UNRESTRICTED_NORMAL".to_string())
                }
            }
            BehavioralAction::ApplyVelocityCap => {
                let clamped_value = core_bounded_demand.clamp(self.contract_config.constraint_cap_min, self.contract_config.constraint_cap_max);
                (clamped_value, format!("DEGRADED_POSTURE_CLAMP: Bounded inside [{} - {}]", self.contract_config.constraint_cap_min, self.contract_config.constraint_cap_max))
            }
            BehavioralAction::ForceStationaryHold => {
                (self.last_validated_scalar, format!("SHADOW_MODE_HOLD_ENFORCED: Fixed value retained: {:.1}", self.last_validated_scalar))
            }
            BehavioralAction::ExecutePassiveFailsafeLock => {
                (self.contract_config.fallback_safe_setpoint, "CRITICAL_LOCKOUT: Active fallback state commanded".to_string())
            }
        };

        if active_posture_action != BehavioralAction::ExecutePassiveFailsafeLock {
            self.last_validated_scalar = sanitized_scalar;
        }

        if cumulative_penalty > 0 {
            self.trust_evaluator.decay_trust(cumulative_penalty);
        } else {
            self.trust_evaluator.register_safe_tick();
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
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JournalEntry {
    pub timestamp_ms: u64, pub actor: String, pub token: String, pub action: String, pub resolution: String,
    pub score: u32, pub system_state: GlobalSystemState, pub trust_mode: TrustMode, pub operator_narrative: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CausalFlightRecorder { pub journal: VecDeque<JournalEntry> }

impl CausalFlightRecorder {
    pub fn new() -> Self { Self { journal: VecDeque::new() } }
    pub fn log(&mut self, ts: u64, actor: &str, token: &str, action: &str, res: &str, state: GlobalSystemState, mode: TrustMode, score: u32, narrative: String) {
        self.journal.push_back(JournalEntry {
            timestamp_ms: ts, actor: actor.to_string(), token: token.to_string(), action: action.to_string(), resolution: res.to_string(),
            score, system_state: state, trust_mode: mode, operator_narrative: narrative,
        });
        if self.journal.len() > 100 { self.journal.pop_front(); }
    }
}
