//! **A deterministic scenario suite feeding the Mick eval harness (`MickEvalSummary`).**
//!
//! `mick_intersection` drives ONE live, LLM-authored junction. This suite instead grounds a broad
//! battery of typed intents through the geometric Occy + KIRRA stack — covering the junction /
//! behavior capabilities built this arc (turn gap-acceptance + the Stackelberg interaction model,
//! right-of-way, permitted vs protected signals, occlusion creep, the joint path optimizer, plain
//! car-following) — with NO model in the loop, so the brain's decision surface can be *measured*
//! against the checker deterministically.
//!
//! Each scenario records a `MickDecisionRecord` (intent → grounded plan → KIRRA verdict). The suite
//! asserts (a) each scenario's documented outcome (the ego ASSERTs or HOLDs as designed), (b) the
//! load-bearing safety invariant that **no Motion plan is ever refused by KIRRA** (a refusal would
//! mean the doer overreached past the checker), and (c) the aggregate scorecard shape. Run with
//! `--nocapture` to print the `MickEvalSummary` scorecard.

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, GeometricPlannerConfig, Goal, Lane,
    LaneControl, LaneEdge, LaneGraph, LineType, MickDecisionRecord, MickEvalSummary, MickIntent,
    Occluder, PlanInput, PlanOutput, Pose, SignalState, TurnDirection,
};
use kirra_ros2_adapter::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_ros2_adapter::state::{PerceivedObject, TrajectoryVerdict};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

const R: f64 = 12.0; // turn radius (gentle enough to admit)
const APPROACH_END: f64 = 30.0; // ego lane (0,0)→(30,0); the conflict line / stop line is here

/// What the ego should do in a scenario — both are SAFE outcomes; the suite measures the split.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Expect {
    Assert, // a forward Motion plan that KIRRA admits
    Hold,   // a controlled SafeStop (yield / red / inadequate gap)
}

fn admits(v: TrajectoryVerdict) -> bool {
    matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp)
}

/// A signalized LEFT-turn junction: approach lane 1 (control optional) → arc lane 2 → exit lane 5.
/// With `row_crossing`, a crossing lane 3 that YIELDS to lane 1 is added (for the right-of-way case).
fn left_junction(control: Option<LaneControl>, row_crossing: bool) -> LaneGraph {
    let arc: Vec<Point> = (0..=12)
        .map(|i| {
            let t = -std::f64::consts::FRAC_PI_2 + std::f64::consts::FRAC_PI_2 * (i as f64 / 12.0);
            Point { x_m: APPROACH_END + R * t.cos(), y_m: R + R * t.sin() }
        })
        .collect();
    let mut approach = Lane::straight(1, 0.0, 0.0, APPROACH_END, 3.0, LineType::Solid, LineType::Solid)
        .with_edge(LaneEdge::Successor { to: 2 });
    if let Some(c) = control {
        approach = approach.with_control(c);
    }
    let mut g = LaneGraph::new()
        .with_lane(approach)
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
            centerline: vec![Point { x_m: APPROACH_END + R, y_m: R }, Point { x_m: APPROACH_END + R, y_m: R + 20.0 }],
            half_width_m: 3.0,
            left_line: LineType::Solid,
            right_line: LineType::Solid,
            heading_rad: std::f64::consts::FRAC_PI_2,
            edges: Vec::new(),
            control: None,
        });
    if row_crossing {
        g = g
            .with_lane(Lane::straight(3, -10.0, 25.0, 40.0, 3.0, LineType::Solid, LineType::Solid).with_heading(std::f64::consts::FRAC_PI_2))
            .with_right_of_way(1, 3);
    }
    g
}

/// A vehicle `south` metres below the junction conflict (30, 0), heading north at `speed`.
fn crosser(id: u64, south: f64, speed: f64) -> PerceivedObject {
    PerceivedObject {
        id,
        pos: Point { x_m: APPROACH_END, y_m: -south },
        velocity_mps: speed,
        heading_rad: std::f64::consts::FRAC_PI_2,
        vel: Point { x_m: 0.0, y_m: speed },
    }
}

fn turn_world<'a>(g: &'a LaneGraph, ego_corr: &'a dyn CorridorSource, objects: &'a [PerceivedObject], signals: &'a [(u64, SignalState)]) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState { pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 4.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: Pose { x_m: APPROACH_END + R, y_m: R + 12.0, heading_rad: std::f64::consts::FRAC_PI_2 } },
        map: ego_corr,
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

/// Ground a LEFT turn and validate the plan against the turn's route corridor.
fn run_turn(g: &LaneGraph, objs: &[PerceivedObject], signals: &[(u64, SignalState)]) -> (PlanOutput, TrajectoryVerdict) {
    let ego_corr = g.lane(1).unwrap().corridor(0.95, 0);
    let w = turn_world(g, &ego_corr, objs, signals);
    let plan = plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::TurnAt { direction: TurnDirection::Left }, &w);
    // Validate against the route corridor when the turn drives, else the approach corridor (a HOLD).
    let verdict = match g.route(1, 5).and_then(|r| g.route_corridor(&r, 0.95, 0)) {
        Some(corr) => validate_trajectory_slow(&plan.trajectory, &corr, objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal),
        None => validate_trajectory_slow(&plan.trajectory, &ego_corr, objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal),
    };
    (plan, verdict)
}

/// Ground a GoTo on a straight corridor (optionally with a lane graph for occlusion) and validate.
fn run_goto(corr: &dyn CorridorSource, g: Option<&LaneGraph>, objs: &[PerceivedObject], goal_x: f64, cfg: GeometricPlannerConfig) -> (PlanOutput, TrajectoryVerdict) {
    let w = PlanInput {
        ego: EgoState { pose: Pose { x_m: 6.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 4.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: Pose { x_m: goal_x, y_m: 0.0, heading_rad: 0.0 } },
        map: corr,
        objects: objs,
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
        lane_graph: g,
        signal_states: &[],
    };
    let plan = plan_for_intent(&mut GeometricPlanner::new(cfg), &MickIntent::GoTo { x_m: goal_x, y_m: 0.0 }, &w);
    let verdict = validate_trajectory_slow(&plan.trajectory, corr, objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal);
    (plan, verdict)
}

/// A straight occluded-approach graph: lane 1 (0,0)→(40,0) with `sight` m of assured-clear sight
/// derived from a corner building, → exit lane 2. Drives the occlusion creep cap.
fn occluded_graph() -> LaneGraph {
    LaneGraph::new()
        .with_lane(Lane::straight(1, 0.0, 0.0, 40.0, 3.0, LineType::Solid, LineType::Solid).with_edge(LaneEdge::Successor { to: 2 }))
        .with_lane(Lane::straight(2, 0.0, 40.0, 80.0, 3.0, LineType::Solid, LineType::Solid))
        .with_derived_occlusion(&[Occluder::new(10.0, 35.0, 3.5, 9.0)]) // edge 5 m before the conflict → sight 5
}

#[test]
fn scenario_suite_feeds_the_eval_harness() {
    let urban = MockCorridorSource::straight_5m_half_width(100.0);
    let mut records: Vec<MickDecisionRecord> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    // (label, intent, plan, verdict, expectation). Each tuple is a grounded-then-checked scenario.
    let mut battery: Vec<(&'static str, MickIntent, PlanOutput, TrajectoryVerdict, Expect)> = Vec::new();

    // --- Car-following / go-to ---
    {
        let (p, v) = run_goto(&urban, None, &[], 60.0, GeometricPlannerConfig::default());
        battery.push(("go_to clear straight", MickIntent::GoTo { x_m: 60.0, y_m: 0.0 }, p, v, Expect::Assert));
    }
    {
        let lead = [PerceivedObject { id: 1, pos: Point { x_m: 30.0, y_m: 0.0 }, velocity_mps: 0.0, heading_rad: 0.0, vel: Point { x_m: 0.0, y_m: 0.0 } }];
        let (p, v) = run_goto(&urban, None, &lead, 60.0, GeometricPlannerConfig::default());
        battery.push(("go_to stops short of a stopped lead", MickIntent::GoTo { x_m: 60.0, y_m: 0.0 }, p, v, Expect::Assert));
    }
    {
        // Joint path optimizer ON, clear corridor — admitted, byte-safe (no-op on straight, slows nothing).
        let cfg = GeometricPlannerConfig { joint_path_optimize: true, ..Default::default() };
        let (p, v) = run_goto(&urban, None, &[], 60.0, cfg);
        battery.push(("go_to with joint optimizer on", MickIntent::GoTo { x_m: 60.0, y_m: 0.0 }, p, v, Expect::Assert));
    }
    {
        // Occluded junction approach → the ego CREEPS (a slow Motion the checker admits).
        let g = occluded_graph();
        let (p, v) = run_goto(&urban, Some(&g), &[], 60.0, GeometricPlannerConfig::default());
        let creeps = p.trajectory.iter().map(|t| t.velocity_mps).fold(0.0_f64, f64::max) < 6.0;
        if !creeps {
            failures.push("occluded approach did not creep (peak speed ≥ 6)".into());
        }
        battery.push(("occluded junction creep", MickIntent::GoTo { x_m: 60.0, y_m: 0.0 }, p, v, Expect::Assert));
    }

    // --- Turn gap-acceptance + the Stackelberg interaction model ---
    {
        let g = left_junction(None, false);
        let (p, v) = run_turn(&g, &[], &[]);
        battery.push(("turn_left clear junction", MickIntent::TurnAt { direction: TurnDirection::Left }, p, v, Expect::Assert));
    }
    {
        let g = left_junction(None, false);
        let (p, v) = run_turn(&g, &[crosser(9, 12.0, 4.0)], &[]);
        battery.push(("turn_left tight fast crosser → yield", MickIntent::TurnAt { direction: TurnDirection::Left }, p, v, Expect::Hold));
    }
    {
        let g = left_junction(None, false);
        let (p, v) = run_turn(&g, &[crosser(9, 8.0, 1.0)], &[]); // slow + close → interaction holds
        battery.push(("turn_left slow-but-close crosser → yield", MickIntent::TurnAt { direction: TurnDirection::Left }, p, v, Expect::Hold));
    }
    {
        let g = left_junction(None, false);
        let (p, v) = run_turn(&g, &[crosser(9, 28.0, 1.0)], &[]); // slow + far → genuinely yielded
        battery.push(("turn_left genuinely-yielded crosser → assert", MickIntent::TurnAt { direction: TurnDirection::Left }, p, v, Expect::Assert));
    }

    // --- Right-of-way (the ego asserts priority over a yielding-lane agent) ---
    {
        let g = left_junction(None, true);
        // A vehicle in the ceding crossing lane 3 (at y=-10), close to the junction.
        let in_lane3 = [PerceivedObject { id: 3, pos: Point { x_m: 30.0, y_m: -10.0 }, velocity_mps: 1.0, heading_rad: std::f64::consts::FRAC_PI_2, vel: Point { x_m: 0.0, y_m: 1.0 } }];
        let (p, v) = run_turn(&g, &in_lane3, &[]);
        battery.push(("turn_left asserts right-of-way over a ceding agent", MickIntent::TurnAt { direction: TurnDirection::Left }, p, v, Expect::Assert));
    }

    // --- Signals: red holds; permitted green yields; protected arrow asserts ---
    {
        let g = left_junction(Some(LaneControl::TrafficLight), false);
        let (p, v) = run_turn(&g, &[], &[(1, SignalState::Red)]);
        battery.push(("turn_left on RED → hold at the line", MickIntent::TurnAt { direction: TurnDirection::Left }, p, v, Expect::Hold));
    }
    {
        let g = left_junction(Some(LaneControl::TrafficLight), false);
        let (p, v) = run_turn(&g, &[crosser(9, 12.0, 4.0)], &[(1, SignalState::Green)]); // permitted: still yields
        battery.push(("turn_left permitted green yields to a tight crosser", MickIntent::TurnAt { direction: TurnDirection::Left }, p, v, Expect::Hold));
    }
    {
        let g = left_junction(Some(LaneControl::TrafficLight), false);
        let (p, v) = run_turn(&g, &[crosser(9, 12.0, 4.0)], &[(1, SignalState::ProtectedGreen)]); // protected: priority
        battery.push(("turn_left protected arrow asserts through the gap", MickIntent::TurnAt { direction: TurnDirection::Left }, p, v, Expect::Assert));
    }

    // ---- Score + assert -------------------------------------------------------------------------
    for (seq, (label, intent, plan, verdict, expect)) in battery.iter().enumerate() {
        // Did the ego PROCEED through the maneuver? Behavioral, not by plan-kind: a controlled
        // decel-to-the-stop-line (a red light) is a Motion plan that nonetheless does NOT advance
        // through the junction, exactly like an outright HOLD.
        let max_x = plan.trajectory.iter().map(|t| t.pose.x_m).fold(0.0_f64, f64::max);
        let max_y = plan.trajectory.iter().map(|t| t.pose.y_m).fold(f64::MIN, f64::max);
        let proceeded = match intent {
            // Climbed meaningfully up the arc (turned) vs held at the approach (y ≈ 0). A single
            // 5 s plan covers the approach + part of the arc, so a few metres of climb = "turned".
            MickIntent::TurnAt { .. } => max_y > 2.0,
            _ => max_x > 12.0, // advanced well past the start (x=6)
        };
        // The load-bearing invariant: if the ego PROCEEDED through the maneuver, KIRRA must admit it
        // (a refused forward maneuver would be a doer overreach past the checker).
        if proceeded && !admits(*verdict) {
            failures.push(format!("{label}: doer OVERREACH — committed maneuver refused by KIRRA ({verdict:?})"));
        }
        let ok = match expect {
            Expect::Assert => proceeded && admits(*verdict),
            Expect::Hold => !proceeded,
        };
        if !ok {
            failures.push(format!("{label}: expected {expect:?}, got proceeded={proceeded} kind={:?} verdict={verdict:?}", plan.kind));
        }
        records.push(MickDecisionRecord::new(seq as u64, seq as u64 * 100, intent, plan, *verdict));
    }

    let summary = MickEvalSummary::from_records(&records);
    eprintln!("\n=== Mick scenario-suite scorecard ({} scenarios) ===\n{summary}\n", records.len());

    assert!(failures.is_empty(), "scenario suite failures:\n  - {}", failures.join("\n  - "));
    // The doer never overreaches the checker across the whole suite.
    assert_eq!(summary.refused(), 0, "a Motion plan was refused — doer overreach");
    // The suite is a real mix of asserts and holds (not trivially all one outcome).
    assert!(summary.admitted() >= 6, "expected a healthy set of admitted maneuvers, got {}", summary.admitted());
    assert!(records.len() >= 12, "the suite should exercise a broad battery");
}
