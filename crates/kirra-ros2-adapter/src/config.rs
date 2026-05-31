// crates/kirra-ros2-adapter/src/config.rs
//
// VehicleConfig — the integrator-supplied vehicle profile the adapter
// hands to the kernel's per-pose kinematics check + the slow-loop
// containment check + the RSS pipeline.
//
// Phase 2A scope: a single struct + `default_urban()` constructor + the
// conversions to the kernel-side types. Phase 4 may grow per-asset
// config and a deserializer.

use kirra_runtime_sdk::gateway::containment::VehicleFootprint;
use kirra_runtime_sdk::gateway::kinematics_contract::VehicleKinematicsContract;

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

    /// Max forward speed, m/s.
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
}

impl VehicleConfig {
    /// Defaults for an urban mid-size AV. Matches the kernel's
    /// `nominal_reference_profile()` for the fields that overlap (wheelbase
    /// 2.8 m, max_speed 35 m/s, max_accel 2.5 m/s², max_brake 4.5 m/s²,
    /// 1.85 × 4.8 m footprint).
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
        }
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
}
