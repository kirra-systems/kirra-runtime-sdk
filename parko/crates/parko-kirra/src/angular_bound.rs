// parko/crates/parko-kirra/src/angular_bound.rs
//
// SOTIF-derived angular-velocity bound for the parko-kirra path
// (issue #136). Replaces the H1 placeholder constants
// (`MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER = 1.5`,
//  `MRC_ANGULAR_VELOCITY_CEILING_RAD_S = 0.5`) with a derived
// `ω_max(v) = min(rollover(v), sweep, ftti)` computed from per-platform
// geometry + safety budgets.
//
// **Status:** DRAFT — pending formal safety-engineer review. This is
// engineering analysis with explicit reasoning; it is NOT yet a
// validated safety claim. See `docs/safety/ANGULAR_VELOCITY_SOTIF.md`
// for the full derivation, assumptions, citations, and the worked
// reference example.
//
// **The three constraints** (each is a hard upper bound on |ω|):
//
//   (a) DYNAMIC ROLLOVER  — `a_lat = v · |ω| ≤ g · t / (2·h)`
//                        → ω_rollover(v) = g·t / (2·h·v)  for v > 0
//       Tip-over threshold from the static stability factor of a rigid
//       body with track width `t` and centre-of-gravity height `h`,
//       assuming the CoG is centred above the wheelbase. Singular at
//       v=0; does not bind in-place rotation.
//
//   (b) SWEEP / CONTACT   — `r_extent · |ω| ≤ v_edge_safe`
//                        → ω_sweep = v_edge_safe / r_extent           (constant)
//       Bound the tangential velocity of the robot's outermost point
//       by a safe contact velocity. Basis: ISO/TS 15066:2016
//       power-and-force-limiting contact-velocity envelopes for
//       collaborative robots (the conservative end of the range; see
//       the doc for the per-body-region table).
//
//   (c) PERCEPTION / FTTI — `|ω| · τ_FTTI ≤ θ_max`
//                        → ω_ftti = θ_max / τ_FTTI                    (constant)
//       Bound the heading change within one fault-tolerant time
//       interval so the perception/policy pipeline never reasons about
//       a state outside the planning horizon.
//
// **ω_max(v) = min(ω_rollover(v), ω_sweep, ω_ftti)** with v=0
// special-cased — rollover does not bind, sweep+ftti take over.
//
// MRC posture derates v_edge_safe + θ_max by `mrc_posture_factor`; the
// rollover constraint is unchanged (vehicle geometry doesn't shrink in
// degraded posture).

/// Per-platform physical + safety parameters needed for the SOTIF
/// angular-velocity derivation. Integrators construct one of these per
/// vehicle and hand it to `AngularVelocityBound::from_platform`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlatformParams {
    /// Track width, m. Distance between left and right wheel contact
    /// points; used in the static stability factor `t / (2·h)`.
    pub track_width_m: f64,
    /// Centre-of-gravity height above the ground, m. With `track_width_m`
    /// determines the rollover threshold. Assumes a centred CoG —
    /// state the assumption explicitly when characterising a real
    /// platform (off-centre CoGs require a directional rollover bound,
    /// out of scope for M1).
    pub cog_height_m: f64,
    /// Robot extent, m. Radius of the bounding circle around the
    /// platform's footprint (including any cantilevered payload).
    /// Used in the sweep constraint `r_extent · ω ≤ v_edge_safe`.
    pub robot_extent_m: f64,
    /// Safe contact velocity, m/s. Basis: ISO/TS 15066:2016 §5.5.5
    /// power-and-force-limiting contact-velocity envelope. The
    /// conservative end of the per-body-region table covers
    /// upper-arm / chest contact; use a tighter value for known head
    /// exposure. Default + rationale in `conservative_default`.
    pub v_edge_safe_mps: f64,
    /// Maximum safe heading change per FTTI, rad. Bound on how far
    /// the platform may rotate inside one fault-tolerant time
    /// interval before perception / policy reasoning extrapolates
    /// beyond its validity envelope. Conservative default = 5° ≈ 0.087 rad.
    pub theta_max_rad: f64,
    /// Fault-tolerant time interval, s. The safety case's worst-case
    /// time-to-safe-state for the angular axis. For the parko path
    /// this matches the inference-loop tick budget (0.1 s @ 10 Hz).
    pub ftti_s: f64,
    /// MRC posture derate factor. Multiplied into `v_edge_safe_mps`
    /// and `theta_max_rad` for the Degraded posture so the contact
    /// + heading-change budgets shrink. Range (0, 1]. Default 0.5
    /// (half the contact velocity, half the heading budget).
    pub mrc_posture_factor: f64,
    /// **Rollover safety factor** `k_roll ∈ (0, 1]` (issue #136, ADR-0029
    /// §3.2 correction). The rigid-body tip threshold `a_tip = g·t/(2·h)`
    /// is an UPPER bound on the true tip-over threshold — a compliant
    /// platform (suspension travel, tyre/payload compliance) tips at a
    /// LOWER lateral acceleration, so enforcing up to the rigid `a_tip`
    /// is optimistic. `k_roll` scales the rollover term down to a
    /// defensible fraction of the rigid threshold (a NHTSA-style dynamic
    /// correction to the static stability factor):
    /// `ω_rollover(v) = k_roll · g·t/(2·h·v)`. Default `0.6` — a
    /// pre-validation starting point; calibrate per platform from the
    /// tilt-table test in `ANGULAR_VELOCITY_SOTIF.md` §8. `1.0` recovers
    /// the rigid (uncorrected) bound.
    pub k_roll: f64,
}

/// **Below this linear velocity, the rollover constraint is treated
/// as non-binding** to avoid the v→0 singularity. At very low v the
/// lateral acceleration `v·ω` is dominated by ω, not v, and the
/// platform behaviour approaches in-place rotation — where rollover
/// physics does not apply (no centripetal force is built up before
/// the rotation reverses sign in a typical fast control loop).
/// 0.05 m/s is a slow walking-pace floor; below it sweep + FTTI bind
/// alone. See the SOTIF doc §3.4 for the rationale.
pub const ROLLOVER_MIN_LINEAR_VELOCITY_MPS: f64 = 0.05;

/// Standard gravity, m/s².
const GRAVITY_MPS2: f64 = 9.81;

impl PlatformParams {
    /// **Conservative default for an uncharacterized platform.**
    /// Chosen so a misconfigured / unprofiled deployment fails toward
    /// safe: small track (low rollover threshold), high CoG, large
    /// extent (low sweep ω), tight FTTI θ_max.
    ///
    /// At every (v, geometry) combination, this default produces a
    /// tighter bound than the reference urban-service-robot platform.
    ///
    /// Numbers chosen for the conservative default:
    ///   - track_width_m  = 0.2  (small mobile base)
    ///   - cog_height_m   = 0.5  (top-heavy assumption)
    ///   - robot_extent_m = 0.5  (large bounding-circle assumption)
    ///   - v_edge_safe    = 0.10 m/s  (ISO/TS 15066 conservative end
    ///                                 of the contact-velocity table
    ///                                 covering vulnerable body regions)
    ///   - theta_max      = 0.05 rad ≈ 2.9°  per FTTI
    ///   - ftti_s         = 0.10 s
    ///   - mrc_posture_factor = 0.5
    ///
    /// Produces ω_max(0) ≈ 0.20 rad/s ≈ 11.5°/s — slow, deliberate.
    #[must_use]
    pub fn conservative_default() -> Self {
        Self {
            track_width_m:   0.20,
            cog_height_m:    0.50,
            robot_extent_m:  0.50,
            v_edge_safe_mps: 0.10,
            theta_max_rad:   0.05,
            ftti_s:          0.10,
            mrc_posture_factor: 0.5,
            k_roll:          0.6,  // rollover dynamic-correction starting point (§3.2)
        }
    }

    /// **Reference urban-service-robot platform** — a small mobile
    /// base of approximately TurtleBot-4 scale. Numbers used in the
    /// SOTIF doc's worked example:
    ///
    ///   - track_width_m  = 0.50
    ///   - cog_height_m   = 0.40
    ///   - robot_extent_m = 0.30
    ///   - v_edge_safe    = 0.25 m/s  (ISO/TS 15066 upper-arm/chest)
    ///   - theta_max      = 0.087 rad ≈ 5°
    ///   - ftti_s         = 0.10 s
    ///
    /// Produces ω_max(0) ≈ 0.833 rad/s ≈ 47.7°/s (sweep binds).
    /// See `docs/safety/ANGULAR_VELOCITY_SOTIF.md` §4 for the worked
    /// numbers across the v ∈ [0, 5] m/s range.
    #[must_use]
    pub fn urban_service_robot_reference() -> Self {
        Self {
            track_width_m:   0.50,
            cog_height_m:    0.40,
            robot_extent_m:  0.30,
            v_edge_safe_mps: 0.25,
            theta_max_rad:   0.087, // 5°
            ftti_s:          0.10,
            mrc_posture_factor: 0.5,
            k_roll:          0.6,  // rollover dynamic-correction starting point (§3.2)
        }
    }

    /// Validate the params. Returns `Err(reason)` for parameters that
    /// would produce nonsensical or unsafe bounds (non-positive
    /// dimensions, mrc_posture_factor outside (0, 1], etc.).
    /// Construction does NOT validate by default; callers can opt in
    /// when reading config from a file.
    pub fn validate(&self) -> Result<(), String> {
        if self.track_width_m   <= 0.0 { return Err("track_width_m must be > 0".into()); }
        if self.cog_height_m    <= 0.0 { return Err("cog_height_m must be > 0".into()); }
        if self.robot_extent_m  <= 0.0 { return Err("robot_extent_m must be > 0".into()); }
        if self.v_edge_safe_mps <= 0.0 { return Err("v_edge_safe_mps must be > 0".into()); }
        if self.theta_max_rad   <= 0.0 { return Err("theta_max_rad must be > 0".into()); }
        if self.ftti_s          <= 0.0 { return Err("ftti_s must be > 0".into()); }
        if !(0.0 < self.mrc_posture_factor && self.mrc_posture_factor <= 1.0) {
            return Err("mrc_posture_factor must be in (0, 1]".into());
        }
        if !(0.0 < self.k_roll && self.k_roll <= 1.0) {
            return Err("k_roll must be in (0, 1]".into());
        }
        Ok(())
    }
}

/// Angular-velocity bound — produces `ω_max(v)` on demand.
///
/// Two variants:
///   - `Scalar(f64)` — direct v-independent override. Used by
///     `KirraGovernor::with_angular_bounds(nominal_rad_s, mrc_rad_s)`
///     for callers that already know the bound and don't need the
///     derivation.
///   - `Derived { params, posture_factor }` — computes
///     `ω_max(v) = min(rollover(v), sweep, ftti)` from platform
///     parameters. `posture_factor = 1.0` for the Nominal bound;
///     `params.mrc_posture_factor` (< 1.0) for the MRC bound.
#[derive(Debug, Clone, PartialEq)]
pub enum AngularVelocityBound {
    /// Direct scalar override (back-compat with `with_angular_bounds`).
    Scalar(f64),
    /// SOTIF-derived bound from platform params.
    Derived {
        params: PlatformParams,
        /// 1.0 for Nominal, `params.mrc_posture_factor` for MRC.
        posture_factor: f64,
    },
}

impl AngularVelocityBound {
    /// Construct the Nominal-posture bound from platform params
    /// (posture_factor = 1.0).
    #[must_use]
    pub fn nominal(params: PlatformParams) -> Self {
        Self::Derived { params, posture_factor: 1.0 }
    }

    /// Construct the MRC bound from platform params (posture_factor =
    /// `params.mrc_posture_factor`, typically 0.5).
    #[must_use]
    pub fn mrc(params: PlatformParams) -> Self {
        let pf = params.mrc_posture_factor;
        Self::Derived { params, posture_factor: pf }
    }

    /// Compute `ω_max(v)` for a proposed linear velocity. Always
    /// non-negative and finite. The bound is on `|ω|`; the caller's
    /// enforcement compares `|proposed.angular_velocity|` to this.
    ///
    // SAFETY: SG8 SG9 | REQ: angular-velocity-bound-sotif | TEST: omega_max_rollover_binds_at_high_v,omega_max_sweep_binds_at_low_v,omega_max_in_place_rotation_returns_sweep_or_ftti,omega_max_ftti_binds_when_theta_is_tight,omega_max_at_v_zero_is_finite,omega_max_at_v_below_rollover_floor_ignores_rollover,omega_max_conservative_default_is_at_or_below_reference_platform,omega_max_mrc_is_tighter_than_nominal
    pub fn omega_max(&self, linear_velocity_mps: f64) -> f64 {
        match self {
            Self::Scalar(c) => *c,
            Self::Derived { params, posture_factor } => {
                let v = linear_velocity_mps.abs();
                // (a) Rollover — only when |v| is above the floor.
                let omega_rollover = if v >= ROLLOVER_MIN_LINEAR_VELOCITY_MPS {
                    // ω_rollover(v) = k_roll · g · t / (2 · h · v).
                    // k_roll (≤ 1) is the §3.2 dynamic-correction factor: the rigid
                    // a_tip = g·t/(2h) is OPTIMISTIC for a compliant platform, so we
                    // enforce a defensible fraction of it.
                    params.k_roll * GRAVITY_MPS2 * params.track_width_m
                        / (2.0 * params.cog_height_m * v)
                } else {
                    f64::INFINITY
                };
                // (b) Sweep — derated by posture_factor on v_edge_safe.
                let v_edge_eff = params.v_edge_safe_mps * posture_factor;
                let omega_sweep = v_edge_eff / params.robot_extent_m;
                // (c) FTTI — derated by posture_factor on θ_max.
                let theta_eff = params.theta_max_rad * posture_factor;
                let omega_ftti = theta_eff / params.ftti_s;

                omega_rollover.min(omega_sweep).min(omega_ftti)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (pure arithmetic; no governor / no posture machinery)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_platform() -> PlatformParams { PlatformParams::urban_service_robot_reference() }

    #[test]
    fn platform_params_conservative_default_validates() {
        let p = PlatformParams::conservative_default();
        assert!(p.validate().is_ok());
    }

    #[test]
    fn platform_params_reference_validates() {
        let p = PlatformParams::urban_service_robot_reference();
        assert!(p.validate().is_ok());
    }

    #[test]
    fn platform_params_validate_rejects_non_positive_geometry() {
        let mut p = ref_platform(); p.track_width_m = 0.0;
        assert!(p.validate().is_err());
    }

    #[test]
    fn platform_params_validate_rejects_out_of_range_mrc_factor() {
        let mut p = ref_platform(); p.mrc_posture_factor = 1.5;
        assert!(p.validate().is_err());
        let mut p = ref_platform(); p.mrc_posture_factor = 0.0;
        assert!(p.validate().is_err());
    }

    /// **Worked reference numbers (SOTIF doc §4):** at v=0 the rollover
    /// term is masked, sweep = 0.25/0.30 = 0.833 rad/s, FTTI =
    /// 0.087/0.10 = 0.87 rad/s. Sweep binds at 0.833.
    #[test]
    fn omega_max_in_place_rotation_returns_sweep_or_ftti() {
        let bound = AngularVelocityBound::nominal(ref_platform());
        let omega = bound.omega_max(0.0);
        // Should be the min(sweep, ftti) = min(0.833, 0.87) = 0.833.
        assert!((omega - 0.833_f64).abs() < 1e-2,
            "in-place rotation must use sweep/FTTI (not rollover); got {omega}");
        // And it must be FINITE — the v=0 singularity must not leak.
        assert!(omega.is_finite());
    }

    #[test]
    fn omega_max_at_v_zero_is_finite() {
        // Pin the no-singularity property by name so a future change
        // to the rollover formula can't accidentally produce NaN/Inf
        // at v=0.
        let bound = AngularVelocityBound::nominal(ref_platform());
        let omega = bound.omega_max(0.0);
        assert!(omega.is_finite(), "ω_max(0) must be finite, got {omega}");
        assert!(omega > 0.0, "ω_max(0) must be positive");
    }

    #[test]
    fn omega_max_at_v_below_rollover_floor_ignores_rollover() {
        // 0.04 m/s is below `ROLLOVER_MIN_LINEAR_VELOCITY_MPS = 0.05`.
        // At this v the rollover formula would give a HUGE bound
        // (>153 rad/s for the reference platform), but the implementation
        // masks rollover below the floor so sweep + FTTI bind.
        let bound = AngularVelocityBound::nominal(ref_platform());
        let omega = bound.omega_max(0.04);
        // Should equal the v=0 result exactly — both fall in the
        // rollover-masked regime.
        assert!((omega - bound.omega_max(0.0)).abs() < 1e-9);
        // And explicit min(sweep, ftti):
        let sweep = ref_platform().v_edge_safe_mps / ref_platform().robot_extent_m;
        let ftti  = ref_platform().theta_max_rad / ref_platform().ftti_s;
        assert!((omega - sweep.min(ftti)).abs() < 1e-9);
    }

    #[test]
    fn omega_max_sweep_binds_at_low_v() {
        // At v = 1 m/s: rollover = 9.81·0.5/(2·0.4·1) ≈ 6.13 rad/s.
        // Sweep = 0.25/0.30 ≈ 0.833. FTTI = 0.87. min = 0.833 (sweep).
        let bound = AngularVelocityBound::nominal(ref_platform());
        let omega = bound.omega_max(1.0);
        let sweep = ref_platform().v_edge_safe_mps / ref_platform().robot_extent_m;
        assert!((omega - sweep).abs() < 1e-9,
            "at v=1 m/s sweep must bind; expected {sweep}, got {omega}");
    }

    #[test]
    fn omega_max_rollover_binds_at_high_v() {
        // Construct a platform with EASY sweep (large v_edge_safe,
        // small extent) and large θ_max so rollover is the only
        // constraint that bites at high v.
        let p = PlatformParams {
            v_edge_safe_mps: 10.0,    // huge — sweep won't bind
            robot_extent_m:  0.1,
            theta_max_rad:   1.0,     // huge — FTTI won't bind
            ftti_s:          0.1,
            ..ref_platform()
        };
        let bound = AngularVelocityBound::nominal(p.clone());
        // At v = 10 m/s: rollover = k_roll·9.81·0.5/(2·0.4·10) = 0.6·0.613 ≈ 0.368 rad/s.
        let omega = bound.omega_max(10.0);
        let rollover = p.k_roll * GRAVITY_MPS2 * p.track_width_m
            / (2.0 * p.cog_height_m * 10.0);
        assert!((omega - rollover).abs() < 1e-9,
            "at v=10 m/s rollover must bind (k_roll-corrected); expected {rollover}, got {omega}");
    }

    #[test]
    fn k_roll_scales_the_rollover_term_only() {
        // In a rollover-binding regime, halving-ish k_roll scales ω_max
        // proportionally; k_roll = 1.0 recovers the rigid (uncorrected) bound.
        // Easy sweep/ftti so rollover is the sole binding constraint at high v.
        let base = PlatformParams {
            v_edge_safe_mps: 10.0, robot_extent_m: 0.1, theta_max_rad: 1.0, ftti_s: 0.1,
            ..ref_platform()
        };
        let rigid = AngularVelocityBound::nominal(PlatformParams { k_roll: 1.0, ..base.clone() });
        let corrected = AngularVelocityBound::nominal(PlatformParams { k_roll: 0.6, ..base.clone() });
        let (r, c) = (rigid.omega_max(10.0), corrected.omega_max(10.0));
        assert!((c - 0.6 * r).abs() < 1e-9,
            "k_roll must scale the rollover bound: expected {} got {c}", 0.6 * r);
        assert!(c < r, "the corrected bound must be tighter than the rigid one");

        // k_roll must NOT touch the sweep/ftti regime (v=0, rollover masked).
        let s_rigid = AngularVelocityBound::nominal(PlatformParams { k_roll: 1.0, ..ref_platform() });
        let s_corr  = AngularVelocityBound::nominal(PlatformParams { k_roll: 0.6, ..ref_platform() });
        assert_eq!(s_rigid.omega_max(0.0), s_corr.omega_max(0.0),
            "k_roll must not change the in-place (sweep-bound) regime");
    }

    #[test]
    fn platform_params_validate_rejects_out_of_range_k_roll() {
        let mut p = ref_platform(); p.k_roll = 0.0;
        assert!(p.validate().is_err());
        let mut p = ref_platform(); p.k_roll = 1.5;
        assert!(p.validate().is_err());
    }

    #[test]
    fn omega_max_ftti_binds_when_theta_is_tight() {
        // Construct a platform where FTTI is the tightest of the three.
        let p = PlatformParams {
            v_edge_safe_mps: 10.0,    // huge sweep → ω_sweep ≈ 33
            robot_extent_m:  0.3,
            theta_max_rad:   0.02,    // 1.15° — very tight FTTI
            ftti_s:          0.10,    // ω_ftti = 0.2
            ..ref_platform()
        };
        let bound = AngularVelocityBound::nominal(p);
        let omega = bound.omega_max(1.0);
        assert!((omega - 0.2).abs() < 1e-9,
            "FTTI must bind when θ_max/τ is tightest; expected 0.2, got {omega}");
    }

    #[test]
    fn omega_max_conservative_default_is_at_or_below_reference_platform() {
        // Property the conservative default must satisfy: at every v
        // in a plausible operating range, the bound is ≤ the
        // reference platform's bound. This ensures an uncharacterised
        // platform configured with the default fails toward safe.
        let cons = AngularVelocityBound::nominal(PlatformParams::conservative_default());
        let refp = AngularVelocityBound::nominal(ref_platform());
        for v in [0.0, 0.1, 0.5, 1.0, 2.0, 3.0, 5.0] {
            let cons_o = cons.omega_max(v);
            let ref_o  = refp.omega_max(v);
            assert!(cons_o <= ref_o + 1e-9,
                "conservative_default at v={v} produced ω={cons_o}, must be ≤ reference {ref_o} \
                 — the default must fail toward safe for an uncharacterised platform");
        }
    }

    #[test]
    fn omega_max_mrc_is_tighter_than_nominal() {
        // MRC posture must always produce a tighter bound than Nominal
        // (the envelope contracts in Degraded). Rollover doesn't
        // shrink (geometry is the same), but sweep + FTTI both
        // shrink via the posture factor — so for any platform where
        // sweep or FTTI binds, MRC < Nominal.
        let nom = AngularVelocityBound::nominal(ref_platform());
        let mrc = AngularVelocityBound::mrc(ref_platform());
        for v in [0.0, 0.5, 1.0, 2.0] {
            let n = nom.omega_max(v);
            let m = mrc.omega_max(v);
            assert!(m < n + 1e-9,
                "MRC must be ≤ Nominal at v={v}: nominal={n}, mrc={m}");
        }
    }

    #[test]
    fn omega_max_scalar_variant_is_v_independent() {
        // The scalar override (back-compat with `with_angular_bounds`)
        // ignores v.
        let bound = AngularVelocityBound::Scalar(0.5);
        for v in [0.0, 1.0, 10.0, 1000.0] {
            assert_eq!(bound.omega_max(v), 0.5);
        }
    }

    #[test]
    fn omega_max_mrc_at_v_zero_for_reference_platform() {
        // Worked numbers from SOTIF doc §4.3 — MRC at v=0:
        //   sweep_eff = 0.25 * 0.5 / 0.30 = 0.4167 rad/s
        //   ftti_eff  = 0.087 * 0.5 / 0.10 = 0.435 rad/s
        //   min       = 0.4167 (sweep)
        let mrc = AngularVelocityBound::mrc(ref_platform());
        let omega = mrc.omega_max(0.0);
        assert!((omega - 0.4167_f64).abs() < 1e-3,
            "MRC at v=0: expected ~0.4167 rad/s (sweep with halved v_edge_safe), got {omega}");
    }

    // -----------------------------------------------------------------------
    // PROPERTY TESTS — invariants of ω_max(v) = min(rollover(v), sweep, ftti),
    // derived from docs/safety/ANGULAR_VELOCITY_SOTIF.md §2-§3 and the
    // rollover physics a_lat = v·|ω| ≤ g·t/(2·h). These assert the SAFETY
    // SHAPE of the bound, not specific numbers; a good property kills a class
    // of mutants (sign flips, min→max, dropped terms).
    // -----------------------------------------------------------------------

    use proptest::prelude::*;

    /// Strategy: physically-plausible platform params (all geometry > 0,
    /// posture factor in (0, 1]). Mirrors `PlatformParams::validate`'s domain.
    fn plausible_params() -> impl Strategy<Value = PlatformParams> {
        (
            0.05_f64..2.0,   // track_width_m
            0.05_f64..2.0,   // cog_height_m
            0.05_f64..2.0,   // robot_extent_m
            0.01_f64..2.0,   // v_edge_safe_mps
            0.005_f64..1.5,  // theta_max_rad
            0.01_f64..1.0,   // ftti_s
            0.05_f64..1.0,   // mrc_posture_factor (in (0,1])
            0.05_f64..1.0,   // k_roll (in (0,1])
        )
            .prop_map(|(t, h, r, ve, th, ftti, pf, kr)| PlatformParams {
                track_width_m: t,
                cog_height_m: h,
                robot_extent_m: r,
                v_edge_safe_mps: ve,
                theta_max_rad: th,
                ftti_s: ftti,
                mrc_posture_factor: pf,
                k_roll: kr,
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// INVARIANT: ω_max(v) is always finite and ≥ 0 (it is a bound on a
        /// non-negative magnitude |ω|). Source: SOTIF §2 — every term is a
        /// ratio of positive quantities; the v→0 singularity is masked.
        #[test]
        fn prop_omega_max_is_finite_and_nonnegative(
            p in plausible_params(),
            v in -50.0_f64..50.0,
        ) {
            let nom = AngularVelocityBound::nominal(p.clone());
            let mrc = AngularVelocityBound::mrc(p);
            for w in [nom.omega_max(v), mrc.omega_max(v)] {
                prop_assert!(w.is_finite(), "ω_max must be finite, got {w}");
                prop_assert!(w >= 0.0, "ω_max must be ≥ 0, got {w}");
            }
        }

        /// INVARIANT: ω_max(v) ≤ the v-independent ceiling min(sweep, ftti) for
        /// EVERY v — the rollover term can only tighten the bound, never loosen
        /// it. Source: SOTIF §2 — ω_max = min(rollover, sweep, ftti) ≤ sweep,
        /// ≤ ftti. This is the "≤ configured Nominal cap" invariant.
        #[test]
        fn prop_omega_max_never_exceeds_sweep_ftti_ceiling(
            p in plausible_params(),
            v in -50.0_f64..50.0,
        ) {
            let sweep = p.v_edge_safe_mps / p.robot_extent_m;
            let ftti  = p.theta_max_rad / p.ftti_s;
            let ceiling = sweep.min(ftti);
            let w = AngularVelocityBound::nominal(p).omega_max(v);
            prop_assert!(w <= ceiling + 1e-9,
                "ω_max({v})={w} exceeded the sweep/ftti ceiling {ceiling}");
        }

        /// INVARIANT: ω_max is NON-INCREASING in |v| — faster forward speed can
        /// only tighten the rollover constraint (ω_rollover(v) = g·t/(2·h·v) is
        /// strictly decreasing in v), never loosen it. Source: rollover physics
        /// a_lat = v·|ω|; SOTIF §2(a). Kills a min→max / sign-flip mutant.
        #[test]
        fn prop_omega_max_is_non_increasing_in_speed(
            p in plausible_params(),
            v1 in 0.0_f64..50.0,
            dv in 0.0_f64..50.0,
        ) {
            let bound = AngularVelocityBound::nominal(p);
            let v2 = v1 + dv; // v2 >= v1
            prop_assert!(
                bound.omega_max(v1) + 1e-9 >= bound.omega_max(v2),
                "ω_max must not increase with speed: ω({v1})={} < ω({v2})={}",
                bound.omega_max(v1), bound.omega_max(v2)
            );
        }

        /// INVARIANT: below the rollover floor (|v| < ROLLOVER_MIN_LINEAR_
        /// VELOCITY_MPS = 0.05) the rollover term is masked and ω_max equals
        /// the constant min(sweep, ftti) — finite, no divide-by-zero. Source:
        /// SOTIF §3.4 (v→0 singularity handling).
        #[test]
        fn prop_below_rollover_floor_returns_constant_ceiling(
            p in plausible_params(),
            v in 0.0_f64..ROLLOVER_MIN_LINEAR_VELOCITY_MPS,
        ) {
            let sweep = p.v_edge_safe_mps / p.robot_extent_m;
            let ftti  = p.theta_max_rad / p.ftti_s;
            let ceiling = sweep.min(ftti);
            let w = AngularVelocityBound::nominal(p).omega_max(v);
            prop_assert!(w.is_finite());
            prop_assert!((w - ceiling).abs() < 1e-9,
                "below the rollover floor ω_max must equal the constant ceiling {ceiling}, got {w}");
        }

        /// INVARIANT: the MRC bound is never looser than the Nominal bound at
        /// the same v (posture_factor ≤ 1 derates sweep + ftti; rollover is
        /// unchanged). Source: SOTIF §2 (MRC derate). The envelope contracts
        /// in Degraded — it must never expand.
        #[test]
        fn prop_mrc_bound_is_at_or_below_nominal(
            p in plausible_params(),
            v in 0.0_f64..50.0,
        ) {
            let nom = AngularVelocityBound::nominal(p.clone());
            let mrc = AngularVelocityBound::mrc(p);
            prop_assert!(mrc.omega_max(v) <= nom.omega_max(v) + 1e-9,
                "MRC bound must be ≤ Nominal at v={v}");
        }

        /// INVARIANT: the conservative default is always tighter than the old
        /// H1 placeholder (1.5 rad/s nominal) it replaced — an uncharacterised
        /// platform fails toward safe. Source: lib.rs #136 note + SOTIF §3.1.
        #[test]
        fn prop_conservative_default_is_below_old_placeholder(
            v in -50.0_f64..50.0,
        ) {
            const OLD_PLACEHOLDER_RAD_S: f64 = 1.5;
            let w = AngularVelocityBound::nominal(PlatformParams::conservative_default())
                .omega_max(v);
            prop_assert!(w <= OLD_PLACEHOLDER_RAD_S,
                "conservative default ω_max({v})={w} must be ≤ old placeholder 1.5");
        }
    }
}
