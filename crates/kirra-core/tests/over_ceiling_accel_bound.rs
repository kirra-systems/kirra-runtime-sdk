// crates/kirra-core/tests/over_ceiling_accel_bound.rs
//
// #1243 — the acceleration bound applies on the OVER-CEILING path too.
//
// THE PROPERTY:
//
//   For every executable return, the implied acceleration
//   |linear − current| / delta_time_s does not exceed the applicable
//   accel/brake limit within tolerance — regardless of which priority produced
//   the linear value.
//
// Stated over the executed pair rather than per branch, so it closes the class
// instead of the one path — the same shape #1242 used for the lateral envelope.
//
// STATUS: green as of the #1243 fix; the first test was RED before it,
// returning the 35.0 ceiling and implying 300 m/s^2 against a 2.5 limit.
//
// Deliberately an integration test, not a module inside
// `kinematics_contract.rs`: the frozen talisman must not gain test modules
// beyond the ones it already carries (its pin is a git blob hash).

use kirra_core::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};

fn straight(linear: f64, current: f64, dt: f64) -> ProposedVehicleCommand {
    ProposedVehicleCommand {
        linear_velocity_mps: linear,
        current_velocity_mps: current,
        delta_time_s: dt,
        // Zero steering keeps P5a/P5b/P6 no-ops, so the only clamp that can
        // appear is the longitudinal one under test.
        steering_angle_deg: 0.0,
        current_steering_angle_deg: 0.0,
    }
}

/// The executed linear velocity, or `None` for a denial (which executes
/// nothing and satisfies the property vacuously).
fn executed_linear(cmd: &ProposedVehicleCommand, action: &EnforceAction) -> Option<f64> {
    match action {
        EnforceAction::Allow | EnforceAction::ClampSteering(_) => Some(cmd.linear_velocity_mps),
        EnforceAction::ClampLinear(v) | EnforceAction::ClampBoth { linear: v, .. } => Some(*v),
        EnforceAction::DenyBreach(_) => None,
    }
}

fn implied_rate(cmd: &ProposedVehicleCommand, v: f64) -> f64 {
    (v - cmd.current_velocity_mps).abs() / cmd.delta_time_s
}

/// The #1243 reproducer. Red before the fix.
#[test]
fn the_over_ceiling_path_now_respects_the_acceleration_limit() {
    let c = VehicleKinematicsContract::nominal_reference_profile();
    let cmd = straight(50.0, 5.0, 0.1);
    assert!(
        cmd.linear_velocity_mps.abs() > c.effective_max_speed_mps(),
        "the request must be over the ceiling or this tests the wrong branch"
    );

    let action = validate_vehicle_command(&cmd, &c);
    let v = executed_linear(&cmd, &action).expect("an over-ceiling command clamps, never denies");

    assert_eq!(v, 5.25, "current + max_accel x dt");
    assert!(
        implied_rate(&cmd, v) <= c.max_accel_mps2 + 1e-9,
        "executed {v} m/s from {} implies {} m/s^2 against a {} limit; action was {action:?}",
        cmd.current_velocity_mps,
        implied_rate(&cmd, v),
        c.max_accel_mps2
    );
    // Before the fix this was 35.0 → 300 m/s^2. Pin the magnitude of what was
    // wrong, so a regression cannot pass by being merely closer.
    assert!(v.abs() < c.effective_max_speed_mps());
}

/// NON-VACUITY CONTROL — the same property on the branch that already held.
///
/// It proves the property is satisfiable and the oracle correct, so a failure
/// above is a real branch-specific defect rather than an impossible assertion.
/// Note the executed value is IDENTICAL to the over-ceiling case: the ceiling
/// made no difference to the answer, which is exactly the point.
#[test]
fn the_within_ceiling_path_was_already_bounded() {
    let c = VehicleKinematicsContract::nominal_reference_profile();
    let cmd = straight(10.0, 5.0, 0.1);
    assert!(
        cmd.linear_velocity_mps.abs() <= c.effective_max_speed_mps(),
        "control must NOT be over the ceiling"
    );

    let action = validate_vehicle_command(&cmd, &c);
    let v = executed_linear(&cmd, &action).expect("must clamp, not deny");
    assert_eq!(v, 5.25);
    assert!(implied_rate(&cmd, v) <= c.max_accel_mps2 + 1e-9);
}

/// The ceiling is still exact where it is the ONLY binding constraint — the
/// half of the old K3 that survives. Long step so the rate bound is slack.
#[test]
fn the_ceiling_is_still_exact_when_no_rate_bound_is_tighter() {
    let c = VehicleKinematicsContract::nominal_reference_profile();
    let max = c.effective_max_speed_mps();
    for sign in [1.0_f64, -1.0] {
        // 5.1 m/s of delta over 4.0 s is 1.275 m/s^2, inside the 2.5 limit.
        let cmd = straight(sign * (max + 5.0), sign * (max - 0.1), 4.0);
        let action = validate_vehicle_command(&cmd, &c);
        let v = executed_linear(&cmd, &action).expect("must clamp, not deny");
        assert_eq!(v.abs(), max, "exactly the ceiling");
        assert_eq!(v.signum(), sign, "direction preserved");
    }
}

/// Direction is NOT preserved when the rate bound binds across a reversal, and
/// that is correct rather than a defect.
///
/// A +50 m/s request from −5 m/s cannot be met in one tick; the bound steps the
/// command from CURRENT toward the request by at most one brake step, so the
/// vehicle is still travelling in reverse when the tick ends. The old K3 clause
/// "direction preserved" meant "matches the request's sign" and is false here.
/// Pinned so the restatement is not quietly re-broadened later.
#[test]
fn a_reversal_request_is_rate_bounded_and_still_travelling_the_old_way() {
    let c = VehicleKinematicsContract::nominal_reference_profile();
    let cmd = straight(50.0, -5.0, 0.1);
    let action = validate_vehicle_command(&cmd, &c);
    let v = executed_linear(&cmd, &action).expect("must clamp, not deny");

    assert_eq!(v, -4.55, "-5.0 + max_brake x dt, decelerating toward zero");
    assert_ne!(
        v.signum(),
        cmd.linear_velocity_mps.signum(),
        "the executed sign follows the vehicle's travel, not the request"
    );
    assert!(implied_rate(&cmd, v) <= c.max_brake_mps2 + 1e-9);
}

/// EXPECTED-BUT-UNDESIRED — the residual #1243 does NOT close.
///
/// When `|current| > ceiling` the ceiling itself forces a rate breach. Here the
/// planner asks for 39.9 m/s from 40.0 m/s: a lawful −1.0 m/s^2, well inside
/// the 4.5 brake limit. Clamping it to the 35.0 ceiling implies −50 m/s^2.
///
/// No ordering of the two priorities avoids this. The vehicle is already
/// outside the envelope and cannot be inside it one tick later without
/// exceeding the brake limit, so something must give. INVARIANT 8 ("clamp to
/// the absolute hard boundary first, then apply rate-of-change limits;
/// envelope cap always wins over rate priority") decides which, and K6 (P-CAP)
/// independently requires the return to respect the ceiling.
///
/// This test asserts the UNDESIRED behaviour on purpose, so that:
///   * the residual is visible in the suite rather than only in prose;
///   * K8's domain restriction (`|current| <= ceiling`) is shown to be a real
///     conflict between two enforced bounds, not a convenience that makes a
///     proof pass;
///   * the day the resolution changes — either bound winning differently — this
///     fails and forces the safety case to be re-argued rather than drifting.
#[test]
fn expected_but_undesired_ceiling_forces_a_brake_breach_above_the_ceiling() {
    let c = VehicleKinematicsContract::nominal_reference_profile();
    let max = c.effective_max_speed_mps();
    let cmd = straight(39.9, 40.0, 0.1);

    assert!(
        cmd.current_velocity_mps.abs() > max,
        "the residual only exists above the ceiling"
    );
    let requested_rate = implied_rate(&cmd, cmd.linear_velocity_mps);
    assert!(
        requested_rate <= c.max_brake_mps2 + 1e-9,
        "the REQUEST is lawful ({requested_rate} m/s^2) — the ceiling is what breaks it"
    );

    let action = validate_vehicle_command(&cmd, &c);
    let v = executed_linear(&cmd, &action).expect("must clamp, not deny");

    assert_eq!(v, max, "the envelope cap wins, per invariant 8");
    let executed_rate = implied_rate(&cmd, v);
    assert!(
        executed_rate > c.max_brake_mps2,
        "and the breach is real: {executed_rate} m/s^2 against a {} limit",
        c.max_brake_mps2
    );
    assert_eq!(executed_rate, 50.0);
}
