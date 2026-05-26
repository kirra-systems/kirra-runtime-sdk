// src/gateway/kinematics_contract.rs
//
// Deterministic vehicle kinematics safety contract for Kirra AV flight envelope protection.
//
// This module answers exactly one question: "Is this proposed vehicle command physically
// safe to execute on this platform, given the current kinematic state?"
//
// The verification pipeline runs checks in strict priority order. A check that fires
// returns immediately. See docs/kinematics_envelope_protection.md for the full spec.
//
// Security invariants respected:
//   - No interaction with KIRRA_ADMIN_TOKEN or any auth primitives (wrong layer)
//   - No DDS/ROS2 publishing (ros2_adapter.rs owns NaN/Inf rejection for that path)
//   - LockedOut handling belongs to the calling policy layer
//   - All arithmetic is deterministic; no RNG, no I/O, no async

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Contract Profiles
// ---------------------------------------------------------------------------

/// All physical limits that govern whether a proposed vehicle command is admissible.
///
/// Two canonical constructors are provided for Nominal and MRC posture states.
/// Custom profiles may be constructed for non-standard platforms.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
pub struct VehicleKinematicsContract {
    /// Maximum allowable forward/reverse speed (m/s). Hard upper bound.
    pub max_speed_mps: f64,
    /// Maximum allowable linear acceleration rate (m/s²).
    pub max_accel_mps2: f64,
    /// Maximum allowable linear deceleration rate (m/s²). Service braking only;
    /// emergency braking is handled by a separate hardware interlock layer.
    pub max_brake_mps2: f64,
    /// Maximum allowable absolute steering angle (degrees). Physical rack limit.
    pub max_steering_deg: f64,
    /// Maximum allowable steering angle rate-of-change (degrees/second).
    pub max_steering_rate_deg_s: f64,
    /// Minimum required following distance (meters). Stored for profile completeness;
    /// not evaluated in `validate_vehicle_command`.
    pub min_follow_distance_m: f64,
    /// Maximum allowable lateral acceleration from the bicycle model (m/s²).
    /// `a_lat = (v² × |tan(δ)|) / L ≤ max_lateral_accel_mps2`
    pub max_lateral_accel_mps2: f64,
    /// Vehicle wheelbase (meters). Used in the bicycle model denominator.
    /// Must match the physical platform.
    pub wheelbase_m: f64,
}

impl VehicleKinematicsContract {
    /// Full operational profile for a standard reference vehicle platform.
    /// Suitable for `FleetPosture::Nominal`.
    pub fn nominal_reference_profile() -> Self {
        Self {
            max_speed_mps: 35.0,
            max_accel_mps2: 2.5,
            max_brake_mps2: 4.5,
            max_steering_deg: 35.0,
            max_steering_rate_deg_s: 45.0,
            min_follow_distance_m: 2.0,
            max_lateral_accel_mps2: 3.5,
            wheelbase_m: 2.8,
        }
    }

    /// Minimal Risk Condition (MRC) fallback profile for degraded fleet posture.
    /// Suitable for `FleetPosture::Degraded`.
    pub fn mrc_fallback_profile() -> Self {
        Self {
            max_speed_mps: 5.0,
            max_accel_mps2: 1.0,
            max_brake_mps2: 3.0,
            max_steering_deg: 15.0,
            max_steering_rate_deg_s: 20.0,
            min_follow_distance_m: 5.0,
            max_lateral_accel_mps2: 1.5,
            wheelbase_m: 2.8,
        }
    }
}

// ---------------------------------------------------------------------------
// Command Input
// ---------------------------------------------------------------------------

/// A proposed actuator command from the motion planning stack, with the current
/// kinematic state required to compute rate-of-change invariants.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProposedVehicleCommand {
    /// Desired forward velocity at end of this time step (m/s).
    /// Negative values indicate reverse motion.
    pub linear_velocity_mps: f64,
    /// Actual forward velocity at start of this time step (m/s).
    pub current_velocity_mps: f64,
    /// Duration of this planning time step (seconds). Must be > 0.
    pub delta_time_s: f64,
    /// Desired steering angle at end of this time step (degrees).
    /// Sign convention: positive = left turn (ISO 8855).
    pub steering_angle_deg: f64,
    /// Actual steering angle at start of this time step (degrees).
    pub current_steering_angle_deg: f64,
}

// ---------------------------------------------------------------------------
// Enforcement Result
// ---------------------------------------------------------------------------

/// Result of `validate_vehicle_command`.
///
/// - `Allow`         → forward to actuator
/// - `ClampLinear`   → replace linear velocity with provided safe value
/// - `ClampSteering` → replace steering angle with provided safe value
/// - `DenyBreach`    → drop command, log reason, emit posture event
///
/// Only one action is returned per call. The first-triggered check wins.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum EnforceAction {
    Allow,
    ClampLinear(f64),
    ClampSteering(f64),
    DenyBreach(String),
}

// ---------------------------------------------------------------------------
// Validation Pipeline
// ---------------------------------------------------------------------------

/// Evaluates a proposed vehicle command against a kinematics contract.
///
/// Checks run in strict priority order (Priority 0 → 6). Returns the first
/// violation found, or `EnforceAction::Allow` if all checks pass.
#[must_use]
pub fn validate_vehicle_command(
    cmd: &ProposedVehicleCommand,
    contract: &VehicleKinematicsContract,
) -> EnforceAction {
    // ------------------------------------------------------------------
    // Priority 0: NaN/Inf guard — must run before ANY arithmetic.
    //
    // IEEE 754 NaN/Inf values poison every subsequent computation silently:
    //   - NaN comparisons always return false → branch logic becomes unsafe
    //   - NaN * finite = NaN → clamping silently produces NaN output
    //   - Inf - Inf = NaN → acceleration check produces NaN, passes as 0.0
    //   - NaN > threshold = false → bicycle model lateral check never fires
    //
    // None of these produce a panic in Rust. They silently pass invalid
    // commands to the actuator — an AV-class safety failure mode.
    //
    // Each field gets a distinct denial code for audit forensics: a NaN in
    // steering_angle_deg implies a different upstream bug than NaN in
    // linear_velocity_mps. Infinity is rejected alongside NaN.
    // ------------------------------------------------------------------
    if !cmd.linear_velocity_mps.is_finite() {
        return EnforceAction::DenyBreach("NAN_INF_LINEAR_VELOCITY".to_string());
    }
    if !cmd.current_velocity_mps.is_finite() {
        return EnforceAction::DenyBreach("NAN_INF_CURRENT_VELOCITY".to_string());
    }
    if !cmd.steering_angle_deg.is_finite() {
        return EnforceAction::DenyBreach("NAN_INF_STEERING_ANGLE".to_string());
    }
    if !cmd.current_steering_angle_deg.is_finite() {
        return EnforceAction::DenyBreach("NAN_INF_CURRENT_STEERING".to_string());
    }
    if !cmd.delta_time_s.is_finite() {
        return EnforceAction::DenyBreach("NAN_INF_DELTA_TIME".to_string());
    }

    // ------------------------------------------------------------------
    // Priority 1: Non-physical time delta
    // Zero or negative dt makes rate-of-change calculations undefined.
    // ------------------------------------------------------------------
    if cmd.delta_time_s <= 0.0 {
        return EnforceAction::DenyBreach("INVALID_TIME_DELTA".to_string());
    }

    // ------------------------------------------------------------------
    // Priority 2: Linear velocity hard ceiling
    // Checked before acceleration rate — a velocity-over-limit command
    // implies an over-limit acceleration; no need to compute it.
    // ------------------------------------------------------------------
    if cmd.linear_velocity_mps.abs() > contract.max_speed_mps {
        let clamped = contract.max_speed_mps * cmd.linear_velocity_mps.signum();
        return EnforceAction::ClampLinear(clamped);
    }

    // ------------------------------------------------------------------
    // Priorities 3–6: apply corrections progressively; evaluate P6 last.
    //
    // The pipeline accumulates corrections into `v` and `delta` rather
    // than returning at the first triggered check. This ensures:
    //
    //   - A P3/P4 velocity clamp does not suppress a P6 lateral-accel
    //     violation that would appear in the resulting state (Bug G).
    //   - A P5a/P5b steering clamp does not suppress a P6 lateral-accel
    //     violation in the rate-limited result (Bug C).
    //
    // P6 uses cmd.linear_velocity_mps (original commanded speed): when
    // ClampSteering is returned the caller applies the clamped steering
    // with the original velocity, so the safe angle is back-solved for
    // that speed. This also satisfies the proptest invariant which checks
    // lateral accel using the original commanded velocity.
    //
    // Return priority: steering corrections take precedence because a
    // lateral-accel violation is the most acute physical safety concern.
    // When only velocity needs correction, ClampLinear is returned.
    // ------------------------------------------------------------------
    let mut v = cmd.linear_velocity_mps;
    let mut v_clamped = false;
    let mut delta = cmd.steering_angle_deg;
    let mut delta_clamped = false;

    // Priority 3: Implied acceleration ceiling
    let implied_accel =
        (cmd.linear_velocity_mps - cmd.current_velocity_mps) / cmd.delta_time_s;

    if implied_accel > 0.0 && implied_accel > contract.max_accel_mps2 + 1e-9 {
        v = (cmd.current_velocity_mps + contract.max_accel_mps2 * cmd.delta_time_s)
            .clamp(-contract.max_speed_mps, contract.max_speed_mps);
        v_clamped = true;
    }

    // Priority 4: Implied deceleration ceiling
    // Asymmetric from acceleration: braking limit is typically higher.
    if implied_accel < 0.0 && implied_accel.abs() > contract.max_brake_mps2 + 1e-9 {
        v = (cmd.current_velocity_mps - contract.max_brake_mps2 * cmd.delta_time_s)
            .clamp(-contract.max_speed_mps, contract.max_speed_mps);
        v_clamped = true;
    }

    // Priority 5a: Absolute steering angle hard limit
    if delta.abs() > contract.max_steering_deg {
        delta = contract.max_steering_deg * delta.signum();
        delta_clamped = true;
    }

    // Priority 5b: Steering rate ceiling
    // Rate is measured from current_steering to the (possibly P5a-clamped)
    // delta so that a bounded target is never inflated back up by the rate.
    let steering_delta = delta - cmd.current_steering_angle_deg;
    let implied_steering_rate = steering_delta.abs() / cmd.delta_time_s;

    if implied_steering_rate > contract.max_steering_rate_deg_s {
        let max_delta_deg = contract.max_steering_rate_deg_s * cmd.delta_time_s;
        delta = (cmd.current_steering_angle_deg + max_delta_deg * steering_delta.signum())
            .clamp(-contract.max_steering_deg, contract.max_steering_deg);
        delta_clamped = true;
    }

    // Priority 6: Dynamic lateral acceleration envelope (bicycle model)
    //
    //   a_lat = (v² × |tan(δ)|) / L
    //
    // Guard: skip at near-zero velocity to avoid division by v² ≈ 0.
    let v2 = cmd.linear_velocity_mps.powi(2);
    if v2 > 1e-6 {
        let delta_rad = delta.to_radians();
        let implied_lat_accel = (v2 * delta_rad.tan().abs()) / contract.wheelbase_m;

        if implied_lat_accel > contract.max_lateral_accel_mps2 {
            let max_safe_tan =
                (contract.max_lateral_accel_mps2 * contract.wheelbase_m) / v2;
            delta = max_safe_tan.atan().to_degrees() * delta.signum();
            delta_clamped = true;
        }
    }

    match (v_clamped, delta_clamped) {
        (_, true) => EnforceAction::ClampSteering(delta),
        (true, false) => EnforceAction::ClampLinear(v),
        (false, false) => EnforceAction::Allow,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod kinematics_contract_tests {
    use super::*;

    // --- Allow cases --------------------------------------------------------

    #[test]
    fn test_nominal_command_passes_unhindered() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 9.5,
            delta_time_s: 0.2,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: 4.5,
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }

    #[test]
    fn test_zero_motion_command_passes() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 0.0,
            current_velocity_mps: 0.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }

    #[test]
    fn test_mrc_command_within_mrc_profile_passes() {
        let contract = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 3.0,
            current_velocity_mps: 2.8,
            delta_time_s: 0.2,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: 4.0,
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }

    #[test]
    fn test_mild_reverse_command_passes() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: -2.0,
            current_velocity_mps: -1.5,
            delta_time_s: 0.5,
            steering_angle_deg: -3.0,
            current_steering_angle_deg: -2.5,
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }

    // --- Deny cases ---------------------------------------------------------

    #[test]
    fn test_zero_time_delta_is_denied() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("INVALID_TIME_DELTA".to_string())
        );
    }

    #[test]
    fn test_negative_time_delta_is_denied() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: -0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("INVALID_TIME_DELTA".to_string())
        );
    }

    // --- Linear velocity clamping -------------------------------------------

    #[test]
    fn test_speed_above_ceiling_triggers_clamp_linear() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 40.0,
            current_velocity_mps: 34.0,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::ClampLinear(35.0)
        );
    }

    #[test]
    fn test_reverse_speed_above_ceiling_clamps_with_correct_sign() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: -40.0,
            current_velocity_mps: -20.0,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::ClampLinear(-35.0)
        );
    }

    #[test]
    fn test_excessive_acceleration_triggers_linear_clamping() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 25.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        match validate_vehicle_command(&cmd, &contract) {
            EnforceAction::ClampLinear(clamped) => {
                let expected = 10.0_f64 + (2.5 * 0.1);
                assert!((clamped - expected).abs() < 1e-9, "expected {expected}, got {clamped}");
            }
            other => panic!("Expected ClampLinear, got {other:?}"),
        }
    }

    #[test]
    fn test_excessive_braking_triggers_linear_clamping() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 0.0,
            current_velocity_mps: 30.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        match validate_vehicle_command(&cmd, &contract) {
            EnforceAction::ClampLinear(clamped) => {
                let expected = 30.0_f64 - (4.5 * 0.1);
                assert!(clamped > 0.0, "should not allow instant stop");
                assert!((clamped - expected).abs() < 1e-9, "expected {expected}, got {clamped}");
            }
            other => panic!("Expected ClampLinear for over-deceleration, got {other:?}"),
        }
    }

    // --- Steering clamping --------------------------------------------------

    #[test]
    fn test_excessive_steering_rate_triggers_steering_clamp() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 30.0,
            current_steering_angle_deg: 0.0,
        };
        match validate_vehicle_command(&cmd, &contract) {
            EnforceAction::ClampSteering(safe) => {
                assert!((safe - 4.5_f64).abs() < 1e-9, "expected 4.5, got {safe}");
            }
            other => panic!("Expected ClampSteering for excessive rate, got {other:?}"),
        }
    }

    #[test]
    fn test_high_speed_lateral_acceleration_forces_steering_clamp() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 30.0,
            current_velocity_mps: 30.0,
            delta_time_s: 0.5,
            steering_angle_deg: 20.0,
            current_steering_angle_deg: 0.0,
        };
        match validate_vehicle_command(&cmd, &contract) {
            EnforceAction::ClampSteering(safe) => {
                assert!(safe < 20.0, "must reduce steering angle");
                assert!(safe > 0.0, "sign must be preserved");
                assert!(safe < 2.0, "at 30 m/s, safe steering must be very small");
            }
            other => panic!("Expected ClampSteering for lateral accel breach, got {other:?}"),
        }
    }

    #[test]
    fn test_near_zero_velocity_skips_bicycle_model_division() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 0.001,
            current_velocity_mps: 0.001,
            delta_time_s: 0.1,
            steering_angle_deg: 30.0,
            current_steering_angle_deg: 27.0,
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }

    // --- MRC profile enforcement --------------------------------------------

    #[test]
    fn test_nominal_speed_breaches_mrc_profile() {
        let contract = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 15.0,
            current_velocity_mps: 14.0,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::ClampLinear(5.0)
        );
    }

    #[test]
    fn test_mrc_lateral_limit_is_tighter_than_nominal() {
        // 18° at 4 m/s: a_lat ≈ 1.857 m/s² — passes nominal (3.5), breaches MRC (1.5)
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 4.0,
            current_velocity_mps: 4.0,
            delta_time_s: 1.0,
            steering_angle_deg: 18.0,
            current_steering_angle_deg: 0.0,
        };
        let nominal = VehicleKinematicsContract::nominal_reference_profile();
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        assert_eq!(validate_vehicle_command(&cmd, &nominal), EnforceAction::Allow);
        match validate_vehicle_command(&cmd, &mrc) {
            EnforceAction::ClampSteering(s) => assert!(s < 18.0),
            other => panic!("MRC should clamp lateral breach, got {other:?}"),
        }
    }

    // --- Priority ordering --------------------------------------------------

    #[test]
    fn test_time_delta_check_fires_before_speed_check() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 999.0,
            current_velocity_mps: 0.0,
            delta_time_s: 0.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("INVALID_TIME_DELTA".to_string())
        );
    }

    #[test]
    fn test_speed_check_fires_before_accel_check() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 50.0,
            current_velocity_mps: 5.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::ClampLinear(35.0)
        );
    }

    // --- NaN/Inf guard (Priority 0) ----------------------------------------

    #[test]
    fn test_nan_linear_velocity_is_denied_before_any_arithmetic() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::NAN,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("NAN_INF_LINEAR_VELOCITY".to_string())
        );
    }

    #[test]
    fn test_inf_linear_velocity_is_denied() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::INFINITY,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("NAN_INF_LINEAR_VELOCITY".to_string())
        );
    }

    #[test]
    fn test_neg_inf_linear_velocity_is_denied() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::NEG_INFINITY,
            current_velocity_mps: 0.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("NAN_INF_LINEAR_VELOCITY".to_string())
        );
    }

    #[test]
    fn test_nan_current_velocity_is_denied_with_specific_code() {
        // NaN current_velocity_mps poisons acceleration calc:
        //   implied_accel = (v_cmd - NaN) / dt = NaN
        //   NaN > max_accel = false → accel check silently passes
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: f64::NAN,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("NAN_INF_CURRENT_VELOCITY".to_string())
        );
    }

    #[test]
    fn test_nan_steering_angle_is_denied_with_specific_code() {
        // NaN steering_angle_deg passed to tan() produces NaN lateral accel.
        // NaN > max_lateral_accel = false → bicycle model silently passes.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: f64::NAN,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("NAN_INF_STEERING_ANGLE".to_string())
        );
    }

    #[test]
    fn test_nan_current_steering_is_denied_with_specific_code() {
        // NaN current_steering_angle_deg poisons steering rate:
        //   steering_delta = angle - NaN = NaN; NaN / dt = NaN
        //   NaN > max_rate = false → rate check silently passes
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: f64::NAN,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("NAN_INF_CURRENT_STEERING".to_string())
        );
    }

    #[test]
    fn test_nan_delta_time_is_denied_with_specific_code() {
        // NaN delta_time_s: NaN <= 0.0 = false → Priority 1 does NOT fire.
        // Without Priority 0: v_delta / NaN = NaN, silently passes all checks.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: f64::NAN,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("NAN_INF_DELTA_TIME".to_string())
        );
    }

    #[test]
    fn test_inf_delta_time_is_denied_before_zero_check() {
        // f64::INFINITY > 0.0 is true → Priority 1 would NOT catch it.
        // v_delta / INFINITY = 0.0 → accel check sees zero, passes everything.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: f64::INFINITY,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("NAN_INF_DELTA_TIME".to_string())
        );
    }

    #[test]
    fn test_nan_guard_fires_before_time_delta_check() {
        // Both NaN dt AND zero dt present. Priority 0 must fire first.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::NAN,
            current_velocity_mps: 0.0,
            delta_time_s: 0.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach("NAN_INF_LINEAR_VELOCITY".to_string()),
            "NaN guard (priority 0) must fire before zero-dt check (priority 1)"
        );
    }
}
