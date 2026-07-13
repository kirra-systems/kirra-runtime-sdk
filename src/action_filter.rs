// src/action_filter.rs

use crate::{ActionResolution, AgentAction, SafetyContract, SafetyGovernor};

pub struct FilterOutput {
    pub resolution: ActionResolution,
    pub sanitized_action: AgentAction,
    pub narrative: String,
}

pub struct ActionFilter<C: SafetyContract> {
    pub contract: C,
}

impl<C: SafetyContract> ActionFilter<C> {
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

                let resolution = if intercept.was_unsafe_attempt
                    && governor.trust_mode() == crate::TrustMode::LockedOut
                {
                    ActionResolution::Failsafe
                } else if mutated {
                    ActionResolution::Mutated
                } else {
                    ActionResolution::Approved
                };

                FilterOutput {
                    resolution,
                    sanitized_action: AgentAction::MoveLinear {
                        velocity: intercept.sanitized_scalar,
                    },
                    narrative: intercept.mitigation.to_string(),
                }
            }
            AgentAction::Rotate { angular_velocity } => {
                if angular_velocity.abs() > self.contract.max_angular_rate() {
                    return FilterOutput {
                        resolution: ActionResolution::Rejected,
                        sanitized_action: AgentAction::HoldPosition,
                        narrative: "REJECTED: Angular rate violates safety envelope.".to_string(),
                    };
                }
                FilterOutput {
                    resolution: ActionResolution::Approved,
                    sanitized_action: AgentAction::Rotate { angular_velocity },
                    narrative: "PASSTHROUGH_NORMAL".to_string(),
                }
            }
            _ => FilterOutput {
                resolution: ActionResolution::Approved,
                sanitized_action: AgentAction::HoldPosition,
                narrative: "PASSTHROUGH_NORMAL".to_string(),
            },
        }
    }
}

// --- Posture-aware action claim evaluation (post-v1 extension) ---------------
//
// ADR-0035 Stage 0b: `ActionClaim` / `ActionDecision` / `evaluate_action_claim`
// (and their tests) were moved VERBATIM into the lean `kirra-policy-types` leaf
// crate (`kirra_policy_types::action_claim`) — the half the industrial layer
// (`protocol_adapter`) consumes. This is a `pub use` re-export shim so every
// existing `crate::action_filter::{evaluate_action_claim, ActionClaim,
// ActionDecision}` path (the service handler, `protocol_adapter`, tests)
// resolves unchanged.
pub use kirra_policy_types::action_claim::{evaluate_action_claim, ActionClaim, ActionDecision};
