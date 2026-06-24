//! End-to-end: **follow a route through a junction turn → Occy → KIRRA**.
//!
//! The map-side TurnAt primitive. A route `[ego_lane → arc junction_lane → exit_lane]`
//! is materialized into one continuous corridor by `LaneGraph::route_corridor` (the
//! longitudinal counterpart to `corridor_over`), handed to the planner as `map`, and the
//! EXISTING planner follows the curving centerline through the turn while KIRRA bounds it
//! (containment + steering-rate / lateral-accel along the arc). No `plan()` surgery — a
//! turn is a curved corridor, which the planner already smooths, speeds for curvature, and
//! the checker already bounds.
//!
//! The doer-checker split holds at the junction: Occy proposes following the route arc;
//! KIRRA admits a gently-radiused turn and refuses one too tight to take safely.
//!
//! The Mick `TurnAt { direction }` intent (which needs the LaneGraph threaded into the
//! grounding seam) is a tracked follow-up; this proves the maneuver it will drive.

use kirra_planner::{
    EgoState, FleetPosture, GeometricPlanner, Goal, Lane, LaneEdge, LaneGraph, LineType, PlanInput,
    Planner, Pose, ProposalKind, TrajectoryVerdict,
};
use kirra_ros2_adapter::corridor::Point;
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

/// A quarter-circle arc (n+1 points) sweeping +π/2 about `(cx, cy)` from `start_angle` —
/// a smooth LEFT-turn centerline.
fn quarter_arc(cx: f64, cy: f64, r: f64, start_angle: f64, n: usize) -> Vec<Point> {
    (0..=n)
        .map(|i| {
            let t = start_angle + std::f64::consts::FRAC_PI_2 * (i as f64 / n as f64);
            Point { x_m: cx + r * t.cos(), y_m: cy + r * t.sin() }
        })
        .collect()
}

/// A left-turn junction of arc radius `r` and lane half-width `half`:
///   lane 1 — straight ego approach (0,0)→(20,0), heading east;
///   lane 2 — quarter-arc turning LEFT from (20,0) up to (20+r, r);
///   lane 3 — straight exit running NORTH (20+r, r)→(20+r, r+20).
fn left_turn(r: f64, half: f64) -> LaneGraph {
    let line = LineType::Solid;
    let arc = quarter_arc(20.0, r, r, -std::f64::consts::FRAC_PI_2, 12);
    let junction = Lane {
        id: 2,
        centerline: arc,
        half_width_m: half,
        left_line: line,
        right_line: line,
        heading_rad: std::f64::consts::FRAC_PI_4,
        edges: vec![LaneEdge::Successor { to: 3 }],
        control: None,    };
    let exit = Lane {
        id: 3,
        centerline: vec![Point { x_m: 20.0 + r, y_m: r }, Point { x_m: 20.0 + r, y_m: r + 20.0 }],
        half_width_m: half,
        left_line: line,
        right_line: line,
        heading_rad: std::f64::consts::FRAC_PI_2,
        edges: Vec::new(),
        control: None,
    };
    LaneGraph::new()
        .with_lane(
            Lane::straight(1, 0.0, 0.0, 20.0, half, line, line).with_edge(LaneEdge::Successor { to: 2 }),
        )
        .with_lane(junction)
        .with_lane(exit)
}

/// Plan from the ego approach toward a goal up the exit lane, following the stitched route
/// corridor; return the plan + KIRRA's verdict on it (checked against the same corridor).
fn plan_turn(r: f64, half: f64) -> (kirra_planner::PlanOutput, TrajectoryVerdict, f64) {
    let g = left_turn(r, half);
    let route = g.route(1, 3).expect("route through the junction");
    let map = g.route_corridor(&route, 0.95, 10).expect("stitch the route corridor");

    let input = PlanInput {
        ego: EgoState {
            // Near the junction mouth, so one planning horizon covers the bulk of the turn
            // (the closed loop completes it over ticks). A few metres of approach remain so
            // the footprint sits inside the corridor, not poking behind its start.
            pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        // Goal up the exit lane (north of the turn).
        goal: Goal { target: Pose { x_m: 20.0 + r, y_m: r + 12.0, heading_rad: std::f64::consts::FRAC_PI_2 } },
        map: &map,
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
    };
    let mut occy = GeometricPlanner::default();
    let plan = occy.plan(&input);
    let verdict = validate_trajectory_slow(
        &plan.trajectory, &map, &[], &VehicleConfig::default_urban(), None, FleetPosture::Nominal,
    );
    let max_heading = plan
        .trajectory
        .iter()
        .map(|p| p.pose.heading_rad)
        .fold(f64::MIN, f64::max);
    (plan, verdict, max_heading)
}

#[test]
fn the_planner_follows_a_gentle_junction_turn_and_kirra_admits() {
    // A comfortably-radiused left turn: the path swings from heading-east toward
    // heading-north (it genuinely turns), reaches up into the exit lane, and KIRRA
    // admits it (the curvature is within the steering-rate / lateral-accel envelope).
    let r = 12.0;
    let (plan, verdict, max_heading) = plan_turn(r, 3.0);

    assert_eq!(plan.kind, ProposalKind::Motion, "the ego drives the turn, not HOLD");
    // It turns: heading climbs well past 45° toward the north (π/2) exit.
    assert!(max_heading > 1.0, "the path turns toward north, got max heading {max_heading} rad");
    // It drives deep into the turn — most of the way up the arc toward the exit (the
    // final stretch completes over subsequent ticks; one horizon covers the bulk).
    let max_y = plan.trajectory.iter().map(|p| p.pose.y_m).fold(f64::MIN, f64::max);
    assert!(max_y > 0.6 * r, "the path climbs through the turn, got max_y {max_y} (arc top {r})");
    assert!(
        matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "a gentle turn is within the envelope → KIRRA admits, got {verdict:?}"
    );
}

#[test]
fn kirra_bounds_a_too_tight_junction_turn() {
    // The doer-checker payoff. A very tight radius forces curvature beyond the comfortable
    // envelope; Occy still proposes following the route arc, but KIRRA does not pass it
    // cleanly — the steering-rate / containment bound bites (a Clamp derate at minimum, or
    // an outright MRC refusal). The checker, not the planner, owns the turn's safety.
    let (_, gentle, _) = plan_turn(12.0, 3.0);
    let (_, tight, _) = plan_turn(3.0, 3.0);

    assert!(
        matches!(gentle, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "control: the gentle turn admits, got {gentle:?}"
    );
    assert_ne!(
        tight, TrajectoryVerdict::Accept,
        "a too-tight turn is NOT cleanly accepted — KIRRA bounds it (Clamp or MRC), got {tight:?}"
    );
}
