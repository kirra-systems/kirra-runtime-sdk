// parko/crates/parko-ros2/src/command_mapping.rs
//
// Mapping: parko-core `ControlCommand` (post-governor) → outgoing
// ROS 2 actuator command. The `OutgoingTwist` shape mirrors
// `geometry_msgs/Twist`'s 2D subset (linear.x + angular.z) so the
// node code can hand it directly to r2r's typed publisher.
//
// `enforce_outgoing_twist` is the pure mapper exercised by stable-lane
// tests. The r2r-side serialisation happens in `node.rs` (feature-gated)
// and consumes an `OutgoingTwist` produced here.

use parko_core::commands::ControlCommand;

/// Outgoing actuator command. Field shape mirrors
/// `geometry_msgs::Twist` 2D (planar) subset:
///   - linear.x in m/s  — forward velocity
///   - angular.z in rad/s — yaw rate
/// The 6-DOF axes are zeroed when this maps to a Twist (the M1
/// PostureTracker / parko-kirra govern these via the 2D subset).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutgoingTwist {
    pub linear_x_mps: f64,
    pub angular_z_rads: f64,
    /// Wall-clock-ms when this twist was produced. Threaded for
    /// the audit ledger + integration trace; not consumed by the
    /// vehicle interface.
    pub stamp_ms: u64,
}

impl OutgoingTwist {
    /// The MRC fallback — both axes zero. Published when:
    ///   - sensor input is stale,
    ///   - inference fails / produces non-finite values,
    ///   - the comparator escalates to LockedOut (Deny path).
    #[must_use]
    pub fn stopped(stamp_ms: u64) -> Self {
        Self {
            linear_x_mps: 0.0,
            angular_z_rads: 0.0,
            stamp_ms,
        }
    }
}

/// Map a post-governor `ControlCommand` to an `OutgoingTwist`. The
/// governor has already gated the command (clamped or denied) by the
/// time this is called — this mapping is a pure projection of axes,
/// not a safety check.
///
/// The mapping also re-validates the finiteness invariant on each axis
/// as a defence-in-depth check; a non-finite value here means a bug
/// downstream of the governor (the governor's `EnforcementAction`
/// pipeline + parko-core's `parse_inference_to_command` should both
/// have caught it earlier). When this fires, MRC.
#[must_use]
pub fn enforce_outgoing_twist(cmd: &ControlCommand) -> OutgoingTwist {
    if !cmd.linear_velocity.is_finite() || !cmd.angular_velocity.is_finite() {
        return OutgoingTwist::stopped(cmd.timestamp_ms);
    }
    OutgoingTwist {
        linear_x_mps: cmd.linear_velocity,
        angular_z_rads: cmd.angular_velocity,
        stamp_ms: cmd.timestamp_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_finite_command_per_axis() {
        let cmd = ControlCommand {
            linear_velocity: 1.5,
            angular_velocity: 0.4,
            timestamp_ms: 100,
        };
        let twist = enforce_outgoing_twist(&cmd);
        assert_eq!(
            twist,
            OutgoingTwist {
                linear_x_mps: 1.5,
                angular_z_rads: 0.4,
                stamp_ms: 100
            }
        );
    }

    #[test]
    fn maps_zero_command_to_stopped_twist() {
        let cmd = ControlCommand::stopped(200);
        let twist = enforce_outgoing_twist(&cmd);
        assert_eq!(twist, OutgoingTwist::stopped(200));
    }

    #[test]
    fn defence_in_depth_nan_linear_velocity_maps_to_stop() {
        // A bug downstream of the governor leaked a NaN — the mapper
        // must catch it and emit a stop, never a NaN twist.
        let cmd = ControlCommand {
            linear_velocity: f64::NAN,
            angular_velocity: 0.2,
            timestamp_ms: 300,
        };
        let twist = enforce_outgoing_twist(&cmd);
        assert_eq!(twist, OutgoingTwist::stopped(300));
    }

    #[test]
    fn defence_in_depth_inf_angular_velocity_maps_to_stop() {
        let cmd = ControlCommand {
            linear_velocity: 1.0,
            angular_velocity: f64::INFINITY,
            timestamp_ms: 400,
        };
        let twist = enforce_outgoing_twist(&cmd);
        assert_eq!(twist, OutgoingTwist::stopped(400));
    }

    #[test]
    fn stopped_twist_is_zero_on_both_axes() {
        let twist = OutgoingTwist::stopped(0);
        assert_eq!(twist.linear_x_mps, 0.0);
        assert_eq!(twist.angular_z_rads, 0.0);
    }
}
