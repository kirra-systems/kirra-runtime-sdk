//! End-to-end: **permitted vs protected turns at a signalized junction → Occy → KIRRA**.
//!
//! ADR-0021 added gap-acceptance to an unprotected turn; this composes it with the live traffic
//! signal. A **permitted** movement — a solid green — means "proceed *if clear*", so the ego still
//! gap-accepts oncoming/cross traffic. A **protected** movement — a green ARROW
//! (`SignalState::ProtectedGreen`) — means the conflicting streams hold a red, so the ego asserts
//! priority and proceeds without waiting. The payoff: on the *identical* tight gap, a permitted
//! green HOLDs while a protected arrow GOES — the signal decides which.
//!
//! KIRRA's head-on / crossing RSS independently backstops whatever the ego commits, so a
//! red-light-runner during a protected turn is still caught by the checker.

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, Goal, Lane, LaneControl, LaneEdge,
    LaneGraph, LineType, MickIntent, PlanInput, PlanOutput, Pose, ProposalKind, SignalState,
    TurnDirection,
};
use kirra_ros2_adapter::corridor::Point;
use kirra_ros2_adapter::state::PerceivedObject;

const R: f64 = 12.0;
const HALF: f64 = 3.0;

fn arc(cx: f64, cy: f64, r: f64, start: f64, sweep: f64, n: usize) -> Vec<Point> {
    (0..=n)
        .map(|i| {
            let t = start + sweep * (i as f64 / n as f64);
            Point { x_m: cx + r * t.cos(), y_m: cy + r * t.sin() }
        })
        .collect()
}

fn lane(id: u64, centerline: Vec<Point>, heading_rad: f64, succ: Option<u64>, control: Option<LaneControl>) -> Lane {
    let edges = succ.map(|s| vec![LaneEdge::Successor { to: s }]).unwrap_or_default();
    Lane { id, centerline, half_width_m: HALF, left_line: LineType::Solid, right_line: LineType::Solid, heading_rad, edges, control }
}

/// A signalized LEFT-turn junction: approach (lane 1, traffic-light controlled) → left arc → exit.
fn signalized_left_junction() -> LaneGraph {
    let mut g = LaneGraph::new().with_lane(
        Lane::straight(1, 0.0, 0.0, 20.0, HALF, LineType::Solid, LineType::Solid)
            .with_edge(LaneEdge::Successor { to: 2 })
            .with_control(LaneControl::TrafficLight),
    );
    g.add_lane(lane(2, arc(20.0, R, R, -std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2, 12), std::f64::consts::FRAC_PI_2, Some(3), None));
    g.add_lane(lane(3, vec![Point { x_m: 20.0 + R, y_m: R }, Point { x_m: 20.0 + R, y_m: R + 20.0 }], std::f64::consts::FRAC_PI_2, None, None));
    g
}

/// A vehicle heading NORTH at `speed`, `south` metres below the conflict point (20, 0) — closing.
fn crosser(id: u64, south: f64, speed: f64) -> PerceivedObject {
    PerceivedObject { id, pos: Point { x_m: 20.0, y_m: -south }, velocity_mps: speed, heading_rad: std::f64::consts::FRAC_PI_2, vel: Point { x_m: 0.0, y_m: speed } }
}

fn ground_left(g: &LaneGraph, objects: &[PerceivedObject], signal: SignalState) -> PlanOutput {
    let ego_corr = g.lane(1).unwrap().corridor(0.95, 0);
    let signals = [(1u64, signal)];
    let input = PlanInput {
        ego: EgoState { pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 2.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: Pose { x_m: 20.0 + R, y_m: R + 12.0, heading_rad: std::f64::consts::FRAC_PI_2 } },
        map: &ego_corr,
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
        signal_states: &signals,
    };
    plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::TurnAt { direction: TurnDirection::Left }, &input)
}

fn turns_left(p: &PlanOutput) -> bool {
    p.kind == ProposalKind::Motion && p.trajectory.iter().map(|t| t.pose.y_m).fold(f64::MIN, f64::max) > 0.6 * R
}

#[test]
fn a_permitted_green_yields_to_oncoming_on_a_tight_gap() {
    // Solid green: the ego MAY turn but must yield. A crosser 12 m out closing at 4 m/s (3 s < the
    // 4 s critical gap) → the permitted turn HOLDs for the gap.
    let g = signalized_left_junction();
    let plan = ground_left(&g, &[crosser(9, 12.0, 4.0)], SignalState::Green);
    assert_eq!(plan.kind, ProposalKind::SafeStop, "a permitted green still yields to oncoming");
}

#[test]
fn a_protected_arrow_proceeds_through_the_same_tight_gap() {
    // The identical tight crosser, but a protected green ARROW: the conflicting stream holds a red,
    // so the ego asserts priority and takes the turn without waiting. Signal decides, same traffic.
    let g = signalized_left_junction();
    let plan = ground_left(&g, &[crosser(9, 12.0, 4.0)], SignalState::ProtectedGreen);
    assert!(turns_left(&plan), "a protected arrow proceeds through the gap (kind {:?})", plan.kind);
}

#[test]
fn a_permitted_green_takes_an_ample_gap() {
    // Permitted green with the crosser 40 m out (10 s) → an adequate gap → the ego turns.
    let g = signalized_left_junction();
    let plan = ground_left(&g, &[crosser(9, 40.0, 4.0)], SignalState::Green);
    assert!(turns_left(&plan), "a permitted green takes an ample gap (kind {:?})", plan.kind);
}

#[test]
fn a_red_light_holds_regardless_of_the_gap() {
    // Red: the stop-line binds before the junction — the ego holds even with no conflicting traffic
    // and no gap-acceptance concern. (Fail-closed: an absent signal also defaults to red.)
    let g = signalized_left_junction();
    let plan = ground_left(&g, &[], SignalState::Red);
    assert!(plan.trajectory.iter().all(|t| t.pose.x_m <= 20.5), "a red light holds at the stop line, max_x past it would be a run");
}
