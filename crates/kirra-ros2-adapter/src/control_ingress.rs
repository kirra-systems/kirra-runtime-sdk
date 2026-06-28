// crates/kirra-ros2-adapter/src/control_ingress.rs
//
// Pure parser for the untyped `autoware_control_msgs/msg/Control` JSON shape
// used by the ROS 2 adapter. Kept outside `node.rs` so the ingress contract is
// unit-tested without requiring a sourced ROS 2/r2r toolchain.

#![cfg_attr(not(feature = "ros2"), allow(dead_code))]

use serde_json::Value;

/// Control-command ingress payload (envelope over the untyped
/// `autoware_control_msgs::Control` JSON map). The fast-loop task converts
/// this to `IncomingControl` for the conformance check.
#[derive(Debug, Clone, PartialEq)]
pub struct IngressControlCommand {
    pub asset_id: String,
    pub linear_velocity_mps: f64,
    pub steering_angle_rad: f64,
    /// Wall-clock ms from the ROS message stamp, or the receipt timestamp when
    /// the message carries no usable top-level stamp.
    pub stamp_ms: u64,
}

/// Parse the minimal Autoware Control fields consumed by the fast-loop
/// conformance gate:
///
/// - `longitudinal.velocity`
/// - `lateral.steering_tire_angle`
///
/// The adapter publishes the same untyped field shape on `~/output/control_cmd`,
/// so this parser closes the input/output schema loop. Missing, non-numeric, or
/// non-finite fields are rejected by the caller, which emits an MRC-triggering
/// command instead of passing the malformed input through.
pub fn parse_control_command_json(
    asset_id: &str,
    msg: &Value,
    received_ms: u64,
) -> Result<IngressControlCommand, &'static str> {
    let velocity = finite_f64_at(msg, &["longitudinal", "velocity"])
        .ok_or("missing_or_nonfinite_longitudinal_velocity")?;
    let steering = finite_f64_at(msg, &["lateral", "steering_tire_angle"])
        .ok_or("missing_or_nonfinite_lateral_steering_tire_angle")?;
    Ok(IngressControlCommand {
        asset_id: asset_id.to_string(),
        linear_velocity_mps: velocity,
        steering_angle_rad: steering,
        stamp_ms: stamp_ms_from_control(msg).unwrap_or(received_ms),
    })
}

/// A finite command that the fast-loop conformance check must reject, causing
/// publication of the configured MRC command. Used for malformed untyped input
/// so parse failure fails closed instead of producing no gated output.
pub fn fail_closed_control_command(asset_id: &str, received_ms: u64) -> IngressControlCommand {
    IngressControlCommand {
        asset_id: asset_id.to_string(),
        linear_velocity_mps: f64::MAX,
        steering_angle_rad: f64::MAX,
        stamp_ms: received_ms,
    }
}

fn finite_f64_at(msg: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = msg;
    for key in path {
        cur = cur.get(*key)?;
    }
    let v = cur.as_f64()?;
    v.is_finite().then_some(v)
}

fn stamp_ms_from_control(msg: &Value) -> Option<u64> {
    stamp_ms_from_value(msg.get("stamp")?)
        .or_else(|| stamp_ms_from_value(msg.get("longitudinal")?.get("stamp")?))
        .or_else(|| stamp_ms_from_value(msg.get("lateral")?.get("stamp")?))
}

fn stamp_ms_from_value(stamp: &Value) -> Option<u64> {
    let sec = stamp.get("sec")?.as_i64()?;
    if sec < 0 {
        return None;
    }
    let nanosec = stamp.get("nanosec")?.as_u64()?;
    if nanosec >= 1_000_000_000 {
        return None;
    }
    Some((sec as u64) * 1_000 + nanosec / 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_minimal_control_command() {
        let msg = json!({
            "stamp": { "sec": 12, "nanosec": 345_000_000_u64 },
            "lateral": { "steering_tire_angle": 0.125 },
            "longitudinal": { "velocity": 4.5 }
        });

        let parsed = parse_control_command_json("ego", &msg, 99).expect("parse control");

        assert_eq!(parsed.asset_id, "ego");
        assert_eq!(parsed.linear_velocity_mps, 4.5);
        assert_eq!(parsed.steering_angle_rad, 0.125);
        assert_eq!(parsed.stamp_ms, 12_345);
    }

    #[test]
    fn falls_back_to_receipt_time_without_stamp() {
        let msg = json!({
            "lateral": { "steering_tire_angle": -0.25 },
            "longitudinal": { "velocity": 1.0 }
        });

        let parsed = parse_control_command_json("ego", &msg, 77).expect("parse control");

        assert_eq!(parsed.stamp_ms, 77);
        assert_eq!(parsed.steering_angle_rad, -0.25);
    }

    #[test]
    fn rejects_missing_required_fields() {
        let msg = json!({
            "lateral": { "steering_tire_angle": 0.0 },
            "longitudinal": {}
        });

        assert!(parse_control_command_json("ego", &msg, 1).is_err());
    }

    #[test]
    fn fail_closed_command_forces_rejection_with_finite_values() {
        let cmd = fail_closed_control_command("ego", 42);

        assert_eq!(cmd.asset_id, "ego");
        assert!(cmd.linear_velocity_mps.is_finite());
        assert!(cmd.steering_angle_rad.is_finite());
        assert_eq!(cmd.linear_velocity_mps, f64::MAX);
        assert_eq!(cmd.stamp_ms, 42);
    }
}
