// src/action_filter.rs

use crate::{SafetyGovernor, ActionResolution, AgentAction, SafetyContract};

pub struct FilterOutput {
    pub resolution: ActionResolution,
    pub sanitized_action: AgentAction,
    pub narrative: String,
}

pub struct AiActionFilterEngine<C: SafetyContract> {
    pub contract: C,
}

impl<C: SafetyContract> AiActionFilterEngine<C> {
    pub fn new(contract: C) -> Self {
        Self { contract }
    }

    pub fn process_agent_intent<G: SafetyGovernor>(
        &self,
        governor: &mut G,
        action: AgentAction,
        dt: f64,
    ) -> FilterOutput {
        match action {
            AgentAction::MoveLinear { velocity } => {
                let intercept = governor.evaluate(velocity, dt);
                let mutated = (intercept.sanitized_scalar - velocity).abs() > 0.001;

                let resolution = if intercept.was_unsafe_attempt && governor.trust_mode() == crate::TrustMode::LockedOut {
                    ActionResolution::Failsafe
                } else if mutated {
                    ActionResolution::Mutated
                } else {
                    ActionResolution::Approved
                };

                FilterOutput {
                    resolution,
                    sanitized_action: AgentAction::MoveLinear { velocity: intercept.sanitized_scalar },
                    narrative: intercept.mitigation_narrative,
                }
            }
            AgentAction::SetPumpRate { gpm } => {
                let intercept = governor.evaluate(gpm, dt);
                let mutated = (intercept.sanitized_scalar - gpm).abs() > 0.001;
                let resolution = if mutated { ActionResolution::Mutated } else { ActionResolution::Approved };

                FilterOutput {
                    resolution,
                    sanitized_action: AgentAction::SetPumpRate { gpm: intercept.sanitized_scalar },
                    narrative: intercept.mitigation_narrative,
                }
            }
            AgentAction::Rotate { angular_velocity } => {
                if angular_velocity.abs() > self.contract.max_angular_rate() {
                    return FilterOutput {
                        resolution: ActionResolution::Rejected,
                        sanitized_action: AgentAction::HoldPosition,
                        narrative: "REJECTED_AGENT_INTENT: Angular rate violates safety contract envelope restrictions.".to_string(),
                    };
                }
                FilterOutput {
                    resolution: ActionResolution::Approved,
                    sanitized_action: AgentAction::Rotate { angular_velocity },
                    narrative: "PASSTHROUGH_UNRESTRICTED_NORMAL".to_string(),
                }
            }
            AgentAction::HoldPosition => FilterOutput {
                resolution: ActionResolution::Approved,
                sanitized_action: AgentAction::HoldPosition,
                narrative: "PASSTHROUGH_UNRESTRICTED_NORMAL".to_string(),
            },
            AgentAction::EmergencyStop => FilterOutput {
                resolution: ActionResolution::Failsafe,
                sanitized_action: AgentAction::EmergencyStop,
                narrative: "CRITICAL_LOCKOUT: Emergency stop commanded by proxy loop.".to_string(),
            },
        }
    }
}
