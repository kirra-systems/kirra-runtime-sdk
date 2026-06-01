// crates/parko-kirra/src/diverse.rs
//
// CERT-006 — DIVERSE second safety governor (structural / algorithmic
// diversity, "Approach A").
//
// WHY THIS EXISTS
// ---------------
// `GovernorComparator` historically ran two *identical* `KirraGovernor`
// instances. Identical redundancy catches RANDOM / transient faults (bit
// flips, memory corruption, state divergence) but is BLIND to a SYSTEMATIC
// fault: a logic or numerical bug in the shared code path computes the same
// wrong answer in both copies, they agree, and the comparator stays silent.
//
// `DiverseKirraGovernor` enforces the SAME safety properties as the primary
// `KirraGovernor` — the same ODD speed cap, the same acceleration / braking
// rate envelope, the same SOTIF angular-velocity bound, the same MRC
// contraction, the same LockedOut hard-stop — but it computes them through a
// deliberately DIFFERENT control-flow and DIFFERENT algebra. A systematic
// *implementation* bug in one is therefore unlikely to manifest identically
// in the other, so the comparator's existing divergence/escalation machinery
// can finally catch it.
//
// HONEST LIMIT (state it, don't oversell — see docs/safety/COMPARATOR_DIVERSITY.md):
//   This is *implementation* diversity. Both governors are derived from the
//   SAME specification and consume the SAME config/contract (the kinematics
//   contract limits and the `AngularVelocityBound` ω_max(v) derivation are
//   shared). A fault in that shared SPEC — e.g. a wrong limit value, or a
//   flaw in the ω_max derivation itself — would appear identically in both
//   and is NOT caught by this. Closing that requires diverse review of the
//   spec and, ultimately, an N-version clean-room reimplementation (the
//   stronger-but-later "Approach B").
//
// DIVERSITY IS NOT A LICENCE TO DIVERGE ON VALID INPUTS
// -----------------------------------------------------
// The critical, testable correctness property is AGREEMENT: on every valid
// input the diverse governor must produce the SAME physical verdict as the
// primary (a false divergence would be a safety-relevant regression — it
// would trip the comparator on a good command). The differences below are
// purely structural; they are algebraically equivalent to the primary and
// the broad agreement test (`tests` module + the proptest) guards that.
//
// CONCRETE STRUCTURAL DIFFERENCES vs `KirraGovernor` (the reviewable core):
//
//   1. REGIME SELECTION. Primary: check LockedOut, then a *separate* RSS
//      early-return, then `match posture`. Diverse: a single `classify`
//      step folds "Degraded posture OR RSS-unsafe" into one MinimumRisk
//      predicate and treats LockedOut as the dominating hard-stop, then
//      dispatches once.
//
//   2. LINEAR RATE ENVELOPE. Primary: computes a scalar implied
//      acceleration `(v - c)/dt` and sign-splits into two magnitude
//      comparisons against the accel / brake limits. Diverse: builds the
//      admissible-velocity INTERVAL `[c - brake·dt, c + accel·dt]` and does
//      interval containment. (The +1e-9 tolerance lives in acceleration
//      space in the primary; it is mapped into velocity space — `1e-9·dt` —
//      here. Algebraically identical, different code path.)
//
//   3. CEILING. Primary uses `effective_max_speed_mps()`; diverse re-derives
//      `min(max_speed, odd_cap)` inline so it does not share that accessor.
//
//   4. CLAMP RECONSTRUCTION. Primary clamps via `value * x.signum()` and
//      `f64::clamp`; diverse uses `f64::copysign` and an explicit
//      `.max(..).min(..)` fold.
//
//   5. NO SHARED ENFORCEMENT CODE. Diverse does NOT call
//      `validate_vehicle_command`, nor any `KirraGovernor` method. It only
//      reads the shared *config* (contract limit fields + the
//      `AngularVelocityBound`), which is the "same limits, computed
//      differently" the safety case requires.

use kirra_runtime_sdk::gateway::kinematics_contract::VehicleKinematicsContract;

use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
use parko_core::RssState;

use crate::angular_bound::{AngularVelocityBound, PlatformParams};
use crate::comparator::RssAwareGovernor;
use crate::MRC_VELOCITY_CEILING_MPS;

/// Acceleration-space tolerance used by the primary's rate checks
/// (`max_accel_mps2 + 1e-9`). Mirrored here so the diverse interval test
/// fires on exactly the same boundary as the primary's magnitude test.
const ACCEL_EPSILON: f64 = 1e-9;

/// Which enforcement envelope applies on this tick. Computed up front by
/// `classify`, replacing the primary's interleaved LockedOut / RSS-gate /
/// posture-match control flow with a single dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Regime {
    /// LockedOut — hard stop, dominates everything (incl. RSS).
    HardStop,
    /// Degraded posture OR RSS-unsafe — minimum-risk (MRC) contraction.
    MinimumRisk,
    /// Nominal posture with a safe RSS state — full kinematic envelope.
    Nominal,
}

/// A diverse second implementation of the Kirra safety governor.
///
/// Implements the same [`SafetyGovernor`] contract as [`crate::KirraGovernor`]
/// and consumes the same configuration (kinematics contract + angular
/// bounds), but reaches its verdict through structurally different
/// computation. See the module documentation for the diversity argument and
/// `docs/safety/COMPARATOR_DIVERSITY.md` (DRAFT) for the safety case.
pub struct DiverseKirraGovernor {
    /// Same nominal kinematics contract the primary holds. Only its limit
    /// FIELDS are read (`max_speed_mps`, `max_accel_mps2`, `max_brake_mps2`,
    /// `odd_speed_cap_mps`) — the diverse path does NOT call
    /// `validate_vehicle_command`.
    nominal_contract: VehicleKinematicsContract,
    rss_state: RssState,
    /// SOTIF-derived angular bound for the Nominal posture — same config
    /// object the primary uses. The ω_max(v) derivation is shared SPEC
    /// (see the honest-limit note); the ENFORCEMENT decision around it is
    /// re-implemented here.
    nominal_angular_bound: AngularVelocityBound,
    /// SOTIF-derived angular bound for the MRC (Degraded / RSS-unsafe) posture.
    mrc_angular_bound: AngularVelocityBound,
}

impl Default for DiverseKirraGovernor {
    fn default() -> Self {
        Self::new()
    }
}

impl DiverseKirraGovernor {
    /// Construct a diverse governor with defaults identical to
    /// [`crate::KirraGovernor::new`] so the two agree by construction on a
    /// shared config. Mirroring the primary's defaults is what makes the
    /// comparator's agreement property meaningful.
    pub fn new() -> Self {
        Self {
            nominal_contract: VehicleKinematicsContract::nominal_reference_profile(),
            rss_state: RssState {
                safe: true,
                longitudinal_margin: f64::MAX,
                lateral_margin: f64::MAX,
            },
            nominal_angular_bound: AngularVelocityBound::nominal(
                PlatformParams::conservative_default(),
            ),
            mrc_angular_bound: AngularVelocityBound::mrc(PlatformParams::conservative_default()),
        }
    }

    /// Override the angular bounds with v-independent scalar values. Mirrors
    /// [`crate::KirraGovernor::with_angular_bounds`] so a comparator can pair
    /// a primary and a diverse governor on the same scalar config.
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
        self.mrc_angular_bound = AngularVelocityBound::Scalar(mrc_rad_s);
        self
    }

    /// Override the angular bounds with platform-parameter-driven SOTIF
    /// derivations. Mirrors [`crate::KirraGovernor::with_platform_params`].
    pub fn with_platform_params(mut self, params: PlatformParams) -> Self {
        params.validate().expect(
            "PlatformParams failed validation; check geometry > 0 and \
             mrc_posture_factor in (0, 1]",
        );
        self.nominal_angular_bound = AngularVelocityBound::nominal(params.clone());
        self.mrc_angular_bound = AngularVelocityBound::mrc(params);
        self
    }

    /// Update the RSS safe-distance state. Same semantics as the primary's
    /// `update_rss_state`; the comparator keeps both governors in lockstep
    /// by calling this through its own `update_rss_state`.
    pub fn update_rss_state(&mut self, state: RssState) {
        self.rss_state = state;
    }

    /// DIFFERENCE #1 — single regime classifier. Folds the primary's
    /// LockedOut-then-RSS-gate-then-posture-match control flow into one
    /// dispatch. LockedOut dominates (matches the primary returning Deny
    /// before its RSS gate); otherwise "Degraded OR RSS-unsafe" both route
    /// to the minimum-risk envelope (the primary reaches the same MRC code
    /// from two different branches).
    fn classify(&self, posture: SafetyPosture) -> Regime {
        if posture == SafetyPosture::LockedOut {
            Regime::HardStop
        } else if posture == SafetyPosture::Degraded || !self.rss_state.safe {
            Regime::MinimumRisk
        } else {
            Regime::Nominal
        }
    }

    /// DIFFERENCE #3 — re-derive the effective ceiling inline rather than
    /// calling `contract.effective_max_speed_mps()`.
    fn effective_ceiling(&self) -> f64 {
        match self.nominal_contract.odd_speed_cap_mps {
            Some(cap) if cap < self.nominal_contract.max_speed_mps => cap,
            _ => self.nominal_contract.max_speed_mps,
        }
    }

    /// Minimum-risk envelope (Degraded / RSS-unsafe). Mirrors the primary's
    /// `apply_mrc_profile` effect — linear capped at the MRC ceiling, angular
    /// capped at the MRC ω_max evaluated at the post-cap linear speed — but
    /// expressed with an explicit over-ceiling guard and `copysign`.
    fn enforce_minimum_risk(&self, proposed: &ControlCommand) -> EnforcementAction {
        let v = proposed.linear_velocity;
        let w = proposed.angular_velocity;

        // DIFFERENCE #4 — over-ceiling guard instead of `v.min(MRC)`.
        let capped_linear = if v > MRC_VELOCITY_CEILING_MPS {
            MRC_VELOCITY_CEILING_MPS
        } else {
            v
        };
        let linear_clamped = capped_linear != v;

        // Angular bound is evaluated at the (post-cap) linear speed — same
        // as the primary's `v_for_bound = safe_linear.abs()`.
        let omega_ceiling = self.mrc_angular_bound.omega_max(capped_linear.abs());
        let angular = clamp_angular(w, omega_ceiling);

        build_action(capped_linear, linear_clamped, angular)
    }

    /// Nominal envelope. Re-implements the primary's linear pipeline
    /// (finiteness + dt guards, ODD ceiling, accel/brake rate limit) via the
    /// interval formulation (DIFFERENCE #2) and re-implements the angular
    /// clamp, then folds the two axes most-restrictively.
    fn enforce_nominal(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
    ) -> EnforcementAction {
        let v = proposed.linear_velocity;
        let w = proposed.angular_velocity;
        let current = previous.map(|p| p.linear_velocity).unwrap_or(0.0);

        // Fail-closed guards. The primary emits a distinct DenyCode per
        // offending field; the comparator compares physical EFFECT (a hard
        // stop is a hard stop), so a single combined guard with a generic
        // reason is the diverse — and equivalent — form.
        if !v.is_finite() || !current.is_finite() || !delta_time_s.is_finite() {
            return EnforcementAction::Deny {
                reason: "DiverseKirraGovernor: non-finite command input — hard stop".to_string(),
            };
        }
        if delta_time_s <= 0.0 {
            return EnforcementAction::Deny {
                reason: "DiverseKirraGovernor: non-physical time delta — hard stop".to_string(),
            };
        }

        let (safe_linear, linear_clamped) =
            self.diverse_linear_envelope(v, current, delta_time_s);

        // DIFFERENCE — angular bound evaluated at the ORIGINAL commanded
        // linear speed (matching the primary's `nominal_angular_clamp`,
        // which uses `proposed.linear_velocity` regardless of any linear
        // clamp that fired).
        let omega_ceiling = self.nominal_angular_bound.omega_max(v.abs());
        let angular = clamp_angular(w, omega_ceiling);

        build_action(safe_linear, linear_clamped, angular)
    }

    /// DIFFERENCE #2 — the linear envelope as an admissible-velocity interval.
    ///
    /// Returns `(safe_linear, clamped)`. Priority, matching the primary:
    ///   1. the absolute ODD ceiling dominates the rate limit (the primary
    ///      early-returns on it before computing acceleration);
    ///   2. otherwise clamp into `[current - brake·dt, current + accel·dt]`,
    ///      itself clipped to `±ceiling`.
    fn diverse_linear_envelope(&self, v: f64, current: f64, dt: f64) -> (f64, bool) {
        let ceiling = self.effective_ceiling();

        // Priority 1: absolute ceiling. Same comparison form as the primary
        // so the boundary decision is identical.
        if v.abs() > ceiling {
            return (f64::copysign(ceiling, v), true);
        }

        // Priority 2: rate envelope as an interval.
        let max_up = current + self.nominal_contract.max_accel_mps2 * dt;
        let max_down = current - self.nominal_contract.max_brake_mps2 * dt;
        // The primary's tolerance is `+1e-9` in acceleration space; in
        // velocity space (over one tick) that is `1e-9 · dt`.
        let band = ACCEL_EPSILON * dt;
        let clip = |x: f64| x.max(-ceiling).min(ceiling);

        if v - max_up > band {
            (clip(max_up), true)
        } else if max_down - v > band {
            (clip(max_down), true)
        } else {
            (v, false)
        }
    }
}

/// DIFFERENCE #4 — angular clamp via `copysign`. `Some(clamped)` when the
/// magnitude exceeds the ceiling, else `None` (axis passes through). The
/// magnitude test is exceeded ⇒ `w != 0`, so `copysign` reconstructs the
/// commanded direction exactly as the primary's `omega * w.signum()` does.
fn clamp_angular(w: f64, omega_ceiling: f64) -> Option<f64> {
    if w.abs() > omega_ceiling {
        Some(f64::copysign(omega_ceiling, w))
    } else {
        None
    }
}

/// Fold the two per-axis verdicts into a single [`EnforcementAction`],
/// most-restrictive-wins. Produces the same variant the primary does for a
/// given `(linear_clamped, angular_clamped)` pair so the physical effect —
/// and the action shape — match.
fn build_action(
    safe_linear: f64,
    linear_clamped: bool,
    angular: Option<f64>,
) -> EnforcementAction {
    match (linear_clamped, angular) {
        (false, None) => EnforcementAction::Allow,
        (true, None) => EnforcementAction::ClampLinearVelocity(safe_linear),
        (false, Some(safe_angular)) => EnforcementAction::ClampAngularVelocity(safe_angular),
        (true, Some(safe_angular)) => EnforcementAction::ClampMotion {
            linear: Some(safe_linear),
            angular: Some(safe_angular),
        },
    }
}

impl SafetyGovernor for DiverseKirraGovernor {
    fn evaluate(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
    ) -> EnforcementAction {
        match self.classify(posture) {
            Regime::HardStop => EnforcementAction::Deny {
                reason: "DiverseKirraGovernor: locked-out hard stop".to_string(),
            },
            Regime::MinimumRisk => self.enforce_minimum_risk(proposed),
            Regime::Nominal => self.enforce_nominal(proposed, previous, delta_time_s),
        }
    }
}

impl RssAwareGovernor for DiverseKirraGovernor {
    fn set_rss_state(&mut self, state: RssState) {
        self.update_rss_state(state);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// The CORRECTNESS property (agreement) is the critical one and lives here:
//   - `agreement_*` hand-built cases span Allow / each Clamp / Deny across
//     every posture and both RSS states and several configs;
//   - `proptest_*` fuzzes the same property across 10k bounded inputs.
//
// Both assert the comparator's OWN predicate (`actions_diverge`) sees no
// divergence between primary and diverse — i.e. the diverse governor never
// introduces a false divergence on a valid input.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comparator::actions_diverge;
    use crate::KirraGovernor;
    use proptest::prelude::*;

    const TOL: f64 = 1e-9;

    fn twist(linear: f64, angular: f64) -> ControlCommand {
        ControlCommand {
            linear_velocity: linear,
            angular_velocity: angular,
            timestamp_ms: 0,
        }
    }

    fn safe_rss() -> RssState {
        RssState {
            safe: true,
            longitudinal_margin: 12.0,
            lateral_margin: 5.0,
        }
    }

    fn unsafe_rss() -> RssState {
        RssState {
            safe: false,
            longitudinal_margin: 1.0,
            lateral_margin: 0.3,
        }
    }

    /// Assert primary and diverse agree (no divergence) on one input under a
    /// shared default config + given RSS state.
    fn assert_agrees(
        rss: RssState,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        dt: f64,
        posture: SafetyPosture,
    ) {
        let mut primary = KirraGovernor::new();
        let mut diverse = DiverseKirraGovernor::new();
        primary.update_rss_state(rss.clone());
        diverse.update_rss_state(rss);

        let p = primary.evaluate(proposed, previous, dt, posture);
        let d = diverse.evaluate(proposed, previous, dt, posture);
        assert!(
            !actions_diverge(&p, &d, proposed, TOL),
            "false divergence: posture={posture:?} cmd=({},{}) prev={:?} dt={dt}\n  primary={p:?}\n  diverse={d:?}",
            proposed.linear_velocity,
            proposed.angular_velocity,
            previous.map(|c| c.linear_velocity),
        );
    }

    // -- Hand-built broad agreement set ----------------------------------

    /// Nominal: in-envelope Allow, ceiling clamp, accel clamp, brake clamp,
    /// angular clamp, both-axes clamp, in-place rotation.
    #[test]
    fn agreement_nominal_spans_allow_and_each_clamp() {
        let prev = twist(3.0, 0.0);
        // Allow — steady state in envelope.
        assert_agrees(safe_rss(), &twist(3.0, 0.0), Some(&prev), 0.05, SafetyPosture::Nominal);
        // Linear ceiling clamp (|v| > 35).
        assert_agrees(safe_rss(), &twist(40.0, 0.0), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert_agrees(safe_rss(), &twist(-50.0, 0.0), Some(&prev), 0.05, SafetyPosture::Nominal);
        // Acceleration rate clamp.
        assert_agrees(safe_rss(), &twist(20.0, 0.0), Some(&twist(0.0, 0.0)), 0.05, SafetyPosture::Nominal);
        // Brake rate clamp.
        assert_agrees(safe_rss(), &twist(-20.0, 0.0), Some(&twist(10.0, 0.0)), 0.05, SafetyPosture::Nominal);
        // Angular-only clamp (linear in envelope, big yaw).
        assert_agrees(safe_rss(), &twist(2.0, 5.0), Some(&twist(2.0, 0.0)), 0.05, SafetyPosture::Nominal);
        // Both axes clamp (over ceiling + big yaw).
        assert_agrees(safe_rss(), &twist(60.0, 5.0), Some(&twist(30.0, 0.0)), 0.05, SafetyPosture::Nominal);
        // In-place rotation (v=0, big yaw).
        assert_agrees(safe_rss(), &twist(0.0, 3.0), Some(&twist(0.0, 0.0)), 0.05, SafetyPosture::Nominal);
    }

    /// Nominal fail-closed Deny paths (non-finite / non-physical dt).
    #[test]
    fn agreement_nominal_deny_paths() {
        let prev = twist(3.0, 0.0);
        assert_agrees(safe_rss(), &twist(f64::NAN, 0.0), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert_agrees(safe_rss(), &twist(3.0, 0.0), Some(&prev), 0.0, SafetyPosture::Nominal);
        assert_agrees(safe_rss(), &twist(3.0, 0.0), Some(&prev), -0.1, SafetyPosture::Nominal);
    }

    /// Degraded posture — MRC contraction on both axes.
    #[test]
    fn agreement_degraded_spans_mrc_envelope() {
        // Below MRC cap → pass.
        assert_agrees(safe_rss(), &twist(3.0, 0.1), None, 0.05, SafetyPosture::Degraded);
        // Above MRC linear cap → clamp to 5.0.
        assert_agrees(safe_rss(), &twist(10.0, 0.0), None, 0.05, SafetyPosture::Degraded);
        // Above MRC angular cap.
        assert_agrees(safe_rss(), &twist(2.0, 4.0), None, 0.05, SafetyPosture::Degraded);
        // Both axes over MRC caps.
        assert_agrees(safe_rss(), &twist(12.0, 4.0), None, 0.05, SafetyPosture::Degraded);
        // Reverse below cap (negative not clamped by min()).
        assert_agrees(safe_rss(), &twist(-3.0, 0.0), None, 0.05, SafetyPosture::Degraded);
    }

    /// RSS-unsafe in Nominal posture must take the same MRC path.
    #[test]
    fn agreement_rss_unsafe_takes_mrc_path() {
        assert_agrees(unsafe_rss(), &twist(10.0, 0.0), None, 0.05, SafetyPosture::Nominal);
        assert_agrees(unsafe_rss(), &twist(2.0, 4.0), None, 0.05, SafetyPosture::Nominal);
        assert_agrees(unsafe_rss(), &twist(3.0, 0.1), None, 0.05, SafetyPosture::Nominal);
    }

    /// LockedOut dominates everything (incl. RSS-unsafe).
    #[test]
    fn agreement_locked_out_hard_stop() {
        for &v in &[0.0_f64, 3.0, 35.0, 100.0, -20.0] {
            assert_agrees(safe_rss(), &twist(v, 2.0), None, 0.05, SafetyPosture::LockedOut);
            assert_agrees(unsafe_rss(), &twist(v, 2.0), None, 0.05, SafetyPosture::LockedOut);
        }
    }

    /// Diversity must hold across config too: same platform params on both
    /// → still agree. Uses the urban reference platform (different ω_max).
    #[test]
    fn agreement_with_shared_platform_params() {
        let p = PlatformParams::urban_service_robot_reference();
        let mut primary = KirraGovernor::new().with_platform_params(p.clone());
        let mut diverse = DiverseKirraGovernor::new().with_platform_params(p);
        primary.update_rss_state(safe_rss());
        diverse.update_rss_state(safe_rss());

        for (v, w, posture) in [
            (1.0, 0.5, SafetyPosture::Nominal),
            (0.0, 1.0, SafetyPosture::Nominal),
            (4.0, 0.9, SafetyPosture::Degraded),
            (10.0, 2.0, SafetyPosture::Nominal),
        ] {
            let cmd = twist(v, w);
            let prev = twist(v, 0.0);
            let pa = primary.evaluate(&cmd, Some(&prev), 0.05, posture);
            let da = diverse.evaluate(&cmd, Some(&prev), 0.05, posture);
            assert!(
                !actions_diverge(&pa, &da, &cmd, TOL),
                "platform-params divergence at ({v},{w},{posture:?}): {pa:?} vs {da:?}"
            );
        }
    }

    /// Same with scalar angular-bound overrides.
    #[test]
    fn agreement_with_shared_scalar_bounds() {
        let mut primary = KirraGovernor::new().with_angular_bounds(0.7, 0.3);
        let mut diverse = DiverseKirraGovernor::new().with_angular_bounds(0.7, 0.3);
        primary.update_rss_state(safe_rss());
        diverse.update_rss_state(safe_rss());
        for (v, w, posture) in [
            (2.0, 0.8, SafetyPosture::Nominal),
            (2.0, 0.4, SafetyPosture::Degraded),
        ] {
            let cmd = twist(v, w);
            let prev = twist(v, 0.0);
            let pa = primary.evaluate(&cmd, Some(&prev), 0.05, posture);
            let da = diverse.evaluate(&cmd, Some(&prev), 0.05, posture);
            assert!(!actions_diverge(&pa, &da, &cmd, TOL), "{pa:?} vs {da:?}");
        }
    }

    // -- Property-based broad agreement ----------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// CORRECTNESS / no-false-divergence: across a broad bounded input
        /// space and every posture + RSS state, primary and diverse must
        /// never diverge. A regression here means the diverse governor would
        /// trip the comparator on a VALID command.
        #[test]
        fn proptest_diverse_agrees_with_primary(
            v in -60.0_f64..60.0,
            w in -12.0_f64..12.0,
            c in -60.0_f64..60.0,
            dt in 0.005_f64..0.5,
            posture_idx in 0usize..3,
            rss_safe in proptest::bool::ANY,
            has_prev in proptest::bool::ANY,
        ) {
            let posture = [
                SafetyPosture::Nominal,
                SafetyPosture::Degraded,
                SafetyPosture::LockedOut,
            ][posture_idx];
            let rss = if rss_safe { safe_rss() } else { unsafe_rss() };

            let mut primary = KirraGovernor::new();
            let mut diverse = DiverseKirraGovernor::new();
            primary.update_rss_state(rss.clone());
            diverse.update_rss_state(rss);

            let cmd = twist(v, w);
            let prev = twist(c, 0.0);
            let previous = if has_prev { Some(&prev) } else { None };

            let p = primary.evaluate(&cmd, previous, dt, posture);
            let d = diverse.evaluate(&cmd, previous, dt, posture);
            prop_assert!(
                !actions_diverge(&p, &d, &cmd, TOL),
                "divergence: posture={:?} v={} w={} c={} dt={} prev={}\n  primary={:?}\n  diverse={:?}",
                posture, v, w, c, dt, has_prev, p, d,
            );
        }
    }
}
