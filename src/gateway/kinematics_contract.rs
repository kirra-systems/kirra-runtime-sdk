// src/gateway/kinematics_contract.rs
//
// Deterministic vehicle kinematics safety contract for Aegis AV flight envelope protection.
//
// This module answers exactly one question: "Is this proposed vehicle command physically
// safe to execute on this platform, given the current kinematic state?"
//
// Checks run in strict priority order. A check that fires returns immediately.
// See docs/kinematics_envelope_protection.md for the full invariant specification.
//
// Security invariants respected:
//   - No interaction with AEGIS_ADMIN_TOKEN or any auth primitives (wrong layer)
//   - No DDS/ROS2 publishing (ros2_adapter.rs owns NaN/Inf rejection)
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
    /// The effective steering limit tightens as speed increases:
    /// `a_lat = (v² × |tan(δ)|) / L ≤ max_lateral_accel_mps2`
    pub max_lateral_accel_mps2: f64,

    /// Vehicle wheelbase (meters). Used in the bicycle model denominator.
    /// Must match the physical platform.
    pub wheelbase_m: f64,
}

impl VehicleKinematicsContract {
    /// Full operational profile for a standard reference vehicle platform.
    ///
    /// Suitable for `FleetPosture::Nominal`.
    pub fn nominal_reference_profile() -> Self {
        Self {
            max_speed_mps: 35.0,           // ~78 mph operational ceiling
            max_accel_mps2: 2.5,           // ~0.25g — comfortable acceleration
            max_brake_mps2: 4.5,           // ~0.46g — service braking
            max_steering_deg: 35.0,        // Maximum low-speed wheel articulation
            max_steering_rate_deg_s: 45.0, // Physical steering rack rate limit
            min_follow_distance_m: 2.0,    // Absolute close-proximity buffer
            max_lateral_accel_mps2: 3.5,   // ~0.36g — prevents rollover/skid onset
            wheelbase_m: 2.8,              // Standard mid-size vehicle wheelbase
        }
    }

    /// Minimal Risk Condition (MRC) fallback profile for degraded fleet posture.
    ///
    /// Suitable for `FleetPosture::Degraded`. Constrains the vehicle to a low-energy
    /// state from which a graceful safe stop can be commanded at any time.
    pub fn mrc_fallback_profile() -> Self {
        Self {
            max_speed_mps: 5.0,            // ~11 mph — safe crawl speed
            max_accel_mps2: 1.0,           // Subdued acceleration curve
            max_brake_mps2: 3.0,           // Gradual deceleration profile
            max_steering_deg: 15.0,        // Restricts high-amplitude maneuvering
            max_steering_rate_deg_s: 20.0, // Slow, deliberate steering only
            min_follow_distance_m: 5.0,    // Expanded safety margins during degradation
            max_lateral_accel_mps2: 1.5,   // ~0.15g — minimizes side-slip risk
            wheelbase_m: 2.8,              // Physical constant; unchanged by posture
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
/// The calling policy layer is responsible for acting on this:
/// - `Allow`         → forward the command to the actuator
/// - `ClampLinear`   → replace linear velocity with the provided safe value
/// - `ClampSteering` → replace steering angle with the provided safe value
/// - `DenyBreach`    → drop the command, log the reason, emit a posture event
///
/// Only one action is returned per call. The first-triggered check wins.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum EnforceAction {
    /// Command is within all physical invariants. Forward to actuator.
    Allow,

    /// Linear velocity component exceeds a limit. Replace with the provided safe value.
    ClampLinear(f64),

    /// Steering angle component exceeds a limit. Replace with the provided safe value.
    /// May be triggered by steering rate (priority 5) or bicycle model (priority 6).
    ClampSteering(f64),

    /// Command is non-physical or violates a hard invariant that cannot be clamped.
    DenyBreach(String),
}

// ---------------------------------------------------------------------------
// Validation Pipeline
// ---------------------------------------------------------------------------

/// Evaluates a proposed vehicle command against a kinematics contract.
///
/// Checks run in strict priority order. Returns the first violation found,
/// or `EnforceAction::Allow` if all checks pass.
#[must_use]
pub fn validate_vehicle_command(
    cmd: &ProposedVehicleCommand,
    contract: &VehicleKinematicsContract,
) -> EnforceAction {
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
    // implies an over-limit acceleration too; no need to compute it.
    // ------------------------------------------------------------------
    if cmd.linear_velocity_mps.abs() > contract.max_speed_mps {
        let clamped = contract.max_speed_mps * cmd.linear_velocity_mps.signum();
        return EnforceAction::ClampLinear(clamped);
    }

    // ------------------------------------------------------------------
    // Priority 3: Implied acceleration ceiling
    // ------------------------------------------------------------------
    let implied_accel =
        (cmd.linear_velocity_mps - cmd.current_velocity_mps) / cmd.delta_time_s;

    if implied_accel > 0.0 && implied_accel > contract.max_accel_mps2 {
        let safe_speed =
            cmd.current_velocity_mps + (contract.max_accel_mps2 * cmd.delta_time_s);
        return EnforceAction::ClampLinear(safe_speed);
    }

    // ------------------------------------------------------------------
    // Priority 4: Implied deceleration ceiling
    // Asymmetric from acceleration: braking limit is typically higher.
    // ------------------------------------------------------------------
    if implied_accel < 0.0 && implied_accel.abs() > contract.max_brake_mps2 {
        let safe_speed =
            cmd.current_velocity_mps - (contract.max_brake_mps2 * cmd.delta_time_s);
        return EnforceAction::ClampLinear(safe_speed);
    }

    // ------------------------------------------------------------------
    // Priority 5: Steering rate ceiling
    // Prevents instantaneous full-lock transitions the physical rack
    // cannot achieve and that produce violent lateral load transfer.
    // ------------------------------------------------------------------
    let steering_delta = cmd.steering_angle_deg - cmd.current_steering_angle_deg;
    let implied_steering_rate = steering_delta.abs() / cmd.delta_time_s;

    if implied_steering_rate > contract.max_steering_rate_deg_s {
        let max_delta = contract.max_steering_rate_deg_s * cmd.delta_time_s;
        let safe_steering =
            cmd.current_steering_angle_deg + (max_delta * steering_delta.signum());
        return EnforceAction::ClampSteering(safe_steering);
    }

    // ------------------------------------------------------------------
    // Priority 6: Dynamic lateral acceleration envelope (bicycle model)
    //
    //   a_lat = (v² × |tan(δ)|) / L
    //
    // If implied lateral acceleration exceeds the contract limit, back-solve
    // the maximum safe steering angle for the current velocity:
    //
    //   δ_max = atan((a_lat_max × L) / v²)
    //
    // Guard: skip at near-zero velocity to avoid dividing by v² ≈ 0;
    // the resulting safe angle would be very large and already bounded
    // by max_steering_deg above.
    // ------------------------------------------------------------------
    let v2 = cmd.linear_velocity_mps.powi(2);
    if v2 > 1e-6 {
        let steering_rad = cmd.steering_angle_deg.to_radians();
        let implied_lat_accel =
            (v2 * steering_rad.tan().abs()) / contract.wheelbase_m;

        if implied_lat_accel > contract.max_lateral_accel_mps2 {
            let max_safe_tan =
                (contract.max_lateral_accel_mps2 * contract.wheelbase_m) / v2;
            let max_safe_steering_deg = max_safe_tan.atan().to_degrees();
            let safe_steering =
                max_safe_steering_deg * cmd.steering_angle_deg.signum();
            return EnforceAction::ClampSteering(safe_steering);
        }
    }

    EnforceAction::Allow
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
        // Expected: 10.0 + (2.5 × 0.1) = 10.25
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
        // Expected: 30.0 - (4.5 × 0.1) = 29.55
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
            steering_angle_deg: 30.0, // 300 deg/s > 45 limit
            current_steering_angle_deg: 0.0,
        };
        // max_delta = 45.0 × 0.1 = 4.5°
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
            delta_time_s: 0.5, // Long dt so steering rate check passes
            steering_angle_deg: 20.0,
            current_steering_angle_deg: 0.0,
        };
        // a_lat = (900 × tan(20°)) / 2.8 ≈ 116.9 m/s² >> 3.5 limit
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
            current_steering_angle_deg: 27.0, // Rate: 30 deg/s < 45 limit
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
            EnforceAction::ClampSteering(s) => {
                assert!(s < 18.0, "MRC must clamp what nominal allows");
            }
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
            linear_velocity_mps: 50.0, // Over 35 m/s ceiling (priority 2)
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
}
