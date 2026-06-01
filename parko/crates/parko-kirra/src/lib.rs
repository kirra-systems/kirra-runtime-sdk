// crates/parko-kirra/src/lib.rs
//
// Adapter from parko-core's SafetyGovernor trait to the
// kirra-runtime-sdk vehicle kinematics contract.
//
// AXIS COVERAGE:
//
// parko's ControlCommand uses a differential-drive Twist model
// (linear_velocity, angular_velocity in m/s and rad/s respectively).
// Kirra's ProposedVehicleCommand uses a bicycle/Ackermann model
// (linear_velocity_mps, steering_angle_deg) — semantically different
// control representations.
//
//   - Linear axis is bridged through Kirra's vehicle kinematics contract
//     (acceleration, deceleration, lateral-accel; see
//     `validate_vehicle_command`).
//   - Angular (yaw-rate) axis is bounded NATIVELY here against
//     `max_angular_velocity_rad_s` — appropriate for differential drive
//     where in-place rotation has no bicycle-steering equivalent and
//     uncapped spin can tip the platform or sweep into a person (H1).
//
// The result is the most-restrictive verdict across the two axes:
//   linear → ClampLinear,  angular → ClampAngularVelocity,
//   both   → ClampMotion { linear: Some, angular: Some },
//   either deny → Deny.
//
// CAVEAT — bound value derivation:
//   The angular-velocity bound is SOTIF-derived as of #136 (see
//   `angular_bound::AngularVelocityBound`, `PlatformParams`, and
//   `docs/safety/ANGULAR_VELOCITY_SOTIF.md`). The H1 placeholder
//   constants are gone. The new bound is `ω_max(v) = min(rollover(v),
//   sweep, ftti)`. **Status: DRAFT — pending formal safety-engineer
//   review.**

use kirra_runtime_sdk::gateway::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};
// `DenyCode` is reached via `EnforceAction::DenyBreach` — see the Nominal branch below.
use kirra_runtime_sdk::verifier::FleetPosture;

use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
use parko_core::RssState;

pub mod angular_bound;
pub mod comparator;
pub mod diverse;
pub use angular_bound::{AngularVelocityBound, PlatformParams, ROLLOVER_MIN_LINEAR_VELOCITY_MPS};
pub use comparator::{GovernorComparator, RssAwareGovernor};
pub use diverse::DiverseKirraGovernor;

/// MRC (Minimum Risk Condition) velocity ceiling.
/// Applied when posture is Degraded or RSS state is unsafe.
/// NOT applied to LockedOut — LockedOut is a hard stop (0.0).
/// Single source of truth. Per ADL-001.
pub const MRC_VELOCITY_CEILING_MPS: f64 = 5.0;

/// **Angular-velocity bound — SOTIF-derived (issue #136).**
///
/// The H1 placeholder constants (`MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER
/// = 1.5`, `MRC_ANGULAR_VELOCITY_CEILING_RAD_S = 0.5`) are removed.
/// The bound is now computed by
/// `crate::angular_bound::AngularVelocityBound::omega_max(v)` from
/// platform parameters (`PlatformParams`):
///
///   ω_max(v) = min(rollover(v), sweep, ftti)
///
/// with rollover masked below `ROLLOVER_MIN_LINEAR_VELOCITY_MPS` to
/// handle the v=0 singularity. See `crate::angular_bound` and
/// `docs/safety/ANGULAR_VELOCITY_SOTIF.md` for the derivation,
/// assumptions, and worked reference numbers.
///
/// **Status:** DRAFT — pending formal safety-engineer review. The
/// improvement over the H1 placeholders is real (reasoning + defensible
/// values where there were none), but treating these numbers as a
/// validated safety claim requires sign-off.

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
    /// SOTIF-derived angular-velocity bound for the Nominal posture.
    /// Default = `AngularVelocityBound::nominal(PlatformParams::conservative_default())`.
    /// Override per platform via `with_platform_params` or
    /// `with_angular_bounds`.
    nominal_angular_bound: AngularVelocityBound,
    /// SOTIF-derived angular-velocity bound for the MRC (Degraded /
    /// RSS-unsafe) posture. Default = `AngularVelocityBound::mrc(...)`
    /// with `mrc_posture_factor = 0.5`. Override per platform similarly.
    mrc_angular_bound: AngularVelocityBound,
}

impl KirraGovernor {
    /// Construct a governor that holds both nominal and MRC fallback
    /// contract profiles and selects between them per-call based on
    /// the posture passed to `evaluate()`.
    ///
    /// Angular-velocity bounds default to the SOTIF-derived bound
    /// from `PlatformParams::conservative_default()` — produces a
    /// tight bound that fails toward safe for an uncharacterised
    /// platform. Override with `with_platform_params(params)` to
    /// pass platform-specific geometry + FTTI, or
    /// `with_angular_bounds(nom, mrc)` for a direct scalar override.
    /// See `crate::angular_bound` for the derivation;
    /// `docs/safety/ANGULAR_VELOCITY_SOTIF.md` is the safety case
    /// (DRAFT — pending formal safety-engineer review).
    pub fn new() -> Self {
        Self {
            nominal_contract: VehicleKinematicsContract::nominal_reference_profile(),
            fallback_contract: VehicleKinematicsContract::mrc_fallback_profile(),
            rss_state: RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX },
            nominal_angular_bound: AngularVelocityBound::nominal(PlatformParams::conservative_default()),
            mrc_angular_bound:     AngularVelocityBound::mrc    (PlatformParams::conservative_default()),
        }
    }

    /// Overrides the angular-velocity bounds with **v-independent
    /// scalar** values. Use this when the SOTIF derivation has already
    /// been done elsewhere and you want a single number per posture.
    /// For per-platform v-dependent bounds (rollover, sweep, FTTI),
    /// use `with_platform_params` instead.
    ///
    /// Panics if either bound is non-finite or non-positive.
    pub fn with_angular_bounds(mut self, nominal_rad_s: f64, mrc_rad_s: f64) -> Self {
        assert!(
            nominal_rad_s.is_finite() && nominal_rad_s > 0.0,
            "nominal angular bound must be a finite positive value, got {}",
            nominal_rad_s
        );
        assert!(
            mrc_rad_s.is_finite() && mrc_rad_s > 0.0,
            "MRC angular bound must be a finite positive value, got {}",
            mrc_rad_s
        );
        self.nominal_angular_bound = AngularVelocityBound::Scalar(nominal_rad_s);
        self.mrc_angular_bound     = AngularVelocityBound::Scalar(mrc_rad_s);
        self
    }

    /// **Issue #136 SOTIF** — overrides the angular-velocity bounds
    /// with platform-parameter-driven `ω_max(v) = min(rollover(v),
    /// sweep, ftti)` derivations. The Nominal bound uses
    /// `posture_factor = 1.0`; the MRC bound uses
    /// `params.mrc_posture_factor` (default 0.5).
    ///
    /// See `crate::angular_bound::PlatformParams` for the field
    /// catalog and `docs/safety/ANGULAR_VELOCITY_SOTIF.md` for the
    /// derivation. DRAFT — pending formal safety-engineer review.
    pub fn with_platform_params(mut self, params: PlatformParams) -> Self {
        params.validate().expect(
            "PlatformParams failed validation; check geometry > 0 and \
             mrc_posture_factor in (0, 1]"
        );
        self.nominal_angular_bound = AngularVelocityBound::nominal(params.clone());
        self.mrc_angular_bound     = AngularVelocityBound::mrc(params);
        self
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
            nominal_angular_bound: AngularVelocityBound::nominal(PlatformParams::conservative_default()),
            mrc_angular_bound:     AngularVelocityBound::mrc    (PlatformParams::conservative_default()),
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
            nominal_angular_bound: AngularVelocityBound::nominal(PlatformParams::conservative_default()),
            mrc_angular_bound:     AngularVelocityBound::mrc    (PlatformParams::conservative_default()),
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
    /// Applies the MRC envelope on BOTH axes — Degraded semantics. Used
    /// by both the Degraded posture branch and the RSS unsafe gate. NOT
    /// used for LockedOut (which is a hard stop returning 0.0).
    ///
    /// Most-restrictive-wins across the two axes:
    ///   - linear within cap + angular within cap  → Allow
    ///   - linear over cap   + angular within cap  → ClampLinearVelocity
    ///   - linear within cap + angular over cap    → ClampAngularVelocity
    ///   - both over cap                            → ClampMotion { Some, Some }
    // SAFETY: SG8 | REQ: mrc-envelope-multiaxis-clamp | TEST: degraded_above_cap_clamps_to_mrc_ceiling,rss_unsafe_above_ceiling_clamps_to_mrc,degraded_angular_above_bound_clamps_to_mrc_angular_ceiling,degraded_both_axes_above_bound_returns_clampmotion
    // (MRC envelope contraction on Degraded posture — clamps both linear
    //  and angular axes rather than denying outright. H1 closeout.)
    fn apply_mrc_profile(&self, proposed: &ControlCommand) -> EnforcementAction {
        let safe_linear = proposed.linear_velocity.min(MRC_VELOCITY_CEILING_MPS);
        let linear_clamped = safe_linear < proposed.linear_velocity;
        // SOTIF-derived: ω_max evaluated at the COMMAND's linear
        // velocity (clamped to the post-linear-cap value so the
        // rollover constraint is consistent with what'll actually
        // be commanded).
        let v_for_bound = safe_linear.abs();
        let mrc_omega_max = self.mrc_angular_bound.omega_max(v_for_bound);
        let angular_clamped = proposed.angular_velocity.abs() > mrc_omega_max;
        let safe_angular = if angular_clamped {
            mrc_omega_max * proposed.angular_velocity.signum()
        } else {
            proposed.angular_velocity
        };
        match (linear_clamped, angular_clamped) {
            (false, false) => EnforcementAction::Allow,
            (true, false)  => EnforcementAction::ClampLinearVelocity(safe_linear),
            (false, true)  => EnforcementAction::ClampAngularVelocity(safe_angular),
            (true, true)   => EnforcementAction::ClampMotion {
                linear:  Some(safe_linear),
                angular: Some(safe_angular),
            },
        }
    }

    /// Applies the Nominal angular-velocity ceiling to a proposed command,
    /// returning the clamped magnitude (sign-preserved) when the bound is
    /// exceeded, else `None`.
    ///
    /// **#136 SOTIF:** the ceiling is `ω_max(v) = min(rollover(v),
    /// sweep, ftti)` from `AngularVelocityBound::omega_max`, evaluated
    /// at the proposed command's linear velocity.
    // SAFETY: SG8 | REQ: angular-velocity-bound-sotif | TEST: nominal_angular_above_bound_clamps_to_max,nominal_angular_below_bound_passes_through,in_place_rotation_above_bound_is_clamped,linear_and_angular_both_above_bound_returns_clampmotion,locked_out_dominates_high_angular_velocity,reverse_spin_above_bound_clamps_with_correct_sign
    fn nominal_angular_clamp(&self, proposed: &ControlCommand) -> Option<f64> {
        let omega_max = self.nominal_angular_bound.omega_max(proposed.linear_velocity.abs());
        if proposed.angular_velocity.abs() > omega_max {
            Some(omega_max * proposed.angular_velocity.signum())
        } else {
            None
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
                    // Steering angle dimension is not bridged from parko's
                    // angular_velocity (differential drive ↔ Ackermann mismatch;
                    // see module doc). The angular axis is bounded natively
                    // below via `nominal_angular_clamp`.
                    steering_angle_deg: 0.0,
                    current_steering_angle_deg: 0.0,
                };
                // Linear axis — Kirra kinematics contract.
                let linear_action = validate_vehicle_command(&kirra_input, &self.nominal_contract);
                // Angular axis — native parko-side bound (H1 closeout).
                let angular_clamp = self.nominal_angular_clamp(proposed);

                match linear_action {
                    // Hard deny on the linear axis dominates — angular bound is
                    // moot if the command is already being rejected outright.
                    // `DenyCode -> String` here is the single per-deny allocation
                    // permitted at this cross-crate adapter boundary: `EnforcementAction::Deny`
                    // owns a `String` reason field by its public contract. The
                    // Governor hot path itself stayed alloc-free (S3 / #115).
                    EnforceAction::DenyBreach(code) => EnforcementAction::Deny {
                        reason: code.reason().to_string(),
                    },
                    EnforceAction::ClampLinear(safe_linear) => match angular_clamp {
                        // Both axes need clamping → multi-axis enforcement.
                        Some(safe_angular) => EnforcementAction::ClampMotion {
                            linear:  Some(safe_linear),
                            angular: Some(safe_angular),
                        },
                        // Only the linear axis needs clamping.
                        None => EnforcementAction::ClampLinearVelocity(safe_linear),
                    },
                    // ClampSteering coming out of the Kirra Ackermann pipeline
                    // is meaningless for differential drive (steering is always
                    // hardcoded to 0.0 in the bridge), so we ignore that channel
                    // and let the angular-axis verdict speak for itself.
                    EnforceAction::Allow | EnforceAction::ClampSteering(_) => match angular_clamp {
                        Some(safe_angular) => EnforcementAction::ClampAngularVelocity(safe_angular),
                        None => EnforcementAction::Allow,
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{KirraGovernor, MRC_VELOCITY_CEILING_MPS, PlatformParams};
    use parko_core::commands::ControlCommand;
    use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
    use parko_core::RssState;

    fn effective_velocity(action: EnforcementAction, proposed: f64) -> f64 {
        match action {
            EnforcementAction::Allow => proposed,
            EnforcementAction::ClampLinearVelocity(v) => v,
            EnforcementAction::ClampAngularVelocity(_) => proposed,
            EnforcementAction::ClampMotion { linear, .. } => linear.unwrap_or(proposed),
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

    // -------------------------------------------------------------------------
    // Tests for H1 — angular-velocity enforcement (Approach A)
    // -------------------------------------------------------------------------
    //
    // The fix adds a NATIVE angular-velocity bound to the parko-kirra
    // bridge. Differential drive has independent linear + angular axes;
    // these tests cover the matrix:
    //
    //   posture   | linear     | angular        | expected
    //   ----------+------------+----------------+----------------------
    //   Nominal   | within     | within         | Allow
    //   Nominal   | within     | above bound    | ClampAngularVelocity
    //   Nominal   | above lin  | above bound    | ClampMotion(Some,Some)
    //   Nominal   | linear=0   | above bound    | ClampAngularVelocity   (in-place rotation)
    //   Degraded  | within     | above MRC bnd  | ClampAngularVelocity   (tighter cap)
    //   Degraded  | above lin  | above MRC bnd  | ClampMotion(Some,Some)
    //   LockedOut | any        | above bound    | Deny (hard stop)
    //   Nominal   | within     | reverse above  | ClampAngularVelocity with sign preserved
    //
    // The H1 enforcement-logic tests pin the angular bound at the
    // pre-SOTIF placeholder values (1.5 / 0.5 rad/s) via the back-
    // compat `with_angular_bounds` (Scalar) overlay. This keeps the
    // tests focused on the enforcement LOGIC (sign preservation,
    // multi-axis ClampMotion, sticky behaviour) without coupling them
    // to the SOTIF derivation's specific numbers — the derivation
    // tests live in `crate::angular_bound::tests` and the
    // `derived_*` tests below.

    /// Local constant — pins the H1 numeric expectations against a
    /// known scalar bound, NOT the SOTIF derivation default. Lets
    /// the existing enforcement-logic assertions read the same way
    /// they did before #136 landed.
    const H1_NOMINAL_RAD_S: f64 = 1.5;
    const H1_MRC_RAD_S:     f64 = 0.5;

    /// Helper: build a governor with the legacy scalar bounds so the
    /// enforcement-logic tests below have a known reference. Tests
    /// of the SOTIF derivation itself construct their own governors
    /// via `with_platform_params`.
    fn legacy_scalar_gov() -> KirraGovernor {
        KirraGovernor::new().with_angular_bounds(H1_NOMINAL_RAD_S, H1_MRC_RAD_S)
    }

    fn cmd_twist(linear: f64, angular: f64) -> ControlCommand {
        ControlCommand { linear_velocity: linear, angular_velocity: angular, timestamp_ms: 0 }
    }

    fn effective_angular(action: &EnforcementAction, proposed: f64) -> f64 {
        match action {
            EnforcementAction::Allow => proposed,
            EnforcementAction::ClampLinearVelocity(_) => proposed,
            EnforcementAction::ClampAngularVelocity(a) => *a,
            EnforcementAction::ClampMotion { angular, .. } => angular.unwrap_or(proposed),
            EnforcementAction::Deny { .. } => 0.0,
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// linear within envelope, angular within bound → Allow.
    #[test]
    fn nominal_angular_below_bound_passes_through() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(3.0, 0.0);
        let proposed = cmd_twist(3.0, H1_NOMINAL_RAD_S - 0.1);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(action, EnforcementAction::Allow),
            "in-envelope command must pass through; got {action:?}"
        );
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// linear within envelope, angular above bound → ClampAngularVelocity
    /// (the linear axis is untouched; only the angular axis is clamped).
    #[test]
    fn nominal_angular_above_bound_clamps_to_max() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(3.0, 0.0);
        let proposed = cmd_twist(3.0, H1_NOMINAL_RAD_S + 1.0);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!(
                    (a - H1_NOMINAL_RAD_S).abs() < 1e-12,
                    "expected angular clamped to {H1_NOMINAL_RAD_S}, got {a}"
                );
            }
            other => panic!("expected ClampAngularVelocity, got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// In-place rotation (linear=0, angular above bound) — the case
    /// Approach B (diff-drive → bicycle conversion) could not represent
    /// (infinite curvature). Approach A handles it cleanly.
    #[test]
    fn in_place_rotation_above_bound_is_clamped() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(0.0, 0.0);
        let proposed = cmd_twist(0.0, H1_NOMINAL_RAD_S * 2.0);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert_eq!(a, H1_NOMINAL_RAD_S);
            }
            other => panic!(
                "in-place rotation with excess spin must clamp angular axis; got {other:?}"
            ),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// Reverse spin (negative angular) above bound — clamps with sign preserved.
    #[test]
    fn reverse_spin_above_bound_clamps_with_correct_sign() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(0.0, 0.0);
        let proposed = cmd_twist(0.0, -(H1_NOMINAL_RAD_S + 0.5));
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert_eq!(a, -H1_NOMINAL_RAD_S);
            }
            other => panic!("expected ClampAngularVelocity with negative sign; got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// Both axes above their respective bounds → ClampMotion with both
    /// fields populated. The linear axis is exceeding the nominal
    /// kinematics-contract velocity ceiling (35 m/s); the angular axis
    /// is exceeding the placeholder bound.
    #[test]
    fn linear_and_angular_both_above_bound_returns_clampmotion() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(30.0, 0.0);
        let proposed = cmd_twist(40.0, H1_NOMINAL_RAD_S + 0.5);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampMotion { linear, angular } => {
                let lin = linear.expect("linear must be Some when over the linear ceiling");
                let ang = angular.expect("angular must be Some when over the angular bound");
                assert!(
                    lin <= 35.0 + 1e-9,
                    "linear must be clamped at or below the vehicle max (35 m/s), got {lin}"
                );
                assert_eq!(ang, H1_NOMINAL_RAD_S);
            }
            other => panic!("expected ClampMotion {{Some,Some}}, got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// Degraded posture — angular above MRC bound (tighter than Nominal)
    /// is clamped. Mirror of the linear MRC ceiling philosophy.
    #[test]
    fn degraded_angular_above_bound_clamps_to_mrc_angular_ceiling() {
        let gov = legacy_scalar_gov();
        // linear is within MRC linear cap so only the angular axis fires.
        let proposed = cmd_twist(
            MRC_VELOCITY_CEILING_MPS - 0.5,
            H1_MRC_RAD_S + 0.3,
        );
        let action = gov.evaluate(&proposed, None, 0.05, SafetyPosture::Degraded);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!(
                    (a - H1_MRC_RAD_S).abs() < 1e-12,
                    "expected angular clamped to MRC ceiling {H1_MRC_RAD_S}, got {a}"
                );
            }
            other => panic!("expected ClampAngularVelocity under Degraded; got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// Degraded — both axes over their MRC caps → ClampMotion.
    #[test]
    fn degraded_both_axes_above_bound_returns_clampmotion() {
        let gov = legacy_scalar_gov();
        let proposed = cmd_twist(
            MRC_VELOCITY_CEILING_MPS + 2.0,
            H1_MRC_RAD_S + 0.4,
        );
        let action = gov.evaluate(&proposed, None, 0.05, SafetyPosture::Degraded);
        match action {
            EnforcementAction::ClampMotion { linear, angular } => {
                assert_eq!(linear, Some(MRC_VELOCITY_CEILING_MPS));
                assert_eq!(angular, Some(H1_MRC_RAD_S));
            }
            other => panic!("expected ClampMotion {{Some,Some}} under Degraded; got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// LockedOut dominates the angular check — no matter what the angular
    /// value is, the verdict is Deny / hard-stop.
    #[test]
    fn locked_out_dominates_high_angular_velocity() {
        let gov = legacy_scalar_gov();
        let proposed = cmd_twist(0.0, H1_NOMINAL_RAD_S * 10.0);
        let action = gov.evaluate(&proposed, None, 0.05, SafetyPosture::LockedOut);
        assert!(
            matches!(action, EnforcementAction::Deny { .. }),
            "LockedOut must Deny regardless of angular value; got {action:?}"
        );
        assert_eq!(effective_angular(&action, proposed.angular_velocity), 0.0);
    }

    /// `with_angular_bounds` overrides the placeholder defaults — verifies
    /// the configurable parameter actually changes the verdict.
    #[test]
    fn with_angular_bounds_override_changes_verdict() {
        // Tighter platform: 0.3 rad/s nominal cap. A command at 0.5 rad/s
        // that would be Allow under the placeholder default now clamps.
        let gov = KirraGovernor::new().with_angular_bounds(0.3, 0.1);
        let prev = cmd_twist(2.0, 0.0);
        let proposed = cmd_twist(2.0, 0.5);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => assert_eq!(a, 0.3),
            other => panic!("expected ClampAngularVelocity(0.3) with tightened bound; got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "nominal angular bound must be a finite positive value")]
    fn with_angular_bounds_rejects_non_positive() {
        let _ = KirraGovernor::new().with_angular_bounds(0.0, 0.5);
    }

    #[test]
    #[should_panic(expected = "MRC angular bound must be a finite positive value")]
    fn with_angular_bounds_rejects_nan() {
        let _ = KirraGovernor::new().with_angular_bounds(1.0, f64::NAN);
    }

    // -----------------------------------------------------------------------
    // #136 — SOTIF derivation INTEGRATION tests (governor side)
    // -----------------------------------------------------------------------
    //
    // Pure ω_max(v) math is in `crate::angular_bound::tests`. These
    // tests verify the governor's evaluate path uses the derived
    // bound at the proposed command's linear velocity — swapping
    // PlatformParams changes the verdict.

    /// SOTIF derivation drives the governor verdict. 0.5 rad/s
    /// passes under urban-reference (sweep ≈ 0.833) but clamps under
    /// conservative default (sweep ≈ 0.2). Same command, different
    /// config, different verdict — proves the derivation isn't a no-op.
    #[test]
    fn derived_bound_changes_verdict_between_platforms() {
        let proposed = cmd_twist(1.0, 0.5);
        let urban = KirraGovernor::new()
            .with_platform_params(PlatformParams::urban_service_robot_reference());
        let action_urban = urban.evaluate(
            &proposed, Some(&cmd_twist(1.0, 0.0)), 0.05, SafetyPosture::Nominal);
        assert!(matches!(action_urban, EnforcementAction::Allow),
            "urban-reference: 0.5 rad/s at v=1 m/s must Allow; got {action_urban:?}");
        let cons = KirraGovernor::new();
        let action_cons = cons.evaluate(
            &proposed, Some(&cmd_twist(1.0, 0.0)), 0.05, SafetyPosture::Nominal);
        match action_cons {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!((a - 0.2_f64).abs() < 1e-9,
                    "conservative default: expected 0.2 rad/s, got {a}");
            }
            other => panic!("conservative default must clamp; got {other:?}"),
        }
    }

    /// v=0 in-place rotation under the derived bound — singularity
    /// masked, sweep + FTTI bind. Urban reference: clamps to ~0.833.
    #[test]
    fn derived_in_place_rotation_clamps_to_sweep_bound() {
        let gov = KirraGovernor::new()
            .with_platform_params(PlatformParams::urban_service_robot_reference());
        let proposed = cmd_twist(0.0, 1.0);
        let action = gov.evaluate(&proposed, None, 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!((a - 0.833_f64).abs() < 1e-2,
                    "in-place: expected ~0.833 (sweep), got {a}");
            }
            other => panic!("expected ClampAngularVelocity at v=0; got {other:?}"),
        }
    }

    /// Degraded MRC tightens the derived bound — sweep budget halves
    /// to ~0.4167 under the 0.5 posture factor.
    #[test]
    fn derived_mrc_in_place_rotation_is_tighter_than_nominal() {
        let gov = KirraGovernor::new()
            .with_platform_params(PlatformParams::urban_service_robot_reference());
        let proposed = cmd_twist(0.0, 1.0);
        let action = gov.evaluate(&proposed, None, 0.05, SafetyPosture::Degraded);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!((a - 0.4167_f64).abs() < 1e-2,
                    "MRC in-place: expected ~0.4167, got {a}");
            }
            other => panic!("expected ClampAngularVelocity under MRC; got {other:?}"),
        }
    }

    /// `with_angular_bounds` (Scalar) is v-independent — back-compat
    /// confirmation for the H1 enforcement-logic tests.
    #[test]
    fn with_angular_bounds_scalar_back_compat_is_v_independent() {
        let gov = KirraGovernor::new().with_angular_bounds(0.7, 0.3);
        for v in [0.0_f64, 1.0, 5.0] {
            let proposed = cmd_twist(v, 0.8);
            let action = gov.evaluate(
                &proposed, Some(&cmd_twist(v, 0.0)), 0.05, SafetyPosture::Nominal);
            match action {
                EnforcementAction::ClampAngularVelocity(a) => {
                    assert!((a - 0.7_f64).abs() < 1e-9, "v={v}: expected 0.7, got {a}");
                }
                other => panic!("v={v}: expected ClampAngularVelocity, got {other:?}"),
            }
        }
    }
}
