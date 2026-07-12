// crates/kirra-ros2-adapter/src/parsing.rs
//
// Phase 4c — typed-payload parsers from the integrator's Autoware ROS 2
// messages to the kernel-shape types the slow / fast loops consume.
//
// Feature-gated behind `ros2` because these functions reference r2r-
// generated message types that only exist when the integrator's ROS env
// is sourced at build time (r2r's build script discovers
// `autoware_planning_msgs` / `autoware_perception_msgs` /
// `nav_msgs` via AMENT_PREFIX_PATH and generates Rust bindings on the
// fly).
//
// Field-shape disposition:
//   The struct-field accesses here are derived from the current Autoware
//   message definitions (autoware_planning_msgs::Trajectory ::TrajectoryPoint,
//   autoware_perception_msgs::PredictedObjects ::PredictedObject,
//   nav_msgs::Odometry). Verification against the actual r2r-generated
//   types requires the integrator's ROS env to be sourced. The fields
//   match the published Autoware Jazzy message specs; any divergence
//   (e.g. an Autoware version that has rotated to a different name —
//   `longitudinal_velocity` vs `longitudinal_velocity_mps`, etc.) is
//   captured by the build failing with a precise field-mismatch error
//   from rustc.
//
// All numeric fields cross the FFI as f32/f64 per the message spec;
// the conversions to f64 here are lossless on the kernel side.

#![cfg(feature = "ros2")]

use crate::corridor::Point;
use crate::geometry::quat_to_yaw;
use crate::state::{
    EgoOdom, IncomingTrajectory, PerceivedObject, PerceivedPedestrian, Pose, TrajectoryPoint,
};

/// Convert an `autoware_planning_msgs::msg::Trajectory` to the kernel's
/// trajectory envelope `IncomingTrajectory`. `received_ms` is set by the
/// caller (the drain task's wall-clock at message receipt).
///
/// Field mapping (per Autoware Jazzy spec):
///   Trajectory.points: Vec<TrajectoryPoint>
///   TrajectoryPoint.pose.position.{x,y,z}      → Pose.{x_m, y_m, _}
///   TrajectoryPoint.pose.orientation.{x,y,z,w} → Pose.heading_rad (quat_to_yaw)
///   TrajectoryPoint.longitudinal_velocity_mps  → velocity_mps
///   TrajectoryPoint.time_from_start            → time_from_start_s (sec + nanosec*1e-9)
///
/// Returns an `IncomingTrajectory` with `received_ms` filled in by the
/// caller via `from_msg_at`; the bare `from_msg` helper sets it to the
/// current wall clock for tests / single-call sites.
pub fn parse_trajectory(
    msg: &r2r::autoware_planning_msgs::msg::Trajectory,
    received_ms: u64,
) -> IncomingTrajectory {
    let points = msg
        .points
        .iter()
        .map(|pt| {
            let heading_rad = quat_to_yaw(
                pt.pose.orientation.x as f64,
                pt.pose.orientation.y as f64,
                pt.pose.orientation.z as f64,
                pt.pose.orientation.w as f64,
            );
            let time_from_start_s =
                pt.time_from_start.sec as f64 + (pt.time_from_start.nanosec as f64) * 1e-9;
            TrajectoryPoint {
                pose: Pose {
                    x_m: pt.pose.position.x as f64,
                    y_m: pt.pose.position.y as f64,
                    heading_rad,
                },
                velocity_mps: pt.longitudinal_velocity_mps as f64,
                time_from_start_s,
            }
        })
        .collect();
    IncomingTrajectory {
        points,
        received_ms,
    }
}

/// Convert an `autoware_perception_msgs::msg::PredictedObjects` batch to
/// the kernel's `Vec<PerceivedObject>` snapshot. The drain task hands the
/// returned vector to `AdaptorState::update_objects`.
///
/// Field mapping:
///   PredictedObjects.objects: Vec<PredictedObject>
///   PredictedObject.object_id.uuid: [u8; 16]
///     → folded into u64 (truncates the high 8 bytes; the kernel only
///       needs object-id stability within one slow-loop cycle, not
///       global uniqueness)
///   .kinematics.initial_pose_with_covariance.pose
///     .position.{x,y}                 → pos: Point
///     .orientation.{x,y,z,w}          → heading_rad (quat_to_yaw)
///   .kinematics.initial_twist_with_covariance.twist
///     .linear.x, .linear.y            → velocity_mps = √(vx² + vy²)
///
/// Note: the kinematic block carries an `initial_pose_with_covariance`
/// (most recent) and a `predicted_paths` array (future propagation).
/// Phase 4c uses only the `initial_*` block; the slow-loop's RSS check
/// is per-pose against the trajectory, so the predicted paths aren't
/// needed at this layer.
pub fn parse_predicted_objects(
    msg: &r2r::autoware_perception_msgs::msg::PredictedObjects,
) -> Vec<PerceivedObject> {
    msg.objects
        .iter()
        .map(|obj| {
            // 16-byte UUID → u64 (low 8 bytes after a left-shift fold). Stable
            // within a single observation but not globally unique; the slow
            // loop only needs intra-cycle identity for the RSS per-object
            // iteration.
            let id = obj
                .object_id
                .uuid
                .iter()
                .take(8)
                .fold(0u64, |acc, b| (acc << 8) | (*b as u64));
            let pose = &obj.kinematics.initial_pose_with_covariance.pose;
            let twist = &obj.kinematics.initial_twist_with_covariance.twist;
            let heading_rad = quat_to_yaw(
                pose.orientation.x as f64,
                pose.orientation.y as f64,
                pose.orientation.z as f64,
                pose.orientation.w as f64,
            );
            let vx = twist.linear.x as f64;
            let vy = twist.linear.y as f64;
            let velocity_mps = (vx * vx + vy * vy).sqrt();
            PerceivedObject {
                id,
                pos: Point {
                    x_m: pose.position.x as f64,
                    y_m: pose.position.y as f64,
                },
                velocity_mps,
                heading_rad,
                // KIRRA-OCCY-PMON-003 §5 (PRESERVE): carry the twist VECTOR through
                // (previously discarded after the magnitude collapse) so the
                // Track-C kinematic ceiling sees the reported map-frame velocity.
                vel: Point { x_m: vx, y_m: vy },
            }
        })
        .collect()
}

/// Convert an `autoware_perception_msgs::msg::PredictedObjects` message received
/// on the DEDICATED `~/input/pedestrians` topic into the kernel's
/// [`PerceivedPedestrian`]s (#789 follow-up 1). Every object on that topic is a
/// pedestrian — the producer (e.g. kirra-taj's `classify_pedestrians`) has
/// already classified — so there is NO re-filtering here; a mis-published
/// non-pedestrian only ever ADDS an omnidirectional stopping bound (fail-safe).
///
/// `age_s` is `0.0`: the measurement is treated as fresh-at-receipt, exactly as
/// the object / channel-B parsers do — the slow loop's staleness budget bounds
/// how old a *snapshot* may be, and mixing the producer's header-stamp clock
/// domain in here would violate AOU-TIMESYNC-001. Position + velocity vector map
/// straight across (same ego-world frame as `PerceivedObject`).
pub fn parse_pedestrians(
    msg: &r2r::autoware_perception_msgs::msg::PredictedObjects,
) -> Vec<PerceivedPedestrian> {
    msg.objects
        .iter()
        .map(|obj| {
            let id = obj
                .object_id
                .uuid
                .iter()
                .take(8)
                .fold(0u64, |acc, b| (acc << 8) | (*b as u64));
            let pose = &obj.kinematics.initial_pose_with_covariance.pose;
            let twist = &obj.kinematics.initial_twist_with_covariance.twist;
            PerceivedPedestrian {
                id,
                pos: Point {
                    x_m: pose.position.x as f64,
                    y_m: pose.position.y as f64,
                },
                vel: Point {
                    x_m: twist.linear.x as f64,
                    y_m: twist.linear.y as f64,
                },
                age_s: 0.0,
            }
        })
        .collect()
}

/// Convert a `nav_msgs::msg::Odometry` to the kernel's `EgoOdom`. The
/// twist is in the ego frame (vehicle body); `linear.x` is forward
/// velocity, `angular.z` is yaw rate — exactly what the slow loop's
/// inverse-bicycle-model steering estimate needs.
///
/// Field mapping:
///   header.stamp.{sec, nanosec}  → stamp_ms = sec*1000 + nanosec/1_000_000
///   twist.twist.linear.x         → linear_x_mps
///   twist.twist.angular.z        → yaw_rate_rads
pub fn parse_odom(msg: &r2r::nav_msgs::msg::Odometry) -> EgoOdom {
    let stamp_ms =
        (msg.header.stamp.sec as u64) * 1_000 + (msg.header.stamp.nanosec as u64) / 1_000_000;
    EgoOdom {
        linear_x_mps: msg.twist.twist.linear.x as f64,
        yaw_rate_rads: msg.twist.twist.angular.z as f64,
        stamp_ms,
    }
}
