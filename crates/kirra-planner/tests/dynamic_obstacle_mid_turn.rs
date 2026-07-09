//! **A dynamic obstacle encountered MID-TURN — doer adapts on the curved arc, KIRRA bounds it.**
//!
//! The junction tests (`turn_at_junction`, `intersection_closed_loop`, `multi_junction_route`)
//! drive turns on a CLEAR route, or with a static vehicle safely off the path. This pins the
//! missing case: a *moving* obstacle that conflicts with the ego while it is committed to the
//! turn — the realistic junction hazard (a vehicle crossing the exit, a slow lead in it). The
//! ego is following the stitched route corridor's CURVED centerline, so this also proves the
//! planner's predictive-yield and lead-follow logic (which project objects onto the `guide`) and
//! KIRRA's per-pose RSS both work on a curved trajectory, not just a straight one.
//!
//! Two complementary behaviors, the doer-checker split at the junction:
//!   * a crossing HAZARD the ego cannot safely clear → the doer YIELDS (decelerates to a stop
//!     short on the arc) AND KIRRA refuses an unsafe drive-through (fail-closed defense in depth);
//!   * a slow LEAD moving up the exit ahead → the doer FOLLOWS it (speed-matched) and KIRRA
//!     ADMITS the follow, while still refusing the cruise-through plan (doer-checker agreement on
//!     a safe maneuver, the checker bounding the unsafe alternative).

use kirra_planner::{
    EgoState, FleetPosture, GeometricPlanner, Goal, Lane, LaneEdge, LaneGraph, LineType,
    MotionState, PerceivedObject, PlanInput, Planner, Pose, ProposalKind, TrajectoryVerdict,
};
use kirra_trajectory::corridor::Point;
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

/// A quarter-circle arc (n+1 points) sweeping +π/2 about `(cx, cy)` from `start` — a smooth
/// LEFT-turn centerline.
fn quarter_arc(cx: f64, cy: f64, r: f64, start: f64, n: usize) -> Vec<Point> {
    (0..=n)
        .map(|i| {
            let t = start + std::f64::consts::FRAC_PI_2 * (i as f64 / n as f64);
            Point {
                x_m: cx + r * t.cos(),
                y_m: cy + r * t.sin(),
            }
        })
        .collect()
}

/// Left-turn junction (arc radius `r`, lane half-width 3): approach east (1) → LEFT arc (2) →
/// straight north exit (3, at x = 20+r).
fn left_turn(r: f64) -> LaneGraph {
    let line = LineType::Solid;
    let arc = quarter_arc(20.0, r, r, -std::f64::consts::FRAC_PI_2, 12);
    LaneGraph::new()
        .with_lane(
            Lane::straight(1, 0.0, 0.0, 20.0, 3.0, line, line)
                .with_edge(LaneEdge::Successor { to: 2 }),
        )
        .with_lane(Lane {
            id: 2,
            centerline: arc,
            half_width_m: 3.0,
            left_line: line,
            right_line: line,
            heading_rad: std::f64::consts::FRAC_PI_4,
            edges: vec![LaneEdge::Successor { to: 3 }],
            control: None,
        })
        .with_lane(Lane {
            id: 3,
            centerline: vec![
                Point {
                    x_m: 20.0 + r,
                    y_m: r,
                },
                Point {
                    x_m: 20.0 + r,
                    y_m: r + 20.0,
                },
            ],
            half_width_m: 3.0,
            left_line: line,
            right_line: line,
            heading_rad: std::f64::consts::FRAC_PI_2,
            edges: Vec::new(),
            control: None,
        })
}

const R: f64 = 12.0;

/// The ego committed to the turn, low on the arc (heading part-way between east and north),
/// driving toward a goal up the exit lane along the stitched route corridor.
fn mid_turn_world<'a>(
    map: &'a dyn kirra_trajectory::corridor::CorridorSource,
    objects: &'a [PerceivedObject],
    motion: &'a [MotionState],
) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: 23.5,
                y_m: 1.0,
                heading_rad: 0.4,
            },
            linear_x_mps: 3.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal {
            target: Pose {
                x_m: 20.0 + R,
                y_m: R + 16.0,
                heading_rad: std::f64::consts::FRAC_PI_2,
            },
        },
        map,
        objects,
        controls: &[],
        lane_boundaries: &[],
        motion,
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
        signal_states: &[],
    }
}

fn verdict(
    traj: &[kirra_planner::TrajectoryPoint],
    map: &dyn kirra_trajectory::corridor::CorridorSource,
    objs: &[PerceivedObject],
) -> TrajectoryVerdict {
    validate_trajectory_slow(
        traj,
        map,
        objs,
        &VehicleConfig::default_urban(),
        None,
        FleetPosture::Nominal,
    )
}
fn admitted(v: TrajectoryVerdict) -> bool {
    matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp)
}
fn max_y(p: &kirra_planner::PlanOutput) -> f64 {
    p.trajectory
        .iter()
        .map(|t| t.pose.y_m)
        .fold(f64::MIN, f64::max)
}
fn terminal_v(p: &kirra_planner::PlanOutput) -> f64 {
    p.trajectory.last().map(|t| t.velocity_mps).unwrap_or(0.0)
}

#[test]
fn a_dynamic_crosser_mid_turn_is_yielded_by_the_doer_and_bounded_by_kirra() {
    let g = left_turn(R);
    let route = g.route(1, 3).unwrap();
    let map = g.route_corridor(&route, 0.95, 10).unwrap();
    let mut occy = GeometricPlanner::default();

    // Control: on a CLEAR turn the ego drives up through the junction past y=18 and KIRRA admits.
    let clear = occy.plan(&mid_turn_world(&map, &[], &[]));
    assert_eq!(clear.kind, ProposalKind::Motion);
    assert!(
        max_y(&clear) > 18.0,
        "clear turn climbs through the junction, got max_y {}",
        max_y(&clear)
    );
    assert!(
        admitted(verdict(&clear.trajectory, &map, &[])),
        "KIRRA admits the clear turn"
    );

    // A vehicle crossing the EXIT lane westbound, lingering in the conflict band. At planning
    // time it sits 4 m right of the exit centerline → OUT of the stop-short lane band, so the
    // PREDICTIVE yield (rolling its motion forward onto the curved guide) is what must catch it.
    let crosser = PerceivedObject {
        id: 7,
        pos: Point {
            x_m: 36.0,
            y_m: 18.0,
        },
        velocity_mps: 1.5,
        heading_rad: std::f64::consts::PI,
        vel: Point {
            x_m: -1.5,
            y_m: 0.0,
        },
    };
    let objs = [crosser];
    let motion = [MotionState {
        id: 7,
        yaw_rate_rad_s: 0.0,
    }];

    // The doer YIELDS: predictive yield fires on the curved arc → the plan decelerates to a stop
    // SHORT of the crosser (well below the clear plan's reach), rather than driving into it.
    let yielded = occy.plan(&mid_turn_world(&map, &objs, &motion));
    assert!(
        max_y(&yielded) < 15.0,
        "the doer stops short of the crossing hazard, got max_y {}",
        max_y(&yielded)
    );
    assert!(
        terminal_v(&yielded) < 0.5,
        "the yield is a decel-to-stop, terminal v {}",
        terminal_v(&yielded)
    );
    assert!(
        max_y(&yielded) < max_y(&clear) - 4.0,
        "the doer yields well short of the clear-turn reach"
    );

    // Doer-checker AGREEMENT (the predictive-yield-gap refinement): the doer's yield leaves the
    // checker's longitudinal-conflict distance before the crossing, so the stopped ego sits
    // outside the window where the lateral RSS against the cutting-in crosser binds → KIRRA
    // ADMITS the smooth yield instead of fail-closing to a safe-stop MRC.
    assert!(
        admitted(verdict(&yielded.trajectory, &map, &objs)),
        "KIRRA admits the doer's predictive-yield mid-turn (smooth yield, not fail-closed MRC), got {:?}",
        verdict(&yielded.trajectory, &map, &objs)
    );

    // KIRRA independently bounds the dynamic obstacle mid-turn: the clear-turn trajectory, driven
    // INTO the crosser, is refused (the laterally cutting-in crosser breaches RSS on the arc).
    assert_eq!(
        verdict(&clear.trajectory, &map, &objs),
        TrajectoryVerdict::MRCFallback,
        "KIRRA refuses driving the turn through the crossing hazard"
    );
}

#[test]
fn a_dynamic_lead_mid_turn_is_followed_by_the_doer_and_kirra_admits_the_follow() {
    let g = left_turn(R);
    let route = g.route(1, 3).unwrap();
    let map = g.route_corridor(&route, 0.95, 10).unwrap();
    let mut occy = GeometricPlanner::default();

    let clear = occy.plan(&mid_turn_world(&map, &[], &[]));

    // A slow vehicle moving NORTH up the exit lane ahead — same direction as the ego's exit. The
    // doer should treat it as a moving LEAD: follow at a gap (speed-matched), not blow past it.
    let lead = PerceivedObject {
        id: 8,
        pos: Point {
            x_m: 20.0 + R,
            y_m: 18.0,
        },
        velocity_mps: 1.5,
        heading_rad: std::f64::consts::FRAC_PI_2,
        vel: Point { x_m: 0.0, y_m: 1.5 },
    };
    let objs = [lead];
    let motion = [MotionState {
        id: 8,
        yaw_rate_rad_s: 0.0,
    }];

    // The doer FOLLOWS the dynamic lead: a motion plan that holds well back behind it (much less
    // reach than the clear turn) — and KIRRA ADMITS the follow (rear-end RSS satisfied on the arc).
    let follow = occy.plan(&mid_turn_world(&map, &objs, &motion));
    assert_eq!(
        follow.kind,
        ProposalKind::Motion,
        "the doer follows the lead (motion), not a dead HOLD"
    );
    assert!(
        max_y(&follow) < max_y(&clear) - 5.0,
        "the follow holds back behind the lead, got max_y {} vs clear {}",
        max_y(&follow),
        max_y(&clear)
    );
    assert!(
        admitted(verdict(&follow.trajectory, &map, &objs)),
        "KIRRA admits the speed-matched follow through the turn"
    );

    // The payoff: the cruise-through plan (ignoring the lead) is refused — KIRRA bounds the
    // dynamic lead mid-turn even though the doer-followed alternative is admitted.
    assert_eq!(
        verdict(&clear.trajectory, &map, &objs),
        TrajectoryVerdict::MRCFallback,
        "KIRRA refuses cruising the turn into the slow lead"
    );
}
