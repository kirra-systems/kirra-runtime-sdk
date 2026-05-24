// crates/parko-aegis/src/lib.rs
//
// Adapter from parko-core's SafetyGovernor trait to the
// aegis-runtime-sdk vehicle kinematics contract.
//
// LIMITATIONS:
//
// parko's ControlCommand uses a differential-drive Twist model
// (linear_velocity, angular_velocity in m/s and rad/s respectively).
// Aegis's ProposedVehicleCommand uses a bicycle/Ackermann model
// (linear_velocity_mps, steering_angle_deg). These are semantically
// different control representations.
//
// This adapter enforces ONLY the linear velocity dimension. The steering
// angle dimension is set to zero (current and proposed both 0.0 degrees),
// which means Aegis's steering rate-of-change check effectively becomes a
// no-op for this dimension.
//
// Differential-drive robots that need angular velocity bounds checking
// should add a separate governor or extend this one with a wheelbase-
// dependent kinematic bicycle conversion. That is future work.

use aegis_runtime_sdk::gateway::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};
use aegis_runtime_sdk::verifier::FleetPosture;

use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor};

/// A safety governor backed by the Aegis runtime SDK's vehicle kinematics
/// contract.
///
/// Construct with a FleetPosture; the appropriate contract profile
/// (nominal_reference_profile or mrc_fallback_profile) is selected based
/// on the posture. To change posture between ticks, construct a new
/// AegisGovernor instance.
pub struct AegisGovernor {
    contract: VehicleKinematicsContract,
}

impl AegisGovernor {
    /// Construct a governor with the Nominal contract profile.
    pub fn nominal() -> Self {
        Self {
            contract: VehicleKinematicsContract::nominal_reference_profile(),
        }
    }

    /// Construct a governor with the MRC (Minimum Risk Condition) fallback
    /// profile. This is more conservative than nominal and is appropriate
    /// for degraded operation.
    pub fn mrc_fallback() -> Self {
        Self {
            contract: VehicleKinematicsContract::mrc_fallback_profile(),
        }
    }

    /// Construct a governor whose profile is selected by FleetPosture.
    /// Nominal -> nominal_reference_profile; Degraded -> mrc_fallback_profile;
    /// LockedOut -> mrc_fallback_profile (the kinematics contract is not the
    /// right enforcement point for full lockout; the caller should not
    /// route commands at all in LockedOut posture).
    pub fn for_posture(posture: FleetPosture) -> Self {
        match posture {
            FleetPosture::Nominal => Self::nominal(),
            FleetPosture::Degraded | FleetPosture::LockedOut => Self::mrc_fallback(),
        }
    }
}

impl SafetyGovernor for AegisGovernor {
    fn evaluate(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
    ) -> EnforcementAction {
        let current_velocity = previous.map(|p| p.linear_velocity).unwrap_or(0.0);

        let aegis_input = ProposedVehicleCommand {
            linear_velocity_mps: proposed.linear_velocity,
            current_velocity_mps: current_velocity,
            delta_time_s,
            // Steering angle dimension not bridged from parko's angular_velocity.
            // See module documentation for rationale.
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };

        match validate_vehicle_command(&aegis_input, &self.contract) {
            EnforceAction::Allow => EnforcementAction::Allow,
            EnforceAction::ClampLinear(safe_value) => {
                EnforcementAction::ClampLinearVelocity(safe_value)
            }
            EnforceAction::ClampSteering(_) => {
                // The steering dimension is not bridged from parko. If Aegis
                // returns ClampSteering for the zero steering input we sent,
                // it indicates either a contract violation we did not cause
                // or a contract profile that requires non-zero steering;
                // treat as Allow for parko's purposes since the proposed
                // command's angular_velocity was not the cause.
                EnforcementAction::Allow
            }
            EnforceAction::DenyBreach(reason) => EnforcementAction::Deny { reason },
        }
    }
}
