// parko/crates/parko-ros2/src/comparator_adapter.rs
//
// Newtype that lets a `GovernorComparator` be attached to an
// `InferenceLoop` (which requires `SafetyGovernor`).
//
// Why this exists: `GovernorComparator::evaluate` has the exact same
// signature as `SafetyGovernor::evaluate`, but parko-kirra does not
// implement the trait directly for the comparator. Filed as an
// upstream simplification opportunity (a one-line `impl
// SafetyGovernor for GovernorComparator` would replace this module);
// for M2 we wrap it locally so the rest of the crate can use the
// stock `InferenceLoop::with_governor` API without a parko-kirra
// patch.

use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
use parko_kirra::{DiverseKirraGovernor, GovernorComparator};

/// Newtype wrapper that exposes a `GovernorComparator` as a
/// `SafetyGovernor`. Delegation is one-line; no logic.
///
/// Generic over the comparator's shadow governor `S`; defaults to the
/// CERT-006 `DiverseKirraGovernor` so the stock wiring is diverse-redundant.
pub struct ComparatorAsGovernor<S: SafetyGovernor = DiverseKirraGovernor>(
    pub GovernorComparator<S>,
);

impl<S: SafetyGovernor> SafetyGovernor for ComparatorAsGovernor<S> {
    fn evaluate(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
    ) -> EnforcementAction {
        self.0.evaluate(proposed, previous, delta_time_s, posture)
    }

    /// Surface the wrapped comparator's divergence-derived posture through the `SafetyGovernor`
    /// trait, so the runtime (which holds a `dyn SafetyGovernor`) can read it after a tick and
    /// ESCALATE the effective posture — the seam that turns governor disagreement into a live
    /// safety posture (`Degraded`, then `LockedOut` when persistent), not just an audit line.
    fn recommended_posture(&self) -> SafetyPosture {
        self.0.recommended_posture()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parko_kirra::KirraGovernor;

    #[test]
    fn comparator_as_governor_delegates_evaluate() {
        let comparator = GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());
        let adapter = ComparatorAsGovernor(comparator);
        let cmd = ControlCommand {
            linear_velocity: 1.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        // A LockedOut posture should produce Deny via the underlying
        // KirraGovernor → GovernorComparator → both arms agree on Deny.
        let action = adapter.evaluate(&cmd, None, 0.05, SafetyPosture::LockedOut);
        assert!(
            matches!(action, EnforcementAction::Deny { .. }),
            "LockedOut must Deny through the adapter; got {action:?}"
        );
    }
}
