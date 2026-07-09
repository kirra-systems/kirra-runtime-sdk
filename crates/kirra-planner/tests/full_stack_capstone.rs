//! **Capstone: the full composed doer-checker stack, end to end.**
//!
//! The session's safety gates were each proven in isolation; this pins their COMPOSITION on one
//! shared two-junction map, the doer proposing and KIRRA bounding throughout:
//!
//! 1. **Route + occlusion (closed loop)** — the ego routes across BOTH junctions while CREEPING
//!    the blind first approach (the assured-clear speed bound), the fast loop only ever tracking a
//!    KIRRA-admitted trajectory; an open-approach control run isolates the occlusion effect.
//! 2. **Dynamic obstacle (on the route)** — a slow lead in the final lane makes the doer FOLLOW
//!    (speed-matched), not blow past, and KIRRA admits the follow.
//! 3. **Lane-follow / map-intention** — an object whose lane curves INTO the path is yielded to
//!    where a constant-velocity predictor would miss it.

use kirra_planner::{
    plan_for_intent, EgoState, FastLoopTracker, FleetPosture, GeometricPlanner, Goal, Lane,
    LaneCorridor, LaneEdge, LaneGraph, LineType, MickDriver, MickIntent, PlanInput, PlanOutput,
    Pose, ScriptedBrain, TrajectoryVerdict,
};
use kirra_trajectory::corridor::{CorridorSource, Point};
use kirra_trajectory::state::PerceivedObject;
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

const FAST_DT_S: f64 = 0.1;
const FAST_DT_MS: u64 = 100;
const TICKS: usize = 360;
const MRC_DECEL: f64 = 3.0;
const REPLAN_MS: u64 = 500;
const R: f64 = 12.0;

fn quarter_arc(cx: f64, cy: f64, r: f64, start_angle: f64, sweep: f64, n: usize) -> Vec<Point> {
    (0..=n)
        .map(|i| {
            let t = start_angle + sweep * (i as f64 / n as f64);
            Point {
                x_m: cx + r * t.cos(),
                y_m: cy + r * t.sin(),
            }
        })
        .collect()
}

fn lane(id: u64, cl: Vec<Point>, heading: f64, succ: &[u64]) -> Lane {
    Lane {
        id,
        centerline: cl,
        half_width_m: 3.0,
        left_line: LineType::Solid,
        right_line: LineType::Solid,
        heading_rad: heading,
        edges: succ.iter().map(|&to| LaneEdge::Successor { to }).collect(),
        control: None,
    }
}

/// Two-junction route (east → LEFT arc → north → RIGHT arc → east), decoy branch at J1.
/// `occlude` flags the FIRST approach (lane 1) as a blind junction (short assured-clear sight).
fn two_junction_route(occlude: bool) -> LaneGraph {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
    let arc_left = quarter_arc(30.0, 12.0, R, -FRAC_PI_2, FRAC_PI_2, 12);
    let arc_right = quarter_arc(54.0, 40.0, R, PI, -FRAC_PI_2, 12);
    let mut g = LaneGraph::new()
        .with_lane(lane(
            1,
            vec![
                Point { x_m: 0.0, y_m: 0.0 },
                Point {
                    x_m: 30.0,
                    y_m: 0.0,
                },
            ],
            0.0,
            &[2, 6],
        ))
        .with_lane(lane(2, arc_left, FRAC_PI_4, &[3]))
        .with_lane(lane(
            3,
            vec![
                Point {
                    x_m: 42.0,
                    y_m: 12.0,
                },
                Point {
                    x_m: 42.0,
                    y_m: 40.0,
                },
            ],
            FRAC_PI_2,
            &[4],
        ))
        .with_lane(lane(4, arc_right, FRAC_PI_4, &[5]))
        .with_lane(lane(
            5,
            vec![
                Point {
                    x_m: 54.0,
                    y_m: 52.0,
                },
                Point {
                    x_m: 90.0,
                    y_m: 52.0,
                },
            ],
            0.0,
            &[],
        ))
        .with_lane(lane(
            6,
            vec![
                Point {
                    x_m: 30.0,
                    y_m: 0.0,
                },
                Point {
                    x_m: 30.0,
                    y_m: -20.0,
                },
            ],
            -FRAC_PI_2,
            &[],
        ));
    if occlude {
        g = g.with_occluded_approach(1, 3.0); // ~3.0 m/s assured-clear creep cap (blind corner)
    }
    g
}

#[allow(clippy::too_many_arguments)]
fn world<'a>(
    ego: EgoState,
    map: &'a dyn CorridorSource,
    objects: &'a [PerceivedObject],
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
        signal_states: &[],
    }
}

struct RunResult {
    approach_peak_mps: f64, // max ego speed while on the occluded approach (x < 28)
    reached_lane5: bool,    // ego reached the final east-bound exit lane
    admitted: u32,
    total: u32,
    ego_final: Pose,
}

/// Run the dual-rate loop over the two-junction route with `objects` present; returns the metrics.
fn run(g: &LaneGraph, objects: &[PerceivedObject]) -> RunResult {
    let route_ids = g.route(1, 5).expect("route");
    let route: LaneCorridor = g.route_corridor(&route_ids, 0.95, 5).expect("corridor");
    let goal = Pose {
        x_m: 84.0,
        y_m: 52.0,
        heading_rad: 0.0,
    };

    let mut driver = MickDriver::new(ScriptedBrain::new(vec![
        MickIntent::RouteTo {
            x_m: 84.0,
            y_m: 52.0
        };
        200
    ]));
    let mut occy = GeometricPlanner::default();
    let mut tracker = FastLoopTracker::new();

    // Start slow so the occlusion creep cap (≈3.0) actually binds the approach (a fast start would
    // already be above it).
    let mut ego = EgoState {
        pose: Pose {
            x_m: 16.0,
            y_m: 0.0,
            heading_rad: 0.0,
        },
        linear_x_mps: 2.0,
        yaw_rate_rads: 0.0,
        stamp_ms: 0,
    };
    let mut last_replan_ms: Option<u64> = None;
    let (mut admitted, mut total) = (0u32, 0u32);
    let mut approach_peak = 0.0_f64;

    for tick in 1..=TICKS {
        let now_ms = tick as u64 * FAST_DT_MS;
        let replan_due = tracker.is_exhausted(now_ms)
            || last_replan_ms.is_none_or(|t| now_ms.saturating_sub(t) >= REPLAN_MS);
        if replan_due {
            let w = world(ego, &route, objects, g, goal);
            let plan = driver.drive_tick(&w, &mut occy, now_ms);
            let v = validate_trajectory_slow(
                &plan.trajectory,
                &route,
                objects,
                &VehicleConfig::default_urban(),
                None,
                FleetPosture::Nominal,
            );
            total += 1;
            if matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp) {
                admitted += 1;
                tracker.promote(plan, now_ms);
                last_replan_ms = Some(now_ms);
            }
        }
        ego = match tracker.track(now_ms) {
            Some(cmd) => EgoState {
                pose: cmd.pose,
                linear_x_mps: cmd.velocity_mps,
                yaw_rate_rads: 0.0,
                stamp_ms: now_ms,
            },
            None => EgoState {
                pose: ego.pose,
                linear_x_mps: (ego.linear_x_mps - MRC_DECEL * FAST_DT_S).max(0.0),
                yaw_rate_rads: 0.0,
                stamp_ms: now_ms,
            },
        };
        if ego.pose.x_m < 28.0 && ego.pose.y_m < 6.0 {
            approach_peak = approach_peak.max(ego.linear_x_mps);
        }
    }

    RunResult {
        approach_peak_mps: approach_peak,
        reached_lane5: ego.pose.x_m > 58.0 && ego.pose.y_m > 45.0,
        admitted,
        total,
        ego_final: ego.pose,
    }
}

#[test]
fn the_full_stack_routes_two_junctions_creeping_the_occluded_approach_kirra_bounding() {
    // Composed closed loop: route across BOTH junctions while CREEPING the blind first approach,
    // KIRRA bounding throughout. An open-approach control run isolates the occlusion effect.
    let occluded = run(&two_junction_route(true), &[]);
    let open = run(&two_junction_route(false), &[]);

    println!(
        "OCCLUDED approach_peak={:.2} reached5={} admitted={}/{} final=({:.1},{:.1}) | OPEN approach_peak={:.2} reached5={}",
        occluded.approach_peak_mps, occluded.reached_lane5, occluded.admitted, occluded.total,
        occluded.ego_final.x_m, occluded.ego_final.y_m, open.approach_peak_mps, open.reached_lane5
    );

    // (1) KIRRA bounds every pose: the fast loop only ever tracks a KIRRA-admitted trajectory.
    assert!(
        occluded.admitted > 0,
        "KIRRA admitted the trajectories the loop tracked"
    );
    // (2) OCCLUSION: the ego CREEPS the blind first approach — under the assured-clear cap (~3 m/s
    //     for 3 m of sight) and markedly slower than the open control taking the SAME approach.
    assert!(
        occluded.approach_peak_mps < 3.5,
        "occluded approach is creep-capped, got {}",
        occluded.approach_peak_mps
    );
    assert!(
        occluded.approach_peak_mps < open.approach_peak_mps - 0.8,
        "occluded approach is slower than the open one ({} vs {})",
        occluded.approach_peak_mps,
        open.approach_peak_mps
    );
    // (3) ROUTE: despite creeping the blind approach, the ego still completes BOTH junctions into
    //     the final east-bound exit lane — the multi-junction route driven end to end.
    assert!(
        occluded.reached_lane5,
        "the ego routes through both junctions to lane 5, final=({:.1},{:.1})",
        occluded.ego_final.x_m, occluded.ego_final.y_m
    );
}

#[test]
fn the_full_stack_follows_a_dynamic_lead_in_the_final_lane_and_kirra_admits() {
    // The dynamic-obstacle gate, composed on the route: with the ego already on the final exit lane
    // (lane 5) following the route corridor, a slow lead ahead makes the doer FOLLOW (speed-matched)
    // — not blow past — and KIRRA admits the follow. A control without the lead drives faster.
    let g = two_junction_route(false);
    let route_ids = g.route(1, 5).unwrap();
    let route: LaneCorridor = g.route_corridor(&route_ids, 0.95, 5).unwrap();
    let ego = EgoState {
        pose: Pose {
            x_m: 58.0,
            y_m: 52.0,
            heading_rad: 0.0,
        },
        linear_x_mps: 5.0,
        yaw_rate_rads: 0.0,
        stamp_ms: 0,
    };
    let goal = Pose {
        x_m: 84.0,
        y_m: 52.0,
        heading_rad: 0.0,
    };
    let lead = [PerceivedObject {
        id: 9,
        pos: Point {
            x_m: 68.0,
            y_m: 52.0,
        },
        velocity_mps: 1.5,
        heading_rad: 0.0,
        vel: Point { x_m: 1.5, y_m: 0.0 },
    }];
    let intent = MickIntent::RouteTo {
        x_m: 84.0,
        y_m: 52.0,
    };
    let peak = |p: &PlanOutput| {
        p.trajectory
            .iter()
            .map(|t| t.velocity_mps)
            .fold(0.0, f64::max)
    };
    let admit = |p: &PlanOutput, objs: &[PerceivedObject]| {
        matches!(
            validate_trajectory_slow(
                &p.trajectory,
                &route,
                objs,
                &VehicleConfig::default_urban(),
                None,
                FleetPosture::Nominal
            ),
            TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
        )
    };

    let clear = plan_for_intent(
        &mut GeometricPlanner::default(),
        &intent,
        &world(ego, &route, &[], &g, goal),
    );
    let follow = plan_for_intent(
        &mut GeometricPlanner::default(),
        &intent,
        &world(ego, &route, &lead, &g, goal),
    );

    assert!(
        peak(&clear) > 5.0,
        "clear lane → drives on, peak {}",
        peak(&clear)
    );
    assert!(
        peak(&follow) < peak(&clear) - 1.0,
        "the ego matches the slow lead, not cruise ({} vs {})",
        peak(&follow),
        peak(&clear)
    );
    assert!(
        admit(&follow, &lead),
        "KIRRA admits the speed-matched follow"
    );
    assert!(admit(&clear, &[]), "KIRRA admits the clear-lane plan");
}

#[test]
fn the_lane_follow_mode_bounds_a_curving_in_merger_on_the_route() {
    // The map-intention gate, composed on the same substrate: an object PARALLEL to the ego lane
    // (CV keeps it clear) on a lane that MERGES into the ego's path. Its lane-follow predicted
    // path traces the merge → the doer yields; without the map (CV) it would not.
    let merge = LaneGraph::new().with_lane(lane(
        1,
        vec![
            Point {
                x_m: 12.0,
                y_m: 4.0,
            },
            Point {
                x_m: 25.0,
                y_m: 4.0,
            },
            Point {
                x_m: 35.0,
                y_m: 0.0,
            },
            Point {
                x_m: 60.0,
                y_m: 0.0,
            },
        ],
        0.0,
        &[],
    ));
    let corr = kirra_trajectory::corridor::MockCorridorSource::straight_5m_half_width(100.0);
    let obj = [PerceivedObject {
        id: 1,
        pos: Point {
            x_m: 20.0,
            y_m: 4.0,
        },
        velocity_mps: 5.0,
        heading_rad: 0.0,
        vel: Point { x_m: 5.0, y_m: 0.0 },
    }];
    let intent = MickIntent::GoTo {
        x_m: 80.0,
        y_m: 0.0,
    };
    let reach = |o: &PlanOutput| o.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
    let ego = EgoState {
        pose: Pose {
            x_m: 5.0,
            y_m: 0.0,
            heading_rad: 0.0,
        },
        linear_x_mps: 2.0,
        yaw_rate_rads: 0.0,
        stamp_ms: 0,
    };
    let goal = Pose {
        x_m: 80.0,
        y_m: 0.0,
        heading_rad: 0.0,
    };

    // CV-only (no lane graph): the object holds y=4, never enters the ego band → no yield.
    let w_cv = PlanInput {
        lane_graph: None,
        ..world(ego, &corr, &obj, &merge, goal)
    };
    let cv = plan_for_intent(&mut GeometricPlanner::default(), &intent, &w_cv);
    // With the map: the lane-follow mode predicts the merge into the ego path → yields short.
    let w_map = world(ego, &corr, &obj, &merge, goal);
    let mapped = plan_for_intent(&mut GeometricPlanner::default(), &intent, &w_map);

    assert!(
        reach(&cv) > 30.0,
        "CV: lane-parallel object → no yield, got {}",
        reach(&cv)
    );
    assert!(
        reach(&mapped) < 28.0,
        "map-intention: the merging object is yielded to, got {}",
        reach(&mapped)
    );
}
