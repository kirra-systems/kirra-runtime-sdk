// parko/crates/parko-ros2/src/imu_shim.rs
//
// sensor_msgs/Imu → ImuSample extraction shim — the deferred ROS half of the IMU
// mapping. PURE field-copy CORE (no r2r, always compiled, unit-tested) + a THIN
// r2r ADAPTER (`#[cfg(feature = "ros2")]`, extraction only).
//
// SAFETY FRAMING. Upstream of the IMU transform → model → governor. The
// transform already fail-closes on non-finite values, so this shim stays thin.
// The two things it MUST get right are MEANING-preserving:
//   1. ORIENTATION AVAILABILITY — `sensor_msgs/Imu` signals "orientation not
//      provided" via `orientation_covariance[0] == -1.0`. In that case the shim
//      emits `None`. It NEVER fabricates an identity quaternion: identity would
//      assert "level + facing forward", a false attitude — exactly the hazard
//      the IMU transform's fail-closed path guards. This is the load-bearing
//      shim behavior; both branches are tested.
//   2. QUATERNION ORDER — `{x,y,z,w}` → `Quaternion{x,y,z,w}` verbatim (pinned).
//
// f64→f32 NARROWING (FLAGGED — narrow-and-pass): the ROS message is f64, the
// transform input is f32. The shim narrows and passes; it does NOT add a second
// non-finite gate. A huge finite f64 narrows to f32 `Inf`, which the IMU
// transform's EXISTING non-finite check rejects — so a second gate here would be
// redundant. Keeping the shim thin is deliberate.
//
// PIPELINE CONNECTION: `ImuSample` + `Quaternion` are the IMU mapping's transform
// input, in main (IMU mapping landed in sensor_mapping.rs). This shim emits those
// exact types — the prior byte-identical mirrors have been collapsed away — so the
// shim's `imu_to_sample(...) -> ImuSample` feeds the mapping's `to_tensor(&ImuSample)`
// with no conversion: the IMU path (Imu msg -> extract -> ImuSample -> mapping ->
// tensor) type-checks end to end. (`ImuRawFields` below is NOT a mirror — it is the
// f64 r2r extraction input — and is deliberately left in place.)
use crate::sensor_mapping::{ImuSample, Quaternion};

/// Plain mirror of the `sensor_msgs/Imu` fields the shim reads — r2r-free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImuRawFields {
    /// `linear_acceleration` {x, y, z} (m/s²).
    pub linear_acceleration: [f64; 3],
    /// `angular_velocity` {x, y, z} (rad/s).
    pub angular_velocity: [f64; 3],
    /// `orientation` in `[x, y, z, w]` order.
    pub orientation_xyzw: [f64; 4],
    /// `orientation_covariance[0]`. `-1.0` = orientation not provided (ROS
    /// convention).
    pub orientation_covariance0: f64,
}

fn narrow3(v: [f64; 3]) -> [f32; 3] {
    // narrow-and-pass: out-of-f32-range values become ±Inf and are rejected by
    // the transform's non-finite check (no second gate here — see module docs).
    [v[0] as f32, v[1] as f32, v[2] as f32]
}

/// Pure field copy + orientation-availability decision into the transform's
/// `ImuSample`. No value validation beyond the availability convention.
#[must_use]
pub fn imu_to_sample(raw: &ImuRawFields) -> ImuSample {
    let orientation = if raw.orientation_covariance0 == -1.0 {
        // Orientation not provided — NEVER fabricate identity.
        None
    } else {
        Some(Quaternion {
            x: raw.orientation_xyzw[0] as f32,
            y: raw.orientation_xyzw[1] as f32,
            z: raw.orientation_xyzw[2] as f32,
            w: raw.orientation_xyzw[3] as f32,
        })
    };
    ImuSample {
        linear_acceleration: narrow3(raw.linear_acceleration),
        angular_velocity: narrow3(raw.angular_velocity),
        orientation,
    }
}

/// THIN r2r ADAPTER — extraction only. Compiles only under `--features ros2`.
#[cfg(feature = "ros2")]
pub fn imu_msg_to_sample(msg: &r2r::sensor_msgs::msg::Imu) -> ImuSample {
    let a = &msg.linear_acceleration;
    let g = &msg.angular_velocity;
    let o = &msg.orientation;
    imu_to_sample(&ImuRawFields {
        linear_acceleration: [a.x, a.y, a.z],
        angular_velocity: [g.x, g.y, g.z],
        orientation_xyzw: [o.x, o.y, o.z, o.w],
        orientation_covariance0: msg.orientation_covariance[0],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(cov0: f64) -> ImuRawFields {
        ImuRawFields {
            linear_acceleration: [1.5, -2.5, 9.81],
            angular_velocity: [0.1, -0.2, 0.3],
            orientation_xyzw: [0.11, 0.22, 0.33, 0.44], // asymmetric → order pinned
            orientation_covariance0: cov0,
        }
    }

    #[test]
    fn covariance_minus_one_yields_no_orientation() {
        // ROS "orientation not provided" — must be None, never identity.
        let s = imu_to_sample(&raw(-1.0));
        assert_eq!(s.orientation, None);
    }

    #[test]
    fn orientation_present_lands_in_xyzw_order() {
        let s = imu_to_sample(&raw(0.0)); // cov0 != -1 → orientation present
        assert_eq!(
            s.orientation,
            Some(Quaternion {
                x: 0.11,
                y: 0.22,
                z: 0.33,
                w: 0.44
            })
        );
    }

    #[test]
    fn accel_and_gyro_narrow_f64_to_f32() {
        let s = imu_to_sample(&raw(0.0));
        assert_eq!(s.linear_acceleration, [1.5_f32, -2.5, 9.81]);
        assert_eq!(s.angular_velocity, [0.1_f32, -0.2, 0.3]);
    }

    #[test]
    fn out_of_f32_range_narrows_to_inf_for_the_transform_to_reject() {
        // narrow-and-pass: 1e300 (finite f64) → f32 Inf; the shim does NOT gate
        // it — the transform's non-finite check does.
        let mut r = raw(0.0);
        r.linear_acceleration = [1e300, 0.0, 0.0];
        let s = imu_to_sample(&r);
        assert!(s.linear_acceleration[0].is_infinite());
    }
}
