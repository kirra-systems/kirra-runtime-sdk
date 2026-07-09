//! EP-15 proofs — the FROZEN kinematics-contract talisman
//! (`crates/kirra-core/src/kinematics_contract.rs`, git blob `ed00f4da…`).
//!
//! Scope is the DECIDABLE prefix of the pipeline, per the EP-15 plan ("bounded
//! floats via `f64::is_finite` case-split; scope to provable forms honestly"):
//! the Priority-0/1 fail-closed guards, the Priority-2 speed-ceiling early
//! return, and the Degraded decel-to-stop denial gates — none of which reach
//! the P6 bicycle-model `tan`/`atan` (transcendentals CBMC cannot decide;
//! those stay covered by the property/MC-DC test suites).
//!
//! Properties (cited from `docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md` §2):
//!  * K1 fail-closed NaN/Inf totality (SG9): ANY non-finite field in a command
//!    → `DenyBreach`, for every possible f64 bit pattern in every field; the
//!    first-priority field maps to its exact forensic code.
//!  * K2 non-physical time delta (SG3): finite fields with `dt ≤ 0` →
//!    `DenyBreach(InvalidTimeDelta)`, before any rate arithmetic.
//!  * K3 the P2 speed ceiling is exact (SG1): a command over the effective max
//!    clamps to EXACTLY `effective_max × signum` — magnitude equal to the
//!    ceiling, direction preserved.
//!  * K4/K5 Degraded decel-to-stop (issue #70, SS-002): re-initiation from a
//!    stop and any speed-magnitude increase are DENIED with their specific
//!    codes, for all finite inputs in those regions.

#[allow(unused_imports)]
use crate::kinematics_contract::{
    enforce_degraded_decel_to_stop, validate_vehicle_command, DenyCode, EnforceAction,
    ProposedVehicleCommand, VehicleKinematicsContract, STOP_EPSILON_MPS,
};

/// A contract with every field a symbolic finite value in a sane positive
/// range, and a symbolic PRESENT-or-ABSENT ODD cap. Only the fields the proved
/// prefix reads matter (`max_speed_mps` / `odd_speed_cap_mps`); the rest are
/// fixed to the nominal reference values.
#[cfg(kani)]
fn any_bounded_contract() -> VehicleKinematicsContract {
    // Grid-scaled per the EP-15 integer-scaling approach: 0.1 m/s steps keep
    // the float derivation exact-by-construction and the solver space bounded.
    let max_speed_raw: u16 = kani::any();
    kani::assume(max_speed_raw >= 1 && max_speed_raw <= 1_000); // 0.1 ..= 100.0 m/s
    let cap_raw: u16 = kani::any();
    kani::assume(cap_raw >= 1 && cap_raw <= 1_000);
    let has_cap: bool = kani::any();

    VehicleKinematicsContract {
        max_speed_mps: f64::from(max_speed_raw) * 0.1,
        max_accel_mps2: 3.0,
        max_brake_mps2: 6.0,
        max_steering_deg: 35.0,
        max_steering_rate_deg_s: 30.0,
        min_follow_distance_m: 5.0,
        max_lateral_accel_mps2: 3.0,
        wheelbase_m: 2.8,
        width_m: 2.0,
        length_m: 4.6,
        overhang_front_m: 0.9,
        overhang_rear_m: 0.9,
        odd_speed_cap_mps: if has_cap { Some(f64::from(cap_raw) * 0.1) } else { None },
    }
}

#[cfg(kani)]
fn any_command() -> ProposedVehicleCommand {
    ProposedVehicleCommand {
        linear_velocity_mps: kani::any(),
        current_velocity_mps: kani::any(),
        delta_time_s: kani::any(),
        steering_angle_deg: kani::any(),
        current_steering_angle_deg: kani::any(),
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// K1 — SG9 fail-closed totality: ANY non-finite field (any NaN payload,
    /// either Inf, in any of the five fields) is denied before arithmetic, and
    /// the first-priority field carries its exact forensic code.
    #[kani::proof]
    fn k1_nonfinite_input_always_denied() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(
            !cmd.linear_velocity_mps.is_finite()
                || !cmd.current_velocity_mps.is_finite()
                || !cmd.steering_angle_deg.is_finite()
                || !cmd.current_steering_angle_deg.is_finite()
                || !cmd.delta_time_s.is_finite(),
        );

        let verdict = validate_vehicle_command(&cmd, &contract);
        assert!(
            matches!(verdict, EnforceAction::DenyBreach(_)),
            "a non-finite field can never Allow or Clamp"
        );
        if !cmd.linear_velocity_mps.is_finite() {
            assert_eq!(
                verdict,
                EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity),
                "the P0 first-priority field maps to its exact code"
            );
        }
    }

    /// K2 — SG3: with all fields finite, a zero-or-negative time delta is
    /// denied with `InvalidTimeDelta` for every such f64.
    #[kani::proof]
    fn k2_nonpositive_dt_denied() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(cmd.linear_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.is_finite());
        kani::assume(cmd.steering_angle_deg.is_finite());
        kani::assume(cmd.current_steering_angle_deg.is_finite());
        kani::assume(cmd.delta_time_s.is_finite() && cmd.delta_time_s <= 0.0);

        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::InvalidTimeDelta)
        );
    }

    /// K3 — SG1: a finite command over the effective ceiling clamps to EXACTLY
    /// the ceiling with direction preserved, and the effective ceiling is
    /// `min(max_speed, odd_cap)` whenever the cap is present and tighter.
    #[kani::proof]
    fn k3_speed_ceiling_clamp_exact() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(cmd.linear_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.is_finite());
        kani::assume(cmd.steering_angle_deg.is_finite());
        kani::assume(cmd.current_steering_angle_deg.is_finite());
        kani::assume(cmd.delta_time_s.is_finite() && cmd.delta_time_s > 0.0);

        let max = contract.effective_max_speed_mps();
        if let Some(cap) = contract.odd_speed_cap_mps {
            assert!(max <= contract.max_speed_mps && max <= cap, "ceiling = the tighter bound");
        }
        kani::assume(cmd.linear_velocity_mps.abs() > max);

        match validate_vehicle_command(&cmd, &contract) {
            EnforceAction::ClampLinear(v) => {
                assert_eq!(v.abs(), max, "clamped magnitude is exactly the ceiling");
                assert_eq!(
                    v.signum(),
                    cmd.linear_velocity_mps.signum(),
                    "direction preserved (reverse stays reverse)"
                );
            }
            other => panic!("over-ceiling must ClampLinear, got {other:?}"),
        }
    }

    /// K4 — Degraded HOLD (issue #70 (c)): from a stop, ANY finite command to
    /// re-initiate motion is denied with the specific code. Early-returns
    /// before the envelope check — decidable for all finite f64 pairs.
    #[kani::proof]
    fn k4_degraded_reinitiation_denied() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(cmd.linear_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.abs() <= STOP_EPSILON_MPS);
        kani::assume(cmd.linear_velocity_mps.abs() > STOP_EPSILON_MPS);

        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::DegradedReinitiationDenied)
        );
    }

    /// K5 — Degraded non-increasing speed (issue #70 (b)): while moving in an
    /// unchanged direction, any meaningful speed-magnitude increase is denied
    /// with the specific code, for all finite f64s in the region.
    #[kani::proof]
    fn k5_degraded_speed_increase_denied() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(cmd.linear_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.is_finite());
        // Moving (not the (c) hold case), same direction (not the (c′) reversal
        // case), and a meaningful magnitude increase (the (b) region).
        kani::assume(cmd.current_velocity_mps.abs() > STOP_EPSILON_MPS);
        kani::assume(
            cmd.linear_velocity_mps.signum() == cmd.current_velocity_mps.signum(),
        );
        kani::assume(
            cmd.linear_velocity_mps.abs() > cmd.current_velocity_mps.abs() + 1e-9,
        );

        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::DegradedSpeedIncreaseDenied)
        );
    }
}

// ---------------------------------------------------------------------------
// Concrete mirrors under plain `cargo test`.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod mirrors {
    use super::*;

    fn contract(max_speed: f64, cap: Option<f64>) -> VehicleKinematicsContract {
        VehicleKinematicsContract {
            max_speed_mps: max_speed,
            max_accel_mps2: 3.0,
            max_brake_mps2: 6.0,
            max_steering_deg: 35.0,
            max_steering_rate_deg_s: 30.0,
            min_follow_distance_m: 5.0,
            max_lateral_accel_mps2: 3.0,
            wheelbase_m: 2.8,
            width_m: 2.0,
            length_m: 4.6,
            overhang_front_m: 0.9,
            overhang_rear_m: 0.9,
            odd_speed_cap_mps: cap,
        }
    }

    fn cmd(linear: f64, current: f64, dt: f64) -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: linear,
            current_velocity_mps: current,
            delta_time_s: dt,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        }
    }

    #[test]
    fn k1_mirror_every_nonfinite_field_denied() {
        let c = contract(30.0, None);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut probe = cmd(5.0, 5.0, 0.1);
            probe.linear_velocity_mps = bad;
            assert_eq!(
                validate_vehicle_command(&probe, &c),
                EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity)
            );
            for field in 0..4 {
                let mut probe = cmd(5.0, 5.0, 0.1);
                match field {
                    0 => probe.current_velocity_mps = bad,
                    1 => probe.steering_angle_deg = bad,
                    2 => probe.current_steering_angle_deg = bad,
                    _ => probe.delta_time_s = bad,
                }
                assert!(matches!(
                    validate_vehicle_command(&probe, &c),
                    EnforceAction::DenyBreach(_)
                ));
            }
        }
    }

    #[test]
    fn k2_mirror_nonpositive_dt() {
        let c = contract(30.0, None);
        for dt in [0.0, -0.0, -1.0, f64::MIN] {
            assert_eq!(
                validate_vehicle_command(&cmd(5.0, 5.0, dt), &c),
                EnforceAction::DenyBreach(DenyCode::InvalidTimeDelta)
            );
        }
    }

    #[test]
    fn k3_mirror_ceiling_exact_both_cap_shapes() {
        // Cap tighter than physical max; forward and reverse.
        for (c, max) in [
            (contract(30.0, Some(22.35)), 22.35),
            (contract(30.0, None), 30.0),
            (contract(10.0, Some(50.0)), 10.0), // cap present but looser
        ] {
            for sign in [1.0, -1.0] {
                match validate_vehicle_command(&cmd(sign * (max + 5.0), 0.0, 0.1), &c) {
                    EnforceAction::ClampLinear(v) => {
                        assert_eq!(v.abs(), max);
                        assert_eq!(v.signum(), sign);
                    }
                    other => panic!("expected ClampLinear, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn k4_mirror_reinitiation_denied() {
        let c = contract(30.0, None);
        for current in [0.0, 0.04, -0.05] {
            for proposed in [0.06, 1.0, -2.0] {
                assert_eq!(
                    enforce_degraded_decel_to_stop(&cmd(proposed, current, 0.1), &c),
                    EnforceAction::DenyBreach(DenyCode::DegradedReinitiationDenied)
                );
            }
        }
    }

    #[test]
    fn k5_mirror_speed_increase_denied() {
        let c = contract(30.0, None);
        for (proposed, current) in [(6.0, 5.0), (-6.0, -5.0), (0.2, 0.1)] {
            assert_eq!(
                enforce_degraded_decel_to_stop(&cmd(proposed, current, 0.1), &c),
                EnforceAction::DenyBreach(DenyCode::DegradedSpeedIncreaseDenied)
            );
        }
    }
}
