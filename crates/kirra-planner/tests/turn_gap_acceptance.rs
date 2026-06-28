//! End-to-end: **unprotected-turn gap-acceptance at a junction → Occy → KIRRA**.
//!
//! A `TurnAt` is not just "follow the route arc" — before committing an unprotected turn the ego
//! must find an adequate GAP in the traffic it has to yield to. Mick's grounding now gates the turn
//! on gap-acceptance (`behavior::accept_turn_gap`): the conflict point is the junction (the ego
//! lane's terminus), the conflicting stream is every vehicle CLOSING on it that the ego has no
//! asserted priority over (not on the right-of-way cede set), and an inadequate gap HOLDs the ego
//! at the junction until one opens. The doer-checker split holds: Occy decides WHEN to go; KIRRA's
//! head-on / crossing RSS independently backstops the turn it commits.
//!
//! This pins the negotiation: a tight gap HOLDs, an ample gap GOES, and asserting right-of-way
//! (the cede set) proceeds through the same tight gap — the ego takes the priority the map grants.

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, Goal, Lane, LaneEdge, LaneGraph,
    LineType, MickIntent, PlanInput, PlanOutput, Pose, ProposalKind, TrajectoryVerdict, TurnDirection,
};
use kirra_trajectory::corridor::Point;
use kirra_trajectory::state::PerceivedObject;
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

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

fn lane(id: u64, centerline: Vec<Point>, heading_rad: f64, succ: Option<u64>) -> Lane {
    let edges = succ.map(|s| vec![LaneEdge::Successor { to: s }]).unwrap_or_default();
    Lane { id, centerline, half_width_m: HALF, left_line: LineType::Solid, right_line: LineType::Solid, heading_rad, edges, control: None }
}

/// Approach (lane 1, east to the junction mouth at x=20) → LEFT arc (lane 2) → north exit (lane 3).
/// The ego turns LEFT; the junction conflict point is the approach terminus (20, 0).
fn left_junction() -> LaneGraph {
    let mut g = LaneGraph::new().with_lane(
        Lane::straight(1, 0.0, 0.0, 20.0, HALF, LineType::Solid, LineType::Solid)
            .with_edge(LaneEdge::Successor { to: 2 }),
    );
    g.add_lane(lane(2, arc(20.0, R, R, -std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2, 12), std::f64::consts::FRAC_PI_2, Some(3)));
    g.add_lane(lane(3, vec![Point { x_m: 20.0 + R, y_m: R }, Point { x_m: 20.0 + R, y_m: R + 20.0 }], std::f64::consts::FRAC_PI_2, None));
    g
}

/// A cross vehicle heading NORTH at `speed`, currently `south_of_junction` metres below the
/// conflict point (20, 0) — so it is closing on the junction along +y.
fn northbound_crosser(id: u64, south_of_junction: f64, speed: f64) -> PerceivedObject {
    PerceivedObject {
        id,
        pos: Point { x_m: 20.0, y_m: -south_of_junction },
        velocity_mps: speed,
        heading_rad: std::f64::consts::FRAC_PI_2,
        vel: Point { x_m: 0.0, y_m: speed },
    }
}

/// Ground a LEFT turn from the approach with `objects` present and `cedes` asserting priority.
fn ground_left_turn(g: &LaneGraph, objects: &[PerceivedObject], cedes: &[u64]) -> PlanOutput {
    let ego_corr = g.lane(1).unwrap().corridor(0.95, 0);
    let input = PlanInput {
        ego: EgoState { pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 2.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: Pose { x_m: 20.0 + R, y_m: R + 12.0, heading_rad: std::f64::consts::FRAC_PI_2 } },
        map: &ego_corr,
        objects,
        controls: &[],
        lane_boundaries: &[],
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: cedes,
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: Some(g),
        signal_states: &[],
    };
    plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::TurnAt { direction: TurnDirection::Left }, &input)
}

fn turns_left(p: &PlanOutput) -> bool {
    p.kind == ProposalKind::Motion
        && p.trajectory.iter().map(|t| t.pose.y_m).fold(f64::MIN, f64::max) > 0.6 * R
}

#[test]
fn a_tight_gap_holds_the_turn() {
    // A crosser 12 m south closing at 4 m/s reaches the junction in 3 s < the 4 s critical gap →
    // the ego HOLDs at the junction rather than committing the turn into it.
    let g = left_junction();
    let objs = [northbound_crosser(9, 12.0, 4.0)];
    let plan = ground_left_turn(&g, &objs, &[]);
    assert_eq!(plan.kind, ProposalKind::SafeStop, "an inadequate gap HOLDs the unprotected turn");
    assert!(plan.trajectory.iter().all(|t| t.velocity_mps == 0.0), "the HOLD is a full stop");
}

#[test]
fn an_ample_gap_takes_the_turn() {
    // The same crosser 40 m away closing at 4 m/s is 10 s out — well over the critical gap — so the
    // ego accepts the gap and drives the turn. KIRRA admits it (the crosser is far off the arc).
    let g = left_junction();
    let objs = [northbound_crosser(9, 40.0, 4.0)];
    let plan = ground_left_turn(&g, &objs, &[]);
    assert!(turns_left(&plan), "an ample gap is accepted — the ego takes the turn (kind {:?})", plan.kind);

    let route = g.route(1, 3).unwrap();
    let corr = g.route_corridor(&route, 0.95, 0).unwrap();
    let verdict = validate_trajectory_slow(&plan.trajectory, &corr, &objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal);
    assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp), "KIRRA admits the accepted turn, got {verdict:?}");
}

#[test]
fn asserting_right_of_way_proceeds_through_the_same_tight_gap() {
    // The tight crosser, but now on the ego's cede set (the map grants the ego priority over it).
    // Gap-acceptance is bypassed for a vehicle that must yield to the ego — the ego takes the turn.
    let g = left_junction();
    let objs = [northbound_crosser(9, 12.0, 4.0)];
    let plan = ground_left_turn(&g, &objs, &[9]);
    assert!(turns_left(&plan), "with asserted priority the ego proceeds through the gap (kind {:?})", plan.kind);
}

#[test]
fn a_non_closing_vehicle_is_not_a_conflict() {
    // A stopped vehicle near the junction is not closing → not a gap-acceptance conflict → the ego
    // takes the turn (a parked car beside the junction must not freeze an unprotected turn forever).
    let g = left_junction();
    let stopped = PerceivedObject { id: 9, pos: Point { x_m: 20.0, y_m: -8.0 }, velocity_mps: 0.0, heading_rad: 0.0, vel: Point { x_m: 0.0, y_m: 0.0 } };
    let plan = ground_left_turn(&g, &[stopped], &[]);
    assert!(turns_left(&plan), "a stopped (non-closing) vehicle does not hold the turn (kind {:?})", plan.kind);
}

#[test]
fn no_conflicting_traffic_takes_the_turn() {
    // Baseline: an empty junction — the turn grounds and drives, unchanged from before the gate.
    let g = left_junction();
    let plan = ground_left_turn(&g, &[], &[]);
    assert!(turns_left(&plan), "an empty junction takes the turn (kind {:?})", plan.kind);
}

#[test]
fn the_interaction_model_holds_a_slow_but_close_agent() {
    // THE STACKELBERG SAFETY POINT. A SLOW crosser (1 m/s) only 8 m from the junction: a naive
    // gap-acceptance reads its time as d/v = 8 s ≫ 4 s and would WRONGLY assert the turn into it.
    // The interaction model does not trust the agent to stay slow — it assumes a worst-case
    // re-acceleration (~2.2 s to the conflict) and HOLDs. KIRRA still backstops.
    let g = left_junction();
    let plan = ground_left_turn(&g, &[northbound_crosser(9, 8.0, 1.0)], &[]);
    assert_eq!(plan.kind, ProposalKind::SafeStop, "a slow-but-close agent is not trusted ⇒ HOLD");
}

#[test]
fn the_interaction_model_asserts_a_genuinely_yielded_agent() {
    // The same slow (1 m/s) crosser but 28 m away: even re-accelerating it needs > 4 s to reach the
    // junction, so the yielded position is real — the ego asserts the turn.
    let g = left_junction();
    let plan = ground_left_turn(&g, &[northbound_crosser(9, 28.0, 1.0)], &[]);
    assert!(turns_left(&plan), "a genuinely-yielded (slow + far) agent lets the ego assert (kind {:?})", plan.kind);
}
