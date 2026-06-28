//! Deterministic proof for **multi-junction routing** (`MickIntent::RouteTo`): a scripted
//! `RouteTo` brain is handed a destination several junctions away, and the ego drives the whole
//! **two-junction** route closed-loop — LEFT through the first junction, NORTH along the
//! connector, then RIGHT through the second — reaching the final east-bound exit lane, with
//! KIRRA bounding every pose it takes.
//!
//! This is the destination-routing sibling of `intersection_closed_loop.rs` (one junction,
//! `TurnAt`): there the brain names a turn; here it names a *place*, and the planner resolves the
//! lane-id route across BOTH junctions (`LaneGraph::route_to_point`, Dijkstra — it must pick the
//! correct branch at the first junction over a decoy that dead-ends), materializes that whole
//! route's corridor (curving through every junction), and follows it. The route is re-resolved
//! from the ego *pose* each replan, so as the ego advances lane by lane the remaining route
//! shrinks — receding-horizon multi-junction following with no per-tick route state. The same
//! dual-rate loop drives it: the slow loop re-plans/validates (promoting only KIRRA-ADMITTED
//! plans), the fast loop tracks the admitted trajectory by elapsed time.

use kirra_planner::{
    EgoState, FastLoopTracker, FleetPosture, GeometricPlanner, Goal, Lane, LaneCorridor, LaneEdge,
    LaneGraph, LineType, MickDriver, MickIntent, PlanInput, Pose, ScriptedBrain, TrajectoryVerdict,
};
use kirra_trajectory::corridor::{CorridorSource, Point};
use kirra_trajectory::state::PerceivedObject;
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

const FAST_DT_S: f64 = 0.1;
const FAST_DT_MS: u64 = 100;
const TICKS: usize = 240;
const MRC_DECEL: f64 = 3.0;
const REPLAN_MS: u64 = 500;
const R: f64 = 12.0;

/// A quarter-circle arc (n+1 points) sweeping `sweep` rad (±π/2) from `start_angle` about
/// `(cx, cy)` (+ = CCW/left, − = CW/right).
fn quarter_arc(cx: f64, cy: f64, r: f64, start_angle: f64, sweep: f64, n: usize) -> Vec<Point> {
    (0..=n)
        .map(|i| {
            let t = start_angle + sweep * (i as f64 / n as f64);
            Point { x_m: cx + r * t.cos(), y_m: cy + r * t.sin() }
        })
        .collect()
}

/// Two-junction route: straight east (1) → LEFT arc (2) → straight north (3) → RIGHT arc (4) →
/// straight east (5). Lane 1 carries a DECOY right branch (6, a dead-end south) so reaching the
/// destination requires the router to pick the correct branch at the first junction.
fn two_junction_route() -> LaneGraph {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
    let arc_left = quarter_arc(30.0, 12.0, R, -FRAC_PI_2, FRAC_PI_2, 12); // (30,0)→(42,12) east→north (CCW)
    let arc_right = quarter_arc(54.0, 40.0, R, PI, -FRAC_PI_2, 12); // (42,40)→(54,52) north→east (CW)
    let lane = |id, cl: Vec<Point>, heading, succ: &[u64]| Lane {
        id,
        centerline: cl,
        half_width_m: 3.0,
        left_line: LineType::Solid,
        right_line: LineType::Solid,
        heading_rad: heading,
        edges: succ.iter().map(|&to| LaneEdge::Successor { to }).collect(),
        control: None,
    };
    LaneGraph::new()
        .with_lane(lane(1, vec![Point { x_m: 0.0, y_m: 0.0 }, Point { x_m: 30.0, y_m: 0.0 }], 0.0, &[2, 6]))
        .with_lane(lane(2, arc_left, FRAC_PI_4, &[3]))
        .with_lane(lane(3, vec![Point { x_m: 42.0, y_m: 12.0 }, Point { x_m: 42.0, y_m: 40.0 }], FRAC_PI_2, &[4]))
        .with_lane(lane(4, arc_right, FRAC_PI_4, &[5]))
        .with_lane(lane(5, vec![Point { x_m: 54.0, y_m: 52.0 }, Point { x_m: 80.0, y_m: 52.0 }], 0.0, &[]))
        .with_lane(lane(6, vec![Point { x_m: 30.0, y_m: 0.0 }, Point { x_m: 30.0, y_m: -20.0 }], -FRAC_PI_2, &[]))
}

#[allow(clippy::too_many_arguments)]
fn world<'a>(ego: EgoState, map: &'a dyn CorridorSource, objects: &'a [PerceivedObject], g: &'a LaneGraph, goal: Pose) -> PlanInput<'a> {
    PlanInput {
        ego,
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
        signal_states: &[],
    }
}

#[test]
fn route_to_drives_the_whole_two_junction_route_kirra_bounding_throughout() {
    let g = two_junction_route();
    // Sanity: the router selects the correct branch at the first junction (over the decoy) and
    // stitches the full route across BOTH turns.
    let route_ids = g.route(1, 5).expect("route across both junctions");
    assert_eq!(route_ids, vec![1, 2, 3, 4, 5], "the router picks the left branch at J1, not the decoy");
    // The full-route corridor KIRRA validates against (the planner re-materializes its own sub-
    // corridor from the current ego lane each tick; that is contained in this one).
    let route: LaneCorridor = g.route_corridor(&route_ids, 0.95, 5).expect("stitch the route corridor");

    let goal = Pose { x_m: 72.0, y_m: 52.0, heading_rad: 0.0 };
    let objs: [PerceivedObject; 0] = [];

    // The brain just keeps naming the destination; the planner re-routes from the ego pose every
    // replan, so the remaining route shrinks as the ego advances through each junction.
    let mut driver = MickDriver::new(ScriptedBrain::new(vec![MickIntent::RouteTo { x_m: 72.0, y_m: 52.0 }; 120]));
    let mut occy = GeometricPlanner::default();
    let mut tracker = FastLoopTracker::new();

    let mut ego = EgoState { pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 5.0, yaw_rate_rads: 0.0, stamp_ms: 0 };
    let mut last_replan_ms: Option<u64> = None;
    let mut ego_max_heading = f64::MIN;
    let (mut promotions, mut admitted_promotions) = (0u32, 0u32);

    for tick in 1..=TICKS {
        let now_ms = tick as u64 * FAST_DT_MS;

        let replan_due = tracker.is_exhausted(now_ms) || last_replan_ms.is_none_or(|t| now_ms.saturating_sub(t) >= REPLAN_MS);
        if replan_due {
            let w = world(ego, &route, &objs, &g, goal);
            let plan = driver.drive_tick(&w, &mut occy, now_ms);
            let v = validate_trajectory_slow(&plan.trajectory, &route, &objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal);
            promotions += 1;
            if matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp) {
                admitted_promotions += 1;
                tracker.promote(plan, now_ms);
                last_replan_ms = Some(now_ms);
            }
        }

        ego = match tracker.track(now_ms) {
            Some(cmd) => EgoState { pose: cmd.pose, linear_x_mps: cmd.velocity_mps, yaw_rate_rads: 0.0, stamp_ms: now_ms },
            None => EgoState { pose: ego.pose, linear_x_mps: (ego.linear_x_mps - MRC_DECEL * FAST_DT_S).max(0.0), yaw_rate_rads: 0.0, stamp_ms: now_ms },
        };
        ego_max_heading = ego_max_heading.max(ego.pose.heading_rad);
    }

    println!("ego_final=({:.2},{:.2})  ego_max_heading={ego_max_heading:.2}  admitted={admitted_promotions}/{promotions}", ego.pose.x_m, ego.pose.y_m);

    // (1) KIRRA bounds every pose: the fast loop only ever tracks a KIRRA-admitted trajectory.
    assert!(admitted_promotions > 0, "KIRRA admitted the trajectories the fast loop tracked");
    // (2) The ego swung NORTH negotiating the first (left) junction — heading reached ≈ π/2.
    assert!(ego_max_heading > 1.4, "the ego turns north through the first junction (≈π/2), got max heading {ego_max_heading}");
    // (3) The ego completed BOTH turns and reached the final EAST-bound exit lane (lane 5 runs
    //     x 54..80 at y≈52) — only reachable by routing the left arc, the connector, and the
    //     right arc in sequence. This is the multi-junction route driven end to end.
    assert!(ego.pose.y_m > 45.0, "the ego reaches the final exit lane (y≈52), got y={}", ego.pose.y_m);
    assert!(ego.pose.x_m > 58.0, "and travels east along it past the second junction, got x={}", ego.pose.x_m);
}
