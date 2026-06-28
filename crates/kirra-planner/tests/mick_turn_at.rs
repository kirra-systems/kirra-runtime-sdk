//! End-to-end: **Mick `TurnAt` intent → route through the junction → Occy → KIRRA**.
//!
//! The full doer path for an intersection turn. Mick chooses a *direction*; grounding
//! resolves it against the lane graph (successor-by-heading), routes through the branch,
//! materializes the route corridor (`route_corridor`), and the planner follows it through
//! the turn — KIRRA bounding the result. The brain never touches the map; grounding does
//! all the routing, and the safety case is unchanged (proven in #526's turn tests).
//!
//! Junction convention used here: a turn-branch lane's `heading_rad` is its OUTGOING
//! (destination) direction, so a left branch reads `+π/2` and a right branch `−π/2` — which
//! is exactly what `TurnDirection`'s ±45° band resolves against.

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, Goal, Lane, LaneEdge, LaneGraph,
    LineType, MickIntent, PlanInput, PlanOutput, Pose, ProposalKind, TrajectoryVerdict,
    TurnDirection,
};
use kirra_trajectory::corridor::Point;
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

const R: f64 = 12.0;
const HALF: f64 = 3.0;

/// A quarter-arc (n+1 pts) of radius `r` about `(cx, cy)`, from `start` sweeping `sweep`.
fn arc(cx: f64, cy: f64, r: f64, start: f64, sweep: f64, n: usize) -> Vec<Point> {
    (0..=n)
        .map(|i| {
            let t = start + sweep * (i as f64 / n as f64);
            Point { x_m: cx + r * t.cos(), y_m: cy + r * t.sin() }
        })
        .collect()
}

fn lane(id: u64, centerline: Vec<Point>, heading_rad: f64, succ: Option<u64>) -> Lane {
    let edges = succ.map(|s| vec![LaneEdge::Successor { to: s }]).unwrap_or_default();
    Lane { id, centerline, half_width_m: HALF, left_line: LineType::Solid, right_line: LineType::Solid, heading_rad, edges, control: None }
}

/// A junction with a LEFT and a RIGHT branch off a straight approach (no straight branch):
///   1  approach (0,0)→(20,0), east, successors {2, 4}
///   2  left arc (20,0)→(20+R,R), heading +π/2 → 3 north exit
///   4  right arc (20,0)→(20+R,−R), heading −π/2 → 5 south exit
fn junction() -> LaneGraph {
    let mut g = LaneGraph::new().with_lane(
        Lane::straight(1, 0.0, 0.0, 20.0, HALF, LineType::Solid, LineType::Solid)
            .with_edge(LaneEdge::Successor { to: 2 })
            .with_edge(LaneEdge::Successor { to: 4 }),
    );
    // Left branch: arc up, then a north exit.
    g.add_lane(lane(2, arc(20.0, R, R, -std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2, 12), std::f64::consts::FRAC_PI_2, Some(3)));
    g.add_lane(lane(3, vec![Point { x_m: 20.0 + R, y_m: R }, Point { x_m: 20.0 + R, y_m: R + 20.0 }], std::f64::consts::FRAC_PI_2, None));
    // Right branch: arc down, then a south exit.
    g.add_lane(lane(4, arc(20.0, -R, R, std::f64::consts::FRAC_PI_2, -std::f64::consts::FRAC_PI_2, 12), -std::f64::consts::FRAC_PI_2, Some(5)));
    g.add_lane(lane(5, vec![Point { x_m: 20.0 + R, y_m: -R }, Point { x_m: 20.0 + R, y_m: -R - 20.0 }], -std::f64::consts::FRAC_PI_2, None));
    g
}

/// Ground a `TurnAt` from the approach near the junction mouth. `goal` is the exit-lane
/// target the planner drives toward along the materialized route corridor.
fn ground_turn<'a>(g: &'a LaneGraph, ego_corr: &'a dyn kirra_trajectory::corridor::CorridorSource, goal: Pose, dir: TurnDirection) -> PlanOutput {
    let input = PlanInput {
        ego: EgoState { pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 2.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: goal },
        map: ego_corr,
        objects: &[],
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
        signal_states: &[],    };
    plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::TurnAt { direction: dir }, &input)
}

#[test]
fn mick_turn_left_grounds_through_the_junction_and_kirra_admits() {
    let g = junction();
    let ego_corr = g.lane(1).unwrap().corridor(0.95, 0);
    let goal = Pose { x_m: 20.0 + R, y_m: R + 12.0, heading_rad: std::f64::consts::FRAC_PI_2 };
    let plan = ground_turn(&g, &ego_corr, goal, TurnDirection::Left);

    assert_eq!(plan.kind, ProposalKind::Motion, "TurnAt grounds to a drive, not HOLD");
    let max_heading = plan.trajectory.iter().map(|p| p.pose.heading_rad).fold(f64::MIN, f64::max);
    let max_y = plan.trajectory.iter().map(|p| p.pose.y_m).fold(f64::MIN, f64::max);
    assert!(max_heading > 1.0, "the path turns LEFT (toward north), got max heading {max_heading}");
    assert!(max_y > 0.6 * R, "and climbs the left branch, got max_y {max_y}");

    // KIRRA bounds the grounded turn against the same route corridor grounding followed.
    let route = g.route(1, 3).unwrap();
    let corr = g.route_corridor(&route, 0.95, 0).unwrap();
    let verdict = validate_trajectory_slow(&plan.trajectory, &corr, &[], &VehicleConfig::default_urban(), None, FleetPosture::Nominal);
    assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp), "KIRRA admits the turn, got {verdict:?}");
}

#[test]
fn mick_turn_right_picks_the_other_branch() {
    // The same junction, direction RIGHT → the SOUTH branch (min_y negative): the
    // successor-by-heading resolution selects by direction, not just "a branch exists".
    let g = junction();
    let ego_corr = g.lane(1).unwrap().corridor(0.95, 0);
    let goal = Pose { x_m: 20.0 + R, y_m: -R - 12.0, heading_rad: -std::f64::consts::FRAC_PI_2 };
    let plan = ground_turn(&g, &ego_corr, goal, TurnDirection::Right);

    let min_y = plan.trajectory.iter().map(|p| p.pose.y_m).fold(f64::MAX, f64::min);
    assert!(min_y < -0.6 * R, "RIGHT takes the south branch, got min_y {min_y}");
}

#[test]
fn a_turn_with_no_such_branch_fails_closed_to_hold() {
    // This junction has no STRAIGHT branch off lane 1 → TurnAt Straight must HOLD.
    let g = junction();
    let ego_corr = g.lane(1).unwrap().corridor(0.95, 0);
    let goal = Pose { x_m: 60.0, y_m: 0.0, heading_rad: 0.0 };
    let plan = ground_turn(&g, &ego_corr, goal, TurnDirection::Straight);

    assert_eq!(plan.kind, ProposalKind::SafeStop, "no straight branch → fail-closed HOLD");
    assert!(plan.trajectory.iter().all(|p| p.velocity_mps == 0.0));
}

#[test]
fn a_turn_without_a_lane_graph_fails_closed_to_hold() {
    // No lane graph supplied → TurnAt cannot resolve → HOLD (the loop's safe default).
    let corr = kirra_trajectory::corridor::MockCorridorSource::straight_5m_half_width(100.0);
    let input = PlanInput {
        ego: EgoState { pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 2.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: Pose { x_m: 40.0, y_m: 0.0, heading_rad: 0.0 } },
        map: &corr,
        objects: &[],
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
        lane_graph: None,
        signal_states: &[],    };
    let plan = plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::TurnAt { direction: TurnDirection::Left }, &input);
    assert_eq!(plan.kind, ProposalKind::SafeStop, "no graph → fail-closed HOLD");
}
