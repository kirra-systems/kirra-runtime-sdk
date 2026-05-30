// crates/parko-core/tests/posture_divergence_proptest.rs
//
// Property-based tests asserting that KirraGovernor always produces output
// within the Kirra profile velocity ceiling for each SafetyPosture. This is
// the core correctness invariant for the governor integration.
//
// KirraGovernor posture authority model:
//   Nominal   → validate_vehicle_command (nominal profile, 35.0 m/s ceiling + rate limits)
//   Degraded  → direct cap at MRC fallback ceiling (5.0 m/s), no rate limits
//   LockedOut → hard stop (Deny → 0.0 m/s), regardless of proposed velocity
//
// The nominal profile also applies rate-of-change limits; with previous=None
// the effective output is bounded by both the speed cap and the acceleration
// limit over one tick period.
//
// Run with: cargo test -p parko-core

use proptest::prelude::*;

use parko_kirra::{KirraGovernor, MRC_VELOCITY_CEILING_MPS};
use parko_core::{
    commands::ControlCommand,
    safety::{EnforcementAction, SafetyGovernor, SafetyPosture},
};

const NOMINAL_CEILING_MPS: f64 = 35.0;

/// Resolve the governor's EnforcementAction to a concrete linear velocity.
fn effective_linear_velocity(action: EnforcementAction, proposed: f64) -> f64 {
    match action {
        EnforcementAction::Allow => proposed,
        EnforcementAction::ClampLinearVelocity(v) => v,
        EnforcementAction::ClampAngularVelocity(_) => proposed,
        EnforcementAction::ClampMotion { linear, .. } => linear.unwrap_or(proposed),
        EnforcementAction::Deny { .. } => 0.0,
    }
}

fn evaluate_governor(proposed: f64, posture: SafetyPosture) -> f64 {
    let governor = KirraGovernor::new();
    let cmd = ControlCommand {
        linear_velocity: proposed,
        angular_velocity: 0.0,
        timestamp_ms: 0,
    };
    let action = governor.evaluate(&cmd, None, 0.05, posture);
    effective_linear_velocity(action, proposed)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    /// Nominal posture: for any positive proposed velocity, the governor output
    /// must not exceed the nominal reference profile ceiling (35.0 m/s).
    /// The nominal profile also applies rate-of-change limits; with previous=None
    /// the effective output is bounded by both the speed cap and the acceleration
    /// limit over one tick period.
    #[test]
    fn governor_never_exceeds_nominal_profile_ceiling(
        proposed in 0.0f64..=1000.0f64
    ) {
        prop_assume!(proposed.is_finite());
        let output = evaluate_governor(proposed, SafetyPosture::Nominal);
        prop_assert!(
            output <= NOMINAL_CEILING_MPS,
            "KirraGovernor Nominal output {} > ceiling {} for proposed {}",
            output, NOMINAL_CEILING_MPS, proposed
        );
    }

    /// Degraded posture: output must equal exactly proposed.min(MRC_VELOCITY_CEILING_MPS).
    /// A ceiling check (<=) is insufficient — the exact contract must hold.
    #[test]
    fn governor_degraded_applies_exact_mrc_contract(
        proposed in 0.0f64..=1000.0f64
    ) {
        prop_assume!(proposed.is_finite());
        let output = evaluate_governor(proposed, SafetyPosture::Degraded);
        prop_assert_eq!(
            output,
            proposed.min(MRC_VELOCITY_CEILING_MPS),
            "Degraded: expected min({}, {}), got {}",
            proposed, MRC_VELOCITY_CEILING_MPS, output
        );
    }

    // LockedOut: hard stop — safety architecture has failed.
    // Governor must return 0.0 for ALL inputs, no exceptions.
    // This is categorically different from Degraded (MRC cap).
    // Per ADL-001 (corrected after PARK-003 identified the bug).
    #[test]
    fn governor_locked_out_always_returns_zero(
        proposed in 0.0f64..=1000.0f64
    ) {
        prop_assume!(proposed.is_finite());
        let output = evaluate_governor(proposed, SafetyPosture::LockedOut);
        prop_assert_eq!(
            output,
            0.0,
            "LockedOut must always produce hard stop (0.0), got {} for input {}",
            output, proposed
        );
    }

    /// LockedOut is strictly more restrictive than Degraded: for any positive
    /// proposed velocity, the LockedOut output (0.0) must be less than the
    /// Degraded output (capped at MRC ceiling, > 0 when proposed > 0).
    #[test]
    fn locked_out_is_more_restrictive_than_degraded(
        proposed in 0.001f64..=1000.0f64
    ) {
        prop_assume!(proposed.is_finite());
        let degraded_out = evaluate_governor(proposed, SafetyPosture::Degraded);
        let locked_out = evaluate_governor(proposed, SafetyPosture::LockedOut);
        prop_assert!(
            locked_out < degraded_out,
            "LockedOut ({}) must be < Degraded ({}) for proposed {}",
            locked_out, degraded_out, proposed
        );
    }
}
