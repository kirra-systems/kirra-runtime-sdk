use serde::{Deserialize, Serialize};

/// A planar velocity command for a differential-drive robot.
///
/// Linear velocity is along the robot's forward axis (m/s).
/// Angular velocity is rotation about the vertical axis (rad/s, positive = CCW).
/// Maps naturally to ROS2 `geometry_msgs/Twist` 2D subset.
///
/// For 6DOF (drones), manipulator joint commands, or steering-angle vehicles,
/// this type is insufficient — define a separate command type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControlCommand {
    pub linear_velocity: f64,
    pub angular_velocity: f64,
    pub timestamp_ms: u64,
}

impl ControlCommand {
    /// A zero-velocity command. Used as a safe default and for emergency stop.
    pub fn stopped(now_ms: u64) -> Self {
        Self {
            linear_velocity: 0.0,
            angular_velocity: 0.0,
            timestamp_ms: now_ms,
        }
    }
}
