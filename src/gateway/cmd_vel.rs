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
            linear_x: lx,
            linear_y: 0.0,
            linear_z: 0.0,
            angular_x: 0.0,
            angular_y: 0.0,
            angular_z: az,
        }
    }

    #[test]
    fn test_cmd_vel_within_bounds() {
        assert!(validate_cmd_vel(
            &sample_cmd(0.25, 0.4),
            DEFAULT_CMD_VEL_LIMITS
        ));
    }

    #[test]
    fn test_cmd_vel_exceeds_linear_x() {
        assert!(!validate_cmd_vel(
            &sample_cmd(0.6, 0.0),
            DEFAULT_CMD_VEL_LIMITS
        ));
    }

    /// SG9 / GAP 1: NaN or Inf in ANY of the six axes must reject before any
    /// arithmetic / bounds-comparison runs. Parameterized across all six.
    #[test]
    fn test_cmd_vel_nan_in_any_axis_rejects() {
        let axes_set_nan: [fn(&mut CmdVel); 6] = [
            |c| c.linear_x = f64::NAN,
            |c| c.linear_y = f64::NAN,
            |c| c.linear_z = f64::NAN,
            |c| c.angular_x = f64::NAN,
            |c| c.angular_y = f64::NAN,
            |c| c.angular_z = f64::NAN,
        ];
        for (i, set_nan) in axes_set_nan.iter().enumerate() {
            let mut cmd = sample_cmd(0.1, 0.1);
            set_nan(&mut cmd);
            assert!(
                !validate_cmd_vel(&cmd, DEFAULT_CMD_VEL_LIMITS),
                "axis index {i}: NaN must reject"
            );
        }
        for (i, set_inf) in [
            |c: &mut CmdVel| c.linear_x = f64::INFINITY,
            |c: &mut CmdVel| c.linear_y = f64::NEG_INFINITY,
            |c: &mut CmdVel| c.linear_z = f64::INFINITY,
            |c: &mut CmdVel| c.angular_x = f64::INFINITY,
            |c: &mut CmdVel| c.angular_y = f64::NEG_INFINITY,
            |c: &mut CmdVel| c.angular_z = f64::INFINITY,
        ]
        .iter()
        .enumerate()
        {
            let mut cmd = sample_cmd(0.1, 0.1);
            set_inf(&mut cmd);
            assert!(
                !validate_cmd_vel(&cmd, DEFAULT_CMD_VEL_LIMITS),
                "axis index {i}: Inf must reject"
            );
        }
    }

    /// SG3 / GAP 2: differential-drive platform restriction — angular_x or
    /// angular_y ≠ 0 must reject regardless of bounds. Exercises the
    /// non-zero arm at validate_cmd_vel l.44.
    #[test]
    fn test_cmd_vel_rejects_nonzero_angular_xy() {
        let mut cmd = sample_cmd(0.25, 0.4);
        cmd.angular_x = 0.1;
        assert!(
            !validate_cmd_vel(&cmd, DEFAULT_CMD_VEL_LIMITS),
            "non-zero angular_x must reject"
        );
        let mut cmd = sample_cmd(0.25, 0.4);
        cmd.angular_y = -0.1;
        assert!(
            !validate_cmd_vel(&cmd, DEFAULT_CMD_VEL_LIMITS),
            "non-zero angular_y must reject"
        );
    }

    // ---------------------------------------------------------------------
    // MC/DC pair-completion tests (S3 / #115 — KIRRA-OCCY-MCDC-001).
    //
    // The first six clauses of `validate_cmd_vel` (the `is_finite()` OR-chain
    // at l.34–39) and the two-clause angular-non-zero guard at l.44 each have
    // both their true and false arms exercised by the parameterised tests
    // above. The final bounded-magnitude AND-chain at l.48–51 also requires
    // an independent-effect demonstration of each clause's FALSE arm. Two
    // clauses cannot have a non-zero limit and a non-zero permitted input
    // simultaneously under `DEFAULT_CMD_VEL_LIMITS` (max_linear_y_abs and
    // max_linear_z_abs are both 0.0 — a differential-drive platform), so we
    // construct a custom `CmdVelLimits` profile that admits non-zero y / z
    // and then push each axis past its bound in isolation.
    // ---------------------------------------------------------------------

    /// MC/DC: independent-effect of `linear_y.abs() <= max_linear_y_abs`
    /// (l.49). All other clauses pass; only the linear_y clause flips
    /// FALSE. Validates the false arm of the AND term.
    #[test]
    fn test_cmd_vel_exceeds_linear_y_with_custom_limits() {
        let limits = CmdVelLimits {
            max_linear_x_abs: 1.0,
            max_linear_y_abs: 0.5,
            max_linear_z_abs: 0.5,
            max_angular_z_abs: 1.0,
        };
        let cmd = CmdVel {
            linear_x: 0.25,
            linear_y: 0.9,
            linear_z: 0.0,
            angular_x: 0.0,
            angular_y: 0.0,
            angular_z: 0.2,
        };
        assert!(
            !validate_cmd_vel(&cmd, limits),
            "linear_y over the per-axis cap must reject"
        );
    }

    /// MC/DC: independent-effect of `linear_z.abs() <= max_linear_z_abs`
    /// (l.50). Only the linear_z clause flips FALSE; all others pass.
    #[test]
    fn test_cmd_vel_exceeds_linear_z_with_custom_limits() {
        let limits = CmdVelLimits {
            max_linear_x_abs: 1.0,
            max_linear_y_abs: 0.5,
            max_linear_z_abs: 0.5,
            max_angular_z_abs: 1.0,
        };
        let cmd = CmdVel {
            linear_x: 0.25,
            linear_y: 0.0,
            linear_z: -0.9,
            angular_x: 0.0,
            angular_y: 0.0,
            angular_z: 0.2,
        };
        assert!(
            !validate_cmd_vel(&cmd, limits),
            "linear_z over the per-axis cap must reject"
        );
    }

    /// MC/DC: all four bounded-magnitude clauses true → Allow.
    /// Anchors the AND-chain's TRUE arm on the custom-limits profile.
    #[test]
    fn test_cmd_vel_all_bounds_satisfied_with_custom_limits() {
        let limits = CmdVelLimits {
            max_linear_x_abs: 1.0,
            max_linear_y_abs: 0.5,
            max_linear_z_abs: 0.5,
            max_angular_z_abs: 1.0,
        };
        let cmd = CmdVel {
            linear_x: 0.25,
            linear_y: 0.2,
            linear_z: -0.3,
            angular_x: 0.0,
            angular_y: 0.0,
            angular_z: 0.2,
        };
        assert!(
            validate_cmd_vel(&cmd, limits),
            "all axes within their per-axis caps must Allow"
        );
    }
}
