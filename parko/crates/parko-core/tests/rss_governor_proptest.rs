// crates/parko-core/tests/rss_governor_proptest.rs
//
// Property-based tests for the RSS pre-actuator gate in KirraGovernor.
// Three posture variants × 10,000 cases each.
//
// Authority model (ADL-001; Issue #70 decel-to-stop-and-hold):
//   LockedOut → 0.0                             (hard stop)
//   Degraded  → decel-to-stop-and-HOLD: a non-increasing / non-re-initiating
//               command is permitted then capped at MRC_VELOCITY_CEILING_MPS;
//               a speed increase or re-initiation from rest is denied (→ 0.0).
//   Nominal   → nominal profile
//   RSS unsafe → Degraded semantics                (NOT a hard stop)
//
// The MRC-cap blocks below pass a non-increasing `previous` (== commanded) so
// the Issue #70 gate is satisfied and the exact MRC-cap contract is what the
// property exercises.
//
// Input strategy: bounded ranges matching plausible vehicle operating
// parameters, not arbitrary f64. Avoids NaN/Inf edge cases and focuses
// coverage on the safety-critical region.
//
// Run with: cargo test -p parko-core

use proptest::prelude::*;

use parko_kirra::{KirraGovernor, MRC_VELOCITY_CEILING_MPS};
use parko_core::{
    commands::ControlCommand,
    rss::{longitudinal_safe_distance, RssState},
    safety::{EnforcementAction, SafetyGovernor, SafetyPosture},
};

/// Resolve EnforcementAction to a concrete linear velocity.
fn effective_linear_velocity(action: EnforcementAction, proposed: f64) -> f64 {
    match action {
        EnforcementAction::Allow => proposed,
        EnforcementAction::ClampLinearVelocity(v) => v,
        EnforcementAction::ClampAngularVelocity(_) => proposed,
        EnforcementAction::ClampMotion { linear, .. } => linear.unwrap_or(proposed),
        EnforcementAction::Deny { .. } => 0.0,
    }
}

fn make_cmd(v: f64) -> ControlCommand {
    ControlCommand { linear_velocity: v, angular_velocity: 0.0, timestamp_ms: 0 }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    /// BLOCK 1 — Nominal posture.
    ///
    /// RSS unsafe + Nominal: RSS gate fires, exact MRC contract must hold.
    /// RSS safe  + Nominal: kinematic envelope checks run; output ≤ commanded.
    #[test]
    fn rss_nominal_exact_mrc_when_unsafe_bounded_when_safe(
        ego_vel   in 0.0f64..150.0,
        lead_vel  in 0.0f64..150.0,
        gap       in 0.001f64..500.0,
        commanded in 0.0f64..150.0,
    ) {
        let safe_dist = longitudinal_safe_distance(ego_vel, lead_vel, 0.5, 3.0, 6.0, 8.0);
        let rss_safe  = gap >= safe_dist;

        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState {
            safe: rss_safe,
            longitudinal_margin: if rss_safe { gap - safe_dist } else { 0.0 },
            lateral_margin: f64::MAX,
        });

        // Issue #70: non-increasing history so the RSS-unsafe MRC path's
        // decel-to-stop gate passes and the MRC-cap property is exercised.
        let prev = make_cmd(commanded);
        let action  = gov.evaluate(&make_cmd(commanded), Some(&prev), 0.05, SafetyPosture::Nominal);
        let out_vel = effective_linear_velocity(action, commanded);

        if !rss_safe {
            let expected = commanded.min(MRC_VELOCITY_CEILING_MPS);
            prop_assert_eq!(
                out_vel, expected,
                "RSS unsafe Nominal: expected min({}, ceiling), got {}",
                commanded, out_vel
            );
        } else {
            prop_assert!(
                out_vel <= commanded,
                "RSS safe Nominal: output {} exceeded commanded {}",
                out_vel, commanded
            );
        }
    }

    /// BLOCK 2 — Degraded posture.
    ///
    /// Degraded applies MRC cap regardless of RSS state (both paths call
    /// apply_mrc_profile). Exact contract: output == min(commanded, MRC_VELOCITY_CEILING_MPS).
    #[test]
    fn rss_degraded_applies_exact_mrc_regardless_of_rss_state(
        ego_vel   in 0.0f64..150.0,
        lead_vel  in 0.0f64..150.0,
        gap       in 0.001f64..500.0,
        commanded in 0.0f64..150.0,
    ) {
        let safe_dist = longitudinal_safe_distance(ego_vel, lead_vel, 0.5, 3.0, 6.0, 8.0);
        let rss_safe  = gap >= safe_dist;

        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState {
            safe: rss_safe,
            longitudinal_margin: if rss_safe { gap - safe_dist } else { 0.0 },
            lateral_margin: f64::MAX,
        });

        // Issue #70: non-increasing history so the Degraded decel-to-stop gate
        // passes and the MRC-cap property is exercised.
        let prev = make_cmd(commanded);
        let action  = gov.evaluate(&make_cmd(commanded), Some(&prev), 0.05, SafetyPosture::Degraded);
        let out_vel = effective_linear_velocity(action, commanded);
        let expected = commanded.min(MRC_VELOCITY_CEILING_MPS);

        prop_assert_eq!(
            out_vel, expected,
            "Degraded: expected min({}, ceiling), got {}",
            commanded, out_vel
        );
    }

    /// BLOCK 3 — LockedOut posture.
    ///
    /// Hard stop — always 0.0 regardless of RSS state or commanded velocity.
    /// LockedOut takes absolute priority over the RSS gate.
    #[test]
    fn rss_locked_out_always_returns_hard_stop(
        ego_vel   in 0.0f64..150.0,
        lead_vel  in 0.0f64..150.0,
        gap       in 0.001f64..500.0,
        commanded in 0.0f64..150.0,
    ) {
        let safe_dist = longitudinal_safe_distance(ego_vel, lead_vel, 0.5, 3.0, 6.0, 8.0);
        let rss_safe  = gap >= safe_dist;

        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState {
            safe: rss_safe,
            longitudinal_margin: if rss_safe { gap - safe_dist } else { 0.0 },
            lateral_margin: f64::MAX,
        });

        let action  = gov.evaluate(&make_cmd(commanded), None, 0.05, SafetyPosture::LockedOut);
        let out_vel = effective_linear_velocity(action, commanded);

        prop_assert_eq!(
            out_vel, 0.0,
            "LockedOut: must return hard stop (0.0), got {} for commanded {}",
            out_vel, commanded
        );
    }
}
