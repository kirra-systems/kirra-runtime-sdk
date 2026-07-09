//! End-to-end: **lane graph → Occy → KIRRA**.
//!
//! The Lanelet2-lite lane substrate ([`kirra_planner::LaneGraph`]) is the map side
//! the planner was missing. This wires it the whole way through for the first time:
//!
//! 1. Build a three-lane road in the graph with the ego in the **center** lane. The
//!    divider to the **right** lane is a **broken** line (crossable); the divider to
//!    the **left** lane is **solid** (no crossing); the outer edges are solid.
//! 2. **Derive** Occy's inputs from the graph — the drivable corridor spanning all
//!    three lanes ([`LaneGraph::corridor_over`], symmetric about the center lane) and
//!    the typed lane boundaries at their real positions
//!    ([`LaneGraph::boundaries_relative_to`]).
//! 3. Command a lane change and let **KIRRA** judge Occy's proposal.
//!
//! The load-bearing property: the lane-line *type* the graph carries decides the
//! maneuver. A change across the **broken** right divider is lawful → Occy shifts
//! and KIRRA admits the contained trajectory. A change across the **solid** left
//! divider is unlawful → Occy refuses (stays in lane) — the rule comes from the
//! map, not a hand-fed literal. The ego sits on the corridor centerline (center
//! lane), so a refused change holds the centerline rather than drifting.

use kirra_planner::{
    EgoState, FleetPosture, GeometricPlanner, Goal, Lane, LaneEdge, LaneGraph, LineType, PlanInput,
    Planner, Pose, TrajectoryVerdict,
};
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

/// Three-lane road, 3.5 m lanes (half-width 1.75): left lane at y=+3.5, center
/// (ego) lane at y=0, right lane at y=-3.5. Right divider BROKEN, left divider
/// SOLID, outer edges SOLID. The shared dividers carry the same `LineType` on both
/// abutting lanes.
fn three_lane_road() -> LaneGraph {
    LaneGraph::new()
        .with_lane(
            // Left lane: outer-left edge solid, divider to center solid.
            Lane::straight(0, 3.5, 0.0, 100.0, 1.75, LineType::Solid, LineType::Solid)
                .with_edge(LaneEdge::RightNeighbor { to: 1 }),
        )
        .with_lane(
            // Center (ego) lane: solid on the left, broken on the right.
            Lane::straight(1, 0.0, 0.0, 100.0, 1.75, LineType::Solid, LineType::Broken)
                .with_edge(LaneEdge::LeftNeighbor { to: 0 })
                .with_edge(LaneEdge::RightNeighbor { to: 2 }),
        )
        .with_lane(
            // Right lane: divider to center broken, outer-right edge solid.
            Lane::straight(2, -3.5, 0.0, 100.0, 1.75, LineType::Broken, LineType::Solid)
                .with_edge(LaneEdge::LeftNeighbor { to: 1 }),
        )
}

const SPAN: [u64; 3] = [0, 1, 2];

#[test]
fn lane_change_across_broken_divider_admits() {
    // Commanded change into the right lane (-3.5) crosses the BROKEN divider →
    // lawful. Occy shifts the held trajectory into the target lane, and KIRRA
    // admits it (the corridor derived from the graph spans all three lanes).
    let g = three_lane_road();
    let corridor = g
        .corridor_over(&SPAN, 0.95, 10)
        .expect("corridor over the span");
    let boundaries = g.boundaries_relative_to(1, &SPAN).expect("boundaries");

    let input = PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: 8.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal {
            target: Pose {
                x_m: 40.0,
                y_m: -3.5,
                heading_rad: 0.0,
            },
        },
        map: &corridor,
        objects: &[],
        controls: &[],
        lane_boundaries: &boundaries,
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: Some(-3.5),
        no_overtake_ids: &[],
        drivable: None,
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
        signal_states: &[],
    };
    let mut planner = GeometricPlanner::default();
    let plan = planner.plan(&input);
    let verdict = validate_trajectory_slow(
        &plan.trajectory,
        &corridor,
        &[],
        &VehicleConfig::default_urban(),
        None,
        FleetPosture::Nominal,
    );

    let min_y = plan
        .trajectory
        .iter()
        .map(|t| t.pose.y_m)
        .fold(0.0, f64::min);
    assert!(
        min_y <= -3.0,
        "shifts into the right lane, got min_y {min_y}"
    );
    assert!(
        matches!(
            verdict,
            TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
        ),
        "KIRRA admits the lane-graph-derived lane change, got {verdict:?}"
    );
}

#[test]
fn lane_change_across_solid_divider_is_refused() {
    // Commanded change to the left (+3.5) would cross the SOLID divider at +1.75 →
    // unlawful. Occy refuses (holds the center-lane centerline): the rule comes
    // from the line TYPE the lane graph carries, not a hand-fed boundary literal.
    let g = three_lane_road();
    let corridor = g.corridor_over(&SPAN, 0.95, 10).unwrap();
    let boundaries = g.boundaries_relative_to(1, &SPAN).unwrap();

    let input = PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: 8.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal {
            target: Pose {
                x_m: 40.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
        },
        map: &corridor,
        objects: &[],
        controls: &[],
        lane_boundaries: &boundaries,
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: Some(3.5),
        no_overtake_ids: &[],
        drivable: None,
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
        signal_states: &[],
    };
    let mut planner = GeometricPlanner::default();
    let plan = planner.plan(&input);

    let max_y = plan
        .trajectory
        .iter()
        .map(|t| t.pose.y_m)
        .fold(0.0, f64::max);
    let min_y = plan
        .trajectory
        .iter()
        .map(|t| t.pose.y_m)
        .fold(0.0, f64::min);
    assert!(
        max_y < 0.5,
        "solid divider → no leftward lane change, got max_y {max_y}"
    );
    assert!(
        min_y > -0.5,
        "and holds the center lane (no drift), got min_y {min_y}"
    );
}
