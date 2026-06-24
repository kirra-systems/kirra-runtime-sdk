//! End-to-end: **overtake into the oncoming lane → Occy → KIRRA**.
//!
//! The sharpest form of the doer-checker split. On an undivided road, Occy passes
//! a stopped car by crossing the (crossable) centerline into the oncoming lane and
//! returning — but it never reasons about the oncoming traffic that pass exposes.
//! **KIRRA's head-on RSS is the sole authority** on whether the oncoming lane is
//! clear enough. Occy proposes the pass; KIRRA admits it on a clear road and
//! refuses it when an oncoming vehicle is too close.
//!
//! This also exercises the `PlanInput` reference-path vs drivable-area decoupling:
//! `map` is the ego LANE (the path Occy follows, keep-right) while `drivable` is
//! the FULL road (the area a pass may borrow). The same `drivable` corridor is
//! handed to KIRRA so its containment + RSS see the whole road.

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, Goal, Lane, LaneCorridor, LaneEdge,
    LaneGraph, LineType, MickIntent, PerceivedObject, PlanInput, Planner, Pose, ProposalKind,
    TrajectoryVerdict,
};
use kirra_ros2_adapter::corridor::{CorridorSource, Point};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

const LEN: f64 = 200.0;

/// A wide undivided road: ego (right) lane center y=-2.5 half 2.5 → spans [-5, 0];
/// oncoming (left) lane center y=+2.5 half 2.5 → [0, +5], travelling the opposite
/// way (heading π). `center` is the centerline marking between them.
fn road(center: LineType) -> LaneGraph {
    LaneGraph::new()
        .with_lane(
            Lane::straight(1, -2.5, 0.0, LEN, 2.5, center, LineType::Solid)
                .with_edge(LaneEdge::LeftNeighbor { to: 2 }),
        )
        .with_lane(
            Lane::straight(2, 2.5, 0.0, LEN, 2.5, LineType::Solid, center)
                .with_heading(std::f64::consts::PI)
                .with_edge(LaneEdge::RightNeighbor { to: 1 }),
        )
}

fn ego_corridor(g: &LaneGraph) -> LaneCorridor {
    g.lane(1).unwrap().corridor(0.95, 5)
}
fn full_road(g: &LaneGraph) -> LaneCorridor {
    g.corridor_over(&[1, 2], 0.95, 5).unwrap()
}

/// A stopped car in the ego lane, positioned to the right of the lane centerline
/// (near the road's right edge) so the left pass needs a modest offset.
fn stopped_car(x_m: f64) -> PerceivedObject {
    PerceivedObject {
        id: 1,
        pos: Point { x_m, y_m: -4.4 },
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }
}

/// An oncoming vehicle in the oncoming lane closing on the ego (heading π).
fn oncoming_car(x_m: f64, speed: f64) -> PerceivedObject {
    PerceivedObject {
        id: 2,
        pos: Point { x_m, y_m: 2.0 },
        velocity_mps: speed,
        heading_rad: std::f64::consts::PI,
        vel: Point { x_m: -speed, y_m: 0.0 },
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_and_check(
    g: &LaneGraph,
    map: &dyn CorridorSource,
    drivable: &dyn CorridorSource,
    objects: &[PerceivedObject],
    with_drivable: bool,
) -> (kirra_planner::PlanOutput, TrajectoryVerdict) {
    let boundaries = g.boundaries_relative_to(1, &[1, 2]).unwrap();
    let input = PlanInput {
        ego: EgoState {
            pose: Pose { x_m: 6.0, y_m: -2.5, heading_rad: 0.0 },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal { target: Pose { x_m: 60.0, y_m: -2.5, heading_rad: 0.0 } },
        map,
        objects,
        controls: &[],
        lane_boundaries: &boundaries,
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: with_drivable.then_some(drivable),
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
    };
    let mut planner = GeometricPlanner::default();
    let plan = planner.plan(&input);
    let verdict = validate_trajectory_slow(
        &plan.trajectory, drivable, objects, &VehicleConfig::default_urban(), None,
        FleetPosture::Nominal,
    );
    (plan, verdict)
}

#[test]
fn occy_proposes_an_overtake_across_the_crossable_centerline() {
    let g = road(LineType::Unmarked);
    let (map, drivable) = (ego_corridor(&g), full_road(&g));
    let cars = [stopped_car(24.0)];
    let (plan, _) = plan_and_check(&g, &map, &drivable, &cars, true);

    assert_eq!(plan.kind, ProposalKind::Motion, "Occy moves to pass, not stops");
    let max_y = plan.trajectory.iter().map(|t| t.pose.y_m).fold(f64::MIN, f64::max);
    assert!(max_y > 0.0, "the pass crosses into the oncoming half, got max_y {max_y}");
}

#[test]
fn solid_centerline_forbids_the_overtake() {
    let g = road(LineType::Solid);
    let (map, drivable) = (ego_corridor(&g), full_road(&g));
    let cars = [stopped_car(24.0)];
    let (plan, _) = plan_and_check(&g, &map, &drivable, &cars, true);

    let max_y = plan.trajectory.iter().map(|t| t.pose.y_m).fold(f64::MIN, f64::max);
    assert!(max_y <= 0.0, "a solid centerline → no pass into oncoming, got max_y {max_y}");
}

#[test]
fn no_drivable_area_means_no_overtake() {
    let g = road(LineType::Unmarked);
    let (map, drivable) = (ego_corridor(&g), full_road(&g));
    let cars = [stopped_car(24.0)];
    // drivable not provided → opt-out → prior behavior (no cross-centerline pass).
    let (plan, _) = plan_and_check(&g, &map, &drivable, &cars, false);

    let max_y = plan.trajectory.iter().map(|t| t.pose.y_m).fold(f64::MIN, f64::max);
    assert!(max_y <= 0.0, "without a drivable area Occy does not overtake, got max_y {max_y}");
}

#[test]
fn kirra_admits_the_pass_when_the_oncoming_lane_is_clear() {
    let g = road(LineType::Unmarked);
    let (map, drivable) = (ego_corridor(&g), full_road(&g));
    let cars = [stopped_car(24.0)];
    let (_, verdict) = plan_and_check(&g, &map, &drivable, &cars, true);

    assert!(
        matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "a clear oncoming lane → KIRRA admits the pass, got {verdict:?}"
    );
}

#[test]
fn a_car_centered_in_the_ego_lane_is_now_overtaken_and_admitted() {
    // The §4 alignment-band payoff. Before the longitudinal-RSS footprint-overlap
    // gate, a car CENTERED in the ego lane (dead on the reference path) could not
    // be passed: clearing the wide 4 m lateral-alignment band needed more side room
    // than the maneuver builds before closing the longitudinal gap. Now the
    // longitudinal bound only applies within the footprint-overlap band, so the
    // ego clears it before the gap closes → the pass admits.
    let g = road(LineType::Unmarked);
    let (map, drivable) = (ego_corridor(&g), full_road(&g));
    let centered = PerceivedObject {
        id: 1,
        pos: Point { x_m: 24.0, y_m: -2.5 }, // ego-lane centerline (on the reference path)
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    };
    let (plan, verdict) = plan_and_check(&g, &map, &drivable, &[centered], true);

    let max_y = plan.trajectory.iter().map(|t| t.pose.y_m).fold(f64::MIN, f64::max);
    assert!(max_y > 0.0, "the pass crosses into the oncoming half, got max_y {max_y}");
    assert!(
        matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "a centered-lane car is now passable (footprint-overlap gate), got {verdict:?}"
    );
}

#[test]
fn kirra_refuses_the_pass_when_oncoming_traffic_is_too_close() {
    // Identical to the admitted clear-pass scene, plus a closing oncoming vehicle in
    // the oncoming lane. Occy still proposes the pass (it never reasons about
    // oncoming); KIRRA refuses. Because the clear case ABOVE admits, the added
    // oncoming vehicle is the cause — the checker's head-on (opposite-direction)
    // longitudinal bound fires on the closing vehicle (~31 m required vs the
    // dozen-metre gap). The checker, not the planner, owns the decision.
    let g = road(LineType::Unmarked);
    let (map, drivable) = (ego_corridor(&g), full_road(&g));
    let cars = [stopped_car(24.0), oncoming_car(38.0, 12.0)];
    let (_, verdict) = plan_and_check(&g, &map, &drivable, &cars, true);

    assert_eq!(
        verdict, TrajectoryVerdict::MRCFallback,
        "oncoming traffic too close → KIRRA refuses the pass, got {verdict:?}"
    );
}

#[test]
fn a_same_direction_vehicle_at_the_same_spot_does_not_block_the_pass() {
    // The clean direction-isolation control, now possible thanks to the §4
    // lateral-RSS longitudinal gate: a vehicle at the EXACT spot the oncoming one
    // occupied, but travelling the SAME direction (heading 0, pulling away). It is
    // longitudinally safe (a receding lead) and — being >8 m ahead — no longer
    // trips the (now longitudinally-gated) lateral RSS. KIRRA ADMITS. So the
    // refusal above is the oncoming DIRECTION (the head-on bound), not merely "a
    // vehicle is present in the pass corridor". (Before the gate, the adapter's
    // lateral RSS MRC'd this fast adjacent vehicle during the angled ramp too,
    // masking the result — COMPETITIVE_PLANNER_ANALYSIS §4.)
    let g = road(LineType::Unmarked);
    let (map, drivable) = (ego_corridor(&g), full_road(&g));
    let same_dir = PerceivedObject {
        id: 2,
        pos: Point { x_m: 38.0, y_m: 2.0 },
        velocity_mps: 12.0,
        heading_rad: 0.0, // same direction, faster → a lead pulling away
        vel: Point { x_m: 12.0, y_m: 0.0 },
    };
    let cars = [stopped_car(24.0), same_dir];
    let (_, verdict) = plan_and_check(&g, &map, &drivable, &cars, true);

    assert!(
        matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "a same-direction vehicle at the same spot is admitted; only the oncoming \
         direction refuses, got {verdict:?}"
    );
}

/// Build the overtake world as a `PlanInput` (with `request_overtake` left false, so the
/// caller's intent is what turns it on) for the Mick end-to-end tests below.
#[allow(clippy::too_many_arguments)]
fn overtake_world<'a>(
    map: &'a dyn CorridorSource,
    drivable: &'a dyn CorridorSource,
    objects: &'a [PerceivedObject],
    boundaries: &'a [kirra_planner::LaneBoundary],
) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState {
            pose: Pose { x_m: 6.0, y_m: -2.5, heading_rad: 0.0 },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal { target: Pose { x_m: 60.0, y_m: -2.5, heading_rad: 0.0 } },
        map,
        objects,
        controls: &[],
        lane_boundaries: boundaries,
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: Some(drivable),
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false, // the MickIntent::Overtake grounding flips this on
        request_pull_over: false,
        lane_graph: None,
    }
}

/// **The full Mick path.** A `MickIntent::Overtake` — the LLM chauffeur's discretionary
/// "pass the slow lead" — is grounded by Occy (`plan_for_intent`) into the cross-centerline
/// maneuver, and KIRRA admits it on a clear oncoming lane. Proves the intent, not just the
/// planner's auto-route-around, drives the pass end to end.
#[test]
fn mick_overtake_intent_grounds_to_a_pass_and_kirra_admits() {
    let g = road(LineType::Unmarked);
    let (map, drivable) = (ego_corridor(&g), full_road(&g));
    let boundaries = g.boundaries_relative_to(1, &[1, 2]).unwrap();
    let cars = [stopped_car(24.0)];
    let w = overtake_world(&map, &drivable, &cars, &boundaries);

    let mut occy = GeometricPlanner::default();
    let plan = plan_for_intent(&mut occy, &MickIntent::Overtake, &w);

    let max_y = plan.trajectory.iter().map(|t| t.pose.y_m).fold(f64::MIN, f64::max);
    assert!(max_y > 0.0, "Mick's Overtake intent crosses into the oncoming half, got max_y {max_y}");

    let verdict = validate_trajectory_slow(
        &plan.trajectory, &drivable, &cars, &VehicleConfig::default_urban(), None,
        FleetPosture::Nominal,
    );
    assert!(
        matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "a clear oncoming lane → KIRRA admits Mick's pass, got {verdict:?}"
    );
}

/// **The payoff, via the Mick path.** Same `MickIntent::Overtake`, but now an oncoming
/// vehicle is closing in the pass corridor. Mick still asks to pass (it never reasons about
/// oncoming traffic); KIRRA's head-on RSS refuses. The doer proposes, the checker bounds.
#[test]
fn mick_overtake_into_oncoming_traffic_is_refused_by_kirra() {
    let g = road(LineType::Unmarked);
    let (map, drivable) = (ego_corridor(&g), full_road(&g));
    let boundaries = g.boundaries_relative_to(1, &[1, 2]).unwrap();
    let cars = [stopped_car(24.0), oncoming_car(38.0, 12.0)];
    let w = overtake_world(&map, &drivable, &cars, &boundaries);

    let mut occy = GeometricPlanner::default();
    let plan = plan_for_intent(&mut occy, &MickIntent::Overtake, &w);
    let verdict = validate_trajectory_slow(
        &plan.trajectory, &drivable, &cars, &VehicleConfig::default_urban(), None,
        FleetPosture::Nominal,
    );
    assert_eq!(
        verdict, TrajectoryVerdict::MRCFallback,
        "oncoming traffic too close → KIRRA refuses Mick's pass, got {verdict:?}"
    );
}

#[test]
fn a_stopped_school_bus_is_never_overtaken_and_the_ego_holds_behind_it() {
    // Identical to `occy_proposes_an_overtake...` (which DOES pass the same object),
    // but the stopped object is flagged a school bus loading children (`no_overtake_
    // ids`). It is ILLEGAL to pass — so Occy must NOT cross into the oncoming lane
    // and must hold behind it, even though the centerline is crossable and the pass
    // is otherwise admissible. The legal rule lives in Occy; KIRRA never enforces it.
    let g = road(LineType::Unmarked);
    let (map, drivable) = (ego_corridor(&g), full_road(&g));
    let boundaries = g.boundaries_relative_to(1, &[1, 2]).unwrap();
    let bus = stopped_car(24.0); // id == 1
    let cars = [bus];

    let input = PlanInput {
        ego: EgoState {
            pose: Pose { x_m: 6.0, y_m: -2.5, heading_rad: 0.0 },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal { target: Pose { x_m: 60.0, y_m: -2.5, heading_rad: 0.0 } },
        map: &map,
        objects: &cars,
        controls: &[],
        lane_boundaries: &boundaries,
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[1], // the bus
        drivable: Some(&drivable),
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
    };
    let mut planner = GeometricPlanner::default();
    let plan = planner.plan(&input);

    let max_y = plan.trajectory.iter().map(|t| t.pose.y_m).fold(f64::MIN, f64::max);
    assert!(max_y <= 0.0, "school bus → no pass into oncoming, got max_y {max_y}");
    // And it HOLDS behind: a controlled decel-to-stop short of the bus (id 1 at
    // x=24, stop gap 5 → stop ~x=19), never reaching/passing it. (The full stop
    // completes over the receding horizon; here it is still decelerating in.)
    let max_x = plan.trajectory.iter().map(|t| t.pose.x_m).fold(f64::MIN, f64::max);
    assert!(max_x < 20.0, "ego stays behind the bus's stop line, got max_x {max_x}");
    assert!(
        plan.trajectory.last().unwrap().velocity_mps <= 2.0,
        "approaching at the slow object-approach speed (decel-to-stop), not cruising"
    );
}
