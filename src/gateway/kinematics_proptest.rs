// src/gateway/kinematics_proptest.rs
//
// Property-based tests for validate_vehicle_command.
//
// WHY PROPTEST
// ============
// The 26 deterministic tests in kinematics_contract.rs cover specific cases.
// They can't cover the space of all f64 inputs — and the bicycle model
// involves transcendental functions (tan, atan) that have edge-case behavior
// at values deterministic tests don't happen to hit.
//
// proptest generates thousands of random inputs per property and shrinks
// failures to the minimal counterexample. It finds:
//   - Panics on any input combination (there must be none)
//   - Silent incorrect behavior the NaN/Inf guard might miss
//   - Clamped values that are still outside the contract
//   - Monotonicity violations in the clamping logic
//
// PROPERTIES TESTED
// =================
// 1. NO_PANIC: validate_vehicle_command never panics on any finite f64 input
// 2. NO_PANIC_NONFINITE: never panics on NaN/Inf inputs either
// 3. CLAMP_IN_BOUNDS: ClampLinear/ClampSteering values satisfy the contract
// 4. DENY_NONFINITE: any NaN/Inf field always produces DenyBreach
// 5. ZERO_DT_DENIED: delta_time_s <= 0 always produces DenyBreach
// 6. ALLOW_IMPLIES_SAFE: Allow result implies all contract invariants hold
// 7. CLAMPED_VELOCITY_IN_RANGE: clamped speed is within [-max_speed, +max_speed]
// 8. CLAMPED_STEERING_FINITE: clamped steering angle is always finite
// 9. BICYCLE_MODEL_AFTER_CLAMP: after ClampSteering, lateral accel <= contract max
// 10.DETERMINISTIC: same input always produces same output

use crate::gateway::contract_profiles::{contract_for, mrc_fallback_for, VehicleClass};
use crate::gateway::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};
use proptest::prelude::*;

/// EVERY member of the contract family (Nominal + MRC for all three classes),
/// as `(label, contract)`. The validation gate (#312): a profile that fails the
/// frozen instance's properties does not ship — that inheritance IS the family's
/// certification story, so the same battery runs against every member.
fn family_contracts() -> Vec<(&'static str, VehicleKinematicsContract)> {
    let mut v = Vec::new();
    for class in [
        VehicleClass::Courier,
        VehicleClass::DeliveryAv,
        VehicleClass::Robotaxi,
    ] {
        v.push((class.as_str(), contract_for(class)));
        v.push((class.as_str(), mrc_fallback_for(class)));
    }
    v
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Strategy for finite f64 values in a physically plausible range.
fn finite_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        -100.0_f64..=100.0_f64,
        -1e-10_f64..=1e-10_f64,
        prop_oneof![Just(f64::MAX / 2.0), Just(f64::MIN / 2.0)],
        Just(0.0_f64),
        Just(35.0_f64),
        Just(-35.0_f64),
        Just(5.0_f64),
        Just(-5.0_f64),
    ]
}

/// Strategy for non-finite f64 values (NaN and Inf variants).
fn nonfinite_f64() -> impl Strategy<Value = f64> {
    prop_oneof![Just(f64::NAN), Just(f64::INFINITY), Just(f64::NEG_INFINITY),]
}

/// Strategy for delta_time_s — includes zero and negative to test Priority 1.
fn delta_time_strategy() -> impl Strategy<Value = f64> {
    prop_oneof![
        0.001_f64..=1.0_f64,
        Just(0.0_f64),
        Just(-0.1_f64),
        Just(-1.0_f64),
        Just(10.0_f64),
    ]
}

/// Strategy for a complete ProposedVehicleCommand with all-finite fields.
fn finite_command() -> impl Strategy<Value = ProposedVehicleCommand> {
    (
        finite_f64(),
        finite_f64(),
        delta_time_strategy(),
        finite_f64(),
        finite_f64(),
    )
        .prop_map(|(v, cv, dt, delta, cdelta)| ProposedVehicleCommand {
            linear_velocity_mps: v,
            current_velocity_mps: cv,
            delta_time_s: dt,
            steering_angle_deg: delta,
            current_steering_angle_deg: cdelta,
        })
}

/// Strategy for a valid (non-zero positive) delta_time_s command.
fn valid_dt_command() -> impl Strategy<Value = ProposedVehicleCommand> {
    (
        finite_f64(),
        finite_f64(),
        0.001_f64..=1.0_f64,
        finite_f64(),
        finite_f64(),
    )
        .prop_map(|(v, cv, dt, delta, cdelta)| ProposedVehicleCommand {
            linear_velocity_mps: v,
            current_velocity_mps: cv,
            delta_time_s: dt,
            steering_angle_deg: delta,
            current_steering_angle_deg: cdelta,
        })
}

/// Builds a command with one nonfinite field at the given slot index (0-4).
fn command_with_one_nonfinite_field(bad_value: f64, field_idx: usize) -> ProposedVehicleCommand {
    let fields = [10.0_f64, 10.0, 0.1, 5.0, 5.0];
    let mut f = fields;
    f[field_idx % 5] = bad_value;
    ProposedVehicleCommand {
        linear_velocity_mps: f[0],
        current_velocity_mps: f[1],
        delta_time_s: f[2],
        steering_angle_deg: f[3],
        current_steering_angle_deg: f[4],
    }
}

// ---------------------------------------------------------------------------
// Property 1: NO_PANIC — never panics on any finite input
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_no_panic_on_finite_inputs(cmd in finite_command()) {
        let nominal = VehicleKinematicsContract::nominal_reference_profile();
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        let _ = validate_vehicle_command(&cmd, &nominal);
        let _ = validate_vehicle_command(&cmd, &mrc);
    }
}

// ---------------------------------------------------------------------------
// Property 2: NO_PANIC_NONFINITE — never panics on NaN/Inf inputs
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_no_panic_on_nonfinite_inputs(
        bad in nonfinite_f64(),
        field_idx in 0usize..5,
    ) {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = command_with_one_nonfinite_field(bad, field_idx);
        let result = validate_vehicle_command(&cmd, &contract);
        prop_assert!(
            matches!(result, EnforceAction::DenyBreach(_)),
            "nonfinite input must always produce DenyBreach, got: {result:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Property 3: DENY_NONFINITE — any NaN/Inf field always DenyBreach
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_nan_in_any_field_produces_deny_breach(field_idx in 0usize..5) {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let cmd = command_with_one_nonfinite_field(bad, field_idx);
            let result = validate_vehicle_command(&cmd, &contract);
            prop_assert!(
                matches!(result, EnforceAction::DenyBreach(_)),
                "field {field_idx} = {bad}: expected DenyBreach, got {result:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Property 4: ZERO_DT_DENIED — delta_time_s <= 0 always DenyBreach
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_non_positive_dt_always_denied(
        v in -50.0_f64..=50.0_f64,
        delta in -35.0_f64..=35.0_f64,
        bad_dt in prop_oneof![
            Just(0.0_f64),
            (-1000.0_f64..=-1e-15_f64),
        ],
    ) {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: v,
            current_velocity_mps: v,
            delta_time_s: bad_dt,
            steering_angle_deg: delta,
            current_steering_angle_deg: delta,
        };
        let result = validate_vehicle_command(&cmd, &contract);
        prop_assert!(
            matches!(result, EnforceAction::DenyBreach(_)),
            "dt={bad_dt}: expected DenyBreach, got {result:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Property 5: CLAMP_IN_BOUNDS — clamped values satisfy the contract
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_clamp_linear_value_within_speed_contract(cmd in valid_dt_command()) {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        if let EnforceAction::ClampLinear(v) = validate_vehicle_command(&cmd, &contract) {
            prop_assert!(
                v.is_finite(),
                "ClampLinear value must be finite, got {v}"
            );
            prop_assert!(
                v.abs() <= contract.max_speed_mps + 1e-9,
                "ClampLinear value {v} must be <= max_speed {}", contract.max_speed_mps
            );
        }
    }
}

proptest! {
    #[test]
    fn prop_clamp_steering_value_is_finite(cmd in valid_dt_command()) {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        if let EnforceAction::ClampSteering(delta) = validate_vehicle_command(&cmd, &contract) {
            prop_assert!(
                delta.is_finite(),
                "ClampSteering value must be finite, got {delta}"
            );
            prop_assert!(
                delta.abs() <= contract.max_steering_deg + 1e-9,
                "ClampSteering value {delta} must be <= max_steering_deg {}",
                contract.max_steering_deg
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Property 6: ALLOW_IMPLIES_SAFE — Allow result satisfies all invariants
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_allow_result_satisfies_speed_contract(cmd in valid_dt_command()) {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        if let EnforceAction::Allow = validate_vehicle_command(&cmd, &contract) {
            prop_assert!(
                cmd.linear_velocity_mps.abs() <= contract.max_speed_mps + 1e-9,
                "Allow: speed {} must be <= {}", cmd.linear_velocity_mps, contract.max_speed_mps
            );
            prop_assert!(
                cmd.steering_angle_deg.abs() <= contract.max_steering_deg + 1e-9,
                "Allow: steering {} must be <= {}", cmd.steering_angle_deg, contract.max_steering_deg
            );
            let v2 = cmd.linear_velocity_mps.powi(2);
            if v2 > 1e-6 && contract.wheelbase_m > 1e-6 {
                let lat = (v2 * cmd.steering_angle_deg.to_radians().tan().abs())
                    / contract.wheelbase_m;
                prop_assert!(
                    lat <= contract.max_lateral_accel_mps2 + 1e-6,
                    "Allow: lateral accel {lat:.6} must be <= {}", contract.max_lateral_accel_mps2
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property 7: BICYCLE_MODEL_AFTER_CLAMP — clamped steering satisfies lat limit
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_clamp_steering_satisfies_lateral_accel_invariant(cmd in valid_dt_command()) {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        if let EnforceAction::ClampSteering(safe_delta) = validate_vehicle_command(&cmd, &contract) {
            let v2 = cmd.linear_velocity_mps.powi(2);
            if v2 > 1e-6 && contract.wheelbase_m > 1e-6 && safe_delta.is_finite() {
                let lat = (v2 * safe_delta.to_radians().tan().abs()) / contract.wheelbase_m;
                prop_assert!(
                    lat <= contract.max_lateral_accel_mps2 + 1e-6,
                    "ClampSteering({safe_delta:.4}) at v={:.4}: lat_accel {lat:.6} > contract max {}",
                    cmd.linear_velocity_mps, contract.max_lateral_accel_mps2
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property 8: DETERMINISTIC — same input always produces same output
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_validate_is_deterministic(cmd in finite_command()) {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let result1 = validate_vehicle_command(&cmd, &contract);
        let result2 = validate_vehicle_command(&cmd, &contract);
        prop_assert_eq!(
            std::mem::discriminant(&result1),
            std::mem::discriminant(&result2),
            "validate_vehicle_command must be deterministic"
        );
        match (&result1, &result2) {
            (EnforceAction::ClampLinear(v1), EnforceAction::ClampLinear(v2)) => {
                prop_assert_eq!(v1.to_bits(), v2.to_bits(), "ClampLinear must be bit-identical");
            }
            (EnforceAction::ClampSteering(d1), EnforceAction::ClampSteering(d2)) => {
                prop_assert_eq!(d1.to_bits(), d2.to_bits(), "ClampSteering must be bit-identical");
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Property 9: SIGN_PRESERVATION — clamped values preserve sign of input
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_clamp_linear_preserves_direction(
        v in prop_oneof![
            (40.0_f64..=200.0_f64),
            (-200.0_f64..=-40.0_f64),
        ],
    ) {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: v,
            current_velocity_mps: v.signum() * 30.0,
            delta_time_s: 1.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        match validate_vehicle_command(&cmd, &contract) {
            EnforceAction::ClampLinear(clamped) => {
                prop_assert_eq!(
                    clamped.signum(), v.signum(),
                    "ClampLinear must preserve direction: v={}, clamped={}", v, clamped
                );
                prop_assert_eq!(
                    clamped.abs(), contract.max_speed_mps,
                    "ClampLinear must clamp to exactly max_speed: got {}", clamped.abs()
                );
            }
            other => { let _ = other; }
        }
    }
}

// ---------------------------------------------------------------------------
// Property 10: MRC_STRICTER — MRC rejects more commands than nominal
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_mrc_is_strictly_more_restrictive_than_nominal(cmd in valid_dt_command()) {
        let nominal = VehicleKinematicsContract::nominal_reference_profile();
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();

        let nominal_result = validate_vehicle_command(&cmd, &nominal);
        let mrc_result = validate_vehicle_command(&cmd, &mrc);

        if let EnforceAction::Allow = mrc_result {
            prop_assert!(
                matches!(nominal_result, EnforceAction::Allow),
                "If MRC allows a command, nominal must also allow it.\n\
                 cmd: v={:.4}, dt={:.4}, delta={:.4}\n\
                 nominal: {nominal_result:?}, mrc: Allow",
                cmd.linear_velocity_mps, cmd.delta_time_s, cmd.steering_angle_deg
            );
        }
    }
}

// ===========================================================================
// THE VALIDATION GATE (#312/#313) — the SAME property battery, run against
// EVERY family member (courier / delivery-av / robotaxi, Nominal + MRC). These
// properties are profile-agnostic: each reads the contract's OWN fields, so a new
// class member inherits the frozen instance's certification by passing them. A
// member that fails any of these does not ship.
// ===========================================================================

proptest! {
    /// NO_PANIC — no family member panics on any finite input.
    #[test]
    fn prop_family_no_panic_on_finite(cmd in finite_command()) {
        for (_label, contract) in family_contracts() {
            let _ = validate_vehicle_command(&cmd, &contract);
        }
    }
}

proptest! {
    /// NO_PANIC + DENY — every member denies a nonfinite field (never panics,
    /// never silently admits).
    #[test]
    fn prop_family_nonfinite_always_denied(
        bad in nonfinite_f64(),
        field_idx in 0usize..5,
    ) {
        let cmd = command_with_one_nonfinite_field(bad, field_idx);
        for (label, contract) in family_contracts() {
            let result = validate_vehicle_command(&cmd, &contract);
            prop_assert!(
                matches!(result, EnforceAction::DenyBreach(_)),
                "{label}: nonfinite field {field_idx}={bad} must DenyBreach, got {result:?}"
            );
        }
    }
}

proptest! {
    /// CLAMP_IN_BOUNDS — every member's ClampLinear/ClampSteering output satisfies
    /// THAT member's own contract (finite + within its bounds).
    #[test]
    fn prop_family_clamp_in_bounds(cmd in valid_dt_command()) {
        for (label, contract) in family_contracts() {
            match validate_vehicle_command(&cmd, &contract) {
                EnforceAction::ClampLinear(v) => {
                    prop_assert!(v.is_finite(), "{label}: ClampLinear {v} not finite");
                    prop_assert!(v.abs() <= contract.max_speed_mps + 1e-9,
                        "{label}: ClampLinear {v} > max_speed {}", contract.max_speed_mps);
                }
                EnforceAction::ClampSteering(d) => {
                    prop_assert!(d.is_finite(), "{label}: ClampSteering {d} not finite");
                    prop_assert!(d.abs() <= contract.max_steering_deg + 1e-9,
                        "{label}: ClampSteering {d} > max_steering {}", contract.max_steering_deg);
                }
                _ => {}
            }
        }
    }
}

proptest! {
    /// ALLOW_IMPLIES_SAFE — an Allow from any member implies that member's speed,
    /// steering, and bicycle-model lateral-accel invariants all hold.
    #[test]
    fn prop_family_allow_implies_safe(cmd in valid_dt_command()) {
        for (label, contract) in family_contracts() {
            if let EnforceAction::Allow = validate_vehicle_command(&cmd, &contract) {
                prop_assert!(cmd.linear_velocity_mps.abs() <= contract.max_speed_mps + 1e-9,
                    "{label}: Allow speed {} > {}", cmd.linear_velocity_mps, contract.max_speed_mps);
                prop_assert!(cmd.steering_angle_deg.abs() <= contract.max_steering_deg + 1e-9,
                    "{label}: Allow steering {} > {}", cmd.steering_angle_deg, contract.max_steering_deg);
                let v2 = cmd.linear_velocity_mps.powi(2);
                if v2 > 1e-6 && contract.wheelbase_m > 1e-6 {
                    let lat = (v2 * cmd.steering_angle_deg.to_radians().tan().abs())
                        / contract.wheelbase_m;
                    prop_assert!(lat <= contract.max_lateral_accel_mps2 + 1e-6,
                        "{label}: Allow lateral {lat:.6} > {}", contract.max_lateral_accel_mps2);
                }
            }
        }
    }
}

proptest! {
    /// BICYCLE_MODEL_AFTER_CLAMP — a member's ClampSteering output satisfies that
    /// member's lateral-accel limit.
    #[test]
    fn prop_family_clamp_steering_satisfies_lateral(cmd in valid_dt_command()) {
        for (label, contract) in family_contracts() {
            if let EnforceAction::ClampSteering(safe_delta) = validate_vehicle_command(&cmd, &contract) {
                let v2 = cmd.linear_velocity_mps.powi(2);
                if v2 > 1e-6 && contract.wheelbase_m > 1e-6 && safe_delta.is_finite() {
                    let lat = (v2 * safe_delta.to_radians().tan().abs()) / contract.wheelbase_m;
                    prop_assert!(lat <= contract.max_lateral_accel_mps2 + 1e-6,
                        "{label}: ClampSteering({safe_delta:.4}) lat {lat:.6} > {}",
                        contract.max_lateral_accel_mps2);
                }
            }
        }
    }
}

proptest! {
    /// DETERMINISTIC — every member is deterministic (discriminant-stable; clamp
    /// values bit-identical).
    #[test]
    fn prop_family_deterministic(cmd in finite_command()) {
        for (label, contract) in family_contracts() {
            let r1 = validate_vehicle_command(&cmd, &contract);
            let r2 = validate_vehicle_command(&cmd, &contract);
            prop_assert_eq!(std::mem::discriminant(&r1), std::mem::discriminant(&r2),
                "{} not deterministic", label);
            match (&r1, &r2) {
                (EnforceAction::ClampLinear(a), EnforceAction::ClampLinear(b)) =>
                    prop_assert_eq!(a.to_bits(), b.to_bits()),
                (EnforceAction::ClampSteering(a), EnforceAction::ClampSteering(b)) =>
                    prop_assert_eq!(a.to_bits(), b.to_bits()),
                _ => {}
            }
        }
    }
}
