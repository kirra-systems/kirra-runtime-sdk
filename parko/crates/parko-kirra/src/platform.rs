//! **Differential-drive platform** under the kirra-core `PlatformKinematics`
//! abstraction (Stage S-PK1b) — the first non-Ackermann sibling.
//!
//! This is the diff-drive analog of `kirra_core::platform_kinematics::AckermannPlatform`:
//! a **wrapper** over parko's existing `SafetyGovernor` diff-drive evaluation, not a
//! reimplementation. It proves the abstraction holds for a genuinely different
//! verdict shape (`EnforcementAction`'s explicit linear/angular clamp channels vs
//! Ackermann's `EnforceAction`).
//!
//! # What the second platform surfaced (see STAGE_S-PK1_PLATFORM_KINEMATICS.md)
//!
//! - **Verdict (D1) holds** via a `DiffDriveVerdict` newtype: the orphan rule
//!   forbids `impl PlatformVerdict for EnforcementAction` here (neither the trait
//!   nor `EnforcementAction` is local to this crate), so we wrap it. The newtype
//!   also keeps parko-core kirra-core-free (S-FI1c).
//! - **`&State` (D2) earns its keep**: diff-drive's per-command verdict needs the
//!   previous command, `delta_time_s`, and posture — carried in [`DiffDriveState`].
//!   (Ackermann is stateless → `()`.)
//! - **`deny_reason` generalized** from `&'static str` to `&str` upstream:
//!   parko's `Deny { reason: String }` is a runtime string, not a static token.
//!
//! # FINDINGS flagged for review (footprint shape, evaluate scope)
//!
//! - **Footprint convention:** `VehicleFootprint` is rear-axle/bicycle-shaped. A
//!   diff-drive is represented with the **geometric-center** convention
//!   (`wheelbase_m = 0`, `overhang_front = overhang_rear = length/2`), so the
//!   footprint corners come out symmetric about the (center) pose. This works, but
//!   whether the shared footprint type should be genericized (drive-agnostic) is a
//!   candidate S-PK1c refinement, not decided here.
//! - **`evaluate` scope asymmetry:** Ackermann's `evaluate` is *pure kinematics*
//!   (`validate_vehicle_command`). Diff-drive's `evaluate` wraps
//!   `SafetyGovernor::evaluate`, which in parko folds in the *pushed* RSS verdict —
//!   so its scope is wider than Ackermann's. This reflects parko's existing
//!   checker architecture (the kinematic envelope is not exposed as a separate
//!   public entry); separating it is out of S-PK1b scope.

use kirra_core::containment::VehicleFootprint;
use kirra_core::platform_kinematics::{PlatformKinematics, PlatformVerdict};
use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};

/// Diff-drive verdict — a newtype over parko's [`EnforcementAction`] so the
/// kirra-core [`PlatformVerdict`] bound can be implemented here without violating
/// the orphan rule. `Deref`s to the inner action for ergonomic access.
/// (`EnforcementAction` is not `PartialEq`, so neither is this; compare via the
/// `PlatformVerdict` view or `Debug`.)
#[derive(Debug, Clone)]
pub struct DiffDriveVerdict(pub EnforcementAction);

impl std::ops::Deref for DiffDriveVerdict {
    type Target = EnforcementAction;
    fn deref(&self) -> &EnforcementAction {
        &self.0
    }
}

impl PlatformVerdict for DiffDriveVerdict {
    fn is_admitted(&self) -> bool {
        // Allow / any Clamp* are admitted (possibly modified) commands; only a
        // hard Deny is a breach.
        !matches!(self.0, EnforcementAction::Deny { .. })
    }
    fn deny_reason(&self) -> Option<&str> {
        match &self.0 {
            EnforcementAction::Deny { reason } => Some(reason.as_str()),
            _ => None,
        }
    }
}

/// Per-tick state the diff-drive verdict needs beyond the proposed command —
/// the [`PlatformKinematics::State`] for this platform. (Ackermann's is `()`.)
#[derive(Debug, Clone)]
pub struct DiffDriveState {
    pub previous: Option<ControlCommand>,
    pub delta_time_s: f64,
    pub posture: SafetyPosture,
}

/// Differential-drive platform — wraps a parko `SafetyGovernor` (the diff-drive
/// per-command checker) plus the platform's footprint and the kinematic limits the
/// cross-checks read. Generic over the governor so it composes with `KirraGovernor`
/// or any `SafetyGovernor` (e.g. a diverse/shadow channel).
#[derive(Debug, Clone)]
pub struct DiffDrivePlatform<G: SafetyGovernor> {
    governor: G,
    footprint: VehicleFootprint,
    max_speed_mps: f64,
    max_brake_mps2: f64,
    stop_epsilon_mps: f64,
}

impl<G: SafetyGovernor> DiffDrivePlatform<G> {
    /// `footprint` should use the geometric-center convention (`wheelbase_m = 0`,
    /// symmetric overhangs) — see the module FINDINGS note. Use
    /// [`Self::centered_footprint`] to build one from width/length.
    pub fn new(
        governor: G,
        footprint: VehicleFootprint,
        max_speed_mps: f64,
        max_brake_mps2: f64,
        stop_epsilon_mps: f64,
    ) -> Self {
        Self { governor, footprint, max_speed_mps, max_brake_mps2, stop_epsilon_mps }
    }

    /// Build a center-referenced [`VehicleFootprint`] for a `width_m × length_m`
    /// differential-drive robot: `wheelbase_m = 0`, symmetric overhangs, so the
    /// containment corners are symmetric about the (center) pose.
    pub fn centered_footprint(width_m: f64, length_m: f64) -> VehicleFootprint {
        VehicleFootprint {
            width_m,
            length_m,
            overhang_front_m: length_m / 2.0,
            overhang_rear_m: length_m / 2.0,
            wheelbase_m: 0.0,
        }
    }
}

// SAFETY: SG8 SG9 | REQ: platform-kinematics-diffdrive-sibling | TEST: diffdrive_evaluate_wraps_governor,diffdrive_verdict_admitted_and_reason,diffdrive_clamp_is_admitted,diffdrive_footprint_and_accessors,centered_footprint_is_symmetric
impl<G: SafetyGovernor> PlatformKinematics for DiffDrivePlatform<G> {
    type Command = ControlCommand;
    type State = DiffDriveState;
    type Verdict = DiffDriveVerdict;

    fn evaluate(&self, command: &ControlCommand, state: &DiffDriveState) -> DiffDriveVerdict {
        // Wraps parko's existing diff-drive per-command verdict VERBATIM.
        DiffDriveVerdict(self.governor.evaluate(
            command,
            state.previous.as_ref(),
            state.delta_time_s,
            state.posture,
        ))
    }

    fn footprint(&self) -> VehicleFootprint {
        self.footprint
    }

    fn max_speed_mps(&self) -> f64 {
        self.max_speed_mps
    }

    fn max_brake_mps2(&self) -> f64 {
        self.max_brake_mps2
    }

    fn stop_epsilon_mps(&self) -> f64 {
        self.stop_epsilon_mps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KirraGovernor;

    fn platform() -> DiffDrivePlatform<KirraGovernor> {
        DiffDrivePlatform::new(
            KirraGovernor::new(),
            DiffDrivePlatform::<KirraGovernor>::centered_footprint(0.6, 0.9),
            1.5,  // max_speed_mps
            1.0,  // max_brake_mps2
            0.05, // stop_epsilon_mps (linear)
        )
    }

    fn state(posture: SafetyPosture) -> DiffDriveState {
        DiffDriveState { previous: None, delta_time_s: 0.1, posture }
    }

    /// The wrapper's verdict is exactly the governor's verdict (verbatim).
    #[test]
    fn diffdrive_evaluate_wraps_governor() {
        let p = platform();
        let gov = KirraGovernor::new();
        let cmd = ControlCommand { linear_velocity: 0.5, angular_velocity: 0.2, timestamp_ms: 0 };
        let st = state(SafetyPosture::Nominal);
        let via_trait = p.evaluate(&cmd, &st);
        let direct = gov.evaluate(&cmd, st.previous.as_ref(), st.delta_time_s, st.posture);
        // EnforcementAction is not PartialEq → compare via Debug.
        assert_eq!(
            format!("{:?}", via_trait.0),
            format!("{direct:?}"),
            "the platform verdict must equal the wrapped governor verdict"
        );
    }

    #[test]
    fn diffdrive_verdict_admitted_and_reason() {
        let allow = DiffDriveVerdict(EnforcementAction::Allow);
        assert!(allow.is_admitted());
        assert_eq!(allow.deny_reason(), None);

        let deny = DiffDriveVerdict(EnforcementAction::Deny { reason: "TEST_DENY".to_string() });
        assert!(!deny.is_admitted(), "a Deny must not be admitted");
        assert_eq!(deny.deny_reason(), Some("TEST_DENY"), "a Deny must surface its runtime reason");
    }

    #[test]
    fn diffdrive_clamp_is_admitted() {
        // Clamp* are admitted (modified-but-allowed) commands, not breaches.
        for v in [
            EnforcementAction::ClampLinearVelocity(0.3),
            EnforcementAction::ClampAngularVelocity(0.1),
            EnforcementAction::ClampMotion { linear: Some(0.2), angular: None },
        ] {
            assert!(DiffDriveVerdict(v).is_admitted());
        }
    }

    #[test]
    fn diffdrive_footprint_and_accessors() {
        let p = platform();
        let fp = p.footprint();
        assert_eq!(fp.width_m, 0.6);
        assert_eq!(fp.length_m, 0.9);
        assert_eq!(p.max_speed_mps(), 1.5);
        assert_eq!(p.max_brake_mps2(), 1.0);
        assert_eq!(p.stop_epsilon_mps(), 0.05);
    }

    #[test]
    fn centered_footprint_is_symmetric() {
        let fp = DiffDrivePlatform::<KirraGovernor>::centered_footprint(0.6, 0.9);
        assert_eq!(fp.wheelbase_m, 0.0, "diff-drive uses the center convention (no wheelbase)");
        assert_eq!(fp.overhang_front_m, fp.overhang_rear_m, "overhangs symmetric about the center pose");
        assert_eq!(fp.overhang_front_m, 0.45);
    }
}
