//! WASM shim around the FROZEN kinematics-contract talisman.
//!
//! The checker under test is included VERBATIM from its shipped location —
//! nothing here reimplements or wraps its logic beyond marshalling f64s across
//! the WASM boundary. The per-class profiles for courier / delivery-AV are
//! constructed HERE (in the shim) with the normative numbers from
//! `docs/CONTRACT_PROFILES.md`; the robotaxi class is the talisman's own
//! frozen `nominal_reference_profile()` plus the ADR-0001 ODD cap, and the
//! Degraded path uses the talisman's own `mrc_fallback_profile()` (ADR-0012:
//! one authoritative MRC envelope).

#[path = "../../../crates/kirra-core/src/kinematics_contract.rs"]
pub mod kinematics_contract;

use kinematics_contract::{
    enforce_degraded_decel_to_stop, validate_vehicle_command, DenyCode, EnforceAction,
    ProposedVehicleCommand, VehicleKinematicsContract, URBAN_ODD_SPEED_CAP_MPS,
};

/// Result slots read by JS: [action_tag, deny_code, clamp_linear, clamp_steering].
/// action_tag: 0 Allow · 1 ClampLinear · 2 ClampSteering · 3 ClampBoth · 4 DenyBreach.
/// deny_code: DenyCode variant index, or -1. Clamp slots are NaN when unused.
static mut OUT: [f64; 4] = [0.0; 4];

#[no_mangle]
pub extern "C" fn out_ptr() -> *const f64 {
    core::ptr::addr_of!(OUT) as *const f64
}

fn deny_index(code: DenyCode) -> f64 {
    match code {
        DenyCode::NanInfLinearVelocity => 0.0,
        DenyCode::NanInfCurrentVelocity => 1.0,
        DenyCode::NanInfSteeringAngle => 2.0,
        DenyCode::NanInfCurrentSteering => 3.0,
        DenyCode::NanInfDeltaTime => 4.0,
        DenyCode::InvalidTimeDelta => 5.0,
        DenyCode::AssetLockedOut => 6.0,
        DenyCode::DrivableSpaceDeparture => 7.0,
        DenyCode::DegradedReinitiationDenied => 8.0,
        DenyCode::DegradedSpeedIncreaseDenied => 9.0,
        DenyCode::FrameIntegrityUntrusted => 10.0,
        DenyCode::TrajectoryHorizonExceeded => 11.0,
    }
}

/// Vehicle-class contracts. 0 = courier, 1 = delivery-AV, 2 = robotaxi
/// (frozen reference + urban ODD cap). Courier/delivery numbers mirror the
/// NORMATIVE table in docs/CONTRACT_PROFILES.md (all validation-pending, as
/// that document states).
fn contract_for(class: u32) -> VehicleKinematicsContract {
    match class {
        0 => VehicleKinematicsContract {
            max_speed_mps: 3.0,
            max_accel_mps2: 1.0,
            max_brake_mps2: 3.0,
            max_steering_deg: 30.0,
            max_steering_rate_deg_s: 30.0,
            min_follow_distance_m: 2.0,
            max_lateral_accel_mps2: 1.5,
            wheelbase_m: 0.5,
            width_m: 0.6,
            length_m: 0.9,
            overhang_front_m: 0.2,
            overhang_rear_m: 0.2,
            odd_speed_cap_mps: Some(2.5),
        },
        1 => VehicleKinematicsContract {
            max_speed_mps: 12.0,
            max_accel_mps2: 1.8,
            max_brake_mps2: 4.0,
            max_steering_deg: 33.0,
            max_steering_rate_deg_s: 40.0,
            min_follow_distance_m: 2.0,
            max_lateral_accel_mps2: 2.5,
            wheelbase_m: 1.9,
            width_m: 1.1,
            length_m: 2.9,
            overhang_front_m: 0.5,
            overhang_rear_m: 0.5,
            odd_speed_cap_mps: Some(11.0),
        },
        _ => {
            let mut c = VehicleKinematicsContract::nominal_reference_profile();
            c.odd_speed_cap_mps = Some(URBAN_ODD_SPEED_CAP_MPS);
            c
        }
    }
}

/// Evaluate one command. posture: 0 = Nominal, 1 = Degraded.
#[no_mangle]
pub extern "C" fn run(
    class: u32,
    posture: u32,
    linear_velocity_mps: f64,
    current_velocity_mps: f64,
    delta_time_s: f64,
    steering_angle_deg: f64,
    current_steering_angle_deg: f64,
) {
    let cmd = ProposedVehicleCommand {
        linear_velocity_mps,
        current_velocity_mps,
        delta_time_s,
        steering_angle_deg,
        current_steering_angle_deg,
    };
    let action = if posture == 1 {
        // ADR-0012 / SS-002: Degraded runs the decel-to-stop gate against the
        // one authoritative MRC envelope.
        enforce_degraded_decel_to_stop(&cmd, &VehicleKinematicsContract::mrc_fallback_profile())
    } else {
        validate_vehicle_command(&cmd, &contract_for(class))
    };
    let out = match action {
        EnforceAction::Allow => [0.0, -1.0, f64::NAN, f64::NAN],
        EnforceAction::ClampLinear(v) => [1.0, -1.0, v, f64::NAN],
        EnforceAction::ClampSteering(s) => [2.0, -1.0, f64::NAN, s],
        EnforceAction::ClampBoth { linear, steering } => [3.0, -1.0, linear, steering],
        EnforceAction::DenyBreach(code) => [4.0, deny_index(code), f64::NAN, f64::NAN],
    };
    unsafe {
        OUT = out;
    }
}

#[cfg(test)]
mod shim_tests {
    use super::*;

    fn run_native(class: u32, posture: u32, l: f64, c: f64, dt: f64, s: f64, cs: f64) -> [f64; 4] {
        run(class, posture, l, c, dt, s, cs);
        unsafe { OUT }
    }

    #[test]
    fn hallucinated_999_clamps_to_odd_cap_on_robotaxi() {
        let out = run_native(2, 0, 999.0, 20.0, 0.1, 0.0, 0.0);
        assert_eq!(out[0], 1.0); // ClampLinear
        assert!((out[2] - URBAN_ODD_SPEED_CAP_MPS).abs() < 1e-12);
    }

    #[test]
    fn nan_velocity_denied_with_specific_code() {
        let out = run_native(2, 0, f64::NAN, 10.0, 0.1, 0.0, 0.0);
        assert_eq!(out[0], 4.0);
        assert_eq!(out[1], 0.0); // NanInfLinearVelocity
    }

    #[test]
    fn zero_dt_denied() {
        let out = run_native(2, 0, 5.0, 5.0, 0.0, 0.0, 0.0);
        assert_eq!(out[0], 4.0);
        assert_eq!(out[1], 5.0); // InvalidTimeDelta
    }

    #[test]
    fn degraded_reinitiation_from_stop_denied() {
        let out = run_native(2, 1, 2.0, 0.0, 0.1, 0.0, 0.0);
        assert_eq!(out[0], 4.0);
        assert_eq!(out[1], 8.0); // DegradedReinitiationDenied
    }

    #[test]
    fn degraded_speed_increase_denied() {
        let out = run_native(2, 1, 4.0, 3.0, 0.1, 0.0, 0.0);
        assert_eq!(out[0], 4.0);
        assert_eq!(out[1], 9.0); // DegradedSpeedIncreaseDenied
    }

    #[test]
    fn degraded_decel_toward_zero_allowed() {
        let out = run_native(2, 1, 2.0, 3.0, 0.5, 0.0, 0.0);
        assert_eq!(out[0], 0.0); // Allow
    }

    #[test]
    fn courier_over_cap_clamps_to_2_5() {
        let out = run_native(0, 0, 3.0, 2.0, 0.5, 0.0, 0.0);
        assert_eq!(out[0], 1.0);
        assert!((out[2] - 2.5).abs() < 1e-12);
    }
}
