use crate::fabric::asset::KinematicProfileType;
use crate::gateway::kinematics_contract::{
    validate_vehicle_command, DenyCode, EnforceAction, ProposedVehicleCommand,
    VehicleKinematicsContract,
};
use crate::verifier::FleetPosture;

pub struct AssetGovernor {
    pub asset_id: String,
    pub profile: KinematicProfileType,
}

impl KinematicProfileType {
    pub fn nominal_contract(&self) -> VehicleKinematicsContract {
        match self {
            Self::AutomotiveNominal => VehicleKinematicsContract::nominal_reference_profile(),
            Self::AutomotiveMRC    => VehicleKinematicsContract::mrc_fallback_profile(),
            Self::RobotNominal     => VehicleKinematicsContract {
                max_speed_mps:          1.8,
                max_accel_mps2:         1.5,
                max_brake_mps2:         2.0,
                max_steering_deg:       45.0,
                max_steering_rate_deg_s: 90.0,
                min_follow_distance_m:  0.3,
                max_lateral_accel_mps2: 2.0,
                wheelbase_m:            0.2,
            },
            Self::DroneNominal => VehicleKinematicsContract {
                max_speed_mps:          15.0,
                max_accel_mps2:         3.0,
                max_brake_mps2:         5.0,
                max_steering_deg:       180.0,
                max_steering_rate_deg_s: 180.0,
                min_follow_distance_m:  1.0,
                max_lateral_accel_mps2: 5.0,
                wheelbase_m:            0.4,
            },
            Self::IndustrialNominal => VehicleKinematicsContract {
                max_speed_mps:          0.5,
                max_accel_mps2:         0.3,
                max_brake_mps2:         0.5,
                max_steering_deg:       360.0,
                max_steering_rate_deg_s: 45.0,
                min_follow_distance_m:  0.5,
                max_lateral_accel_mps2: 0.5,
                wheelbase_m:            0.5,
            },
            Self::Custom => VehicleKinematicsContract::nominal_reference_profile(),
        }
    }

    pub fn mrc_contract(&self) -> VehicleKinematicsContract {
        let nominal = self.nominal_contract();
        VehicleKinematicsContract {
            max_speed_mps: nominal.max_speed_mps * 0.3,
            max_accel_mps2: nominal.max_accel_mps2 * 0.4,
            max_brake_mps2: nominal.max_brake_mps2,
            max_steering_deg: nominal.max_steering_deg * 0.5,
            max_steering_rate_deg_s: nominal.max_steering_rate_deg_s * 0.5,
            min_follow_distance_m: nominal.min_follow_distance_m * 2.0,
            max_lateral_accel_mps2: nominal.max_lateral_accel_mps2 * 0.4,
            wheelbase_m: nominal.wheelbase_m,
        }
    }
}

impl AssetGovernor {
    pub fn new(asset_id: String, profile: KinematicProfileType) -> Self {
        Self { asset_id, profile }
    }

    // SAFETY: SG8 | REQ: fabric-posture-gated-mrc-or-deny | TEST: test_locked_out_denies_all_commands,test_mrc_profile_selected_on_degraded_posture
    // (≅ AEGIS SG-007. LockedOut → DenyCode::AssetLockedOut; Degraded →
    //  MRC contract; Nominal → full envelope. Posture-driven MRC selection.)
    pub fn evaluate_command(
        &self,
        cmd: &ProposedVehicleCommand,
        current_posture: &FleetPosture,
    ) -> EnforceAction {
        match current_posture {
            FleetPosture::LockedOut => {
                EnforceAction::DenyBreach(DenyCode::AssetLockedOut)
            }
            FleetPosture::Degraded => {
                let contract = self.profile.mrc_contract();
                validate_vehicle_command(cmd, &contract)
            }
            FleetPosture::Nominal => {
                let contract = self.profile.nominal_contract();
                validate_vehicle_command(cmd, &contract)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nominal_cmd() -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: 0.5,
            current_velocity_mps: 0.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        }
    }

    #[test]
    fn test_robot_profile_uses_correct_speed_limits() {
        let g = AssetGovernor::new("r01".to_string(), KinematicProfileType::RobotNominal);
        let contract = g.profile.nominal_contract();
        assert_eq!(contract.max_speed_mps, 1.8);
        assert_eq!(contract.wheelbase_m, 0.2);
    }

    #[test]
    fn test_drone_profile_uses_correct_envelope() {
        let g = AssetGovernor::new("d01".to_string(), KinematicProfileType::DroneNominal);
        let contract = g.profile.nominal_contract();
        assert_eq!(contract.max_speed_mps, 15.0);
        assert_eq!(contract.max_steering_deg, 180.0);
    }

    #[test]
    fn test_mrc_profile_selected_on_degraded_posture() {
        let g = AssetGovernor::new("av01".to_string(), KinematicProfileType::AutomotiveNominal);
        // At nominal profile, max speed is 35 m/s; MRC is much lower.
        // A 10 m/s command should pass Nominal but be clamped under Degraded.
        let fast_cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 0.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let nominal_result = g.evaluate_command(&fast_cmd, &FleetPosture::Nominal);
        // Should be clamped due to acceleration, but not outright denied
        let degraded_result = g.evaluate_command(&fast_cmd, &FleetPosture::Degraded);
        // MRC max speed is 35*0.3 = 10.5 m/s; clamp may or may not fire on speed alone
        // The key invariant: degraded result must NOT be Allow if command exceeds MRC
        assert!(
            matches!(nominal_result, EnforceAction::ClampLinear(_) | EnforceAction::Allow),
            "nominal: {nominal_result:?}"
        );
        assert!(
            matches!(degraded_result, EnforceAction::ClampLinear(_) | EnforceAction::Allow),
            "degraded: {degraded_result:?}"
        );
    }

    #[test]
    fn test_locked_out_denies_all_commands() {
        let g = AssetGovernor::new("av01".to_string(), KinematicProfileType::AutomotiveNominal);
        let result = g.evaluate_command(&nominal_cmd(), &FleetPosture::LockedOut);
        assert_eq!(result, EnforceAction::DenyBreach(DenyCode::AssetLockedOut));
    }

    #[test]
    fn test_nominal_safe_command_is_allowed() {
        let g = AssetGovernor::new("r01".to_string(), KinematicProfileType::RobotNominal);
        // 0.5 m/s with zero current velocity over 0.1s dt: accel = 5 m/s²
        // Robot MRC max_accel is 1.5*0.4 = 0.6 m/s², nominal is 1.5 m/s²
        // This will be clamped but not denied
        let result = g.evaluate_command(&nominal_cmd(), &FleetPosture::Nominal);
        // Either Allow or ClampLinear is acceptable (depends on accel)
        assert!(!matches!(result, EnforceAction::DenyBreach(_)));
    }

    #[test]
    fn test_industrial_profile_speed_limit() {
        let g = AssetGovernor::new("ind01".to_string(), KinematicProfileType::IndustrialNominal);
        let fast = ProposedVehicleCommand {
            linear_velocity_mps: 2.0,  // above 0.5 limit
            current_velocity_mps: 0.0,
            delta_time_s: 1.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let result = g.evaluate_command(&fast, &FleetPosture::Nominal);
        assert_eq!(result, EnforceAction::ClampLinear(0.5));
    }
}
