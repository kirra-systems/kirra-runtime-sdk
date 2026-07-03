// crates/parko-core/tests/posture_divergence_proptest.rs
//
// Property-based tests asserting that KirraGovernor always produces output
// within the Kirra profile velocity ceiling for each SafetyPosture. This is
// the core correctness invariant for the governor integration.
//
// KirraGovernor posture authority model (Issue #70):
//   Nominal   → validate_vehicle_command (nominal profile, 35.0 m/s ceiling + rate limits)
//   Degraded  → controlled DECEL-TO-STOP-and-HOLD: a command is permitted only
//               if it is non-increasing in speed and does not re-initiate motion
//               from a stop; a permitted (decelerating) command is then capped
//               at the MRC fallback ceiling (5.0 m/s). A speed increase or a
//               re-initiation from rest is DENIED (→ 0.0, MRC controlled stop).
//   LockedOut → hard stop (Deny → 0.0 m/s), regardless of proposed velocity
//
// Because the Degraded gate is relative to the CURRENT velocity, these
// properties pass a `previous` (last commanded) velocity: a moving,
// non-increasing history isolates the MRC-cap property from the gate; a
// `None`/zero history exercises the no-re-initiation property.
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

/// Evaluate with an explicit previous (current) velocity. `None` ⇒ no history
/// (treated as stopped by the Degraded gate).
///
/// The governor is FED (`with_external_rss_gate`) so the scene-RSS tier is
/// satisfied and the POSTURE→kinematic tier is what's exercised. This matters
/// since WS-0.1 (#770): a bare `KirraGovernor::new()` is `RssFeed::NeverFed`
/// and short-circuits EVERY posture to `apply_mrc_profile` — which silently
/// made the Nominal-ceiling property below vacuous (`0.0 <= 35.0`). Feeding
/// restores the real Nominal envelope; the unfed fail-closed default is pinned
/// separately by `unfed_governor_holds_at_zero_under_nominal`.
fn evaluate_governor_with(proposed: f64, previous: Option<f64>, posture: SafetyPosture) -> f64 {
    let governor = KirraGovernor::new().with_external_rss_gate();
    let cmd = ControlCommand {
        linear_velocity: proposed,
        angular_velocity: 0.0,
        timestamp_ms: 0,
    };
    let prev = previous.map(|v| ControlCommand {
        linear_velocity: v,
        angular_velocity: 0.0,
        timestamp_ms: 0,
    });
    let action = governor.evaluate(&cmd, prev.as_ref(), 0.05, posture);
    effective_linear_velocity(action, proposed)
}

/// Backward-compatible helper: no history (`previous = None`).
fn evaluate_governor(proposed: f64, posture: SafetyPosture) -> f64 {
    evaluate_governor_with(proposed, None, posture)
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
        // previous == proposed ⇒ zero implied acceleration, so the from-zero
        // accel limit does NOT bind and the 35 m/s SPEED ceiling is what the
        // property actually exercises (with previous=None every input clamps to
        // ~accel·dt and the ceiling itself is never reached — a weaker test).
        let output = evaluate_governor_with(proposed, Some(proposed), SafetyPosture::Nominal);
        prop_assert!(
            output <= NOMINAL_CEILING_MPS,
            "KirraGovernor Nominal output {} > ceiling {} for proposed {}",
            output, NOMINAL_CEILING_MPS, proposed
        );
    }

    /// Degraded posture, DECELERATING command (Issue #70): a non-increasing
    /// command (previous == proposed) passes the decel-to-stop gate and is
    /// capped at the MRC ceiling — output must equal exactly
    /// proposed.min(MRC_VELOCITY_CEILING_MPS). A ceiling check (<=) is
    /// insufficient — the exact MRC-cap contract must hold for a permitted
    /// command.
    #[test]
    fn governor_degraded_applies_exact_mrc_contract(
        proposed in 0.0f64..=1000.0f64
    ) {
        prop_assume!(proposed.is_finite());
        // previous == proposed ⇒ non-increasing (decel/hold), passes the gate.
        let output = evaluate_governor_with(proposed, Some(proposed), SafetyPosture::Degraded);
        prop_assert_eq!(
            output,
            proposed.min(MRC_VELOCITY_CEILING_MPS),
            "Degraded (decelerating): expected min({}, {}), got {}",
            proposed, MRC_VELOCITY_CEILING_MPS, output
        );
    }

    /// Degraded posture, RE-INITIATION from rest (Issue #70 — Cruise lesson):
    /// with no motion history (previous = None ⇒ stopped), any command above
    /// the stop floor must be DENIED → 0.0. The governor never autonomously
    /// re-initiates motion under Degraded.
    #[test]
    fn governor_degraded_denies_reinitiation_from_rest(
        proposed in 0.1f64..=1000.0f64
    ) {
        prop_assume!(proposed.is_finite());
        let output = evaluate_governor(proposed, SafetyPosture::Degraded);
        prop_assert_eq!(
            output,
            0.0,
            "Degraded must deny re-initiation from rest (got {} for proposed {})",
            output, proposed
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

    /// WS-0.1 (#770) fail-closed default: an UNFED `KirraGovernor::new()`
    /// (RssFeed::NeverFed) must HOLD at zero under Nominal for any command above
    /// the stop floor — the scene-RSS tier gates as unsafe → decel-to-stop, so
    /// no motion is ever autonomously re-initiated without an RSS feed. This is
    /// the invariant that made the fed helpers necessary; pin it directly so a
    /// regression to a fail-OPEN default is caught.
    #[test]
    fn unfed_governor_holds_at_zero_under_nominal(
        proposed in 0.1f64..=1000.0f64
    ) {
        prop_assume!(proposed.is_finite());
        let governor = KirraGovernor::new(); // UNFED — NeverFed
        let cmd = ControlCommand { linear_velocity: proposed, angular_velocity: 0.0, timestamp_ms: 0 };
        let action = governor.evaluate(&cmd, None, 0.05, SafetyPosture::Nominal);
        prop_assert_eq!(
            effective_linear_velocity(action, proposed),
            0.0,
            "unfed governor must hold at zero under Nominal (fail-closed), got motion for proposed {}",
            proposed
        );
    }

    /// LockedOut is at least as restrictive as Degraded, and strictly more so
    /// when Degraded admits motion. For a DECELERATING command (previous ==
    /// proposed, so the decel-to-stop gate passes), Degraded keeps the vehicle
    /// moving (capped at the MRC ceiling, > 0 for proposed > 0) while LockedOut
    /// is a hard stop (0.0). (Issue #70: for a re-initiation-from-rest command
    /// the two coincide at 0.0 — both refuse to start motion — so the strict
    /// inequality is asserted on the case where Degraded is permitted to move.)
    #[test]
    fn locked_out_is_more_restrictive_than_degraded(
        proposed in 0.001f64..=1000.0f64
    ) {
        prop_assume!(proposed.is_finite());
        let degraded_out =
            evaluate_governor_with(proposed, Some(proposed), SafetyPosture::Degraded);
        let locked_out = evaluate_governor(proposed, SafetyPosture::LockedOut);
        prop_assert!(
            locked_out < degraded_out,
            "LockedOut ({}) must be < Degraded ({}) for a decelerating proposed {}",
            locked_out, degraded_out, proposed
        );
    }
}
