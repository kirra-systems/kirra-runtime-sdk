// crates/kirra-core/tests/speed_cap_lateral_envelope.rs
//
// #1242 regression — the branch-independent lateral-envelope property, applied
// to the command the kernel actually RETURNS.
//
// THE PROPERTY:
//
//   Every returned executable command satisfies the active lateral-acceleration
//   envelope, regardless of which priority or clamp variant produced it.
//
// Stated over the returned PAIR rather than per-branch, so it closes the class
// instead of one path.
//
// STATUS: both cases GREEN as of the #1242 talisman fix. The speed-cap case was
// red and `#[ignore]`d before it; the accel-bounded case was green throughout and
// is the non-vacuity control — it proves the property is satisfiable and the
// oracle correct, so the speed-cap failure was a real branch-specific defect
// rather than an impossible assertion.
//
// Deliberately an integration test, not a module inside
// `kinematics_contract.rs`: the frozen talisman must not gain proof modules
// (its pin is a git blob hash).

use kirra_core::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};
use kirra_core::kinematics_sim::apply_enforce_action;

/// Same formula and tolerance as the simulator's own per-step invariant
/// (`SimState::lateral_accel_mps2` + `run_simulation`'s `FLOAT_TOLERANCE`), so
/// this test measures the envelope exactly as the existing harness does.
const FLOAT_TOLERANCE: f64 = 1e-6;

fn lateral_accel_mps2(cmd: &ProposedVehicleCommand, c: &VehicleKinematicsContract) -> f64 {
    let v2 = cmd.linear_velocity_mps.powi(2);
    if v2 <= 1e-6 || c.wheelbase_m <= 1e-6 {
        return 0.0;
    }
    (v2 * cmd.steering_angle_deg.to_radians().tan().abs()) / c.wheelbase_m
}

/// Assert the property on whatever the kernel returns for `cmd`.
///
/// `DenyBreach` is a PASS: refusing the command is a legitimate way to satisfy
/// the envelope. The property constrains what may be EXECUTED, not what must be
/// admitted.
fn assert_returned_command_is_in_envelope(
    cmd: &ProposedVehicleCommand,
    c: &VehicleKinematicsContract,
    context: &str,
) {
    let action = validate_vehicle_command(cmd, c);
    let Some(enforced) = apply_enforce_action(cmd, &action) else {
        return; // DenyBreach — nothing is executed.
    };
    let a_lat = lateral_accel_mps2(&enforced, c);
    assert!(
        a_lat <= c.max_lateral_accel_mps2 + FLOAT_TOLERANCE,
        "{context}\n  \
         action returned: {action:?}\n  \
         executed pair:   v = {:.4} m/s, delta = {:.4} deg\n  \
         implied a_lat:   {a_lat:.4} m/s^2 (envelope {:.4})\n  \
         the returned command is OUTSIDE the envelope the kernel enforces",
        enforced.linear_velocity_mps,
        enforced.steering_angle_deg,
        c.max_lateral_accel_mps2,
    );
}

/// Contract whose SPEED CAP binds while accel/brake do not, so the longitudinal
/// clamp comes from the cap and not from the rate limits. Rack limit left
/// generous (35 deg) so the steering demand below is inside it — the point is
/// that P6, not P5a, is the bound being dropped.
fn speed_capped_contract() -> VehicleKinematicsContract {
    VehicleKinematicsContract {
        max_speed_mps: 5.225,
        max_accel_mps2: 5.0,
        max_brake_mps2: 8.0,
        ..VehicleKinematicsContract::nominal_reference_profile()
    }
}

/// #1242 — the defect this slice fixes.
///
/// BEFORE the fix the kernel returned `ClampLinear(5.225)` alone: the speed was derated but the
/// steering demand passes through untouched, even though at the ENFORCED speed
/// it implies a lateral acceleration far outside the envelope. P6 permits only
/// ~19.75 deg at 5.225 m/s; 34 deg implies ~6.58 m/s^2 against a 3.5 limit.
///
/// `ClampBoth`'s own documentation describes exactly the outcome that is missing
/// here — "`steering` is back-solved for the ENFORCED `linear` ... so the
/// resulting `(linear, steering)` pair honors BOTH the longitudinal ceiling and
/// the lateral-accel envelope."
///
/// GREEN as of the #1242 talisman fix: Priority 2 now accumulates its
/// correction instead of finalizing, so a speed-capped command reaches P5a/P5b/P6
/// and returns `ClampBoth` with the steering back-solved for the enforced linear.
/// This test was `#[ignore]`d and red before that change; the un-ignore is the
/// recorded closure evidence.
#[test]
fn a_speed_capped_command_still_honours_the_lateral_envelope() {
    let c = speed_capped_contract();
    // Sweep the whole demand range that is inside the rack but outside P6 at
    // the capped speed, so the test does not depend on one lucky angle.
    for steering_deg in [24.0_f64, 28.0, 30.0, 34.0] {
        assert!(
            steering_deg < c.max_steering_deg,
            "fixture: the demand must be inside the rack limit, else P5a \
             explains the clamp and P6 is not the bound under test"
        );
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 5.4, // above the 5.225 cap
            current_velocity_mps: 5.4,
            delta_time_s: 0.1,
            steering_angle_deg: steering_deg,
            // Rate demand zeroed so P5b cannot be what fires.
            current_steering_angle_deg: steering_deg,
        };
        assert_returned_command_is_in_envelope(
            &cmd,
            &c,
            &format!("speed-cap branch, steering demand {steering_deg} deg"),
        );
    }
}

/// The same property on the ACCEL-bounded branch, where the kernel composes
/// correctly today: it returns `ClampBoth { linear, steering }` with the
/// steering back-solved for the enforced linear.
///
/// This is the non-vacuity control for the test above. It proves the
/// property is SATISFIABLE and the oracle is correct, so the speed-cap failure
/// is a genuine branch-specific defect rather than a property no command could
/// meet. If this ever goes red, the oracle is wrong and the #1242 diagnosis
/// needs revisiting before anything is changed.
#[test]
fn an_accel_bounded_command_honours_the_lateral_envelope_today() {
    let c = VehicleKinematicsContract {
        max_speed_mps: 50.0, // keep the speed cap OUT of it
        max_accel_mps2: 5.0,
        max_brake_mps2: 8.0,
        ..VehicleKinematicsContract::nominal_reference_profile()
    };
    for steering_deg in [24.0_f64, 30.0, 34.0] {
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0, // accel-bounded to 5.5 over 0.1 s
            current_velocity_mps: 5.0,
            delta_time_s: 0.1,
            steering_angle_deg: steering_deg,
            current_steering_angle_deg: steering_deg,
        };
        // Precondition: this branch really is the composing one.
        let action = validate_vehicle_command(&cmd, &c);
        assert!(
            matches!(action, EnforceAction::ClampBoth { .. }),
            "fixture: expected the accel-bounded branch to compose both axes, \
             got {action:?} — if this changed, the control is no longer testing \
             what it claims"
        );
        assert_returned_command_is_in_envelope(
            &cmd,
            &c,
            &format!("accel-bounded branch, steering demand {steering_deg} deg"),
        );
    }
}
