// crates/kirra-policy-types/src/action_claim.rs
//
// ADR-0035 Stage 0b: the posture-aware action-claim evaluation, moved VERBATIM
// from the root crate's `src/action_filter.rs` (pure move — behaviour-identical).
// This is the half the industrial layer (Stage 1 `kirra-industrial`) consumes
// (`protocol_adapter` imports exactly `evaluate_action_claim` / `ActionClaim` /
// `ActionDecision`); moving it here unblocks that extraction. The governor-trait-
// generic `ActionFilter<C>` stays in the root crate (it needs the governor traits
// still defined in `lib.rs`). `kirra-verifier::action_filter` re-exports these.
//
// Depends only on `kirra_core::FleetPosture` + the sibling `cmd_vel` module +
// serde/serde_json — still a lean leaf.

use crate::cmd_vel::{validate_cmd_vel, CmdVel, DEFAULT_CMD_VEL_LIMITS};
use kirra_core::FleetPosture;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ActionClaim {
    pub action_type: String,
    pub target_node: String,
    pub risk_class: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ActionDecision {
    pub allowed: bool,
    pub reason: String,
}

pub fn evaluate_action_claim(claim: ActionClaim, posture: FleetPosture) -> ActionDecision {
    if claim.action_type.is_empty() {
        return ActionDecision {
            allowed: false,
            reason: "MISSING_ACTION_TYPE".to_string(),
        };
    }
    if claim.target_node.is_empty() {
        return ActionDecision {
            allowed: false,
            reason: "MISSING_TARGET_NODE".to_string(),
        };
    }

    match posture {
        FleetPosture::Nominal => match claim.action_type.as_str() {
            "cmd_vel" => match serde_json::from_value::<CmdVel>(claim.payload.clone()) {
                Ok(cmd) => {
                    if validate_cmd_vel(&cmd, DEFAULT_CMD_VEL_LIMITS) {
                        ActionDecision {
                            allowed: true,
                            reason: "NOMINAL_VALID_KINEMATICS".to_string(),
                        }
                    } else {
                        ActionDecision {
                            allowed: false,
                            reason: "KINEMATIC_ENVELOPE_BREACH".to_string(),
                        }
                    }
                }
                Err(_) => ActionDecision {
                    allowed: false,
                    reason: "MALFORMED_CMD_VEL_PAYLOAD".to_string(),
                },
            },
            _ => ActionDecision {
                allowed: false,
                reason: "UNKNOWN_ACTION_TYPE".to_string(),
            },
        },
        FleetPosture::Degraded => {
            if claim.risk_class == "kinetic_write" || claim.action_type == "cmd_vel" {
                ActionDecision {
                    allowed: false,
                    reason: "DEGRADED_POSTURE_KINETIC_DENIED".to_string(),
                }
            } else if claim.action_type == "read_telemetry" {
                ActionDecision {
                    allowed: true,
                    reason: "DEGRADED_READ_ONLY_PERMITTED".to_string(),
                }
            } else {
                ActionDecision {
                    allowed: false,
                    reason: "DEGRADED_UNSUPPORTED_CLAIM_TYPE".to_string(),
                }
            }
        }
        FleetPosture::LockedOut => ActionDecision {
            allowed: false,
            reason: "LOCKEDOUT_POSTURE_ABSOLUTE_DENIAL".to_string(),
        },
    }
}

#[cfg(test)]
mod action_claim_tests {
    use super::*;
    use kirra_core::FleetPosture;

    fn nominal_cmd_vel() -> ActionClaim {
        ActionClaim {
            action_type: "cmd_vel".to_string(),
            target_node: "robot_base".to_string(),
            risk_class: "kinetic_write".to_string(),
            payload: serde_json::json!({ "linear_x": 0.5, "linear_y": 0.0, "linear_z": 0.0, "angular_x": 0.0, "angular_y": 0.0, "angular_z": 0.1 }),
        }
    }

    fn read_claim() -> ActionClaim {
        ActionClaim {
            action_type: "read_telemetry".to_string(),
            target_node: "sensor_01".to_string(),
            risk_class: "read".to_string(),
            payload: serde_json::json!({}),
        }
    }

    fn unknown_claim() -> ActionClaim {
        ActionClaim {
            action_type: "deploy_payload_at_max_velocity".to_string(),
            target_node: "drone_01".to_string(),
            risk_class: "kinetic_write".to_string(),
            payload: serde_json::json!({ "velocity": 999.0 }),
        }
    }

    #[test]
    fn test_unknown_action_type_always_denied() {
        // Unknown action types must be denied in ALL posture states
        for posture in [
            FleetPosture::Nominal,
            FleetPosture::Degraded,
            FleetPosture::LockedOut,
        ] {
            let d = evaluate_action_claim(unknown_claim(), posture);
            assert!(!d.allowed, "Unknown action must be denied in {posture:?}");
        }
    }

    #[test]
    fn test_hallucinated_velocity_denied_by_kinematic_contract() {
        // Hallucinated extreme velocity should fail kinematic validation
        let claim = ActionClaim {
            action_type: "cmd_vel".to_string(),
            target_node: "robot_base".to_string(),
            risk_class: "kinetic_write".to_string(),
            payload: serde_json::json!({ "linear_x": 999.0, "linear_y": 0.0, "linear_z": 0.0, "angular_x": 0.0, "angular_y": 0.0, "angular_z": 0.0 }),
        };
        let d = evaluate_action_claim(claim, FleetPosture::Nominal);
        assert!(!d.allowed);
        assert_eq!(d.reason, "KINEMATIC_ENVELOPE_BREACH");
    }

    #[test]
    fn test_action_denied_when_locked_out() {
        let d = evaluate_action_claim(nominal_cmd_vel(), FleetPosture::LockedOut);
        assert!(!d.allowed);
        assert_eq!(d.reason, "LOCKEDOUT_POSTURE_ABSOLUTE_DENIAL");
    }

    #[test]
    fn test_cmd_vel_denied_when_degraded() {
        let d = evaluate_action_claim(nominal_cmd_vel(), FleetPosture::Degraded);
        assert!(!d.allowed);
        assert_eq!(d.reason, "DEGRADED_POSTURE_KINETIC_DENIED");
    }

    #[test]
    fn test_read_telemetry_allowed_when_degraded() {
        let d = evaluate_action_claim(read_claim(), FleetPosture::Degraded);
        assert!(d.allowed);
        assert_eq!(d.reason, "DEGRADED_READ_ONLY_PERMITTED");
    }

    #[test]
    fn test_read_telemetry_denied_when_locked_out() {
        let d = evaluate_action_claim(read_claim(), FleetPosture::LockedOut);
        assert!(!d.allowed);
    }

    #[test]
    fn test_valid_cmd_vel_allowed_when_nominal() {
        let d = evaluate_action_claim(nominal_cmd_vel(), FleetPosture::Nominal);
        assert!(d.allowed);
        assert_eq!(d.reason, "NOMINAL_VALID_KINEMATICS");
    }

    #[test]
    fn test_missing_action_type_always_denied() {
        let claim = ActionClaim {
            action_type: "".to_string(),
            target_node: "robot_base".to_string(),
            risk_class: "kinetic_write".to_string(),
            payload: serde_json::json!({}),
        };
        let d = evaluate_action_claim(claim, FleetPosture::Nominal);
        assert!(!d.allowed);
        assert_eq!(d.reason, "MISSING_ACTION_TYPE");
    }

    // NOTE: test_malformed_json_returns_400_not_500 requires the full HTTP stack
    // and is covered by the handler hardening in evaluate_action_filter()
    // (uses axum::extract::rejection::JsonRejection to return 400 BAD_REQUEST
    //  with {"error":"MALFORMED_REQUEST","detail":"...","allowed":false}).
}
