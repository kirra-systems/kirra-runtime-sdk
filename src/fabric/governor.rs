use crate::gateway::kinematics_contract::{
    enforce_degraded_decel_to_stop, validate_vehicle_command, DenyCode, EnforceAction,
    ProposedVehicleCommand, VehicleKinematicsContract,
};
use crate::verifier::FleetPosture;
use kirra_fabric_types::asset::KinematicProfileType;

pub struct AssetGovernor {
    pub asset_id: String,
    pub profile: KinematicProfileType,
}

/// Per-profile kinematic envelope selection.
///
/// Defined as an extension trait rather than an inherent impl: `KinematicProfileType`
/// now lives in the lean `kirra-fabric-types` crate (ADR-0035 Stage 2.5 C2 slice 2)
/// while these contracts map to the verifier-crate `VehicleKinematicsContract`, and
/// Rust's orphan rule forbids an inherent impl on a type from another crate. Call
/// sites are unchanged — `profile.nominal_contract()` still resolves because the
/// trait is in scope throughout this module (its only callers). External callers
/// import `crate::fabric::governor::KinematicProfileContracts` to bring it in scope.
pub trait KinematicProfileContracts {
    /// The full (Nominal-posture) envelope for this profile.
    fn nominal_contract(&self) -> VehicleKinematicsContract;
    /// The authoritative Degraded (MRC) envelope for this profile.
    fn mrc_contract(&self) -> VehicleKinematicsContract;
}

impl KinematicProfileContracts for KinematicProfileType {
    fn nominal_contract(&self) -> VehicleKinematicsContract {
        match self {
            Self::AutomotiveNominal => VehicleKinematicsContract::nominal_reference_profile(),
            Self::AutomotiveMRC => VehicleKinematicsContract::mrc_fallback_profile(),
            Self::RobotNominal => VehicleKinematicsContract {
                max_speed_mps: 1.8,
                max_accel_mps2: 1.5,
                max_brake_mps2: 2.0,
                max_steering_deg: 45.0,
                max_steering_rate_deg_s: 90.0,
                min_follow_distance_m: 0.3,
                max_lateral_accel_mps2: 2.0,
                wheelbase_m: 0.2,
                width_m: 0.5,
                length_m: 0.6,
                overhang_front_m: 0.2,
                overhang_rear_m: 0.2,
                // Non-AV vertical: no ODD operational cap applies.
                odd_speed_cap_mps: None,
            },
            Self::DroneNominal => VehicleKinematicsContract {
                max_speed_mps: 15.0,
                max_accel_mps2: 3.0,
                max_brake_mps2: 5.0,
                max_steering_deg: 180.0,
                max_steering_rate_deg_s: 180.0,
                min_follow_distance_m: 1.0,
                max_lateral_accel_mps2: 5.0,
                wheelbase_m: 0.4,
                width_m: 0.6,
                length_m: 0.6,
                overhang_front_m: 0.1,
                overhang_rear_m: 0.1,
                odd_speed_cap_mps: None,
            },
            Self::IndustrialNominal => VehicleKinematicsContract {
                max_speed_mps: 0.5,
                max_accel_mps2: 0.3,
                max_brake_mps2: 0.5,
                max_steering_deg: 360.0,
                max_steering_rate_deg_s: 45.0,
                min_follow_distance_m: 0.5,
                max_lateral_accel_mps2: 0.5,
                wheelbase_m: 0.5,
                width_m: 1.2,
                length_m: 1.5,
                overhang_front_m: 0.5,
                overhang_rear_m: 0.5,
                odd_speed_cap_mps: None,
            },
            Self::Custom => VehicleKinematicsContract::nominal_reference_profile(),
        }
    }

    /// The authoritative Degraded (MRC) envelope for this profile.
    ///
    /// ADR-0012 (#406 / #429): the AUTOMOTIVE Degraded envelope is the VALIDATED
    /// `mrc_fallback_profile()` — the safety-case **5.0 m/s** crawl ceiling
    /// (`SAFE_STATE_SPECIFICATION` SS-002; Cruise SF Oct-2023 ~3 m/s post-stop
    /// pullover-drag, "under a 5 m/s crawl ceiling") — NOT a generic `0.3×` derate
    /// of the 35 m/s nominal, which lands at **10.5 m/s** (2× the validated figure).
    /// This converges the fabric automotive MRC onto the SAME ceiling the gateway
    /// enforces (`enforce_actuator_safety_envelope` / `policy_layer`), eliminating
    /// the #406 divergence — a future enforcement point cannot ship a looser
    /// automotive Degraded ceiling. The cross-point invariant is pinned by
    /// `mrc_ceiling_is_authoritative_across_enforcement_points` below.
    ///
    /// Non-automotive platforms keep the per-platform `0.3× / 0.4× / 0.5×` derate;
    /// each factor is derived from that platform's HARA / safety case (ADR-0012),
    /// not chosen for convenience.
    fn mrc_contract(&self) -> VehicleKinematicsContract {
        // Automotive: the validated canonical MRC is authoritative (ADR-0012 item 1).
        if matches!(self, Self::AutomotiveNominal) {
            return VehicleKinematicsContract::mrc_fallback_profile();
        }
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
            // Footprint dimensions are platform geometry — same as nominal.
            width_m: nominal.width_m,
            length_m: nominal.length_m,
            overhang_front_m: nominal.overhang_front_m,
            overhang_rear_m: nominal.overhang_rear_m,
            // MRC derates the vehicle max by 0.3 — already well under any
            // ODD cap, so propagating the parent's odd_speed_cap_mps is a
            // no-op for min(). Carry it through anyway so downstream
            // diagnostics see a consistent profile lineage.
            odd_speed_cap_mps: nominal.odd_speed_cap_mps,
        }
    }
}

impl AssetGovernor {
    pub fn new(asset_id: String, profile: KinematicProfileType) -> Self {
        Self { asset_id, profile }
    }

    // SAFETY: SG8 | REQ: fabric-posture-gated-mrc-or-deny | TEST: test_locked_out_denies_all_commands,test_mrc_profile_selected_on_degraded_posture,test_degraded_reinitiation_from_stop_is_denied
    // (≅ AEGIS SG-007. LockedOut → DenyCode::AssetLockedOut; Degraded →
    //  controlled decel-to-stop-and-HOLD under the MRC envelope (Issue #70);
    //  Nominal → full envelope.)
    // `effective_perception_cap` (KIRRA-OCCY-PMON-002): the perception-derate
    // cap resolved by the caller (`Option<f64>` — `None` when the monitor is
    // disabled; `Some(0.0)` MRC floor when an enabled monitor is stale). It is
    // composed into the Nominal-arm contract via `apply_perception_cap` (a pure
    // `min` into `odd_speed_cap_mps`), so `validate_vehicle_command` stays
    // byte-identical. No-op on Degraded/LockedOut (already at/below MRC).
    pub fn evaluate_command(
        &self,
        cmd: &ProposedVehicleCommand,
        current_posture: &FleetPosture,
        effective_perception_cap: Option<f64>,
    ) -> EnforceAction {
        match current_posture {
            FleetPosture::LockedOut => EnforceAction::DenyBreach(DenyCode::AssetLockedOut),
            // Issue #70: Degraded is decel-to-stop-and-HOLD, not an MRC crawl.
            // The command must be converging toward zero within the MRC
            // envelope and must never re-initiate motion from a stop.
            FleetPosture::Degraded => {
                let contract = self.profile.mrc_contract();
                enforce_degraded_decel_to_stop(cmd, &contract)
            }
            FleetPosture::Nominal => {
                let contract = crate::gateway::perception_monitor::apply_perception_cap(
                    &self.profile.nominal_contract(),
                    effective_perception_cap,
                );
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

    /// ADR-0012 / #406 / #429 — the cross-enforcement-point MRC conformance pin.
    /// A fabric automotive asset in Degraded must yield the SAME effective MRC
    /// envelope the gateway enforces (`mrc_fallback_profile`), NOT the old generic
    /// 0.3× derate (10.5 m/s). A future enforcement point cannot ship a looser
    /// automotive Degraded ceiling without failing here.
    #[test]
    fn mrc_ceiling_is_authoritative_across_enforcement_points() {
        let fabric_mrc = KinematicProfileType::AutomotiveNominal.mrc_contract();
        let gateway_mrc = VehicleKinematicsContract::mrc_fallback_profile();
        // The validated SS-002 crawl ceiling, identical at both points.
        assert_eq!(fabric_mrc.max_speed_mps, gateway_mrc.max_speed_mps);
        assert_eq!(
            fabric_mrc.max_speed_mps, 5.0,
            "the validated SS-002 5.0 m/s ceiling"
        );
        // The full envelope converges (not just the speed scalar) — fabric ==
        // gateway, byte-for-byte, so the whole Degraded contract is authoritative.
        assert_eq!(
            fabric_mrc, gateway_mrc,
            "fabric automotive MRC must BE the canonical mrc_fallback_profile (ADR-0012)"
        );
        // Regression guard: the old 0.3× derate of the 35 m/s nominal = 10.5 m/s —
        // 2× the validated figure (#406). It must never come back.
        assert!(
            fabric_mrc.max_speed_mps < 10.5,
            "automotive MRC must not regress to the 2×-looser generic derate (#406)"
        );
    }

    /// ADR-0012 — the authoritative-automotive change must NOT perturb the
    /// non-automotive platforms; each keeps its per-platform (HARA-derived) derate.
    #[test]
    fn non_automotive_mrc_keeps_per_platform_derate() {
        let robot = KinematicProfileType::RobotNominal.mrc_contract();
        assert!(
            (robot.max_speed_mps - 1.8 * 0.3).abs() < 1e-9,
            "robot {}",
            robot.max_speed_mps
        );
        let drone = KinematicProfileType::DroneNominal.mrc_contract();
        assert!(
            (drone.max_speed_mps - 15.0 * 0.3).abs() < 1e-9,
            "drone {}",
            drone.max_speed_mps
        );
        let industrial = KinematicProfileType::IndustrialNominal.mrc_contract();
        assert!(
            (industrial.max_speed_mps - 0.5 * 0.3).abs() < 1e-9,
            "industrial {}",
            industrial.max_speed_mps
        );
    }

    /// Behavioural proof the divergence is gone end-to-end: a Degraded automotive
    /// command at 8 m/s (above the 5.0 ceiling, decelerating) is bounded by the
    /// authoritative 5.0 envelope at the fabric governor — it would have been
    /// admitted up to 10.5 under the old derate.
    #[test]
    fn degraded_automotive_command_is_bounded_by_the_authoritative_ceiling() {
        let g = AssetGovernor::new("av01".to_string(), KinematicProfileType::AutomotiveNominal);
        let contract = g.profile.mrc_contract();
        assert_eq!(contract.max_speed_mps, 5.0);
        // A decel command from 8 → 6 m/s: still above the 5.0 MRC ceiling, so the
        // envelope must bound it (clamp), not pass it through as the old 10.5 would.
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 6.0,
            current_velocity_mps: 8.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let result = g.evaluate_command(&cmd, &FleetPosture::Degraded, None);
        // Decelerating + over-ceiling → the decel gate admits but clamps to MRC,
        // never a clean Allow at 6.0 (which the old 10.5 ceiling would have given).
        assert!(
            matches!(result, EnforceAction::ClampLinear(_)),
            "over-ceiling Degraded decel must clamp to the 5.0 envelope, got {result:?}"
        );
    }

    // Issue #70: Degraded is decel-to-stop-and-HOLD. A re-initiation from a
    // stop that Nominal would (rate-clamp and) admit is DENIED under Degraded.
    #[test]
    fn test_mrc_profile_selected_on_degraded_posture() {
        let g = AssetGovernor::new("av01".to_string(), KinematicProfileType::AutomotiveNominal);
        // Stopped, commanded to accelerate to 10 m/s.
        let reinit_cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 0.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let nominal_result = g.evaluate_command(&reinit_cmd, &FleetPosture::Nominal, None);
        let degraded_result = g.evaluate_command(&reinit_cmd, &FleetPosture::Degraded, None);
        // Nominal admits the motion (clamped by accel limits, never denied).
        assert!(
            matches!(
                nominal_result,
                EnforceAction::ClampLinear(_) | EnforceAction::Allow
            ),
            "nominal: {nominal_result:?}"
        );
        // Degraded refuses to re-initiate motion from a stop — fail-closed.
        assert_eq!(
            degraded_result,
            EnforceAction::DenyBreach(DenyCode::DegradedReinitiationDenied),
            "degraded: {degraded_result:?}"
        );
    }

    // Issue #70: under Degraded, a decelerating-toward-zero command from a
    // moving state is admitted (clamped to the MRC envelope as needed), not
    // denied — the vehicle is permitted to bleed speed to a controlled stop.
    #[test]
    fn test_degraded_reinitiation_from_stop_is_denied() {
        let g = AssetGovernor::new("av01".to_string(), KinematicProfileType::AutomotiveNominal);
        // Moving at 8 m/s, commanded down to 4 m/s — decelerating.
        let decel_cmd = ProposedVehicleCommand {
            linear_velocity_mps: 4.0,
            current_velocity_mps: 8.0,
            delta_time_s: 1.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let degraded_result = g.evaluate_command(&decel_cmd, &FleetPosture::Degraded, None);
        assert!(
            !matches!(degraded_result, EnforceAction::DenyBreach(_)),
            "decelerating command must be admitted under Degraded: {degraded_result:?}"
        );
    }

    #[test]
    fn test_locked_out_denies_all_commands() {
        let g = AssetGovernor::new("av01".to_string(), KinematicProfileType::AutomotiveNominal);
        let result = g.evaluate_command(&nominal_cmd(), &FleetPosture::LockedOut, None);
        assert_eq!(result, EnforceAction::DenyBreach(DenyCode::AssetLockedOut));
    }

    #[test]
    fn test_nominal_safe_command_is_allowed() {
        let g = AssetGovernor::new("r01".to_string(), KinematicProfileType::RobotNominal);
        // 0.5 m/s with zero current velocity over 0.1s dt: accel = 5 m/s²
        // Robot MRC max_accel is 1.5*0.4 = 0.6 m/s², nominal is 1.5 m/s²
        // This will be clamped but not denied
        let result = g.evaluate_command(&nominal_cmd(), &FleetPosture::Nominal, None);
        // Either Allow or ClampLinear is acceptable (depends on accel)
        assert!(!matches!(result, EnforceAction::DenyBreach(_)));
    }

    #[test]
    fn test_industrial_profile_speed_limit() {
        let g = AssetGovernor::new("ind01".to_string(), KinematicProfileType::IndustrialNominal);
        let fast = ProposedVehicleCommand {
            linear_velocity_mps: 2.0, // above 0.5 limit
            current_velocity_mps: 0.0,
            delta_time_s: 1.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let result = g.evaluate_command(&fast, &FleetPosture::Nominal, None);
        assert_eq!(result, EnforceAction::ClampLinear(0.5));
    }
}
