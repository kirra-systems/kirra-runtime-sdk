//! **A real *maneuvering* learned planner behind the seam, still bounded by KIRRA.**
//!
//! The speed-only `LearnedPlanner` (see `learned_doer_bounded_by_kirra.rs`) proved the §5
//! thesis but could only choose how fast to drive STRAIGHT at the goal. `LearnedManeuverPlanner`
//! generalizes it to a real 2-D Hydra-MDP trajectory vocabulary (lateral offset × speed), so the
//! learned scorer can ROUTE AROUND a hazard. The safety case is unchanged — KIRRA is still the bound:
//!
//!   * on a CLEAR road both regimes drive straight, KIRRA admits;
//!   * a SAFETY-AWARE net learns to PASS a hazard where the road is wide → KIRRA admits the pass
//!     (the >4 m clearance clears the RSS lateral band);
//!   * the SAME pass on a NARROW road does not fit → KIRRA REJECTS it → safe-stop fallback
//!     (the bound catches a maneuver the corridor cannot contain — the net does not know width);
//!   * a PROGRESS-ONLY (misaligned) net barrels STRAIGHT through → KIRRA REJECTS it.

use kirra_core::FleetPosture;
use kirra_planner::{
    plan_for_intent, EgoState, Goal, LearnedManeuverPlanner, MickIntent, PlanInput, PlanOutput,
    Pose, Teacher,
};
use kirra_trajectory::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_trajectory::state::{PerceivedObject, TrajectoryVerdict};
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

const SEED: u64 = 0xC0FFEE;

/// A straight corridor of arbitrary half-width — the wide road the pass needs (the stock
/// `MockCorridorSource` is fixed at ±5 m, too narrow to contain a >4 m lateral pass).
struct WideCorridor {
    left: Vec<Point>,
    right: Vec<Point>,
}
impl WideCorridor {
    fn new(half_width_m: f64, length_m: f64) -> Self {
        Self {
            left: vec![
                Point {
                    x_m: 0.0,
                    y_m: half_width_m,
                },
                Point {
                    x_m: length_m,
                    y_m: half_width_m,
                },
            ],
            right: vec![
                Point {
                    x_m: 0.0,
                    y_m: -half_width_m,
                },
                Point {
                    x_m: length_m,
                    y_m: -half_width_m,
                },
            ],
        }
    }
}
impl CorridorSource for WideCorridor {
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

fn world<'a>(map: &'a dyn CorridorSource, objects: &'a [PerceivedObject]) -> PlanInput<'a> {
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
                x_m: 40.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
        },
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
        lane_graph: None,
        signal_states: &[],
    }
}

fn stopped_car(x: f64) -> PerceivedObject {
    PerceivedObject {
        id: 1,
        pos: Point { x_m: x, y_m: 0.0 },
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }
}

fn kirra_verdict(
    out: &PlanOutput,
    corr: &dyn CorridorSource,
    objs: &[PerceivedObject],
) -> TrajectoryVerdict {
    validate_trajectory_slow(
        &out.trajectory,
        corr,
        objs,
        &VehicleConfig::default_urban(),
        None,
        FleetPosture::Nominal,
    )
}

fn admitted(v: TrajectoryVerdict) -> bool {
    matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp)
}

fn reach(out: &PlanOutput) -> f64 {
    out.trajectory
        .iter()
        .map(|t| t.pose.x_m)
        .fold(0.0, f64::max)
}

fn max_abs_y(out: &PlanOutput) -> f64 {
    out.trajectory
        .iter()
        .map(|t| t.pose.y_m.abs())
        .fold(0.0, f64::max)
}

/// Sanity: on a clear road the maneuvering net drives straight toward the goal (no gratuitous
/// detour — the lateral-cost term makes straight the default), and KIRRA admits.
#[test]
fn maneuver_planner_drives_straight_on_a_clear_road() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let w = world(&corr, &[]);
    let intent = MickIntent::GoTo {
        x_m: 40.0,
        y_m: 0.0,
    };

    for teacher in [Teacher::SafetyAware, Teacher::ProgressOnly] {
        let mut p = LearnedManeuverPlanner::trained(SEED, teacher);
        let (offset, _) = p.chosen_candidate(&w);
        assert_eq!(
            offset, 0.0,
            "{teacher:?}: drives straight on a clear road (no detour)"
        );
        let out = plan_for_intent(&mut p, &intent, &w);
        assert!(
            reach(&out) > 15.0,
            "{teacher:?}: makes progress, got {}",
            reach(&out)
        );
        assert!(
            max_abs_y(&out) < 0.5,
            "{teacher:?}: stays centered, max|y|={}",
            max_abs_y(&out)
        );
        assert!(
            admitted(kirra_verdict(&out, &corr, &[])),
            "{teacher:?}: KIRRA admits the clear-road plan"
        );
    }
}

/// THE new capability: a SAFETY-AWARE maneuvering net ROUTES AROUND a stopped car where the
/// road is wide enough — a learned lateral pass, not a slow-down — and KIRRA ADMITS it (the
/// >4 m clearance clears the RSS lateral-alignment band).
#[test]
fn safety_aware_maneuver_routes_around_and_kirra_admits_on_a_wide_road() {
    let corr = WideCorridor::new(8.0, 100.0);
    let objs = [stopped_car(25.0)];
    let w = world(&corr, &objs);
    let intent = MickIntent::GoTo {
        x_m: 40.0,
        y_m: 0.0,
    };

    let mut p = LearnedManeuverPlanner::trained(SEED, Teacher::SafetyAware);
    let (offset, _) = p.chosen_candidate(&w);
    assert!(
        offset.abs() > 1.0,
        "the safety-aware net chooses a lateral pass, offset={offset}"
    );

    let out = plan_for_intent(&mut p, &intent, &w);
    assert!(
        reach(&out) > 25.0,
        "the pass drives PAST the hazard (does not stop short), reach={}",
        reach(&out)
    );
    assert!(
        max_abs_y(&out) > 4.0,
        "the path swings clear of the centerline hazard, max|y|={}",
        max_abs_y(&out)
    );
    assert!(
        admitted(kirra_verdict(&out, &corr, &objs)),
        "KIRRA admits the route-around on a wide road"
    );
}

/// The SAME safety-aware pass on a NARROW road does not fit the corridor → KIRRA REJECTS it,
/// and the always-available safe-stop takes over. The net does not know the corridor width;
/// KIRRA is the bound that catches the maneuver the road cannot contain.
#[test]
fn kirra_bounds_the_pass_that_does_not_fit_a_narrow_road() {
    let narrow = MockCorridorSource::straight_5m_half_width(100.0); // ±5 m: a >4 m pass won't fit
    let objs = [stopped_car(25.0)];
    let w = world(&narrow, &objs);
    let intent = MickIntent::GoTo {
        x_m: 40.0,
        y_m: 0.0,
    };

    let mut p = LearnedManeuverPlanner::trained(SEED, Teacher::SafetyAware);
    let (offset, _) = p.chosen_candidate(&w);
    assert!(
        offset.abs() > 1.0,
        "the net still proposes the pass (blind to corridor width), offset={offset}"
    );

    let out = plan_for_intent(&mut p, &intent, &w);
    assert_eq!(
        kirra_verdict(&out, &narrow, &objs),
        TrajectoryVerdict::MRCFallback,
        "KIRRA rejects the pass the narrow corridor cannot contain"
    );
    let fallback = PlanOutput::safe_stop(w.ego.pose);
    assert!(
        admitted(kirra_verdict(&fallback, &narrow, &objs)),
        "the safe-stop fallback is admissible"
    );
}

/// A PROGRESS-ONLY (misaligned) maneuvering net ignores clearance → it barrels STRAIGHT through
/// the hazard (the lateral-cost term makes a gratuitous detour strictly worse) → KIRRA REJECTS
/// it. The 2-D vocabulary does not weaken the bound on a misaligned net.
#[test]
fn misaligned_maneuver_planner_barrels_straight_through_and_kirra_rejects() {
    let corr = WideCorridor::new(8.0, 100.0);
    let objs = [stopped_car(25.0)];
    let w = world(&corr, &objs);
    let intent = MickIntent::GoTo {
        x_m: 40.0,
        y_m: 0.0,
    };

    let mut p = LearnedManeuverPlanner::trained(SEED, Teacher::ProgressOnly);
    let (offset, _) = p.chosen_candidate(&w);
    assert_eq!(
        offset, 0.0,
        "the misaligned net drives straight (a detour only costs it progress)"
    );

    let out = plan_for_intent(&mut p, &intent, &w);
    assert!(
        reach(&out) > 25.0,
        "it drives through the hazard, reach={}",
        reach(&out)
    );
    assert!(
        max_abs_y(&out) < 0.5,
        "straight through, not around, max|y|={}",
        max_abs_y(&out)
    );
    assert_eq!(
        kirra_verdict(&out, &corr, &objs),
        TrajectoryVerdict::MRCFallback,
        "KIRRA rejects the misaligned straight-through plan"
    );
}
