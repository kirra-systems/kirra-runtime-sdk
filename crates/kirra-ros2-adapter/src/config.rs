// crates/kirra-ros2-adapter/src/config.rs
//
// VehicleConfig — the integrator-supplied vehicle profile the adapter
// hands to the kernel's per-pose kinematics check + the slow-loop
// containment check + the RSS pipeline.
//
// Phase 2A scope: a single struct + `default_urban()` constructor + the
// conversions to the kernel-side types. Phase 4 may grow per-asset
// config and a deserializer.

use kirra_core::containment::VehicleFootprint;
use kirra_core::kinematics_contract::{
    VehicleKinematicsContract, URBAN_ODD_SPEED_CAP_MPS,
};

/// Integrator-supplied vehicle profile. Holds the platform geometry +
/// dynamic limits the validator needs. All units SI.
///
/// Field selection follows the brief; the conversions to
/// `VehicleKinematicsContract` (per-pose check) and `VehicleFootprint`
/// (containment) round-trip without loss.
#[derive(Debug, Clone, Copy)]
pub struct VehicleConfig {
    /// Distance between front and rear axle, m. Used by the bicycle
    /// model in `validate_vehicle_command` (P6 lateral-accel) and by
    /// the steering-from-curvature derivation in the slow-loop
    /// per-pose mapping.
    pub wheelbase_m: f64,
    /// Distance between left and right wheel centres, m. Stored for
    /// future use; not consumed by Phase 2A.
    pub track_width_m: f64,
    /// Half of bumper-to-bumper length, m. The footprint conversion
    /// derives `length_m` (full length) from `2 * half_length_m`,
    /// then splits into front / rear overhang via wheelbase.
    pub half_length_m: f64,
    /// Half of bumper-to-bumper width, m. Footprint `width_m = 2 * half_width_m`.
    pub half_width_m: f64,

    /// Max forward speed, m/s. **Vehicle physical capability** — the
    /// mechanical / drivetrain ceiling. Distinct from
    /// `odd_speed_cap_mps`, which is the safety-case operational ODD
    /// ceiling. The enforced max is `min(max_speed_mps, odd_speed_cap_mps)`.
    pub max_speed_mps: f64,
    /// Max acceleration, m/s².
    pub max_accel_mps2: f64,
    /// Max deceleration (service brake), m/s². The kernel-side field is
    /// `max_brake_mps2`; the conversion maps `max_decel_mps2 →
    /// max_brake_mps2`.
    pub max_decel_mps2: f64,
    /// Max absolute steering angle, RAD. The kernel stores degrees; the
    /// conversion converts.
    pub max_steering_rad: f64,
    /// **ODD operational speed cap** (m/s). This is the safety-case
    /// ceiling derived from the deployment ODD (e.g.
    /// `URBAN_ODD_SPEED_CAP_MPS` = 22.35 m/s per ADR-0001), **not** the
    /// vehicle physical max. The kinematics pipeline enforces
    /// `min(max_speed_mps, odd_speed_cap_mps)`.
    ///
    /// `None` is permitted but emits a startup warning via
    /// [`VehicleConfig::warn_if_missing_odd_cap`] — a deployment that
    /// drops the cap by accident is loud, not silent.
    pub odd_speed_cap_mps: Option<f64>,

    /// **RSS lateral-alignment band** (m): the lateral offset below which an object is
    /// "in my lane" and so subject to RSS longitudinal evaluation; beyond it, containment
    /// covers it. This is **per-class** — a lane-width-scale number for a robotaxi (4.0 m),
    /// but a much tighter band for a small robot (a sidewalk courier's "lane" is ~1 m wide,
    /// not ~4 m). Making it a config field instead of a global constant is what lets a small
    /// robot pass an obstacle a robotaxi could not, WITHOUT changing the robotaxi number
    /// (see `docs/CONTRACT_PROFILES.md`, the sibling rule).
    pub rss_lateral_alignment_tolerance_m: f64,
}

/// Robotaxi-class RSS lateral band (m) — the frozen reference value (was the module
/// constant `RSS_LATERAL_ALIGNMENT_TOLERANCE_M` in `validation.rs`). `default_urban` uses
/// this verbatim, so the robotaxi/AV path is byte-identical.
pub const DEFAULT_RSS_LATERAL_ALIGNMENT_TOLERANCE_M: f64 = 4.0;

impl VehicleConfig {
    /// Defaults for an urban mid-size AV. Matches the kernel's
    /// `nominal_reference_profile()` for the fields that overlap (wheelbase
    /// 2.8 m, max_speed 35 m/s, max_accel 2.5 m/s², max_brake 4.5 m/s²,
    /// 1.85 × 4.8 m footprint).
    ///
    /// `odd_speed_cap_mps` defaults to `URBAN_ODD_SPEED_CAP_MPS` (22.35 m/s,
    /// 50 mph) — the urban Occy ODD cap per ADR-0001 / SPEED_ENVELOPE.md /
    /// S8 Item C (KIRRA-OCCY-SPEED-VAL-001).
    pub fn default_urban() -> Self {
        Self {
            wheelbase_m:        2.8,
            track_width_m:      1.55,
            half_length_m:      2.4,    // → length 4.8 m
            half_width_m:       0.925,  // → width  1.85 m
            max_speed_mps:      35.0,
            max_accel_mps2:     2.5,
            max_decel_mps2:     4.5,
            // 35° steering rack on a 2.8 m wheelbase ≈ 0.6109 rad.
            max_steering_rad:   35.0_f64.to_radians(),
            odd_speed_cap_mps:  Some(URBAN_ODD_SPEED_CAP_MPS),
            rss_lateral_alignment_tolerance_m: DEFAULT_RSS_LATERAL_ALIGNMENT_TOLERANCE_M,
        }
    }

    /// **Courier / small-robot class** (a sibling of [`default_urban`], per
    /// `docs/CONTRACT_PROFILES.md`). Robot-scale footprint + kinematics + a tight RSS
    /// lateral band so the slow-loop checker judges a sidewalk/indoor robot, not a 4.8 m
    /// car. The checker LOGIC is identical to the robotaxi path — only these numbers differ,
    /// and `default_urban` is untouched, so the AV profile cannot regress.
    ///
    /// Numbers track the `docs/CONTRACT_PROFILES.md` Courier column (footprint 0.6 × 0.9 m,
    /// wheelbase 0.5 m, max 3.0 m/s, ODD cap 2.5 m/s, accel 1.0, brake 3.0, steering 30°) and
    /// are **VALIDATION-PENDING** — placeholders with a stated basis, not certified values.
    /// The RSS band (0.6 m) is the courier "lane" half-scale; tune per chassis.
    pub fn courier() -> Self {
        Self {
            wheelbase_m:        0.5,
            track_width_m:      0.4,
            half_length_m:      0.45,   // → length 0.9 m
            half_width_m:       0.3,    // → width  0.6 m
            max_speed_mps:      3.0,
            max_accel_mps2:     1.0,
            max_decel_mps2:     3.0,
            max_steering_rad:   30.0_f64.to_radians(),
            odd_speed_cap_mps:  Some(2.5),
            rss_lateral_alignment_tolerance_m: 0.6,
        }
    }

    /// Deployment-time check. Logs a WARN if no ODD cap is configured or
    /// if the vehicle physical max sits above the cap by more than its
    /// own value (i.e. the integrator hasn't actually tightened the
    /// ceiling). Returns `true` when a warning was emitted, for testability.
    ///
    /// Call this at adapter node startup so a missing-cap deployment is
    /// loud, not silent.
    ///
    /// SAFETY: SG1 | REQ: odd-speed-cap-startup-warning
    pub fn warn_if_missing_odd_cap(&self) -> bool {
        match self.odd_speed_cap_mps {
            None => {
                tracing::warn!(
                    max_speed_mps = self.max_speed_mps,
                    "VehicleConfig has no ODD operational speed cap; \
                     enforcement falls back to the vehicle physical max \
                     ({} m/s). Integrators deploying into a defined ODD \
                     (e.g. urban 50 mph per ADR-0001) MUST set \
                     odd_speed_cap_mps (URBAN_ODD_SPEED_CAP_MPS = 22.35 m/s).",
                    self.max_speed_mps,
                );
                true
            }
            Some(cap) if cap >= self.max_speed_mps => {
                tracing::warn!(
                    odd_speed_cap_mps = cap,
                    max_speed_mps = self.max_speed_mps,
                    "VehicleConfig ODD speed cap ({}) is not more restrictive than \
                     the vehicle physical max ({}); the ODD ceiling is a no-op.",
                    cap,
                    self.max_speed_mps,
                );
                true
            }
            Some(_) => false,
        }
    }

    /// Builds the kernel-side `VehicleKinematicsContract` from this
    /// config. Used by the per-pose `validate_vehicle_command` calls in
    /// the slow loop.
    ///
    /// Fields not represented in `VehicleConfig` fall back to the
    /// kernel's `nominal_reference_profile()` values (steering rate,
    /// min-follow-distance, max-lateral-accel) — these are dynamic-limit
    /// concerns the integrator's config may override later (Phase 4).
    pub fn to_kinematics_contract(&self) -> VehicleKinematicsContract {
        // Split the full length into front / rear overhang. With the
        // wheelbase fixed at the rear axle, the rear axle is at the
        // origin (Pose convention in containment.rs); the rear overhang
        // is the distance from the rear axle to the rear bumper. We
        // place the rear axle so that the wheelbase fits between the
        // overhangs: length = wheelbase + overhang_front + overhang_rear.
        // Default split: 45% front overhang, 55% rear (matches
        // nominal_reference_profile()).
        let length_m = 2.0 * self.half_length_m;
        let extra = (length_m - self.wheelbase_m).max(0.0);
        let overhang_front_m = extra * 0.45;
        let overhang_rear_m  = extra * 0.55;
        VehicleKinematicsContract {
            max_speed_mps:           self.max_speed_mps,
            max_accel_mps2:          self.max_accel_mps2,
            max_brake_mps2:          self.max_decel_mps2,
            max_steering_deg:        self.max_steering_rad.to_degrees(),
            max_steering_rate_deg_s: 45.0,  // kernel-default; tracked for Phase 4
            min_follow_distance_m:   2.0,
            max_lateral_accel_mps2:  3.5,   // kernel-default; tracked for Phase 4
            wheelbase_m:             self.wheelbase_m,
            width_m:                 2.0 * self.half_width_m,
            length_m,
            overhang_front_m,
            overhang_rear_m,
            // Propagate the ODD operational cap into the kernel contract
            // so `validate_vehicle_command` enforces it.
            odd_speed_cap_mps:       self.odd_speed_cap_mps,
        }
    }

    /// Builds the MRC-derated kinematics contract — same integrator
    /// geometry (wheelbase, footprint), but the dynamic limits
    /// (max_speed, max_accel, max_brake, max_steering, lateral_accel)
    /// are replaced with the kernel's `mrc_fallback_profile()` values.
    ///
    /// Used in `Degraded` posture by the adapter slow loop (M1) to
    /// mirror parko-kirra's posture→profile mapping while preserving
    /// the per-platform footprint required by SG2 containment + the
    /// bicycle-model lateral-accel check.
    ///
    /// The `odd_speed_cap_mps` field survives the derate via the
    /// shallow-copy from `to_kinematics_contract()` (we deliberately
    /// do NOT overwrite it). Effective Degraded ceiling becomes
    /// `min(mrc.max_speed_mps, odd_speed_cap_mps)` — for the urban
    /// default `min(5.0, 22.35) = 5.0`, so the MRC cap binds and the
    /// ODD cap is redundant-but-explicit (no `None`-cap surprise).
    // SAFETY: SG8 | REQ: mrc-derated-contract-shape | TEST: to_mrc_kinematics_contract_keeps_geometry_swaps_dynamic,to_mrc_kinematics_contract_preserves_odd_cap
    pub fn to_mrc_kinematics_contract(&self) -> VehicleKinematicsContract {
        let mut c   = self.to_kinematics_contract();
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        c.max_speed_mps           = mrc.max_speed_mps;
        c.max_accel_mps2          = mrc.max_accel_mps2;
        c.max_brake_mps2          = mrc.max_brake_mps2;
        c.max_steering_deg        = mrc.max_steering_deg;
        c.max_steering_rate_deg_s = mrc.max_steering_rate_deg_s;
        c.min_follow_distance_m   = mrc.min_follow_distance_m;
        c.max_lateral_accel_mps2  = mrc.max_lateral_accel_mps2;
        // `c.odd_speed_cap_mps` intentionally left untouched — it carries
        // the integrator's ODD cap forward (see doc-comment above).
        c
    }

    /// Builds the kernel-side `VehicleFootprint` from this config. The
    /// containment check (`validate_trajectory_containment`) consumes
    /// this directly.
    pub fn to_vehicle_footprint(&self) -> VehicleFootprint {
        VehicleFootprint::from(&self.to_kinematics_contract())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_urban_rss_band_is_the_frozen_robotaxi_value() {
        // The robotaxi/AV path must be byte-identical: the RSS lateral band stays the
        // 4.0 m that was the global constant before it became per-class (#1 / the
        // CONTRACT_PROFILES.md sibling rule — change a number ⇒ change it deliberately).
        assert_eq!(
            VehicleConfig::default_urban().rss_lateral_alignment_tolerance_m,
            DEFAULT_RSS_LATERAL_ALIGNMENT_TOLERANCE_M
        );
        assert_eq!(DEFAULT_RSS_LATERAL_ALIGNMENT_TOLERANCE_M, 4.0);
    }

    #[test]
    fn courier_is_a_smaller_sibling_not_the_robotaxi() {
        // The small-robot profile differs ONLY in numbers (tighter band, smaller
        // footprint, lower speed) — it is a sibling, not a fork of the logic.
        let robot = VehicleConfig::courier();
        let car = VehicleConfig::default_urban();
        assert!(robot.rss_lateral_alignment_tolerance_m < car.rss_lateral_alignment_tolerance_m);
        assert!(robot.half_length_m < car.half_length_m);
        assert!(robot.half_width_m < car.half_width_m);
        assert!(robot.max_speed_mps < car.max_speed_mps);
        assert_eq!(robot.rss_lateral_alignment_tolerance_m, 0.6);
    }

    #[test]
    fn default_urban_matches_kernel_nominal_geometry() {
        let cfg = VehicleConfig::default_urban();
        let kc = cfg.to_kinematics_contract();
        let nominal = VehicleKinematicsContract::nominal_reference_profile();

        // Geometry (the integrator-supplied + derived dimensions) must
        // line up with the kernel's reference profile.
        assert_eq!(kc.wheelbase_m, nominal.wheelbase_m);
        assert!((kc.width_m  - nominal.width_m ).abs() < 1e-9);
        assert!((kc.length_m - nominal.length_m).abs() < 1e-9);
        assert_eq!(kc.max_speed_mps, nominal.max_speed_mps);
        assert_eq!(kc.max_accel_mps2, nominal.max_accel_mps2);
        assert_eq!(kc.max_brake_mps2, nominal.max_brake_mps2);
    }

    #[test]
    fn footprint_roundtrip_through_kinematics_contract() {
        let cfg = VehicleConfig::default_urban();
        let fp = cfg.to_vehicle_footprint();
        assert!((fp.width_m  - 1.85).abs() < 1e-9);
        assert!((fp.length_m - 4.8 ).abs() < 1e-9);
        assert_eq!(fp.wheelbase_m, 2.8);
    }

    #[test]
    fn max_steering_rad_converts_to_degrees() {
        let cfg = VehicleConfig::default_urban();
        let kc = cfg.to_kinematics_contract();
        // default_urban uses 35° (0.6109… rad). Round-trip back to
        // degrees should hit 35.0 within numeric tolerance.
        assert!((kc.max_steering_deg - 35.0).abs() < 1e-9);
    }

    #[test]
    fn to_mrc_kinematics_contract_keeps_geometry_swaps_dynamic() {
        let cfg = VehicleConfig::default_urban();
        let nominal = cfg.to_kinematics_contract();
        let mrc     = cfg.to_mrc_kinematics_contract();
        let kernel_mrc = VehicleKinematicsContract::mrc_fallback_profile();

        // Dynamic limits must come from the kernel's MRC profile.
        assert_eq!(mrc.max_speed_mps,           kernel_mrc.max_speed_mps);
        assert_eq!(mrc.max_accel_mps2,          kernel_mrc.max_accel_mps2);
        assert_eq!(mrc.max_brake_mps2,          kernel_mrc.max_brake_mps2);
        assert_eq!(mrc.max_steering_deg,        kernel_mrc.max_steering_deg);
        assert_eq!(mrc.max_steering_rate_deg_s, kernel_mrc.max_steering_rate_deg_s);
        assert_eq!(mrc.min_follow_distance_m,   kernel_mrc.min_follow_distance_m);
        assert_eq!(mrc.max_lateral_accel_mps2,  kernel_mrc.max_lateral_accel_mps2);

        // Geometry must come from the integrator's nominal contract.
        assert_eq!(mrc.wheelbase_m,      nominal.wheelbase_m);
        assert_eq!(mrc.width_m,          nominal.width_m);
        assert_eq!(mrc.length_m,         nominal.length_m);
        assert_eq!(mrc.overhang_front_m, nominal.overhang_front_m);
        assert_eq!(mrc.overhang_rear_m,  nominal.overhang_rear_m);

        // The MRC speed cap is strictly tighter than the nominal vehicle max.
        assert!(mrc.max_speed_mps < nominal.max_speed_mps,
            "MRC cap ({}) must be tighter than vehicle nominal max ({})",
            mrc.max_speed_mps, nominal.max_speed_mps);
    }

    #[test]
    fn to_mrc_kinematics_contract_preserves_odd_cap() {
        // Cross-fix reconciliation (H2 + M1): the MRC-derate must NOT
        // drop the ODD cap. The effective Degraded ceiling is
        // min(5.0 MRC, 22.35 ODD) = 5.0 — MRC binds, ODD cap is
        // explicit-but-redundant (no None-cap surprise on this path).
        let cfg = VehicleConfig::default_urban();
        let mrc = cfg.to_mrc_kinematics_contract();
        assert_eq!(mrc.odd_speed_cap_mps, Some(URBAN_ODD_SPEED_CAP_MPS),
            "MRC-derate must carry the integrator's ODD cap forward");
        assert_eq!(mrc.effective_max_speed_mps(), 5.0,
            "MRC max (5.0) is more restrictive than ODD cap (22.35) — MRC wins");
    }

    #[test]
    fn default_urban_carries_urban_odd_speed_cap() {
        let cfg = VehicleConfig::default_urban();
        assert_eq!(cfg.odd_speed_cap_mps, Some(URBAN_ODD_SPEED_CAP_MPS));
        assert_eq!(cfg.max_speed_mps, 35.0);
        let kc = cfg.to_kinematics_contract();
        assert_eq!(kc.odd_speed_cap_mps, Some(URBAN_ODD_SPEED_CAP_MPS));
        // The enforced ceiling is the more restrictive of the two.
        assert_eq!(kc.effective_max_speed_mps(), URBAN_ODD_SPEED_CAP_MPS);
    }

    #[test]
    fn warn_if_missing_odd_cap_fires_when_none() {
        let mut cfg = VehicleConfig::default_urban();
        cfg.odd_speed_cap_mps = None;
        assert!(
            cfg.warn_if_missing_odd_cap(),
            "missing ODD cap on an urban deployment must emit a startup warning"
        );
    }

    #[test]
    fn warn_if_missing_odd_cap_silent_when_cap_set_below_vehicle_max() {
        let cfg = VehicleConfig::default_urban();
        assert!(
            !cfg.warn_if_missing_odd_cap(),
            "a properly-configured urban deployment must not emit the warning"
        );
    }

    #[test]
    fn warn_if_missing_odd_cap_fires_when_cap_does_not_tighten_ceiling() {
        let mut cfg = VehicleConfig::default_urban();
        // ODD cap >= vehicle max → cap is a no-op; warn.
        cfg.odd_speed_cap_mps = Some(40.0);
        assert!(cfg.warn_if_missing_odd_cap());
    }
}
