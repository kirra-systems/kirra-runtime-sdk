// crates/parko-core/src/safety.rs

use crate::commands::ControlCommand;

/// Operational posture passed to safety governors. Parallel to but
/// independent of any specific safety system's posture vocabulary.
/// parko-core owns this type; adapter crates (e.g., parko-aegis) map
/// it to their respective external types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyPosture {
    /// Normal operation; full safety envelope.
    Nominal,
    /// Degraded operation; conservative safety envelope.
    Degraded,
    /// Locked out; all commands should result in safe stop.
    LockedOut,
}

/// The action a safety governor decides to take on a proposed command.
#[derive(Debug, Clone)]
pub enum EnforcementAction {
    /// The proposed command is safe; pass it through unmodified.
    Allow,
    /// The proposed command's linear velocity exceeds safe bounds.
    /// Use the provided value instead.
    ClampLinearVelocity(f64),
    /// The proposed command's angular velocity exceeds safe bounds.
    /// Use the provided value instead.
    ClampAngularVelocity(f64),
    /// Clamp linear and/or angular velocity simultaneously. `None` on an
    /// axis means "leave that axis unconstrained". Used when more than
    /// one axis must be bounded in a single decision (e.g. the minimal-
    /// risk envelope: decelerate linearly while limiting yaw). Catch-all
    /// safe envelope that does NOT require a full stop.
    ///
    /// FOLLOW-UP: the single-axis `ClampLinearVelocity` and
    /// `ClampAngularVelocity` variants can be expressed as
    /// `ClampMotion { linear: Some, angular: None }` and
    /// `ClampMotion { linear: None, angular: Some }`. Long-term cleanup
    /// would collapse the three Clamp* variants into this one;
    /// deliberately additive-only here to bound the blast radius of the
    /// introduction.
    ClampMotion {
        linear: Option<f64>,
        angular: Option<f64>,
    },
    /// The proposed command violates a hard safety invariant and cannot be
    /// safely clamped. Stop the vehicle.
    Deny { reason: String },
}

/// A safety governor evaluates proposed control commands against a safety
/// envelope and returns the action to take.
///
/// Implementations are expected to be deterministic and side-effect-free
/// — given the same proposed command and previous command, they should
/// return the same EnforcementAction. Backends with internal state
/// (logging, audit chain, etc.) should ensure side effects do not affect
/// the return value.
pub trait SafetyGovernor: Send + Sync {
    fn evaluate(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
    ) -> EnforcementAction;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test governor that allows everything.
    struct AllowAllGovernor;
    impl SafetyGovernor for AllowAllGovernor {
        fn evaluate(
            &self,
            _proposed: &ControlCommand,
            _previous: Option<&ControlCommand>,
            _delta_time_s: f64,
            _posture: SafetyPosture,
        ) -> EnforcementAction {
            EnforcementAction::Allow
        }
    }

    /// Test governor that always clamps linear velocity to 1.0 m/s.
    struct ClampToOneGovernor;
    impl SafetyGovernor for ClampToOneGovernor {
        fn evaluate(
            &self,
            proposed: &ControlCommand,
            _previous: Option<&ControlCommand>,
            _delta_time_s: f64,
            _posture: SafetyPosture,
        ) -> EnforcementAction {
            if proposed.linear_velocity > 1.0 {
                EnforcementAction::ClampLinearVelocity(1.0)
            } else {
                EnforcementAction::Allow
            }
        }
    }

    #[test]
    fn allow_all_governor_returns_allow() {
        let g = AllowAllGovernor;
        let cmd = ControlCommand {
            linear_velocity: 100.0,
            angular_velocity: 5.0,
            timestamp_ms: 0,
        };
        match g.evaluate(&cmd, None, 0.05, SafetyPosture::Nominal) {
            EnforcementAction::Allow => {}
            other => panic!("expected Allow, got {:?}", other),
        }
    }

    #[test]
    fn clamp_governor_clamps_high_velocity() {
        let g = ClampToOneGovernor;
        let cmd = ControlCommand {
            linear_velocity: 5.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        match g.evaluate(&cmd, None, 0.05, SafetyPosture::Nominal) {
            EnforcementAction::ClampLinearVelocity(v) => {
                assert_eq!(v, 1.0);
            }
            other => panic!("expected ClampLinearVelocity(1.0), got {:?}", other),
        }
    }

    #[test]
    fn clamp_governor_allows_low_velocity() {
        let g = ClampToOneGovernor;
        let cmd = ControlCommand {
            linear_velocity: 0.5,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        match g.evaluate(&cmd, None, 0.05, SafetyPosture::Nominal) {
            EnforcementAction::Allow => {}
            other => panic!("expected Allow, got {:?}", other),
        }
    }

    // ── ClampMotion multi-axis variant ──────────────────────────────────────
    //
    // The variant must be representable, observable in a match, and behave
    // as documented (Some → override that axis; None → leave proposed).
    // The actuator apply-site mapping lives in `scheduler.rs`; these tests
    // exercise the enum-shape contract.

    #[test]
    fn test_clampmotion_applies_both_axes() {
        // Both axes Some → both override the proposed values. This is the
        // case the comparator will emit when reconciling on both axes.
        let action = EnforcementAction::ClampMotion {
            linear: Some(2.5),
            angular: Some(0.4),
        };
        if let EnforcementAction::ClampMotion { linear, angular } = action {
            assert_eq!(linear, Some(2.5));
            assert_eq!(angular, Some(0.4));
        } else {
            unreachable!()
        }
    }

    #[test]
    fn test_clampmotion_partial_axis() {
        // linear=Some, angular=None → leave angular unconstrained.
        let only_linear = EnforcementAction::ClampMotion {
            linear: Some(1.0),
            angular: None,
        };
        if let EnforcementAction::ClampMotion { linear, angular } = only_linear {
            assert_eq!(linear, Some(1.0));
            assert!(angular.is_none(), "None axis means unconstrained");
        } else {
            unreachable!()
        }

        // linear=None, angular=Some → leave linear unconstrained.
        let only_angular = EnforcementAction::ClampMotion {
            linear: None,
            angular: Some(0.2),
        };
        if let EnforcementAction::ClampMotion { linear, angular } = only_angular {
            assert!(linear.is_none(), "None axis means unconstrained");
            assert_eq!(angular, Some(0.2));
        } else {
            unreachable!()
        }
    }

    /// effective-velocity helpers across the codebase resolve ClampMotion's
    /// linear field, falling back to the proposed value when the axis is
    /// unconstrained. This test models the convention the helpers use.
    #[test]
    fn test_effective_velocity_handles_clampmotion() {
        fn effective_linear(action: &EnforcementAction, proposed: f64) -> f64 {
            match action {
                EnforcementAction::Allow => proposed,
                EnforcementAction::ClampLinearVelocity(v) => *v,
                EnforcementAction::ClampAngularVelocity(_) => proposed,
                EnforcementAction::ClampMotion { linear, .. } => linear.unwrap_or(proposed),
                EnforcementAction::Deny { .. } => 0.0,
            }
        }

        // Some linear value → that value
        assert_eq!(
            effective_linear(
                &EnforcementAction::ClampMotion { linear: Some(3.0), angular: Some(0.1) },
                10.0
            ),
            3.0
        );
        // None linear → proposed value passes through unconstrained
        assert_eq!(
            effective_linear(
                &EnforcementAction::ClampMotion { linear: None, angular: Some(0.1) },
                10.0
            ),
            10.0
        );
        // Both None → both axes pass through; effectively `Allow`-like
        assert_eq!(
            effective_linear(
                &EnforcementAction::ClampMotion { linear: None, angular: None },
                7.0
            ),
            7.0
        );
    }
}
