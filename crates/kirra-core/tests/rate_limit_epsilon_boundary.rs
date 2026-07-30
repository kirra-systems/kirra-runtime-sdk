// crates/kirra-core/tests/rate_limit_epsilon_boundary.rs
//
// #1242 — the exact `+ 1e-9` boundary of the Priority-3/4 accel and brake rate
// limits, and the sign-preservation identity in the Priority-2 speed ceiling.
//
// WHY THESE EXIST. The #1242 change touched all three lines, so `cargo-mutants
// --in-diff` began mutating their operators. Four mutants survived the whole
// existing suite:
//
//   :544 `*` -> `/`   in the ceiling's `effective_max * signum`
//   :621 `>` -> `>=`  accel:  implied_rate > max_accel + 1e-9
//   :627 `>` -> `>=`  brake:  implied_rate > max_brake + 1e-9
//   :627 `+` -> `-`   brake:  the epsilon's sign
//
// The three rate-limit ones are killed here. They survived not because the
// boundary is unreachable but because no test had ever landed ON it: the suite
// drives rates comfortably inside or outside the limit, and every mutant above
// changes behaviour only for an input whose implied rate sits exactly at the
// threshold. Constructing that input needs `delta_time_s == 1.0` so the
// division is exact and the implied rate is the speed delta itself, bit for
// bit.
//
// The fourth is a genuine EQUIVALENT mutant and is excluded in
// `.cargo/mutants.toml`; the premise its equivalence argument rests on is
// pinned by the last test in this file rather than left as prose.
//
// Deliberately an integration test, not a module inside
// `kinematics_contract.rs`: the frozen talisman must not gain test modules
// (its pin is a git blob hash).

use kirra_core::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};

/// The kernel's own epsilon, spelled the same way it is spelled there so the
/// test lands on the identical double rather than a nearby one.
const RATE_EPS: f64 = 1e-9;

fn straight(linear: f64, current: f64) -> ProposedVehicleCommand {
    ProposedVehicleCommand {
        linear_velocity_mps: linear,
        current_velocity_mps: current,
        // 1.0 s makes `speed_delta.abs() / delta_time_s` an exact identity, so
        // the implied rate is a value the test controls to the bit. Any other
        // dt introduces a rounding step and the boundary case stops being
        // reachable at all.
        delta_time_s: 1.0,
        // Zero steering keeps P5a/P5b/P6 no-ops, so the only clamp that can
        // appear is the longitudinal one under test.
        steering_angle_deg: 0.0,
        current_steering_angle_deg: 0.0,
    }
}

/// Priority 3 (acceleration), at EXACTLY `max_accel + 1e-9`.
///
/// `>` admits it (equal is not greater); `>=` clamps. Accelerating from rest so
/// the `from_rest` arm selects the accel branch without a direction subtlety.
#[test]
fn acceleration_exactly_at_the_epsilon_boundary_is_admitted_not_clamped() {
    let c = VehicleKinematicsContract::nominal_reference_profile();
    let boundary = c.max_accel_mps2 + RATE_EPS;
    assert!(
        boundary > c.max_accel_mps2,
        "the epsilon must be representable above the limit at this magnitude, \
         else this test asserts nothing"
    );

    let cmd = straight(boundary, 0.0);
    assert!(
        cmd.linear_velocity_mps.abs() <= c.effective_max_speed_mps(),
        "must not trip the P2 ceiling — that would short-circuit the branch \
         under test"
    );

    assert_eq!(
        validate_vehicle_command(&cmd, &c),
        EnforceAction::Allow,
        "an implied rate EQUAL to max_accel + 1e-9 is inside the limit; \
         clamping it means the comparison went non-strict"
    );

    // Non-vacuity: one ULP further out and the same command IS clamped, so the
    // assertion above is a boundary, not a region where nothing ever fires.
    let over = straight(f64::from_bits(boundary.to_bits() + 1), 0.0);
    assert!(
        matches!(
            validate_vehicle_command(&over, &c),
            EnforceAction::ClampLinear(_)
        ),
        "one ULP above the boundary must clamp"
    );
}

/// Priority 4 (braking), at EXACTLY `max_brake + 1e-9`.
///
/// Decelerating to a full stop from just over the brake limit: same-direction
/// (both signums positive) and magnitude decreasing, so the `else` arm runs.
#[test]
fn braking_exactly_at_the_epsilon_boundary_is_admitted_not_clamped() {
    let c = VehicleKinematicsContract::nominal_reference_profile();
    let boundary = c.max_brake_mps2 + RATE_EPS;
    assert!(boundary > c.max_brake_mps2);

    let cmd = straight(0.0, boundary);
    assert_eq!(
        validate_vehicle_command(&cmd, &c),
        EnforceAction::Allow,
        "an implied rate EQUAL to max_brake + 1e-9 is inside the limit; \
         clamping it means the comparison went non-strict"
    );

    let over = straight(0.0, f64::from_bits(boundary.to_bits() + 1));
    assert!(
        matches!(
            validate_vehicle_command(&over, &c),
            EnforceAction::ClampLinear(_)
        ),
        "one ULP above the boundary must clamp"
    );
}

/// Priority 4, at EXACTLY `max_brake` — the epsilon's SIGN.
///
/// This separates `+ 1e-9` from `- 1e-9`. A rate of exactly the brake limit is
/// inside `max_brake + 1e-9` and outside `max_brake - 1e-9`, so the two spell
/// opposite verdicts for this one command. The boundary test above cannot see
/// the difference — at `max_brake + 1e-9` both forms agree.
#[test]
fn braking_exactly_at_the_limit_is_admitted_so_the_epsilon_widens_not_narrows() {
    let c = VehicleKinematicsContract::nominal_reference_profile();
    let cmd = straight(0.0, c.max_brake_mps2);

    assert_eq!(
        validate_vehicle_command(&cmd, &c),
        EnforceAction::Allow,
        "braking at exactly the limit must be admitted; denying it means the \
         tolerance subtracts instead of adds, tightening the limit below its \
         declared value"
    );

    // The mirror case: the epsilon is a tolerance, not a licence. Well outside
    // the limit still clamps.
    let hard = straight(0.0, c.max_brake_mps2 * 2.0);
    assert!(matches!(
        validate_vehicle_command(&hard, &c),
        EnforceAction::ClampLinear(_)
    ));
}

/// The premise behind the `:544` equivalent-mutant exclusion.
///
/// The ceiling writes `effective_max_speed * cmd.linear_velocity_mps.signum()`.
/// Mutating `*` to `/` cannot be killed, because `f64::signum` returns only
/// `±1.0` here and `x * ±1.0` and `x / ±1.0` are the SAME double for every `x`
/// — the operation is an exact sign flip either way, not an approximation.
///
/// That argument has two premises, and both are checked here rather than
/// asserted in a config comment: signum's range on this path, and the identity
/// itself. The path premise holds because Priority 0 denies every non-finite
/// field before the ceiling runs, so signum's NaN case is unreachable and
/// `±0.0` still yields `±1.0`.
#[test]
fn the_ceiling_sign_flip_is_multiplication_division_invariant() {
    let probes = [
        0.0_f64,
        -0.0,
        1.0,
        -1.0,
        f64::MIN_POSITIVE,
        5e-324,
        0.05,
        5.225,
        35.0,
        -35.0,
        f64::MAX,
        f64::MIN,
    ];
    for v in probes {
        let s = v.signum();
        assert_eq!(
            s.abs(),
            1.0,
            "signum of a finite {v:e} must be ±1.0 — the premise of the :544 \
             equivalence exclusion"
        );
        let ceiling = 13.4_f64;
        assert_eq!(
            (ceiling * s).to_bits(),
            (ceiling / s).to_bits(),
            "ceiling * signum({v:e}) != ceiling / signum({v:e}); the :544 \
             mutant is NOT equivalent and its exclusion must be withdrawn"
        );
    }
}
