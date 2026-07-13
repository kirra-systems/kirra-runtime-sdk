// src/gateway/cmd_vel.rs
//
// ADR-0035 Stage 0b: the cmd_vel kinematic gate (`CmdVel` / `CmdVelLimits` /
// `DEFAULT_CMD_VEL_LIMITS` / `validate_cmd_vel` + its MC/DC tests) was moved
// VERBATIM into the lean `kirra-policy-types` leaf crate. This module is now a
// re-export shim so every existing `crate::gateway::cmd_vel::{...}` path resolves
// unchanged.
pub use kirra_policy_types::cmd_vel::{
    validate_cmd_vel, CmdVel, CmdVelLimits, DEFAULT_CMD_VEL_LIMITS,
};
