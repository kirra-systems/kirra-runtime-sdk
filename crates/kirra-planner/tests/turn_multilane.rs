//! End-to-end: **a multi-lane turn route — route-around within the turn → Occy → KIRRA**.
//!
//! The payoff of the widened TurnAt grounding (#526/#527 gave a single-lane turn; this adds
//! the *width*). On a turn route whose lanes have lateral neighbors, grounding sets `map` =
//! `route_corridor` (the reference path through the turn), `drivable` = `route_drivable`
//! (the route + its neighbors — the borrowable width), and `lane_boundaries` =
//! `boundaries_relative_to(ego_lane, lane + neighbors)` (the lines). So the planner can
//! route-around an obstacle on the route by borrowing the neighbor lane across a *crossable*
//! divider, and KIRRA bounds the result — the mid-route maneuver a single-lane turn corridor
//! could not express.
//!
//! The route-around here is on the (straight) approach segment of the turn route, so the
//! geometry is robust; the mechanism (borrow the neighbor width along the route) is the same
//! anywhere on the route.

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, Goal, Lane, LaneEdge, LaneGraph,
    LineType, MickIntent, PerceivedObject, PlanInput, PlanOutput, Planner, Pose, TrajectoryVerdict,
    TurnDirection,
};
use kirra_ros2_adapter::corridor::{CorridorSource, Point};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

/// A LEFT-turn junction whose approach is a WIDE two-lane carriageway with a crossable
/// (Broken) divider — wide and long enough for the planner's overtake-based route-around to
/// fit (its ≥4.5 m RSS clearance + ~13 m ramp; a single 3.5 m neighbor on a short approach
/// is too tight — the documented narrow-road bound). A lighter lane-change route-around that
/// would use the width on tight turns too is a tracked follow-up.
///   1  ego approach (0,0)→(APR,0), y=0, LeftNeighbor 2, Successor → 3 (left arc)
///   2  wide left neighbor (0,4.5)→(APR,4.5), half 2.75, RightNeighbor 1
///   3  left-turn arc from (APR,0) up to (APR+R, R), heading +π/2, Successor → 5
///   5  north exit
const APR: f64 = 40.0;
fn multilane_turn(r: f64) -> LaneGraph {
    let arc: Vec<Point> = (0..=12)
        .map(|i| {
            let t = -std::f64::consts::FRAC_PI_2 + std::f64::consts::FRAC_PI_2 * (i as f64 / 12.0);
            Point { x_m: APR + r * t.cos(), y_m: r + r * t.sin() }
        })
        .collect();
    let turn = Lane {
        id: 3,
        centerline: arc,
        half_width_m: 1.75,
        left_line: LineType::Solid,
        right_line: LineType::Solid,
        heading_rad: std::f64::consts::FRAC_PI_2,
        edges: vec![LaneEdge::Successor { to: 5 }],
        control: None,
    };
    let exit = Lane {
        id: 5,
        centerline: vec![Point { x_m: APR + r, y_m: r }, Point { x_m: APR + r, y_m: r + 20.0 }],
        half_width_m: 1.75,
        left_line: LineType::Solid,
        right_line: LineType::Solid,
        heading_rad: std::f64::consts::FRAC_PI_2,
        edges: Vec::new(),
        control: None,
    };
    LaneGraph::new()
        .with_lane(
            // ego lane: Broken divider on the LEFT (to lane 2), Solid road edge on the right.
            Lane::straight(1, 0.0, 0.0, APR, 1.75, LineType::Broken, LineType::Solid)
                .with_edge(LaneEdge::LeftNeighbor { to: 2 })
                .with_edge(LaneEdge::Successor { to: 3 }),
        )
        .with_lane(
            // wide neighbor (half 2.75): its right edge (4.5−2.75=1.75) meets the ego lane's
            // left edge → a clean Broken divider, and its left edge (7.25) gives the overtake
            // room to fit the 4.5 m clearance + footprint.
            Lane::straight(2, 4.5, 0.0, APR, 2.75, LineType::Solid, LineType::Broken)
                .with_edge(LaneEdge::RightNeighbor { to: 1 }),
        )
        .with_lane(turn)
        .with_lane(exit)
}

fn ego_world<'a>(g: &'a LaneGraph, map: &'a dyn CorridorSource, objects: &'a [PerceivedObject], goal: Pose) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState { pose: Pose { x_m: 4.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 3.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: goal },
        map,
        objects,
        controls: &[],
        lane_boundaries: &[],
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: Some(g),
        signal_states: &[],    }
}

/// Records the drivable presence + boundary lines a grounding handed the planner.
struct WidthRecorder {
    drivable_present: bool,
    boundaries: Vec<kirra_planner::LaneBoundary>,
}
impl Planner for WidthRecorder {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        self.drivable_present = input.drivable.is_some();
        self.boundaries = input.lane_boundaries.to_vec();
        PlanOutput::safe_stop(input.ego.pose)
    }
}

#[test]
fn turn_at_grounding_widens_the_drivable_and_supplies_the_divider_line() {
    // The wiring: a TurnAt onto a route whose approach has a neighbor gets a wide drivable
    // and the typed divider line — the inputs a within-turn route-around needs.
    let g = multilane_turn(12.0);
    let map = g.lane(1).unwrap().corridor(0.95, 0);
    let goal = Pose { x_m: 52.0, y_m: 24.0, heading_rad: std::f64::consts::FRAC_PI_2 };
    let w = ego_world(&g, &map, &[], goal);

    let mut rec = WidthRecorder { drivable_present: false, boundaries: Vec::new() };
    let _ = plan_for_intent(&mut rec, &MickIntent::TurnAt { direction: TurnDirection::Left }, &w);

    assert!(rec.drivable_present, "TurnAt grounds with a widened drivable area");
    assert!(
        rec.boundaries.iter().any(|b| b.line == LineType::Broken && (b.y_m - 1.75).abs() < 1e-6),
        "the crossable divider to the neighbor lane is supplied, got {:?}",
        rec.boundaries
    );
}

#[test]
fn the_ego_routes_around_an_obstacle_by_borrowing_the_neighbor_lane_and_kirra_admits() {
    // An obstacle blocks the ego's lane on the turn route's approach. With only a single-lane
    // turn corridor the ego could merely stop short; with the widened drivable it borrows the
    // neighbor lane (crossing the Broken divider), and KIRRA admits the pass.
    let g = multilane_turn(12.0);
    let map = g.lane(1).unwrap().corridor(0.95, 0);
    let drivable = g.route_drivable(&g.route(1, 5).unwrap(), 0.95, 0).unwrap();
    let stopped = [PerceivedObject { id: 1, pos: Point { x_m: 25.0, y_m: 0.0 }, velocity_mps: 0.0, heading_rad: 0.0, vel: Point { x_m: 0.0, y_m: 0.0 } }];
    let goal = Pose { x_m: 52.0, y_m: 24.0, heading_rad: std::f64::consts::FRAC_PI_2 };
    let w = ego_world(&g, &map, &stopped, goal);

    let plan = plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::TurnAt { direction: TurnDirection::Left }, &w);

    let max_y = plan.trajectory.iter().map(|t| t.pose.y_m).fold(f64::MIN, f64::max);
    assert!(max_y > 1.75, "the ego borrows the neighbor lane (crosses the +1.75 divider), got max_y {max_y}");

    let verdict = validate_trajectory_slow(&plan.trajectory, &drivable, &stopped, &VehicleConfig::default_urban(), None, FleetPosture::Nominal);
    assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp), "KIRRA admits the borrow-the-neighbor route-around, got {verdict:?}");
}
