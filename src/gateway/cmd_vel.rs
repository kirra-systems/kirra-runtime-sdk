// src/gateway/cmd_vel.rs

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CmdVel {
    pub linear_x: f64,
    pub linear_y: f64,
    pub linear_z: f64,
    pub angular_x: f64,
    pub angular_y: f64,
    pub angular_z: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct CmdVelLimits {
    pub max_linear_x_abs: f64,
    pub max_linear_y_abs: f64,
    pub max_linear_z_abs: f64,
    pub max_angular_z_abs: f64,
}

pub const DEFAULT_CMD_VEL_LIMITS: CmdVelLimits = CmdVelLimits {
    max_linear_x_abs: 0.5,
    max_linear_y_abs: 0.0,
    max_linear_z_abs: 0.0,
    max_angular_z_abs: 1.0,
};

// SAFETY: SG3 SG9 | REQ: cmd-vel-finite-and-bounded | TEST: test_cmd_vel_within_bounds,test_cmd_vel_exceeds_linear_x
// (≅ AEGIS SG-001 + SG-004 — finite-input check feeds SG9 fail-closed;
//  per-axis bounds feed SG3 envelope.)
pub fn validate_cmd_vel(cmd: &CmdVel, limits: CmdVelLimits) -> bool {
    if !cmd.linear_x.is_finite()
        || !cmd.linear_y.is_finite()
        || !cmd.linear_z.is_finite()
        || !cmd.angular_x.is_finite()
        || !cmd.angular_y.is_finite()
        || !cmd.angular_z.is_finite()
    {
        return false;
    }

    if cmd.angular_x != 0.0 || cmd.angular_y != 0.0 {
        return false;
    }

    cmd.linear_x.abs() <= limits.max_linear_x_abs
        && cmd.linear_y.abs() <= limits.max_linear_y_abs
        && cmd.linear_z.abs() <= limits.max_linear_z_abs
        && cmd.angular_z.abs() <= limits.max_angular_z_abs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cmd(lx: f64, az: f64) -> CmdVel {
        CmdVel {
            linear_x: lx, linear_y: 0.0, linear_z: 0.0,
            angular_x: 0.0, angular_y: 0.0, angular_z: az,
        }
    }

    #[test]
    fn test_cmd_vel_within_bounds() {
        assert!(validate_cmd_vel(&sample_cmd(0.25, 0.4), DEFAULT_CMD_VEL_LIMITS));
    }

    #[test]
    fn test_cmd_vel_exceeds_linear_x() {
        assert!(!validate_cmd_vel(&sample_cmd(0.6, 0.0), DEFAULT_CMD_VEL_LIMITS));
    }
}
