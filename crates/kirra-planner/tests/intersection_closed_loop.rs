//! Deterministic proof for the `mick_intersection` demo: a scripted goal-seeking brain drives
//! a signalized left-turn junction closed-loop, and the whole stack behaves. The TRAFFIC
//! LIGHT (#531) HOLDs the ego at the stop line while RED (it never crosses); on GREEN the
//! grounding authors a LEFT-turn trajectory through the junction along the materialized route
//! corridor (#526/#527 geometry) and KIRRA admits it; the map's right-of-way (#528) derives
//! the crossing vehicle into the cede set; and KIRRA admits every tick. The example shows the
//! same loop with a *live* model; this pins the geometry/wiring with no Ollama dependency.
//!
//! Note on the maneuver: the brain heads for the goal across the junction (`GoTo`), so the
//! ego tracks the route corridor through the arc — robust under per-tick re-planning. A bare
//! `TurnAt` is a single-shot *approach* maneuver (it resolves the branch from the approach
//! lane); sustaining it tick-by-tick through the arc is a separate follow-up.

use kirra_planner::{
    EgoState, FleetPosture, GeometricPlanner, Goal, Lane, LaneControl, LaneCorridor, LaneEdge,
    LaneGraph, LineType, MickDriver, MickIntent, PlanInput, PlanOutput, Pose, ScriptedBrain,
    SignalState, TrajectoryVerdict,
};
use kirra_ros2_adapter::corridor::{CorridorSource, Point};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

const FAST_DT_S: f64 = 0.1;
const FAST_DT_MS: u64 = 100;
const TICKS: usize = 120;
const MRC_DECEL: f64 = 3.0;
const GREEN_AT_MS: u64 = 3500;
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

fn target_at(plan: &PlanOutput, t: f64) -> Option<(Pose, f64)> {
    plan.trajectory.iter().find(|p| p.time_from_start_s >= t).or_else(|| plan.trajectory.last()).map(|p| (p.pose, p.velocity_mps))
}

#[test]
fn gemma_holds_on_red_then_turns_left_on_green_kirra_bounding_throughout() {
    let g = junction();
    let route: LaneCorridor = g.route(1, 5).and_then(|r| g.route_corridor(&r, 0.95, 5)).expect("route");
    // A stationary vehicle in the crossing lane (id 7), well off the left-turn path — the ego
    // has map right-of-way over it on approach (the cede is derived), and it never interferes.
    let objs = [PerceivedObject { id: 7, pos: Point { x_m: 33.0, y_m: -10.0 }, velocity_mps: 0.0, heading_rad: std::f64::consts::FRAC_PI_2, vel: Point { x_m: 0.0, y_m: 0.0 } }];
    let goal = Pose { x_m: APPROACH_END + TURN_RADIUS, y_m: 28.0, heading_rad: std::f64::consts::FRAC_PI_2 };

    // The model heads for the goal across the junction (a left turn): grounding follows the
    // materialized route corridor through the arc, the red light HOLDs it at the line first.
    let mut driver = MickDriver::new(ScriptedBrain::new(vec![MickIntent::GoTo { x_m: 42.0, y_m: 28.0 }; 20]));
    let mut occy = GeometricPlanner::default();

    let mut ego = EgoState { pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 5.0, yaw_rate_rads: 0.0, stamp_ms: 0 };
    let mut accepted: Option<PlanOutput> = None;
    let mut slot_t = 0.0_f64;

    let mut red_max_x = f64::MIN;
    let mut all_admitted = true;
    let cede_at_approach = g.junction_context(Point { x_m: ego.pose.x_m, y_m: ego.pose.y_m }, &objs).cedes_to_ego;
    let mut ego_max_y = f64::MIN;
    let mut green_plan_max_y = f64::MIN; // how far into the arc the admitted GREEN plan reaches

    for tick in 1..=TICKS {
        let now_ms = tick as u64 * FAST_DT_MS;
        let light = if now_ms < GREEN_AT_MS { SignalState::Red } else { SignalState::Green };
        let signals = [(1u64, light)];
        let w = world(ego, &route, &objs, &signals, &g, goal);

        let plan = driver.drive_tick(&w, &mut occy, now_ms);
        let v = validate_trajectory_slow(&plan.trajectory, &route, &objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal);
        let admitted = matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp);
        all_admitted &= admitted;

        if admitted && light == SignalState::Green {
            let plan_y = plan.trajectory.iter().map(|p| p.pose.y_m).fold(f64::MIN, f64::max);
            green_plan_max_y = green_plan_max_y.max(plan_y);
        }

        if admitted { accepted = Some(plan); slot_t = 0.0; }
        slot_t += FAST_DT_S;
        ego = match accepted.as_ref().and_then(|p| target_at(p, slot_t)) {
            Some((pose, vel)) => EgoState { pose, linear_x_mps: vel, yaw_rate_rads: 0.0, stamp_ms: now_ms },
            None => EgoState { pose: ego.pose, linear_x_mps: (ego.linear_x_mps - MRC_DECEL * FAST_DT_S).max(0.0), yaw_rate_rads: 0.0, stamp_ms: now_ms },
        };

        if light == SignalState::Red { red_max_x = red_max_x.max(ego.pose.x_m); }
        ego_max_y = ego_max_y.max(ego.pose.y_m);
    }

    println!("red_max_x={red_max_x:.2}  ego_final=({:.2},{:.2})  ego_max_y={ego_max_y:.2}  green_plan_max_y={green_plan_max_y:.2}  cedes={cede_at_approach:?}  admitted={all_admitted}", ego.pose.x_m, ego.pose.y_m);

    // (1) KIRRA bounds every tick of the negotiation.
    assert!(all_admitted, "KIRRA admits every tick of the junction negotiation");
    // (2) #528 — the map's right-of-way derives the crossing vehicle into the cede set.
    assert_eq!(cede_at_approach, vec![7], "the crossing vehicle is derived into the cede set (right-of-way)");
    // (3) #531 — the RED light HOLDs the ego at/before the stop line (it never crosses).
    assert!(red_max_x <= APPROACH_END + 0.6, "the ego HOLDs at/before the stop line while RED (never crosses x={APPROACH_END}), got red_max_x {red_max_x}");
    // (4) on GREEN the planner produces an admitted LEFT-turn trajectory deep into the arc
    //     (#526/#527 route geometry) — the turn the red light had vetoed is now authored.
    assert!(green_plan_max_y > 8.0, "on GREEN an admitted plan turns deep into the arc, got green_plan_max_y {green_plan_max_y}");
    // (5) and the ego is released from the line into the turn (it crosses and climbs the arc).
    //     The full traversal is gradual — the planner accelerates gently from the stop and the
    //     fast loop conforms 0.1 s at a time — so this asserts release, not completion.
    assert!(ego_max_y > 1.0 && ego.pose.x_m > APPROACH_END, "GREEN releases the ego into the turn, got ego_max_y {ego_max_y} x {}", ego.pose.x_m);
}
