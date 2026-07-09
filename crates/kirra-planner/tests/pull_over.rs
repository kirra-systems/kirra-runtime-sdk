//! End-to-end: **pull over to the road edge and stop → Occy → KIRRA**.
//!
//! The pull-over maneuver primitive. On a request (Mick's `PullOver` intent — e.g.
//! to yield to an emergency vehicle, or a commanded curb stop) Occy shifts as far
//! RIGHT as containment admits and decelerates to a controlled stop at the edge. It
//! only PROPOSES the park; KIRRA bounds the result — the ego never parks into an
//! obstacle, and a nearer hazard stops it before it finishes the move.
//!
//! These tests build a sized straight corridor so the (long, gentle) lateral ramp
//! and the decel-to-stop both fit inside the checker's trajectory horizon. The
//! emergency-vehicle *trigger* (tagging which object is an ambulance/police/fire) is
//! a separate, decided-but-deferred concern (an `emergency_vehicle_ids` input); this
//! PR is the maneuver it will drive.

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, Goal, LaneBoundary, LineType,
    MickIntent, PerceivedObject, PlanInput, Planner, Pose, ProposalKind, TrajectoryVerdict,
};
use kirra_trajectory::corridor::{CorridorSource, Point};
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

/// A straight corridor of a given half-width along +X (two-vertex polylines, the
/// minimum the checker accepts). A 4.5 m half-width gives a 4.8 m vehicle the room
/// to curb-park: mid-ramp the (long) footprint angles ~19° and its nose swings
/// toward the edge, so a narrower road (≈3.5 m half-width) cannot admit a full pull
/// to the edge — an honest kinematic bound, the pull-over analogue of "a narrow road
/// can't admit an overtake". A real shoulder would be supplied via `drivable`.
struct Straight {
    left: Vec<Point>,
    right: Vec<Point>,
}
impl Straight {
    fn new(half: f64, len: f64) -> Self {
        Self {
            left: vec![
                Point {
                    x_m: 0.0,
                    y_m: half,
                },
                Point {
                    x_m: len,
                    y_m: half,
                },
            ],
            right: vec![
                Point {
                    x_m: 0.0,
                    y_m: -half,
                },
                Point {
                    x_m: len,
                    y_m: -half,
                },
            ],
        }
    }
}
impl CorridorSource for Straight {
    fn left_boundary(&self) -> &[Point] {
        &self.left
    }
    fn right_boundary(&self) -> &[Point] {
        &self.right
    }
    fn confidence(&self) -> f32 {
        0.95
    }
    fn age_ms(&self) -> u64 {
        10
    }
}

const HALF: f64 = 4.5;

/// Ego cruising up the corridor centerline (y=0) with a far goal, optionally
/// requesting a pull-over and/or carrying lane boundaries that gate the move.
fn world<'a>(
    map: &'a dyn CorridorSource,
    objects: &'a [PerceivedObject],
    lane_boundaries: &'a [LaneBoundary],
    request_pull_over: bool,
) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: 5.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal {
            target: Pose {
                x_m: 80.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
        },
        map,
        objects,
        controls: &[],
        lane_boundaries,
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over,
        lane_graph: None,
        signal_states: &[],
    }
}

fn verdict(
    plan: &kirra_planner::PlanOutput,
    map: &dyn CorridorSource,
    objs: &[PerceivedObject],
) -> TrajectoryVerdict {
    validate_trajectory_slow(
        &plan.trajectory,
        map,
        objs,
        &VehicleConfig::default_urban(),
        None,
        FleetPosture::Nominal,
    )
}

fn min_y(plan: &kirra_planner::PlanOutput) -> f64 {
    plan.trajectory
        .iter()
        .map(|t| t.pose.y_m)
        .fold(f64::MAX, f64::min)
}
fn max_x(plan: &kirra_planner::PlanOutput) -> f64 {
    plan.trajectory
        .iter()
        .map(|t| t.pose.x_m)
        .fold(f64::MIN, f64::max)
}
fn final_v(plan: &kirra_planner::PlanOutput) -> f64 {
    plan.trajectory
        .last()
        .map(|t| t.velocity_mps)
        .unwrap_or(0.0)
}

#[test]
fn pull_over_shifts_to_the_right_edge_and_stops() {
    let map = Straight::new(HALF, 200.0);
    let mut occy = GeometricPlanner::default();
    let plan = occy.plan(&world(&map, &[], &[], true));

    // The ego moves RIGHT (negative y) toward the edge — far past the centerline it
    // was cruising on — and comes to a controlled stop there.
    assert_eq!(
        plan.kind,
        ProposalKind::Motion,
        "a pull-over is a move-then-stop, not an instant HOLD"
    );
    assert!(
        min_y(&plan) < -1.5,
        "the ego shifts toward the right edge, got min_y {}",
        min_y(&plan)
    );
    assert!(
        final_v(&plan) <= 0.05,
        "and decelerates to a stop at the edge, final v {}",
        final_v(&plan)
    );
}

#[test]
fn kirra_admits_a_clear_pull_over() {
    let map = Straight::new(HALF, 200.0);
    let mut occy = GeometricPlanner::default();
    let plan = occy.plan(&world(&map, &[], &[], true));

    assert!(
        matches!(
            verdict(&plan, &map, &[]),
            TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
        ),
        "a clear edge → KIRRA admits the pull-over, got {:?}",
        verdict(&plan, &map, &[])
    );
}

#[test]
fn no_request_means_no_pull_over() {
    // The opt-in gate: without the request the ego keeps cruising the centerline —
    // byte-for-byte prior behavior, no rightward drift.
    let map = Straight::new(HALF, 200.0);
    let mut occy = GeometricPlanner::default();
    let plan = occy.plan(&world(&map, &[], &[], false));

    assert!(
        min_y(&plan) > -0.5,
        "without a request the ego stays centered, got min_y {}",
        min_y(&plan)
    );
}

#[test]
fn a_solid_right_edge_line_forbids_the_pull_over() {
    // A solid line / barrier on the right (no lawful shoulder access) forbids the
    // rightward move — the legal constraint lives in Occy. The ego stays in lane.
    let map = Straight::new(HALF, 200.0);
    let boundaries = [LaneBoundary {
        y_m: -1.5,
        line: LineType::Solid,
    }];
    let mut occy = GeometricPlanner::default();
    let plan = occy.plan(&world(&map, &[], &boundaries, true));

    assert!(
        min_y(&plan) > -1.5,
        "a solid right edge → no pull-over across it, got min_y {}",
        min_y(&plan)
    );
}

#[test]
fn the_ego_never_parks_into_an_object_on_the_shoulder() {
    // The KIRRA-bounded safety property. A stopped object sits ahead at the very
    // lateral the pull-over would bring the ego to rest. Occy still shifts right (it
    // proposes the park) but must never drive INTO the object — it stops short. And
    // the always-available safe-stop fallback is admissible, so whether KIRRA admits
    // the cautious approach or MRC-rejects it as too close, the ego is held safe.
    // (Defense in depth: the planner stops short, and the checker is the floor — this
    // mirrors `learned_doer_bounded_by_kirra`.)
    let map = Straight::new(HALF, 200.0);
    let obj_x = 16.0;
    let shoulder_obj = PerceivedObject {
        id: 1,
        pos: Point {
            x_m: obj_x,
            y_m: -2.3,
        }, // out at the pull-over park lateral
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    };
    let objs = [shoulder_obj];
    let mut occy = GeometricPlanner::default();
    let plan = occy.plan(&world(&map, &objs, &[], true));

    assert!(
        min_y(&plan) < -1.0,
        "the ego still moves toward the edge to park, got min_y {}",
        min_y(&plan)
    );
    assert!(
        max_x(&plan) < obj_x,
        "but it stops short of the shoulder object, never reaching it (max_x {} vs object {obj_x})",
        max_x(&plan)
    );
    // The KIRRA floor: the immediate safe-stop is always an admissible action, so the
    // ego is never forced into an unsafe pull-over.
    let ego_pose = Pose {
        x_m: 5.0,
        y_m: 0.0,
        heading_rad: 0.0,
    };
    let fallback = kirra_planner::PlanOutput::safe_stop(ego_pose);
    assert!(
        matches!(
            verdict(&fallback, &map, &objs),
            TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
        ),
        "the safe-stop fallback is admissible, got {:?}",
        verdict(&fallback, &map, &objs)
    );
}

#[test]
fn mick_pull_over_intent_grounds_and_kirra_admits() {
    // The full Mick path: the `PullOver` intent (the LLM chauffeur yielding to an
    // emergency vehicle) grounds through Occy to the edge-park maneuver, and KIRRA
    // admits the clear pull-over.
    let map = Straight::new(HALF, 200.0);
    let mut occy = GeometricPlanner::default();
    let plan = plan_for_intent(
        &mut occy,
        &MickIntent::PullOver,
        &world(&map, &[], &[], false),
    );

    assert!(
        min_y(&plan) < -1.5,
        "Mick's PullOver intent drives the ego to the edge, got min_y {}",
        min_y(&plan)
    );
    assert!(
        final_v(&plan) <= 0.05,
        "and stops there, final v {}",
        final_v(&plan)
    );
    assert!(
        matches!(
            verdict(&plan, &map, &[]),
            TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
        ),
        "KIRRA admits Mick's pull-over, got {:?}",
        verdict(&plan, &map, &[])
    );
}
