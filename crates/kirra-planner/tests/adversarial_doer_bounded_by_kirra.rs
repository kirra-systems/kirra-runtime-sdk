//! **The KIRRA thesis demo — a black-box doer bounded by the checker.**
//!
//! `plan_for_intent` is generic over `impl Planner`. This test drives the SAME Mick
//! intent through TWO doers behind that one seam:
//!
//!   * `GeometricPlanner` — the real Occy reference proposer (careful, safety-aware);
//!   * `RecklessDoer` — a stand-in for a *learned / black-box* planner that has NOT
//!     internalized safety: it plans a straight line toward the goal, driving through
//!     whatever is in the way (it samples no obstacle, corridor, or lane rule). It is
//!     otherwise a well-formed proposal, so its rejections are attributable to the
//!     hazard it ignored — not to a malformed shape.
//!
//! The claim it proves: **the safety outcome is invariant to the doer.** For the
//! same intent + same world, Occy stops short and KIRRA admits it, while the reckless
//! doer drives through the hazard and KIRRA *rejects* it (`MRCFallback`) — at which
//! point the consumer falls back to the always-available `PlanOutput::safe_stop`. No
//! unsafe trajectory reaches the actuator regardless of who authored it. And on a
//! clear road KIRRA *admits* the reckless doer too — the bound is precise (it rejects
//! unsafe outputs, not the doer per se), not blanket paranoia. That precision is what
//! makes "swap in a learned planner, the safety case is unchanged" a real claim.

use kirra_planner::{
    plan_for_intent, EgoState, GeometricPlanner, Goal, MickIntent, PlanInput, PlanOutput, Planner,
    Pose, ProposalKind, TrajectoryPoint,
};
use kirra_ros2_adapter::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_ros2_adapter::state::{PerceivedObject, TrajectoryVerdict};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};
use kirra_core::FleetPosture;

/// KIRRA refuses to certify a trajectory longer than this many poses — a WCET bound
/// on the containment check (`kirra_core::containment::
/// MAX_TRAJECTORY_HORIZON`). Any *valid* planner respects it; the reckless doer must
/// too, so that its rejections come from the HAZARD it ignores, not a length overrun.
const MAX_TRAJECTORY_HORIZON: usize = 50;

/// A learned / black-box doer that ignores safety: it lays a straight line from the
/// ego toward the goal at the ego's current (cruising) speed, sampling no obstacle,
/// corridor, or lane rule. It is otherwise a *well-formed* proposal — continuous in
/// speed, within the horizon cap, in a straight line — so the ONLY unsafe thing it
/// can do is drive *through* whatever happens to be in the way. That isolation is
/// deliberate: it makes any KIRRA rejection attributable to the hazard, nothing else.
struct RecklessDoer;

impl Planner for RecklessDoer {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        let ego = input.ego.pose;
        let goal = input.goal.target;
        let heading = (goal.y_m - ego.y_m).atan2(goal.x_m - ego.x_m);
        let dt = 0.1;
        // A gentle, in-envelope speed profile: start at the ego's current speed and
        // accelerate smoothly toward a cruise. Poses are spaced by `v·dt`, so geometry
        // and the velocity field agree — the proposal is kinematically well-formed.
        // The recklessness is purely spatial: it never samples what it drives over.
        let accel = 1.2;
        let v_cruise = 8.0;
        let (cos_h, sin_h) = (heading.cos(), heading.sin());
        let mut v = input.ego.linear_x_mps.max(1.0);
        let mut s = 0.0;
        let trajectory = (0..MAX_TRAJECTORY_HORIZON)
            .map(|i| {
                let p = TrajectoryPoint {
                    pose: Pose {
                        x_m: ego.x_m + s * cos_h,
                        y_m: ego.y_m + s * sin_h,
                        heading_rad: heading,
                    },
                    velocity_mps: v,
                    time_from_start_s: i as f64 * dt,
                };
                s += v * dt;
                v = (v + accel * dt).min(v_cruise);
                p
            })
            .collect();
        PlanOutput { trajectory, kind: ProposalKind::Motion }
    }
}

fn world<'a>(map: &'a dyn CorridorSource, objects: &'a [PerceivedObject]) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState {
            pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal { target: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 } },
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

/// KIRRA's verdict on a proposal — the one authority both doers answer to.
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

/// THE DEMO: same intent, same world, two doers — KIRRA is the invariant bound.
#[test]
fn kirra_bounds_the_reckless_doer_exactly_as_it_bounds_occy() {
    // A stopped car blocks the lane at x=25; Mick's intent is "go to x=40" (past it).
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let objs = [stopped_car(25.0)];
    let w = world(&corr, &objs);
    let intent = MickIntent::GoTo { x_m: 40.0, y_m: 0.0 };

    // Doer 1 — the real Occy: it stops short of the car, and KIRRA admits the plan.
    let mut occy = GeometricPlanner::default();
    let occy_out = plan_for_intent(&mut occy, &intent, &w);
    let occy_max_x = occy_out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
    assert!(occy_max_x < 25.0, "Occy stops short of the obstacle, got {occy_max_x}");
    assert!(admitted(kirra_verdict(&occy_out, &corr, &objs)), "KIRRA admits Occy's safe plan");

    // Doer 2 — the reckless/learned doer behind the SAME seam: it drives straight
    // THROUGH the car. KIRRA rejects it; the doer's choice does not reach the actuator.
    let mut reckless = RecklessDoer;
    let reckless_out = plan_for_intent(&mut reckless, &intent, &w);
    let reckless_max_x = reckless_out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
    assert!(reckless_max_x > 25.0, "the reckless doer drives past the obstacle, got {reckless_max_x}");
    assert_eq!(
        kirra_verdict(&reckless_out, &corr, &objs),
        TrajectoryVerdict::MRCFallback,
        "KIRRA REJECTS the reckless trajectory — the same bound that admitted Occy"
    );

    // On rejection the architecture's always-available fallback takes over: a safe
    // stop the checker accepts. The unsafe intent never actuates, whoever authored it.
    let fallback = PlanOutput::safe_stop(w.ego.pose);
    assert!(admitted(kirra_verdict(&fallback, &corr, &objs)), "the safe-stop fallback is admissible");
}

/// The bound is PRECISE, not blanket: on a clear road the very same reckless doer is
/// ADMITTED. KIRRA rejects unsafe *outputs*, not the doer — which is exactly why a
/// learned planner can be dropped in without changing the safety case.
#[test]
fn kirra_admits_the_reckless_doer_when_its_output_is_actually_safe() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let w = world(&corr, &[]); // clear road, no obstacles
    let intent = MickIntent::GoTo { x_m: 40.0, y_m: 0.0 };

    let mut reckless = RecklessDoer;
    let out = plan_for_intent(&mut reckless, &intent, &w);
    let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
    assert!(max_x > 25.0, "the reckless doer drives toward the goal, got {max_x}");
    assert!(
        admitted(kirra_verdict(&out, &corr, &[])),
        "with nothing to hit, KIRRA admits the reckless straight-line — the bound is precise"
    );
}
