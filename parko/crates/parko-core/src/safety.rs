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
}
