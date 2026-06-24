//! Emits the console fixture for the "KIRRA bounds a black-box doer" demo as JSON.
//!
//! This runs the SAME pipeline as `tests/adversarial_doer_bounded_by_kirra.rs`
//! (Mick intent → doer → KIRRA verdict) and serializes the real trajectories +
//! verdicts to stdout, so the web console renders measured data, not hand-drawn
//! mock-ups. Regenerate with:
//!
//!   cargo run -p kirra-planner --example doer_bound_fixture > console/lib/doer-bound.json
//!
//! The wiring (the `RecklessDoer`, the world, the horizon cap) mirrors the test;
//! see that file for the rationale behind each scenario constant.

use kirra_planner::{
    plan_for_intent, EgoState, GeometricPlanner, Goal, MickIntent, PlanInput, PlanOutput, Planner,
    Pose, ProposalKind, TrajectoryPoint,
};
use kirra_ros2_adapter::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_ros2_adapter::state::{PerceivedObject, TrajectoryVerdict};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};
use kirra_core::FleetPosture;

const MAX_TRAJECTORY_HORIZON: usize = 50;

struct RecklessDoer;
impl Planner for RecklessDoer {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        let ego = input.ego.pose;
        let goal = input.goal.target;
        let heading = (goal.y_m - ego.y_m).atan2(goal.x_m - ego.x_m);
        let dt = 0.1;
        let accel = 1.2;
        let v_cruise = 8.0;
        let (cos_h, sin_h) = (heading.cos(), heading.sin());
        let mut v = input.ego.linear_x_mps.max(1.0);
        let mut s = 0.0;
        let trajectory = (0..MAX_TRAJECTORY_HORIZON)
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

fn world<'a>(map: &'a dyn CorridorSource, objects: &'a [PerceivedObject]) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState { pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 2.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 } },
        map, objects,
        controls: &[], lane_boundaries: &[], motion: &[], predicted_paths: &[],
        cedes_to_ego_ids: &[], lane_change_to_m: None, no_overtake_ids: &[], drivable: None,
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
    }
}

fn verdict(out: &PlanOutput, corr: &dyn CorridorSource, objs: &[PerceivedObject]) -> TrajectoryVerdict {
    validate_trajectory_slow(&out.trajectory, corr, objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal)
}

/// `(verdict_string, admitted_bool)` in the console's vocabulary.
fn verdict_json(v: TrajectoryVerdict) -> (&'static str, bool) {
    match v {
        TrajectoryVerdict::Accept => ("Accept", true),
        TrajectoryVerdict::Clamp => ("Clamp", true),
        TrajectoryVerdict::MRCFallback => ("MRCFallback", false),
        TrajectoryVerdict::Pending => ("Pending", false),
    }
}

/// Serialize a trajectory as a compact `[[x,y],...]` polyline (every 2nd point).
fn poly(out: &PlanOutput) -> String {
    let pts: Vec<String> = out
        .trajectory
        .iter()
        .step_by(2)
        .map(|p| format!("[{:.2},{:.2}]", p.pose.x_m, p.pose.y_m))
        .collect();
    format!("[{}]", pts.join(","))
}

fn reach(out: &PlanOutput) -> f64 {
    out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max)
}

fn doer_json(name: &str, out: &PlanOutput, corr: &dyn CorridorSource, objs: &[PerceivedObject]) -> String {
    let (v, ok) = verdict_json(verdict(out, corr, objs));
    format!(
        r#"{{"doer":"{name}","reach_m":{:.2},"verdict":"{v}","admitted":{ok},"path":{}}}"#,
        reach(out),
        poly(out)
    )
}

fn main() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let intent = MickIntent::GoTo { x_m: 40.0, y_m: 0.0 };

    // Blocked world: stopped car at x=25.
    let objs = [PerceivedObject { id: 1, pos: Point { x_m: 25.0, y_m: 0.0 }, velocity_mps: 0.0, heading_rad: 0.0, vel: Point { x_m: 0.0, y_m: 0.0 } }];
    let w = world(&corr, &objs);

    let occy = doer_json("occy", &plan_for_intent(&mut GeometricPlanner::default(), &intent, &w), &corr, &objs);
    let reckless = doer_json("reckless", &plan_for_intent(&mut RecklessDoer, &intent, &w), &corr, &objs);
    let (fb_v, fb_ok) = verdict_json(verdict(&PlanOutput::safe_stop(w.ego.pose), &corr, &objs));

    // Clear world: same reckless doer, no obstacle.
    let cw = world(&corr, &[]);
    let reckless_clear = doer_json("reckless", &plan_for_intent(&mut RecklessDoer, &intent, &cw), &corr, &[]);

    println!(
        r#"{{
  "intent": "GoTo(40, 0)",
  "egoX": 5.0,
  "goalX": 40.0,
  "corridorHalfWidth": 5.0,
  "blocked": {{
    "obstacleX": 25.0,
    "doers": [{occy}, {reckless}],
    "fallback": {{"verdict":"{fb_v}","admitted":{fb_ok}}}
  }},
  "clear": {{
    "doers": [{reckless_clear}]
  }}
}}"#
    );
}
