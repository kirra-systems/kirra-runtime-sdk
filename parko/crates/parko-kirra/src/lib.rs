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
//   `MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER` is a conservative ballpark
//   for an urban service robot, NOT a validated per-platform limit. The
//   real value requires a SOTIF/robot-spec derivation (rollover analysis,
//   sweep envelope vs. pedestrian clearance, platform CoG + track width),
//   analogous to how the linear speed cap traces to ADR-0001. Filed for
//   per-platform SOTIF work — see CAVEATS in the constant doc.

use kirra_runtime_sdk::gateway::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};
// `DenyCode` is reached via `EnforceAction::DenyBreach` — see the Nominal branch below.
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

/// Conservative **placeholder** angular-velocity (yaw-rate) ceiling for
/// the Nominal posture, rad/s. **NOT a validated limit.**
///
/// # SOTIF derivation gap (TODO)
///
/// The real per-platform bound depends on:
///   - Rollover threshold (CoG height, track width, surface friction).
///   - Sweep-envelope vs. pedestrian clearance (a `max_track_radius × ω`
///     surface velocity above which the platform cannot stop short of
///     a human leg in the FTTI budget).
///   - Actuator hardware limit (wheel-encoder slip, drive-current limit).
///
/// Until a SOTIF derivation produces a defensible value per platform,
/// this placeholder is used so the axis is **checked** even if the
/// number is provisional. Do not treat `1.5 rad/s` as authoritative —
/// it is approximately 86 °/s, a slow turn-in-place for a small urban
/// service robot. Tighter is safer.
///
/// Analogous to how `URBAN_ODD_SPEED_CAP_MPS` traces to ADR-0001 /
/// KIRRA-OCCY-SPEED-VAL-001 — when this bound earns a SOTIF basis it
/// should be promoted to a sourced constant in the safety case.
///
// TODO(SOTIF): replace with per-platform derivation; this is a placeholder.
pub const MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER: f64 = 1.5;

/// MRC (degraded / RSS-unsafe) angular-velocity ceiling, rad/s.
///
/// Mirrors the linear MRC philosophy: when posture contracts, the
/// envelope contracts on **both** axes. Ratio chosen as 1/3 of the
/// Nominal placeholder (`0.5 rad/s` ≈ 29 °/s — a deliberate, slow
/// reposition). Same SOTIF derivation gap as the Nominal value.
///
// TODO(SOTIF): replace with per-platform derivation; this is a placeholder.
pub const MRC_ANGULAR_VELOCITY_CEILING_RAD_S: f64 = 0.5;

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
    /// Absolute angular-velocity ceiling, rad/s, applied in the Nominal
    /// posture (both directions: bound is on `|angular_velocity|`).
    /// Default = `MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER`. See the
    /// constant's doc for the SOTIF derivation gap.
    max_angular_velocity_rad_s: f64,
    /// Absolute angular-velocity ceiling, rad/s, applied in MRC (Degraded
    /// or RSS-unsafe). Default = `MRC_ANGULAR_VELOCITY_CEILING_RAD_S`.
    mrc_max_angular_velocity_rad_s: f64,
}

impl KirraGovernor {
    /// Construct a governor that holds both nominal and MRC fallback
    /// contract profiles and selects between them per-call based on
    /// the posture passed to `evaluate()`.
    ///
    /// Angular-velocity bounds default to the placeholder constants
    /// (`MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER`,
    /// `MRC_ANGULAR_VELOCITY_CEILING_RAD_S`) — see the constant docs
    /// for the SOTIF derivation gap. Use
    /// `KirraGovernor::with_angular_bounds` to override per platform.
    pub fn new() -> Self {
        Self {
            nominal_contract: VehicleKinematicsContract::nominal_reference_profile(),
            fallback_contract: VehicleKinematicsContract::mrc_fallback_profile(),
            rss_state: RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX },
            max_angular_velocity_rad_s: MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER,
            mrc_max_angular_velocity_rad_s: MRC_ANGULAR_VELOCITY_CEILING_RAD_S,
        }
    }

    /// Overrides the angular-velocity bounds with platform-specific
    /// values. Use this once a SOTIF derivation produces a defensible
    /// per-platform ceiling.
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
        self.max_angular_velocity_rad_s = nominal_rad_s;
        self.mrc_max_angular_velocity_rad_s = mrc_rad_s;
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
            max_angular_velocity_rad_s: MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER,
            mrc_max_angular_velocity_rad_s: MRC_ANGULAR_VELOCITY_CEILING_RAD_S,
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
            max_angular_velocity_rad_s: MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER,
            mrc_max_angular_velocity_rad_s: MRC_ANGULAR_VELOCITY_CEILING_RAD_S,
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
        let angular_clamped =
            proposed.angular_velocity.abs() > self.mrc_max_angular_velocity_rad_s;
        let safe_angular = if angular_clamped {
            self.mrc_max_angular_velocity_rad_s * proposed.angular_velocity.signum()
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
    // SAFETY: SG8 | REQ: angular-velocity-bound | TEST: nominal_angular_above_bound_clamps_to_max,nominal_angular_below_bound_passes_through,in_place_rotation_above_bound_is_clamped,linear_and_angular_both_above_bound_returns_clampmotion,locked_out_dominates_high_angular_velocity,reverse_spin_above_bound_clamps_with_correct_sign
    fn nominal_angular_clamp(&self, proposed: &ControlCommand) -> Option<f64> {
        if proposed.angular_velocity.abs() > self.max_angular_velocity_rad_s {
            Some(self.max_angular_velocity_rad_s * proposed.angular_velocity.signum())
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
    use super::{KirraGovernor, MRC_VELOCITY_CEILING_MPS};
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
    // The bound used here is the placeholder constant
    // `MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER` (1.5 rad/s); see its doc
    // for the SOTIF derivation gap.

    use super::{MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER, MRC_ANGULAR_VELOCITY_CEILING_RAD_S};

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
        let gov = KirraGovernor::new();
        let prev = cmd_twist(3.0, 0.0);
        let proposed = cmd_twist(3.0, MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER - 0.1);
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
        let gov = KirraGovernor::new();
        let prev = cmd_twist(3.0, 0.0);
        let proposed = cmd_twist(3.0, MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER + 1.0);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!(
                    (a - MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER).abs() < 1e-12,
                    "expected angular clamped to {MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER}, got {a}"
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
        let gov = KirraGovernor::new();
        let prev = cmd_twist(0.0, 0.0);
        let proposed = cmd_twist(0.0, MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER * 2.0);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert_eq!(a, MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER);
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
        let gov = KirraGovernor::new();
        let prev = cmd_twist(0.0, 0.0);
        let proposed = cmd_twist(0.0, -(MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER + 0.5));
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert_eq!(a, -MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER);
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
        let gov = KirraGovernor::new();
        let prev = cmd_twist(30.0, 0.0);
        let proposed = cmd_twist(40.0, MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER + 0.5);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampMotion { linear, angular } => {
                let lin = linear.expect("linear must be Some when over the linear ceiling");
                let ang = angular.expect("angular must be Some when over the angular bound");
                assert!(
                    lin <= 35.0 + 1e-9,
                    "linear must be clamped at or below the vehicle max (35 m/s), got {lin}"
                );
                assert_eq!(ang, MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER);
            }
            other => panic!("expected ClampMotion {{Some,Some}}, got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// Degraded posture — angular above MRC bound (tighter than Nominal)
    /// is clamped. Mirror of the linear MRC ceiling philosophy.
    #[test]
    fn degraded_angular_above_bound_clamps_to_mrc_angular_ceiling() {
        let gov = KirraGovernor::new();
        // linear is within MRC linear cap so only the angular axis fires.
        let proposed = cmd_twist(
            MRC_VELOCITY_CEILING_MPS - 0.5,
            MRC_ANGULAR_VELOCITY_CEILING_RAD_S + 0.3,
        );
        let action = gov.evaluate(&proposed, None, 0.05, SafetyPosture::Degraded);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!(
                    (a - MRC_ANGULAR_VELOCITY_CEILING_RAD_S).abs() < 1e-12,
                    "expected angular clamped to MRC ceiling {MRC_ANGULAR_VELOCITY_CEILING_RAD_S}, got {a}"
                );
            }
            other => panic!("expected ClampAngularVelocity under Degraded; got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// Degraded — both axes over their MRC caps → ClampMotion.
    #[test]
    fn degraded_both_axes_above_bound_returns_clampmotion() {
        let gov = KirraGovernor::new();
        let proposed = cmd_twist(
            MRC_VELOCITY_CEILING_MPS + 2.0,
            MRC_ANGULAR_VELOCITY_CEILING_RAD_S + 0.4,
        );
        let action = gov.evaluate(&proposed, None, 0.05, SafetyPosture::Degraded);
        match action {
            EnforcementAction::ClampMotion { linear, angular } => {
                assert_eq!(linear, Some(MRC_VELOCITY_CEILING_MPS));
                assert_eq!(angular, Some(MRC_ANGULAR_VELOCITY_CEILING_RAD_S));
            }
            other => panic!("expected ClampMotion {{Some,Some}} under Degraded; got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// LockedOut dominates the angular check — no matter what the angular
    /// value is, the verdict is Deny / hard-stop.
    #[test]
    fn locked_out_dominates_high_angular_velocity() {
        let gov = KirraGovernor::new();
        let proposed = cmd_twist(0.0, MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER * 10.0);
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
}
