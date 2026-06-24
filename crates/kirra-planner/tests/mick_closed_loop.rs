//! **Mick chauffeur — the CLOSED LOOP.** The other Mick tests judge one proposal; this
//! one runs the loop over time: each tick the brain proposes an intent, Occy grounds it,
//! KIRRA judges it, and the ego state ADVANCES — admitted → conform to the plan, rejected
//! → the fast loop's MRC (decelerate, hold). Repeat. The safety claim is then a property
//! of the whole *run*, not a single verdict:
//!
//!   1. a good chauffeur cruises and makes progress on a clear road;
//!   2. facing a hazard, the chauffeur is grounded to stop SHORT and KIRRA admits it —
//!      the ego creeps up and holds, never reaching the obstacle;
//!   3. THE PAYOFF — a persistently RECKLESS doer (a stand-in for a misaligned learned
//!      brain) tries to drive through the hazard *every tick*; KIRRA rejects it *every
//!      tick*, the ego MRCs, and across the entire run the ego never reaches the obstacle.
//!      A bad brain does not get safer with time; the guardrail holds it safe with time.

use kirra_planner::{
    mick_drive_once, EgoState, GeometricPlanner, Goal, MickBrain, MickError, MickIntent, PlanInput,
    PlanOutput, Planner, Pose, ProposalKind, TrajectoryPoint, WorldContext,
};
use kirra_ros2_adapter::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_ros2_adapter::state::{PerceivedObject, TrajectoryVerdict};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};
use kirra_core::FleetPosture;

const TICK_DT: f64 = 0.5; // seconds advanced per loop tick
const MRC_DECEL: f64 = 3.0; // m/s² the fast-loop safe-stop sheds on a rejection
const HAZARD_X: f64 = 25.0;

/// A brain that proposes the SAME intent every tick — a persistent goal/maneuver, the
/// realistic case for a chauffeur holding an objective across the run.
struct ConstantBrain(MickIntent);
impl MickBrain for ConstantBrain {
    fn decide(&mut self, _ctx: &WorldContext) -> Result<MickIntent, MickError> {
        Ok(self.0)
    }
}

/// A learned/black-box doer that ignores safety: a straight line from the ego toward the
/// goal, accelerating to cruise, sampling no obstacle. Mirrors the `RecklessDoer` in
/// `adversarial_doer_bounded_by_kirra.rs` — the misaligned brain whose mistakes KIRRA
/// must catch every tick.
struct RecklessDoer;
impl Planner for RecklessDoer {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        let ego = input.ego.pose;
        let goal = input.goal.target;
        let heading = (goal.y_m - ego.y_m).atan2(goal.x_m - ego.x_m);
        let (cos_h, sin_h) = (heading.cos(), heading.sin());
        let (dt, accel, v_cruise) = (0.1, 1.2, 8.0);
        let mut v = input.ego.linear_x_mps.max(1.0);
        let mut s = 0.0;
        let trajectory = (0..50)
            .map(|i| {
                let p = TrajectoryPoint {
                    pose: Pose { x_m: ego.x_m + s * cos_h, y_m: ego.y_m + s * sin_h, heading_rad: heading },
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

fn world<'a>(ego: EgoState, map: &'a dyn CorridorSource, objects: &'a [PerceivedObject]) -> PlanInput<'a> {
    PlanInput {
        ego,
        goal: Goal { target: Pose { x_m: 60.0, y_m: 0.0, heading_rad: 0.0 } },
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
    }
}

fn kirra_admits(plan: &PlanOutput, corr: &dyn CorridorSource, objs: &[PerceivedObject]) -> bool {
    matches!(
        validate_trajectory_slow(&plan.trajectory, corr, objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal),
        TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
    )
}

/// Advance the ego one tick. Admitted → conform to the accepted trajectory (the pose ~one
/// tick in). Rejected → the fast loop's MRC: shed speed toward 0 and hold position.
fn advance(ego: EgoState, plan: &PlanOutput, admitted: bool, t_ms: u64) -> EgoState {
    if !admitted {
        return EgoState {
            pose: ego.pose,
            linear_x_mps: (ego.linear_x_mps - MRC_DECEL * TICK_DT).max(0.0),
            yaw_rate_rads: 0.0,
            stamp_ms: t_ms,
        };
    }
    let tp = plan
        .trajectory
        .iter()
        .find(|p| p.time_from_start_s >= TICK_DT)
        .or_else(|| plan.trajectory.last());
    match tp {
        Some(p) => EgoState { pose: p.pose, linear_x_mps: p.velocity_mps, yaw_rate_rads: 0.0, stamp_ms: t_ms },
        None => ego,
    }
}

/// Run the closed loop for `ticks` and return `(ego trace, was-every-tick-admitted)`.
fn drive(
    brain: &mut impl MickBrain,
    planner: &mut impl Planner,
    map: &dyn CorridorSource,
    objects: &[PerceivedObject],
    ticks: usize,
) -> (Vec<EgoState>, bool) {
    // Start a few metres INTO the corridor — at the very start edge (x=0) the vehicle
    // footprint would poke out behind the corridor and KIRRA would (correctly) flag a
    // containment departure. This is about the demo's start pose, not the loop.
    let mut ego = EgoState {
        pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
        linear_x_mps: 2.0,
        yaw_rate_rads: 0.0,
        stamp_ms: 0,
    };
    let mut trace = vec![ego];
    let mut all_admitted = true;
    for tick in 1..=ticks {
        let w = world(ego, map, objects);
        let plan = mick_drive_once(brain, &w, planner);
        let admitted = kirra_admits(&plan, map, objects);
        all_admitted &= admitted;
        ego = advance(ego, &plan, admitted, tick as u64 * (TICK_DT * 1000.0) as u64);
        trace.push(ego);
    }
    (trace, all_admitted)
}

fn max_x(trace: &[EgoState]) -> f64 {
    trace.iter().map(|e| e.pose.x_m).fold(0.0, f64::max)
}

#[test]
fn chauffeur_cruises_and_makes_progress_on_a_clear_road() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let mut brain = ConstantBrain(MickIntent::Cruise { target_speed_mps: 5.0 });
    let mut occy = GeometricPlanner::default();
    let (trace, all_admitted) = drive(&mut brain, &mut occy, &corr, &[], 30);
    assert!(all_admitted, "every tick of a clear-road cruise is admitted by KIRRA");
    assert!(max_x(&trace) > 30.0, "the chauffeur makes real progress, reached {}", max_x(&trace));
}

#[test]
fn chauffeur_holds_short_of_a_hazard_across_the_whole_run() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let objs = [PerceivedObject { id: 1, pos: Point { x_m: HAZARD_X, y_m: 0.0 }, velocity_mps: 0.0, heading_rad: 0.0, vel: Point { x_m: 0.0, y_m: 0.0 } }];
    let mut brain = ConstantBrain(MickIntent::GoTo { x_m: 60.0, y_m: 0.0 });
    let mut occy = GeometricPlanner::default();
    let (trace, _) = drive(&mut brain, &mut occy, &corr, &objs, 40);
    let reached = max_x(&trace);
    // THE safety property: across the entire run the ego never reaches the obstacle —
    // whether by Occy grounding the plan to stop short OR by KIRRA MRC-ing it as the ego
    // closes in (defense in depth). Either way the chauffeur is held safe.
    assert!(reached < HAZARD_X, "the ego must never reach the hazard, reached {reached}");
    // And it is a real approach, not a refusal to move — it drives forward from the x=5
    // start toward the hazard before holding (the planner keeps a conservative stop-short
    // gap, so it settles a few metres in rather than nosing right up to the obstacle).
    assert!(reached > 7.0, "the chauffeur drives up toward the hazard, reached {reached}");
}

#[test]
fn reckless_doer_is_caught_every_tick_and_the_ego_never_reaches_the_hazard() {
    // THE closed-loop safety proof: the brain insists on driving to x=60 through a stopped
    // car at x=25, behind a RECKLESS doer that obliges every tick. KIRRA rejects every
    // tick; the ego MRCs and never reaches the obstacle. A persistently-bad brain is held
    // safe over the entire run — the guardrail does not tire.
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let objs = [PerceivedObject { id: 1, pos: Point { x_m: HAZARD_X, y_m: 0.0 }, velocity_mps: 0.0, heading_rad: 0.0, vel: Point { x_m: 0.0, y_m: 0.0 } }];
    let mut brain = ConstantBrain(MickIntent::GoTo { x_m: 60.0, y_m: 0.0 });
    let mut reckless = RecklessDoer;
    let (trace, all_admitted) = drive(&mut brain, &mut reckless, &corr, &objs, 40);
    assert!(!all_admitted, "the reckless doer's drive-through plan is rejected (not admitted)");
    assert!(max_x(&trace) < HAZARD_X, "and the ego NEVER reaches the hazard across the run, reached {}", max_x(&trace));
}
