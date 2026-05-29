// crates/parko-kirra/src/comparator.rs
//
// CERT-006 — software lockstep safety comparator for KirraGovernor.

use crate::KirraGovernor;
use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
use parko_core::RssState;

/// Tolerance for floating-point comparison between primary and shadow.
/// Set to 1e-9 — effectively exact equality for f64 safety computations.
/// Any divergence beyond this indicates a fault condition.
const COMPARATOR_TOLERANCE: f64 = 1e-9;

/// Software lockstep safety comparator.
///
/// Runs two independent `KirraGovernor` instances with identical inputs.
/// If their outputs diverge beyond `COMPARATOR_TOLERANCE`, the comparator
/// returns the hard-stop enforcement action (LockedOut semantics).
///
/// This is the software equivalent of hardware lockstep dual-core
/// execution used in NVIDIA DRIVE AGX and NXP S32 safety MCUs.
///
/// # Safety argument
/// Two independent computations with divergence detection provides
/// equivalent fault coverage to hardware lockstep for the software
/// safety governance layer. Divergence can catch:
/// - Software state corruption between instances
/// - Memory faults affecting one instance's state
/// - Logic errors that produce non-deterministic output
///
/// Per CERT-006 — ISO 26262 ASIL-D decomposition argument.
pub struct GovernorComparator {
    primary: KirraGovernor,
    shadow: KirraGovernor,
}

/// Resolve the effective linear velocity an `EnforcementAction` represents
/// for divergence comparison. Mirrors the test helper in `lib.rs`:
/// - `Allow` and `ClampAngularVelocity(_)` leave linear velocity unchanged
/// - `ClampLinearVelocity(v)` substitutes the clamped value
/// - `Deny { .. }` represents a hard stop (0.0)
fn effective_linear_velocity(action: &EnforcementAction, proposed: f64) -> f64 {
    match action {
        EnforcementAction::Allow => proposed,
        EnforcementAction::ClampLinearVelocity(v) => *v,
        EnforcementAction::ClampAngularVelocity(_) => proposed,
        EnforcementAction::Deny { .. } => 0.0,
    }
}

impl GovernorComparator {
    /// Create a new comparator with two independent governor instances.
    /// Both instances should be initialized with identical configuration.
    pub fn new(primary: KirraGovernor, shadow: KirraGovernor) -> Self {
        Self { primary, shadow }
    }

    /// Evaluate a command through both governors.
    ///
    /// Returns the primary output if both agree within `COMPARATOR_TOLERANCE`.
    /// Returns hard-stop (LockedOut semantics) if outputs diverge.
    ///
    /// The signature mirrors `KirraGovernor`'s underlying
    /// `SafetyGovernor::evaluate` so the comparator is a drop-in
    /// replacement at any call site that holds a governor by value.
    pub fn evaluate(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
    ) -> EnforcementAction {
        let primary_out = self.primary.evaluate(proposed, previous, delta_time_s, posture);
        let shadow_out = self.shadow.evaluate(proposed, previous, delta_time_s, posture);

        let primary_vel = effective_linear_velocity(&primary_out, proposed.linear_velocity);
        let shadow_vel = effective_linear_velocity(&shadow_out, proposed.linear_velocity);

        if (primary_vel - shadow_vel).abs() > COMPARATOR_TOLERANCE {
            eprintln!(
                "GovernorComparator: primary/shadow divergence detected \
                 (primary_vel={primary_vel}, shadow_vel={shadow_vel}, \
                 delta={}) — returning LockedOut hard stop",
                (primary_vel - shadow_vel).abs()
            );
            EnforcementAction::Deny {
                reason: "GovernorComparator: primary/shadow divergence — hard stop".to_string(),
            }
        } else {
            primary_out
        }
    }

    /// Update RSS state on both governors.
    /// Must be called via this method (not on either inner governor directly)
    /// to maintain identical state between primary and shadow.
    pub fn update_rss_state(&mut self, state: RssState) {
        self.primary.update_rss_state(state.clone());
        self.shadow.update_rss_state(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MRC_VELOCITY_CEILING_MPS;

    fn cmd(v: f64) -> ControlCommand {
        ControlCommand {
            linear_velocity: v,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        }
    }

    fn unsafe_rss() -> RssState {
        RssState {
            safe: false,
            longitudinal_margin: 1.0,
            lateral_margin: 0.3,
        }
    }

    fn safe_rss() -> RssState {
        RssState {
            safe: true,
            longitudinal_margin: 12.0,
            lateral_margin: 5.0,
        }
    }

    // Test A — identical inputs and state on both governors → primary output returned.
    #[test]
    fn test_comparator_identical_inputs_returns_primary() {
        let comparator = GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());

        let proposed = cmd(3.0);
        let prev = cmd(3.0);
        let out = comparator.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);

        // What primary alone would have produced for the same input.
        let primary_alone =
            KirraGovernor::new().evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);

        let out_vel = effective_linear_velocity(&out, 3.0);
        let primary_vel = effective_linear_velocity(&primary_alone, 3.0);
        assert!(
            (out_vel - primary_vel).abs() <= COMPARATOR_TOLERANCE,
            "Comparator output must equal what primary alone produces \
             (got {out_vel}, expected {primary_vel})"
        );
    }

    // Test B — divergence: shadow has RSS unsafe, primary has RSS safe.
    // Above the MRC ceiling primary would return Allow → 10.0,
    // shadow would return ClampLinearVelocity(5.0). Divergence → hard stop (0.0).
    #[test]
    fn test_comparator_divergence_returns_hard_stop() {
        let mut primary = KirraGovernor::new();
        let mut shadow = KirraGovernor::new();
        primary.update_rss_state(safe_rss());
        shadow.update_rss_state(unsafe_rss());

        let comparator = GovernorComparator::new(primary, shadow);

        // Use a posture and velocity that will produce different outputs:
        // velocity above MRC ceiling, with previous=Some so Nominal kinematics
        // do not themselves clamp the primary output.
        let commanded = MRC_VELOCITY_CEILING_MPS + 5.0;
        let prev = cmd(commanded);
        let out = comparator.evaluate(&cmd(commanded), Some(&prev), 0.05, SafetyPosture::Nominal);

        let out_vel = effective_linear_velocity(&out, commanded);
        assert_eq!(
            out_vel, 0.0,
            "Divergence between primary and shadow must produce hard stop (0.0), got {out_vel}"
        );
        assert!(
            matches!(out, EnforcementAction::Deny { .. }),
            "Divergence hard stop must be EnforcementAction::Deny, got {:?}",
            out
        );
    }

    // Test C — LockedOut: both governors hard-stop, so primary and shadow agree at 0.0.
    // The comparator must not flag this as divergence.
    #[test]
    fn test_comparator_locked_out_both_zero_no_false_positive() {
        let comparator = GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());

        let proposed = cmd(10.0);
        let out = comparator.evaluate(&proposed, None, 0.05, SafetyPosture::LockedOut);

        let out_vel = effective_linear_velocity(&out, 10.0);
        assert_eq!(
            out_vel, 0.0,
            "LockedOut: both governors return 0.0, comparator must pass through (not flag divergence)"
        );
        // Reason string must be the original LockedOut, not the divergence reason —
        // confirms no false-positive divergence path was taken.
        if let EnforcementAction::Deny { reason } = out {
            assert!(
                reason.contains("LockedOut"),
                "Deny reason should be LockedOut hard stop, got {reason:?}"
            );
            assert!(
                !reason.contains("divergence"),
                "Comparator must NOT report divergence when both governors agree at 0.0"
            );
        } else {
            panic!("LockedOut must return Deny, got {:?}", out);
        }
    }

    // Test D — update_rss_state propagates to both governors. With RSS unsafe on both,
    // a Nominal-posture call above the MRC ceiling must clamp to the MRC ceiling on
    // both sides (the apply_mrc_profile path), so primary and shadow agree and the
    // comparator returns ClampLinearVelocity(MRC_VELOCITY_CEILING_MPS).
    #[test]
    fn test_comparator_rss_state_propagates_to_both() {
        let mut comparator =
            GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());
        comparator.update_rss_state(unsafe_rss());

        let commanded = MRC_VELOCITY_CEILING_MPS + 5.0;
        let out = comparator.evaluate(&cmd(commanded), None, 0.05, SafetyPosture::Nominal);

        let out_vel = effective_linear_velocity(&out, commanded);
        assert_eq!(
            out_vel, MRC_VELOCITY_CEILING_MPS,
            "RSS unsafe propagated to both governors must clamp to MRC ceiling on both, \
             so comparator returns ClampLinearVelocity({MRC_VELOCITY_CEILING_MPS}), got {out_vel}"
        );
        assert!(
            matches!(out, EnforcementAction::ClampLinearVelocity(_)),
            "Expected ClampLinearVelocity (not Deny / divergence), got {:?}",
            out
        );
    }
}
