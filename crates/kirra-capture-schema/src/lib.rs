// crates/kirra-capture-schema/src/lib.rs
//
// Wire schema for Kirra's learning-loop capture records (docs/COLLECTOR_DESIGN.md
// [D6 / C1]). This is the SINGLE authoritative definition of the on-disk capture
// JSONL shape, shared by two sides:
//   - the SDK emitters (`kirra_verifier::capture`), which BUILD records via
//     constructors that touch governor types and SERIALIZE them, and
//   - the offline `kirra-collector`, which DESERIALIZES them to assemble the
//     training dataset.
//
// §0 SAFETY BOUNDARY (enforced by the dependency graph, not a comment): this
// crate depends on `serde` ONLY. It holds no governor types and no logic — only
// leaf data. Because the collector depends on THIS crate and never on
// `kirra-runtime-sdk`, it is mechanically incapable of linking or reaching the
// verdict path. The SDK depends on this crate and re-exports the types
// (`pub use kirra_capture_schema::*;`) so every existing `crate::capture::*` path
// keeps resolving unchanged.
//
// Owned strings (NOT `&'static str`): `posture` and `deny_code` are `String` /
// `Option<String>` here, not the SDK's internal `&'static str` tokens — serde
// cannot `Deserialize` a `&'static str`, and the collector must read these back.
// The on-disk JSON is byte-for-byte identical (a JSON string is a string either
// way); only the in-memory backing differs. The SDK constructors `.to_string()`
// their static tokens into these fields.
//
// Note on what is NOT here:
//   - `TrajectoryDecision` stays in the SDK — it is a constructor INPUT (the
//     SDK-side mirror of the adapter's `TrajectoryVerdict`), never a serialized
//     field, so it has no place in the wire schema.
//   - `From<&ProposedVehicleCommand> for ProposedCommandSnapshot` stays in the
//     SDK (inlined into the constructor) — it references a governor type and so
//     cannot live in this serde-only crate.

use serde::{Deserialize, Serialize};

/// The decision Kirra reached, as a stable token (the "correction" kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CaptureOutcome {
    Allow,
    ClampLinear,
    ClampSteering,
    Deny,
}

/// Which enforcement point emitted the record. Phase 1 had one emit (the
/// command gateway, fast loop); Phase 1.5 (docs/CAPTURE_PIPELINE_SPEC.md §3)
/// adds the slow-loop trajectory verdict in the ROS 2 adapter. The Linux
/// collector keys on this to bucket fast- vs slow-loop corrections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CaptureSource {
    /// The actuator command gateway (`policy_layer`), per-command fast loop.
    CommandGateway,
    /// The ROS 2 adapter's slow-loop trajectory validator.
    SlowLoopTrajectory,
}

/// A snapshot of the doer's proposal (correlation context; the Linux collector
/// joins this with the bus-observed perception/ego/model-version). The SDK
/// builds this from the governor's `ProposedVehicleCommand` at the emit site.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProposedCommandSnapshot {
    pub linear_velocity_mps: f64,
    pub current_velocity_mps: f64,
    pub steering_angle_deg: f64,
    pub current_steering_angle_deg: f64,
    pub delta_time_s: f64,
}

/// A single world-frame pose, the BOUNDED endpoints of a trajectory summary.
/// Only the first + last pose are recorded, never the full point sequence — the
/// summary must stay O(1) so the slow-loop emit never regresses WCET.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PoseSnapshot {
    pub x_m: f64,
    pub y_m: f64,
    pub heading_rad: f64,
}

/// BOUNDED slow-loop trajectory summary + join keys. The trajectory analogue of
/// `ProposedCommandSnapshot`: just enough to JOIN the record Linux-side
/// (asset/trajectory ids + the objects-snapshot freshness stamp) plus a
/// fixed-size shape summary (counts + endpoint poses + the planner's target
/// speed). It deliberately does NOT clone the full point or object vectors —
/// only their lengths and endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryCaptureExt {
    /// Join key — the per-asset id ("ego" in single-asset deployments).
    pub asset_id: String,
    /// Join key — the planner/adapter-assigned monotonic trajectory id.
    pub trajectory_id: u64,
    /// Join key — wall-clock ms of the objects snapshot the verdict used
    /// (the same freshness stamp the perception tick is keyed on); lets the
    /// collector line the record up with the bus-observed perception frame.
    pub objects_ms: u64,
    /// Number of points in the candidate trajectory (shape, not the points).
    pub point_count: usize,
    /// Number of perceived objects the verdict saw (shape, not the objects).
    pub object_count: usize,
    /// First pose of the candidate (None for an empty trajectory).
    pub first_pose: Option<PoseSnapshot>,
    /// Last pose of the candidate (None for an empty trajectory).
    pub last_pose: Option<PoseSnapshot>,
    /// The planner's commanded speed at the last point (m/s) — the "target"
    /// the slow loop validated against. None for an empty trajectory.
    pub target_speed_mps: Option<f64>,
}

/// The small, fixed-shape capture record. Carries the correction Kirra imposed +
/// the proposal/trajectory context + join keys. Deliberately does NOT carry the
/// doer's model version — Kirra doesn't know it; it is joined Linux-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureRecord {
    /// Monotonic per-decision sequence (the join/order key).
    pub decision_seq: u64,
    /// Monotonic ns since process start (ordering, skew-free).
    pub t_mono_ns: u128,
    /// Wall-clock ms (bus join).
    pub t_wall_ms: u64,
    /// Which enforcement point emitted this record (fast loop vs slow loop).
    pub source: CaptureSource,
    /// The doer's proposal (correlation + context). Present for the command
    /// gateway (`CommandGateway`); absent for the slow-loop trajectory record,
    /// which carries its context in `traj` instead.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub proposed: Option<ProposedCommandSnapshot>,
    /// Bounded slow-loop trajectory summary + join keys. Present only for
    /// `SlowLoopTrajectory` records; `None` (and omitted from JSON) for the
    /// command-gateway record.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub traj: Option<TrajectoryCaptureExt>,
    /// What Kirra decided.
    pub outcome: CaptureOutcome,
    /// Which check fired, if a deny (the `DenyCode` token); `None` otherwise.
    /// Owned `String` (not `&'static str`) so the collector can deserialize it.
    pub deny_code: Option<String>,
    /// The safe value Kirra substituted on a clamp (m/s for linear, deg for
    /// steering); `None` for Allow/Deny.
    pub safe_value: Option<f64>,
    /// Controlled-stop substitution (Degraded → decel-to-stop-and-HOLD envelope).
    pub mrc: bool,
    /// Posture context. Owned `String` (not `&'static str`) for deserialization;
    /// the SDK writes one of `NOMINAL` / `DEGRADED` / `LOCKED_OUT`.
    pub posture: String,
    /// Whether the perception derate was enabled (so passes are attributable).
    pub derate_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway_clamp_record() -> CaptureRecord {
        CaptureRecord {
            decision_seq: 0,
            t_mono_ns: 0,
            t_wall_ms: 1000,
            source: CaptureSource::CommandGateway,
            proposed: Some(ProposedCommandSnapshot {
                linear_velocity_mps: 40.0,
                current_velocity_mps: 40.0,
                steering_angle_deg: 0.0,
                current_steering_angle_deg: 0.0,
                delta_time_s: 0.1,
            }),
            traj: None,
            outcome: CaptureOutcome::ClampLinear,
            deny_code: None,
            safe_value: Some(35.0),
            mrc: false,
            posture: "NOMINAL".to_string(),
            derate_enabled: false,
        }
    }

    fn trajectory_mrc_record() -> CaptureRecord {
        CaptureRecord {
            decision_seq: 3,
            t_mono_ns: 0,
            t_wall_ms: 2000,
            source: CaptureSource::SlowLoopTrajectory,
            proposed: None,
            traj: Some(TrajectoryCaptureExt {
                asset_id: "ego".to_string(),
                trajectory_id: 7,
                objects_ms: 123_456,
                point_count: 12,
                object_count: 3,
                first_pose: Some(PoseSnapshot { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 }),
                last_pose: Some(PoseSnapshot { x_m: 5.0, y_m: 1.0, heading_rad: 0.1 }),
                target_speed_mps: Some(8.0),
            }),
            outcome: CaptureOutcome::Deny,
            deny_code: Some("TRAJECTORY_MRC_FALLBACK".to_string()),
            safe_value: None,
            mrc: true,
            posture: "DEGRADED".to_string(),
            derate_enabled: false,
        }
    }

    /// The on-disk wire shape, pinned. These exact strings are the
    /// pre-extraction shape (declared-field order; SCREAMING_SNAKE enums;
    /// skip_serializing_if on proposed/traj only; deny_code/posture as JSON
    /// strings/null). If any of these change, the collector's deserialize and
    /// every prior captured file break — so they are pinned, not asserted loosely.
    #[test]
    fn gateway_record_wire_shape_is_pinned() {
        let s = serde_json::to_string(&gateway_clamp_record()).unwrap();
        assert_eq!(
            s,
            r#"{"decision_seq":0,"t_mono_ns":0,"t_wall_ms":1000,"source":"COMMAND_GATEWAY","proposed":{"linear_velocity_mps":40.0,"current_velocity_mps":40.0,"steering_angle_deg":0.0,"current_steering_angle_deg":0.0,"delta_time_s":0.1},"outcome":"CLAMP_LINEAR","deny_code":null,"safe_value":35.0,"mrc":false,"posture":"NOMINAL","derate_enabled":false}"#
        );
    }

    #[test]
    fn trajectory_record_wire_shape_is_pinned() {
        let s = serde_json::to_string(&trajectory_mrc_record()).unwrap();
        assert_eq!(
            s,
            r#"{"decision_seq":3,"t_mono_ns":0,"t_wall_ms":2000,"source":"SLOW_LOOP_TRAJECTORY","traj":{"asset_id":"ego","trajectory_id":7,"objects_ms":123456,"point_count":12,"object_count":3,"first_pose":{"x_m":0.0,"y_m":0.0,"heading_rad":0.0},"last_pose":{"x_m":5.0,"y_m":1.0,"heading_rad":0.1},"target_speed_mps":8.0},"outcome":"DENY","deny_code":"TRAJECTORY_MRC_FALLBACK","safe_value":null,"mrc":true,"posture":"DEGRADED","derate_enabled":false}"#
        );
    }

    /// Full round-trip: serialize → deserialize → re-serialize must equal the
    /// original JSON, for both sources. This is what proves the owned-`String`
    /// switch (vs the SDK's `&'static str`) did not move the wire format and that
    /// the collector reads exactly what the SDK writes.
    #[test]
    fn round_trip_is_lossless_for_each_source() {
        for original in [gateway_clamp_record(), trajectory_mrc_record()] {
            let s1 = serde_json::to_string(&original).unwrap();
            let back: CaptureRecord = serde_json::from_str(&s1).unwrap();
            assert_eq!(back, original, "deserialized record must equal the original");
            let s2 = serde_json::to_string(&back).unwrap();
            assert_eq!(s1, s2, "re-serialized JSON must be byte-identical");
        }
    }

    /// The collector can deserialize a gateway line that OMITS `traj` and a
    /// trajectory line that OMITS `proposed` — the `#[serde(default)]` on both
    /// optional join blocks makes the skip-serialized shape readable.
    #[test]
    fn deserialize_tolerates_omitted_optional_blocks() {
        let gw: CaptureRecord = serde_json::from_str(
            &serde_json::to_string(&gateway_clamp_record()).unwrap(),
        )
        .unwrap();
        assert!(gw.traj.is_none());
        assert!(gw.proposed.is_some());

        let tj: CaptureRecord = serde_json::from_str(
            &serde_json::to_string(&trajectory_mrc_record()).unwrap(),
        )
        .unwrap();
        assert!(tj.proposed.is_none());
        assert!(tj.traj.is_some());
    }
}
