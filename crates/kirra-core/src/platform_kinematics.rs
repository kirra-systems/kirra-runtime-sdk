//! **kirra-core platform-kinematics abstraction** (Stage S-PK1a).
//!
//! One platform-parameterized governor surface so the existing safety
//! architecture (envelope, containment, RSS, decel-to-stop, frame-integrity)
//! extends to non-Ackermann platforms. See
//! `docs/safety/STAGE_S-PK1_PLATFORM_KINEMATICS.md`.
//!
//! # The shared surface is minimal and behavioral (D1–D3)
//!
//! Each platform's *shape* stays in its impl; the trait exposes only the safety
//! *primitives* the cross-checks consume:
//! - [`PlatformKinematics::evaluate`] — the per-command kinematic verdict (the
//!   talisman's job for Ackermann), returning a platform-specific
//!   [`PlatformKinematics::Verdict`] bounded by [`PlatformVerdict`] so audit /
//!   posture / consumer-safe-stop act on any platform uniformly.
//! - [`PlatformKinematics::footprint`] — for SG2 containment.
//! - [`PlatformKinematics::max_speed_mps`] / [`max_brake_mps2`] /
//!   [`stop_epsilon_mps`] — the limits the decel-to-stop gate / RSS reason about.
//!
//! **Mechanism is hidden.** `wheelbase_m`, steering geometry, ICR, etc. stay
//! private to the impl's `evaluate`; the moment one is on the trait, that
//! platform has leaked into the abstraction.
//!
//! # S-PK1a scope
//!
//! This sub-stage adds the trait + bound + the **Ackermann verbatim adapter**
//! only. The adapter's `evaluate` *literally calls* the frozen
//! [`validate_vehicle_command`], so behaviour is unchanged by construction; the
//! equivalence test proves it. The differential-drive sibling and the
//! containment/RSS wiring are S-PK1b / S-PK1c. The scalar `KirraKernelGovernor`
//! is the composable primitive (clamp + rate-limit), **not** a platform (D3).

use crate::containment::VehicleFootprint;
use crate::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
    STOP_EPSILON_MPS,
};

/// Uniform safety view over any platform's per-command verdict, so the audit /
/// posture / consumer-safe-stop paths act on every platform identically without
/// knowing its actuation shape. The bound is deliberately tiny — per-platform
/// fidelity lives in the concrete verdict type, not here.
pub trait PlatformVerdict {
    /// True iff the command was admitted (possibly clamped) — i.e. NOT a breach.
    fn is_admitted(&self) -> bool;
    /// The audit/deny reason token when the verdict denies, else `None`.
    /// Borrowed from the verdict (`&self` lifetime), so platforms with a
    /// `&'static` token (Ackermann's `DenyCode::reason()`) and platforms with a
    /// runtime reason string (e.g. parko's `EnforcementAction::Deny { reason }`)
    /// both fit — surfaced by the differential-drive sibling (S-PK1b).
    fn deny_reason(&self) -> Option<&str>;
}

/// The frozen Ackermann verdict ([`EnforceAction`]) is admitted unless it is a
/// `DenyBreach`. This impl reads the existing enum; it does not change it.
impl PlatformVerdict for EnforceAction {
    fn is_admitted(&self) -> bool {
        !matches!(self, EnforceAction::DenyBreach(_))
    }
    fn deny_reason(&self) -> Option<&str> {
        match self {
            EnforceAction::DenyBreach(code) => Some(code.reason()),
            _ => None,
        }
    }
}

/// A platform the governor bounds spatially. One abstraction *for platforms*;
/// the scalar clamp/rate-limit primitive that platforms are *built from* is a
/// separate thing (the `KirraKernelGovernor`), never a `PlatformKinematics`.
pub trait PlatformKinematics {
    /// The proposed-command type this platform evaluates (Ackermann:
    /// [`ProposedVehicleCommand`]).
    type Command;
    /// Per-tick state beyond the command, if any (Ackermann is stateless — the
    /// current velocity/steering ride in the command — so `()`).
    type State;
    /// The platform-specific verdict (Ackermann: [`EnforceAction`], unchanged).
    type Verdict: PlatformVerdict;

    /// Per-command kinematic verdict — the hard-envelope check.
    fn evaluate(&self, command: &Self::Command, state: &Self::State) -> Self::Verdict;

    /// 2D spatial footprint for SG2 drivable-space containment.
    fn footprint(&self) -> VehicleFootprint;

    /// Maximum permitted speed magnitude (m/s) — a hard upper bound the
    /// cross-checks reason about.
    fn max_speed_mps(&self) -> f64;
    /// Maximum service-braking deceleration magnitude (m/s²) — the decel
    /// capability the decel-to-stop gate / RSS reason about.
    fn max_brake_mps2(&self) -> f64;
    /// Speed magnitude at/below which the platform is "stopped" for the
    /// converge-to-zero / no-re-initiation rule.
    fn stop_epsilon_mps(&self) -> f64;
}

/// Ackermann (bicycle-model) platform — a **verbatim adapter** over the frozen
/// kinematics-contract talisman. `evaluate` calls [`validate_vehicle_command`]
/// unchanged; `footprint` is the existing [`VehicleFootprint`] projection. The
/// AV safety case is preserved exactly: this wraps the talisman, never modifies
/// it.
#[derive(Debug, Clone)]
pub struct AckermannPlatform {
    pub contract: VehicleKinematicsContract,
}

impl AckermannPlatform {
    pub fn new(contract: VehicleKinematicsContract) -> Self {
        Self { contract }
    }
}

// SAFETY: SG9 SG2 | REQ: platform-kinematics-ackermann-adapter | TEST: ackermann_evaluate_is_verbatim_validate_vehicle_command,ackermann_footprint_matches_contract_projection,ackermann_verdict_admitted_and_reason,ackermann_accessors_mirror_contract
impl PlatformKinematics for AckermannPlatform {
    type Command = ProposedVehicleCommand;
    type State = ();
    type Verdict = EnforceAction;

    #[inline]
    fn evaluate(&self, command: &ProposedVehicleCommand, _state: &()) -> EnforceAction {
        // VERBATIM — the frozen talisman. Zero behaviour delta by construction.
        validate_vehicle_command(command, &self.contract)
    }

    #[inline]
    fn footprint(&self) -> VehicleFootprint {
        VehicleFootprint::from(&self.contract)
    }

    #[inline]
    fn max_speed_mps(&self) -> f64 {
        self.contract.max_speed_mps
    }

    #[inline]
    fn max_brake_mps2(&self) -> f64 {
        self.contract.max_brake_mps2
    }

    #[inline]
    fn stop_epsilon_mps(&self) -> f64 {
        STOP_EPSILON_MPS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> VehicleKinematicsContract {
        VehicleKinematicsContract::nominal_reference_profile()
    }

    fn cmd(linear: f64, current: f64, dt: f64, steer: f64, cur_steer: f64) -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: linear,
            current_velocity_mps: current,
            delta_time_s: dt,
            steering_angle_deg: steer,
            current_steering_angle_deg: cur_steer,
        }
    }

    /// THE zero-delta proof: the adapter's verdict is identical to calling the
    /// frozen talisman directly, across a battery covering Allow / Clamp / Deny /
    /// NaN paths.
    #[test]
    fn ackermann_evaluate_is_verbatim_validate_vehicle_command() {
        let c = contract();
        let platform = AckermannPlatform::new(c.clone());
        let battery = [
            cmd(5.0, 5.0, 0.1, 0.0, 0.0),        // clean → Allow
            cmd(1000.0, 5.0, 0.1, 0.0, 0.0),     // over speed → Clamp/Deny
            cmd(5.0, 5.0, 0.1, 90.0, 0.0),       // over steering
            cmd(5.0, 5.0, 0.1, 10.0, 0.0),       // lateral-accel territory
            cmd(f64::NAN, 5.0, 0.1, 0.0, 0.0),   // NaN → DenyBreach
            cmd(5.0, 5.0, 0.0, 0.0, 0.0),        // dt=0 → DenyBreach
            cmd(-3.0, -2.0, 0.1, -5.0, -5.0),    // reverse
        ];
        for command in battery {
            assert_eq!(
                platform.evaluate(&command, &()),
                validate_vehicle_command(&command, &c),
                "adapter must be byte-identical to the talisman for {command:?}"
            );
        }
    }

    #[test]
    fn ackermann_footprint_matches_contract_projection() {
        let c = contract();
        let platform = AckermannPlatform::new(c.clone());
        let direct = VehicleFootprint::from(&c);
        let viatrait = platform.footprint();
        assert_eq!(direct.width_m, viatrait.width_m);
        assert_eq!(direct.length_m, viatrait.length_m);
        assert_eq!(direct.wheelbase_m, viatrait.wheelbase_m);
        assert_eq!(direct.overhang_front_m, viatrait.overhang_front_m);
        assert_eq!(direct.overhang_rear_m, viatrait.overhang_rear_m);
    }

    #[test]
    fn ackermann_verdict_admitted_and_reason() {
        // Allow is admitted with no reason; a NaN command denies with the
        // existing byte-stable token.
        assert!(EnforceAction::Allow.is_admitted());
        assert_eq!(EnforceAction::Allow.deny_reason(), None);

        let c = contract();
        let platform = AckermannPlatform::new(c);
        let denied = platform.evaluate(&cmd(f64::NAN, 5.0, 0.1, 0.0, 0.0), &());
        assert!(!denied.is_admitted(), "a NaN command must not be admitted");
        assert!(denied.deny_reason().is_some(), "a denied verdict must carry a reason token");
    }

    #[test]
    fn ackermann_accessors_mirror_contract() {
        let c = contract();
        let platform = AckermannPlatform::new(c.clone());
        assert_eq!(platform.max_speed_mps(), c.max_speed_mps);
        assert_eq!(platform.max_brake_mps2(), c.max_brake_mps2);
        assert_eq!(platform.stop_epsilon_mps(), STOP_EPSILON_MPS);
    }
}
