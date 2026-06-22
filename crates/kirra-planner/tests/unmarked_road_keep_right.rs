//! End-to-end: **unmarked road → keep-right → Occy → KIRRA**.
//!
//! On a road with no painted centerline (an undivided two-lane road, or a dirt
//! road) perception reports **one wide drivable corridor**. Following that
//! corridor's centerline would drive the ego **down the middle** — wrong, and
//! unsafe w.r.t. oncoming traffic. The US rule of the road (keep **right**) still
//! applies with no paint on the ground.
//!
//! This proves the substrate applies keep-right **structurally**:
//! [`LaneGraph::from_undivided_corridor`] splits the wide road into a right-half
//! ego lane and a left-half oncoming lane (divided by an unmarked, crossable
//! centerline), and the ego "keeps right" simply by following its synthesized
//! lane. The contrast test shows the divergence: naive centering drives the
//! middle; the keep-right derivation drives the right half — and KIRRA admits it.

use kirra_planner::{
    EgoState, FleetPosture, GeometricPlanner, Goal, LaneGraph, PlanInput, Planner, Pose,
    TrajectoryVerdict,
};
use kirra_ros2_adapter::corridor::{CorridorSource, MockCorridorSource};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

/// Plan straight ahead along `map`, ego starting on the road center (y=0), and
/// return the most-rightward (min) `y` the path reaches + KIRRA's verdict.
fn drive(map: &dyn CorridorSource) -> (f64, TrajectoryVerdict) {
    let input = PlanInput {
        ego: EgoState {
            pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal { target: Pose { x_m: 40.0, y_m: 0.0, heading_rad: 0.0 } },
        map,
        objects: &[],
        controls: &[],
        lane_boundaries: &[],
        motion: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture: FleetPosture::Nominal,
    };
    let mut planner = GeometricPlanner::default();
    let plan = planner.plan(&input);
    let verdict = validate_trajectory_slow(
        &plan.trajectory,
        map,
        &[],
        &VehicleConfig::default_urban(),
        None,
        FleetPosture::Nominal,
    );
    let min_y = plan.trajectory.iter().map(|t| t.pose.y_m).fold(0.0, f64::min);
    (min_y, verdict)
}

#[test]
fn keep_right_drives_the_right_half_not_the_middle() {
    // One wide (±5) undivided road — no centerline paint.
    let road = MockCorridorSource::straight_5m_half_width(100.0);

    // Naive: follow the wide corridor directly → drives down the MIDDLE (y≈0).
    let (naive_min_y, naive_verdict) = drive(&road);
    assert!(naive_min_y > -0.5, "naive centering drives the middle, got min_y {naive_min_y}");
    assert!(matches!(naive_verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp));

    // Keep-right: synthesize the two-lane split and follow the EGO (right) lane.
    let g = LaneGraph::from_undivided_corridor(&road, 1, 2).expect("synthesize undivided road");
    let ego_lane = g.lane(1).unwrap().corridor(0.95, 10);
    let (kr_min_y, kr_verdict) = drive(&ego_lane);
    assert!(kr_min_y <= -2.0, "keep-right drives the right half, got min_y {kr_min_y}");
    assert!(
        matches!(kr_verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "KIRRA admits the keep-right plan, got {kr_verdict:?}"
    );

    // The point: same road, the keep-right derivation sits a full half-lane right
    // of the naive one.
    assert!(kr_min_y < naive_min_y - 1.5, "keep-right is meaningfully right of center");
}
