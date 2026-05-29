// crates/parko-kirra/src/lib.rs
//
// Adapter from parko-core's SafetyGovernor trait to the
// kirra-runtime-sdk vehicle kinematics contract.
//
// LIMITATIONS:
//
// parko's ControlCommand uses a differential-drive Twist model
// (linear_velocity, angular_velocity in m/s and rad/s respectively).
// Kirra's ProposedVehicleCommand uses a bicycle/Ackermann model
// (linear_velocity_mps, steering_angle_deg). These are semantically
// different control representations.
//
// This adapter enforces ONLY the linear velocity dimension. The steering
// angle dimension is set to zero (current and proposed both 0.0 degrees),
// which means Kirra's steering rate-of-change check effectively becomes a
// no-op for this dimension.
//
// Differential-drive robots that need angular velocity bounds checking
// should add a separate governor or extend this one with a wheelbase-
// dependent kinematic bicycle conversion. That is future work.

use kirra_runtime_sdk::gateway::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};
use kirra_runtime_sdk::verifier::FleetPosture;

use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
use parko_core::RssState;

pub mod comparator;
pub use comparator::GovernorComparator;

/// MRC (Minimum Risk Condition) velocity ceiling.
/// Applied when posture is Degraded or RSS state is unsafe.
/// NOT applied to LockedOut — LockedOut is a hard stop (0.0).
/// Single source of truth. Per ADL-001.
pub const MRC_VELOCITY_CEILING_MPS: f64 = 5.0;

/// A safety governor backed by the Kirra runtime SDK's vehicle kinematics
/// contract.
///
/// Holds both nominal and MRC fallback contract profiles and selects
/// between them per-call based on the posture passed to `evaluate()`.
pub struct KirraGovernor {
    nominal_contract: VehicleKinematicsContract,
    #[allow(dead_code)]
    fallback_contract: VehicleKinematicsContract,
    rss_state: RssState,
}

impl KirraGovernor {
    /// Construct a governor that holds both nominal and MRC fallback
    /// contract profiles and selects between them per-call based on
    /// the posture passed to `evaluate()`.
    pub fn new() -> Self {
        Self {
            nominal_contract: VehicleKinematicsContract::nominal_reference_profile(),
            fallback_contract: VehicleKinematicsContract::mrc_fallback_profile(),
            rss_state: RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX },
        }
    }

    /// Updates the RSS safe-distance state.
    /// Called by the control loop after each RSS evaluation cycle.
    pub fn update_rss_state(&mut self, state: RssState) {
        self.rss_state = state;
    }

    /// Construct a governor that uses the nominal profile regardless of
    /// the posture passed to evaluate(). Kept for convenience and
    /// backward compatibility.
    pub fn nominal() -> Self {
        let profile = VehicleKinematicsContract::nominal_reference_profile();
        Self {
            nominal_contract: profile.clone(),
            fallback_contract: profile,
            rss_state: RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX },
        }
    }

    /// Construct a governor that uses the MRC fallback profile regardless
    /// of the posture passed to evaluate(). Kept for convenience and
    /// backward compatibility.
    pub fn mrc_fallback() -> Self {
        let profile = VehicleKinematicsContract::mrc_fallback_profile();
        Self {
            nominal_contract: profile.clone(),
            fallback_contract: profile,
            rss_state: RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX },
        }
    }

    /// Backward-compatible posture-based constructor. Equivalent to
    /// new() but kept for callers using the older API.
    pub fn for_posture(posture: FleetPosture) -> Self {
        match posture {
            FleetPosture::Nominal => Self::nominal(),
            // Degraded uses the MRC fallback profile as its nominal contract.
            FleetPosture::Degraded => Self::mrc_fallback(),
            // LockedOut: evaluate() always returns Deny (0.0) regardless of
            // the contract stored here; use the full profile so the struct is
            // valid and the Nominal branch works if posture changes.
            FleetPosture::LockedOut => Self::new(),
        }
    }
}

impl KirraGovernor {
    /// Applies the MRC velocity cap — Degraded semantics.
    /// Used by both the Degraded posture branch and the RSS unsafe gate.
    /// NOT used for LockedOut (which is a hard stop returning 0.0).
    fn apply_mrc_profile(&self, proposed: &ControlCommand) -> EnforcementAction {
        let safe = proposed.linear_velocity.min(MRC_VELOCITY_CEILING_MPS);
        if safe < proposed.linear_velocity {
            EnforcementAction::ClampLinearVelocity(safe)
        } else {
            EnforcementAction::Allow
        }
    }
}

impl SafetyGovernor for KirraGovernor {
    fn evaluate(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
    ) -> EnforcementAction {
        // LockedOut check first — hard stop takes absolute priority.
        if posture == SafetyPosture::LockedOut {
            return EnforcementAction::Deny {
                reason: "LockedOut: hard stop".to_string(),
            };
        }

        // RSS gate second — unsafe state applies Degraded semantics (MRC cap).
        // Per ADL-001: a sensor gap is recoverable; hard stop (0.0) is not.
        if !self.rss_state.safe {
            return self.apply_mrc_profile(proposed);
        }

        match posture {
            SafetyPosture::LockedOut => unreachable!("handled above"),
            SafetyPosture::Degraded => self.apply_mrc_profile(proposed),
            SafetyPosture::Nominal => {
                let current_velocity = previous.map(|p| p.linear_velocity).unwrap_or(0.0);
                let kirra_input = ProposedVehicleCommand {
                    linear_velocity_mps: proposed.linear_velocity,
                    current_velocity_mps: current_velocity,
                    delta_time_s,
                    // Steering angle dimension not bridged from parko's angular_velocity.
                    // See module documentation for rationale.
                    steering_angle_deg: 0.0,
                    current_steering_angle_deg: 0.0,
                };
                match validate_vehicle_command(&kirra_input, &self.nominal_contract) {
                    EnforceAction::Allow => EnforcementAction::Allow,
                    EnforceAction::ClampLinear(safe_value) => {
                        EnforcementAction::ClampLinearVelocity(safe_value)
                    }
                    EnforceAction::ClampSteering(_) => EnforcementAction::Allow,
                    EnforceAction::DenyBreach(reason) => EnforcementAction::Deny { reason },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{KirraGovernor, MRC_VELOCITY_CEILING_MPS};
    use parko_core::commands::ControlCommand;
    use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
    use parko_core::RssState;

    fn effective_velocity(action: EnforcementAction, proposed: f64) -> f64 {
        match action {
            EnforcementAction::Allow => proposed,
            EnforcementAction::ClampLinearVelocity(v) => v,
            EnforcementAction::ClampAngularVelocity(_) => proposed,
            EnforcementAction::Deny { .. } => 0.0,
        }
    }

    fn cmd(v: f64) -> ControlCommand {
        ControlCommand { linear_velocity: v, angular_velocity: 0.0, timestamp_ms: 0 }
    }

    // Test 1 — LockedOut is a hard stop across the full input range.
    #[test]
    fn locked_out_is_hard_stop_for_all_inputs() {
        let gov = KirraGovernor::new();
        for &v in &[0.0_f64, 1.0, 3.0, 5.0, 10.0, 35.0, 100.0] {
            let action = gov.evaluate(&cmd(v), None, 0.05, SafetyPosture::LockedOut);
            assert_eq!(
                effective_velocity(action, v),
                0.0,
                "LockedOut must always return 0.0 — hard stop (input {})",
                v
            );
        }
    }

    // Test 2 — Degraded applies the MRC cap.
    #[test]
    fn degraded_above_cap_clamps_to_mrc_ceiling() {
        let gov = KirraGovernor::new();
        let action = gov.evaluate(&cmd(10.0), None, 0.05, SafetyPosture::Degraded);
        assert_eq!(
            effective_velocity(action, 10.0),
            MRC_VELOCITY_CEILING_MPS,
            "Degraded: input above MRC ceiling must be capped"
        );
    }

    #[test]
    fn degraded_below_cap_allows_through() {
        let gov = KirraGovernor::new();
        let action = gov.evaluate(&cmd(3.0), None, 0.05, SafetyPosture::Degraded);
        assert_eq!(
            effective_velocity(action, 3.0),
            3.0,
            "Degraded: input below MRC ceiling must pass through"
        );
    }

    // Test 3 — LockedOut and Degraded must produce different outputs for non-zero input.
    #[test]
    fn locked_out_and_degraded_produce_different_outputs() {
        let gov = KirraGovernor::new();
        let locked_out = effective_velocity(
            gov.evaluate(&cmd(3.0), None, 0.05, SafetyPosture::LockedOut),
            3.0,
        );
        let degraded = effective_velocity(
            gov.evaluate(&cmd(3.0), None, 0.05, SafetyPosture::Degraded),
            3.0,
        );
        assert_ne!(
            locked_out, degraded,
            "LockedOut and Degraded must never produce the same output \
             for non-zero input — they are different code paths"
        );
    }

    // Test 4 — Nominal passes through valid input.
    #[test]
    fn nominal_steady_state_below_ceiling_allows_through() {
        let gov = KirraGovernor::new();
        // Use steady-state previous to suppress rate-of-change clamping.
        let prev = cmd(3.0);
        let action = gov.evaluate(&cmd(3.0), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert_eq!(
            effective_velocity(action, 3.0),
            3.0,
            "Nominal: input within envelope must pass through unchanged"
        );
    }

    // -------------------------------------------------------------------------
    // Tests A–E: RSS pre-actuator gate (PARK-016)
    // -------------------------------------------------------------------------

    fn unsafe_rss() -> RssState {
        RssState { safe: false, longitudinal_margin: 1.0, lateral_margin: 0.3 }
    }

    fn safe_rss() -> RssState {
        RssState { safe: true, longitudinal_margin: 12.0, lateral_margin: 5.0 }
    }

    // Test A — RSS unsafe, input above MRC ceiling: exact MRC contract — ADL-001
    #[test]
    fn rss_unsafe_above_ceiling_clamps_to_mrc() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(unsafe_rss());
        let commanded = MRC_VELOCITY_CEILING_MPS + 5.0;
        let action = gov.evaluate(&cmd(commanded), None, 0.05, SafetyPosture::Nominal);
        assert_eq!(
            effective_velocity(action, commanded),
            commanded.min(MRC_VELOCITY_CEILING_MPS),
            "RSS unsafe: exact MRC contract — ADL-001"
        );
    }

    // Test B — RSS safe, input within nominal envelope: passes through.
    #[test]
    fn rss_safe_nominal_input_passes_through() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(safe_rss());
        let prev = cmd(3.0);
        let action = gov.evaluate(&cmd(3.0), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert_eq!(
            effective_velocity(action, 3.0),
            3.0,
            "RSS safe: input within nominal envelope must pass through"
        );
    }

    // Test C — RSS unsafe, input below MRC ceiling: cap not triggered, passes through.
    #[test]
    fn rss_unsafe_below_ceiling_passes_through() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(unsafe_rss());
        let commanded = MRC_VELOCITY_CEILING_MPS - 1.0;
        let action = gov.evaluate(&cmd(commanded), None, 0.05, SafetyPosture::Nominal);
        assert_eq!(
            effective_velocity(action, commanded),
            commanded,
            "RSS unsafe: input below MRC ceiling must pass through unchanged"
        );
    }

    // Test D — RSS unsafe and Degraded share one code path (apply_mrc_profile).
    #[test]
    fn rss_unsafe_and_degraded_share_mrc_code_path() {
        let mut gov = KirraGovernor::new();

        // Degraded with RSS safe
        gov.update_rss_state(safe_rss());
        let output_degraded = effective_velocity(
            gov.evaluate(&cmd(10.0), None, 0.05, SafetyPosture::Degraded),
            10.0,
        );

        // Nominal with RSS unsafe
        gov.update_rss_state(unsafe_rss());
        let output_rss_unsafe = effective_velocity(
            gov.evaluate(&cmd(10.0), None, 0.05, SafetyPosture::Nominal),
            10.0,
        );

        assert_eq!(
            output_degraded, output_rss_unsafe,
            "Degraded and RSS-unsafe must produce identical output — single apply_mrc_profile path"
        );
    }

    // Test E — LockedOut hard stop takes priority over RSS gate.
    #[test]
    fn locked_out_dominates_rss_unsafe() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(unsafe_rss());
        let action = gov.evaluate(&cmd(10.0), None, 0.05, SafetyPosture::LockedOut);
        assert_eq!(
            effective_velocity(action, 10.0),
            0.0,
            "LockedOut hard stop must dominate RSS gate — LockedOut always returns 0.0"
        );
    }
}
