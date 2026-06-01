// crates/kirra-ros2-adapter/src/geometry.rs
//
// Small geometric helpers used by the typed-payload parsers (Phase 4c).
// Not feature-gated: the math is pure-Rust and the tests exercise it on
// the default-lane build; the parsers in `parsing.rs` consume it under
// the `ros2` feature.

/// Extract the planar yaw (rotation about Z) from a unit quaternion
/// `(x, y, z, w)` in the geometry-msgs convention.
///
/// Standard Z-rotation extraction:
///   yaw = atan2( 2·(w·z + x·y),  1 − 2·(y² + z²) )
///
/// The quaternion is assumed normalized; the parser exposes the raw
/// values from `geometry_msgs::msg::Quaternion` and lets the kernel
/// per-pose NaN/Inf guard catch upstream nonsense (Phase 4c does not
/// re-normalize on every call — that's the publisher's responsibility).
#[inline]
pub fn quat_to_yaw(x: f64, y: f64, z: f64, w: f64) -> f64 {
    let siny_cosp = 2.0 * (w * z + x * y);
    let cosy_cosp = 1.0 - 2.0 * (y * y + z * z);
    siny_cosp.atan2(cosy_cosp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

    /// Identity quaternion → 0 rad yaw.
    #[test]
    fn quat_identity_is_zero_yaw() {
        let y = quat_to_yaw(0.0, 0.0, 0.0, 1.0);
        assert!(y.abs() < 1e-12, "identity quat must give yaw = 0; got {y}");
    }

    /// 90° Z-rotation: q = (0, 0, sin(π/4), cos(π/4)).
    #[test]
    fn quat_90deg_z_rotation_is_half_pi() {
        let s = (FRAC_PI_4).sin();
        let c = (FRAC_PI_4).cos();
        let y = quat_to_yaw(0.0, 0.0, s, c);
        assert!(
            (y - FRAC_PI_2).abs() < 1e-12,
            "expected yaw = π/2 ≈ 1.5708; got {y}"
        );
    }

    /// 180° Z-rotation: q = (0, 0, 1, 0). atan2(2·0, 1 − 2·1) = atan2(0, −1) = π.
    #[test]
    fn quat_180deg_z_rotation_is_pi() {
        let y = quat_to_yaw(0.0, 0.0, 1.0, 0.0);
        assert!(
            (y.abs() - PI).abs() < 1e-12,
            "expected |yaw| = π; got {y}"
        );
    }

    /// Negative quaternion is the same orientation (double cover) — yaw
    /// should match the positive form within numerical noise.
    #[test]
    fn quat_double_cover_yields_same_yaw() {
        let y_pos = quat_to_yaw(0.0, 0.0, 0.5, 0.866_025_403_8);
        let y_neg = quat_to_yaw(0.0, 0.0, -0.5, -0.866_025_403_8);
        assert!(
            (y_pos - y_neg).abs() < 1e-9,
            "double-cover quat must give the same yaw; got {y_pos} vs {y_neg}"
        );
    }

    /// Tilt-only quaternion (pitch about Y) — Z-yaw extraction must still
    /// return ~0 because we're only reading the Z component.
    /// q = (0, sin(π/4), 0, cos(π/4)): pitch = π/2, yaw = 0.
    #[test]
    fn quat_pure_pitch_has_zero_yaw() {
        let s = (FRAC_PI_4).sin();
        let c = (FRAC_PI_4).cos();
        let y = quat_to_yaw(0.0, s, 0.0, c);
        // siny_cosp = 2·(c·0 + 0·s) = 0; cosy_cosp = 1 − 2·s² = 0 → atan2(0, 0)
        // returns 0 in Rust's f64 implementation, which is correct for the
        // "no yaw, pure pitch" interpretation.
        assert!(y.abs() < 1e-12, "pure-pitch quat must give yaw ≈ 0; got {y}");
    }
}
