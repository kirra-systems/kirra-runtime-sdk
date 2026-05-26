// crates/parko-kirra/src/lib.rs
//
// Adapter from parko-core's SafetyGovernor trait to the
// kirra-runtime-sdk vehicle kinematics contract.
//
// LIMITATIONS:
//
// parko's ControlCommand uses a differential-drive Twist model
// (linear_velocity, angular_velocity in m/s and rad/s respectively).
// Kirra's ProposedVehicleCommand uses a bicycle/Ackermann model
// (linear_velocity_mps, steering_angle_deg). These are semantically
// different control representations.
//
// This adapter enforces ONLY the linear velocity dimension. The steering
// angle dimension is set to zero (current and proposed both 0.0 degrees),
// which means Kirra's steering rate-of-change check effectively becomes a
// no-op for this dimension.
//
// Differential-drive robots that need angular velocity bounds checking
// should add a separate governor or extend this one with a wheelbase-
// dependent kinematic bicycle conversion. That is future work.

use kirra_runtime_sdk::gateway::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};
use kirra_runtime_sdk::verifier::FleetPosture;

use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};

/// A safety governor backed by the Kirra runtime SDK's vehicle kinematics
/// contract.
///
/// Holds both nominal and MRC fallback contract profiles and selects
/// between them per-call based on the posture passed to `evaluate()`.
pub struct KirraGovernor {
    nominal_contract: VehicleKinematicsContract,
    fallback_contract: VehicleKinematicsContract,
}

impl KirraGovernor {
    /// Construct a governor that holds both nominal and MRC fallback
    /// contract profiles and selects between them per-call based on
    /// the posture passed to `evaluate()`.
    pub fn new() -> Self {
        Self {
            nominal_contract: VehicleKinematicsContract::nominal_reference_profile(),
            fallback_contract: VehicleKinematicsContract::mrc_fallback_profile(),
        }
    }

    /// Construct a governor that uses the nominal profile regardless of
    /// the posture passed to evaluate(). Kept for convenience and
    /// backward compatibility.
    pub fn nominal() -> Self {
        let profile = VehicleKinematicsContract::nominal_reference_profile();
        Self {
            nominal_contract: profile.clone(),
            fallback_contract: profile,
        }
    }

    /// Construct a governor that uses the MRC fallback profile regardless
    /// of the posture passed to evaluate(). Kept for convenience and
    /// backward compatibility.
    pub fn mrc_fallback() -> Self {
        let profile = VehicleKinematicsContract::mrc_fallback_profile();
        Self {
            nominal_contract: profile.clone(),
            fallback_contract: profile,
        }
    }

    /// Backward-compatible posture-based constructor. Equivalent to
    /// new() but kept for callers using the older API.
    pub fn for_posture(posture: FleetPosture) -> Self {
        match posture {
            FleetPosture::Nominal => Self::nominal(),
            FleetPosture::Degraded | FleetPosture::LockedOut => Self::mrc_fallback(),
        }
    }
}

impl SafetyGovernor for KirraGovernor {
    fn evaluate(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
    ) -> EnforcementAction {
        let current_velocity = previous.map(|p| p.linear_velocity).unwrap_or(0.0);

        let contract = match posture {
            SafetyPosture::Nominal => &self.nominal_contract,
            SafetyPosture::Degraded | SafetyPosture::LockedOut => &self.fallback_contract,
        };

        let kirra_input = ProposedVehicleCommand {
            linear_velocity_mps: proposed.linear_velocity,
            current_velocity_mps: current_velocity,
            delta_time_s,
            // Steering angle dimension not bridged from parko's angular_velocity.
            // See module documentation for rationale.
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };

        match validate_vehicle_command(&kirra_input, contract) {
            EnforceAction::Allow => EnforcementAction::Allow,
            EnforceAction::ClampLinear(safe_value) => {
                EnforcementAction::ClampLinearVelocity(safe_value)
            }
            EnforceAction::ClampSteering(_) => EnforcementAction::Allow,
            EnforceAction::DenyBreach(reason) => EnforcementAction::Deny { reason },
        }
    }
}
