// crates/kirra-core/src/trajectory.rs (de-monolith Stage 6a: relocated verbatim from the
// kirra-ros2-adapter `state` module)
//
// The lean trajectory/perception data types the planner and the shared lane map
// (kirra-map) consume: `Pose`, `TrajectoryPoint`, `TrajectoryVerdict`, and
// `PerceivedObject`. These are pure plain-old-data — no DashMap, no PostureTracker,
// no async. The adapter's `AdaptorState` runtime store, `AcceptedTrajectory` slot
// record, `EgoOdom`, and `IncomingTrajectory` STAY in the adapter (they couple to the
// DashMap concurrency model) and reference these types via re-export.

use crate::corridor::Point;

/// One pose along a trajectory, in world frame. The shape matches
/// `kirra_core::containment::Pose` so a conversion is field-for-field (no
/// semantic translation required).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub x_m: f64,
    pub y_m: f64,
    pub heading_rad: f64,
}

/// One sample from an Autoware-shaped trajectory. The `time_from_start_s`
/// is the planner's intended time-to-pose; the slow loop uses it to derive
/// per-step `delta_time_s` for the kinematics check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrajectoryPoint {
    pub pose: Pose,
    pub velocity_mps: f64,
    pub time_from_start_s: f64,
}

/// Outcome of validating a candidate trajectory. The slow loop emits this
/// when it promotes (or refuses to promote) a new trajectory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrajectoryVerdict {
    /// Promoted — the per-asset slot now holds this trajectory; the fast
    /// loop will conform commands to it.
    Accept,
    /// Clamp-only path: per-pose kinematics requested a Clamp (linear or
    /// steering) on at least one pose, but containment + RSS both passed.
    /// The caller's policy is "promote a speed-derated variant" — see
    /// design §3. Fast loop treats this as a special Accept where the
    /// permissible-velocity envelope is below the planner's commanded
    /// velocity; safe to drive but not at the planned speed.
    Clamp,
    /// Refused — the slow loop rejected the candidate (or no candidate
    /// has ever been validated). Fast loop must MRC.
    MRCFallback,
    /// Initial / transitional state, set when an asset is registered but
    /// no validation has completed yet. Always fails closed
    /// (`fail_closed()` collapses this to `MRCFallback`).
    Pending,
}

/// A perceived object reported by the integrator's perception stack.
/// The fields are the minimal set RSS needs (the slow loop runs
/// `longitudinal_safe_distance` + `lateral_safe_distance` per object × per
/// pose). Position is the centroid in world frame; heading is the object's
/// motion direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerceivedObject {
    pub id: u64,
    pub pos: Point,
    pub velocity_mps: f64,
    pub heading_rad: f64,
    /// Reported map-frame ground-velocity **vector** `{x_m, y_m}` (m/s),
    /// preserved from the upstream Autoware twist (KIRRA-OCCY-PMON-003 §5
    /// sub-decision = PRESERVE). `velocity_mps` stays the magnitude RSS uses;
    /// `vel` feeds the Track-C kinematic-plausibility ceiling (a vector-
    /// magnitude check). Frame note: rests on the upstream message contract
    /// being map/world-frame absolute — see PMON-003 §4 / D4.
    pub vel: Point,
}
