//! Deterministic proof for the `mick_intersection` demo: a scripted goal-seeking brain drives a
//! signalized left-turn junction closed-loop and the ego **completes the turn**, under a proper
//! dual-rate loop. The TRAFFIC LIGHT (#531) HOLDs the ego at the stop line while RED (it never
//! crosses); on GREEN the ego tracks the route corridor THROUGH the arc into the exit; the
//! [`FastLoopTracker`] follows the admitted trajectory by elapsed time, so the ego **accelerates
//! from the red-light standstill and holds the curve** instead of creeping off the line (the toy
//! 0.1 s-sample conformance could do neither); the map's right-of-way (#528) derives the crossing
//! vehicle into the cede set; and the fast loop only ever conforms to a KIRRA-admitted trajectory.
//!
//! Dual-rate: the slow loop (System-2 → Occy → KIRRA) re-plans/validates every `REPLAN_MS` (and
//! when the committed trajectory is exhausted) — promoting only ADMITTED plans; the fast loop
//! tracks that trajectory each 100 ms tick. The brain heads for the goal across the junction
//! (`GoTo`), which tracks the full route corridor without per-tick lane re-resolution. A bare
//! `TurnAt` drives the same turn (route-progress, #533) but cannot yet complete it in THIS loop:
//! `Lane::contains` uses a `mean_y` bounding box that excludes a curved lane's own ends, so
//! `lane_at` returns None at the approach→arc seam and the per-tick re-resolution HOLDs there —
//! a curved-lane containment fix is the remaining follow-up.

use kirra_planner::{
    EgoState, FastLoopTracker, FleetPosture, GeometricPlanner, Goal, Lane, LaneControl,
    LaneCorridor, LaneEdge, LaneGraph, LineType, MickDriver, MickIntent, PlanInput, Pose,
    ScriptedBrain, SignalState, TrajectoryVerdict,
};
use kirra_ros2_adapter::corridor::{CorridorSource, Point};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

const FAST_DT_S: f64 = 0.1;
const FAST_DT_MS: u64 = 100;
const TICKS: usize = 160;
const MRC_DECEL: f64 = 3.0;
const GREEN_AT_MS: u64 = 3500;
const REPLAN_MS: u64 = 500; // slow-loop (System-2) re-plan cadence
const TURN_RADIUS: f64 = 12.0;
const APPROACH_END: f64 = 30.0;

fn junction() -> LaneGraph {
    let r = TURN_RADIUS;
    let arc: Vec<Point> = (0..=12)
        .map(|i| {
            let t = -std::f64::consts::FRAC_PI_2 + std::f64::consts::FRAC_PI_2 * (i as f64 / 12.0);
            Point { x_m: APPROACH_END + r * t.cos(), y_m: r + r * t.sin() }
        })
        .collect();
    LaneGraph::new()
        .with_lane(
            Lane::straight(1, 0.0, 0.0, APPROACH_END, 3.0, LineType::Solid, LineType::Solid)
                .with_control(LaneControl::TrafficLight)
                .with_edge(LaneEdge::Successor { to: 2 }),
        )
        .with_lane(Lane {
            id: 2,
            centerline: arc,
            half_width_m: 3.0,
            left_line: LineType::Solid,
            right_line: LineType::Solid,
            heading_rad: std::f64::consts::FRAC_PI_2,
            edges: vec![LaneEdge::Successor { to: 5 }],
            control: None,
        })
        .with_lane(Lane {
            id: 5,
            centerline: vec![Point { x_m: APPROACH_END + r, y_m: r }, Point { x_m: APPROACH_END + r, y_m: r + 20.0 }],
            half_width_m: 3.0,
            left_line: LineType::Solid,
            right_line: LineType::Solid,
            heading_rad: std::f64::consts::FRAC_PI_2,
            edges: Vec::new(),
            control: None,
        })
        .with_lane(
            Lane::straight(3, -10.0, 36.0, 25.0, 2.0, LineType::Solid, LineType::Solid)
                .with_heading(std::f64::consts::FRAC_PI_2),
        )
        .with_right_of_way(1, 3)
}

fn world<'a>(
    ego: EgoState,
    map: &'a dyn CorridorSource,
    objects: &'a [PerceivedObject],
    signals: &'a [(u64, SignalState)],
    g: &'a LaneGraph,
    goal: Pose,
) -> PlanInput<'a> {
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
        signal_states: signals,
    }
}

use kirra_ros2_adapter::state::PerceivedObject;

#[test]
fn gemma_holds_on_red_then_completes_the_left_turn_kirra_bounding_throughout() {
    let g = junction();
    let route: LaneCorridor = g.route(1, 5).and_then(|r| g.route_corridor(&r, 0.95, 5)).expect("route");
    // A stationary vehicle in the crossing lane (id 7), well off the left-turn path — the ego
    // has map right-of-way over it on approach (the cede is derived), and it never interferes.
    let objs = [PerceivedObject { id: 7, pos: Point { x_m: 33.0, y_m: -10.0 }, velocity_mps: 0.0, heading_rad: std::f64::consts::FRAC_PI_2, vel: Point { x_m: 0.0, y_m: 0.0 } }];
    let goal = Pose { x_m: APPROACH_END + TURN_RADIUS, y_m: 28.0, heading_rad: std::f64::consts::FRAC_PI_2 };

    // The model heads for the goal across the junction (a left turn). It tracks the full route
    // corridor (map) through the arc — so the fast-loop tracker carries the turn to completion,
    // accelerating from the red-light stop, without per-tick lane re-resolution.
    let mut driver = MickDriver::new(ScriptedBrain::new(vec![MickIntent::GoTo { x_m: 42.0, y_m: 28.0 }; 60]));
    let mut occy = GeometricPlanner::default();
    let mut tracker = FastLoopTracker::new();

    let mut ego = EgoState { pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 5.0, yaw_rate_rads: 0.0, stamp_ms: 0 };
    let mut last_replan_ms: Option<u64> = None;

    let mut red_max_x = f64::MIN;
    let cede_at_approach = g.junction_context(Point { x_m: ego.pose.x_m, y_m: ego.pose.y_m }, &objs).cedes_to_ego;
    let mut ego_max_y = f64::MIN;
    let mut ego_max_heading = f64::MIN;
    let (mut promotions, mut admitted_promotions) = (0u32, 0u32);

    for tick in 1..=TICKS {
        let now_ms = tick as u64 * FAST_DT_MS;
        let light = if now_ms < GREEN_AT_MS { SignalState::Red } else { SignalState::Green };
        let signals = [(1u64, light)];

        // SLOW loop (System-2 → Occy → KIRRA): re-plan at the slow cadence or when the
        // committed trajectory is spent. Only an ADMITTED plan is promoted to the tracker.
        let replan_due = tracker.is_exhausted(now_ms) || last_replan_ms.is_none_or(|t| now_ms.saturating_sub(t) >= REPLAN_MS);
        if replan_due {
            let w = world(ego, &route, &objs, &signals, &g, goal);
            let plan = driver.drive_tick(&w, &mut occy, now_ms);
            let v = validate_trajectory_slow(&plan.trajectory, &route, &objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal);
            promotions += 1;
            if matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp) {
                admitted_promotions += 1;
                tracker.promote(plan, now_ms);
                last_replan_ms = Some(now_ms);
            }
        }

        // FAST loop: track the committed trajectory by elapsed time; MRC if none/exhausted.
        ego = match tracker.track(now_ms) {
            Some(cmd) => EgoState { pose: cmd.pose, linear_x_mps: cmd.velocity_mps, yaw_rate_rads: 0.0, stamp_ms: now_ms },
            None => EgoState { pose: ego.pose, linear_x_mps: (ego.linear_x_mps - MRC_DECEL * FAST_DT_S).max(0.0), yaw_rate_rads: 0.0, stamp_ms: now_ms },
        };

        if light == SignalState::Red { red_max_x = red_max_x.max(ego.pose.x_m); }
        ego_max_y = ego_max_y.max(ego.pose.y_m);
        ego_max_heading = ego_max_heading.max(ego.pose.heading_rad);
    }

    println!("red_max_x={red_max_x:.2}  ego_final=({:.2},{:.2})  ego_max_y={ego_max_y:.2}  ego_max_heading={ego_max_heading:.2}  cedes={cede_at_approach:?}  admitted={admitted_promotions}/{promotions}", ego.pose.x_m, ego.pose.y_m);

    // (1) KIRRA bounds every pose the ego takes: the fast loop only ever tracks a KIRRA-admitted
    //     trajectory — a rejected re-plan is never promoted (the loop holds the last admitted
    //     one or MRCs), and KIRRA DID admit the trajectories that drove the turn. (Some transient
    //     and post-completion re-plans are rejected; the fast loop correctly never tracks those.)
    assert!(admitted_promotions > 0, "KIRRA admitted the trajectories the fast loop tracked");
    // (2) #528 — the map's right-of-way derives the crossing vehicle into the cede set.
    assert_eq!(cede_at_approach, vec![7], "the crossing vehicle is derived into the cede set (right-of-way)");
    // (3) #531 — the RED light HOLDs the ego at/before the stop line (it never crosses).
    assert!(red_max_x <= APPROACH_END + 0.6, "the ego HOLDs at/before the stop line while RED (never crosses x={APPROACH_END}), got red_max_x {red_max_x}");
    // (4) the ego COMPLETES the left turn: it climbs the arc up into the exit lane and its
    //     heading swings from east all the way to north (π/2) — the FastLoopTracker accelerates
    //     it from the red-light standstill and holds the curve, where the toy 0.1 s-sample
    //     conformance only crept off the line.
    assert!(ego_max_y > 10.0, "the ego completes the turn up the arc into the exit, got ego_max_y {ego_max_y}");
    assert!(ego_max_heading > 1.4, "the ego's heading swings east→north (≈π/2), got max heading {ego_max_heading}");
}
