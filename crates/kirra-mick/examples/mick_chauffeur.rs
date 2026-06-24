//! **Watch real Gemma drive — bounded by KIRRA, at realistic dual rates.** Runs the
//! closed loop with a live `MickDriver<LlmBrain<OllamaClient>>`: the FAST loop ticks at
//! 10 Hz (grounding the current intent against the fresh world and conforming to the
//! KIRRA-accepted trajectory), while the SLOW System-2 path only re-asks Gemma for a new
//! *intent* every ~500 ms. The trace prints, each tick, the intent Gemma last chose and
//! what the governor did — so you can see the maneuver *persist* between the model's
//! (infrequent) decisions while the trajectory keeps tracking live perception.
//!
//! Run it:
//!   ollama pull gemma3:4b           # one-time
//!   cargo run -p kirra-mick --example mick_chauffeur
//!
//! No Ollama running? The driver fails closed — you'll see HOLD throughout, exactly the
//! safe default. The model can never make the car unsafe: its intent is grounded by Occy
//! and bounded by KIRRA; this binary only shows the loop.

use kirra_core::FleetPosture;
use kirra_mick::OllamaClient;
use kirra_planner::{
    EgoState, GeometricPlanner, Goal, LlmBrain, MickDecisionRecord, MickDriver, MickEvalLog,
    PlanInput, PlanOutput, Pose,
};
use kirra_ros2_adapter::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_ros2_adapter::state::{PerceivedObject, TrajectoryVerdict};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

const FAST_DT_S: f64 = 0.1; // 10 Hz fast loop
const FAST_DT_MS: u64 = 100;
const TICKS: usize = 24; // 2.4 s
const MRC_DECEL: f64 = 3.0;

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
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
        signal_states: &[],    }
}

fn verdict(plan: &PlanOutput, corr: &dyn CorridorSource, objs: &[PerceivedObject]) -> TrajectoryVerdict {
    validate_trajectory_slow(&plan.trajectory, corr, objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal)
}

/// The (pose, velocity) at time `t` along a plan — the fast-loop conformance target.
fn target_at(plan: &PlanOutput, t: f64) -> Option<(Pose, f64)> {
    plan.trajectory
        .iter()
        .find(|p| p.time_from_start_s >= t)
        .or_else(|| plan.trajectory.last())
        .map(|p| (p.pose, p.velocity_mps))
}

fn main() {
    let client = OllamaClient::new();
    let url = std::env::var("KIRRA_OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".into());
    println!("Mick chauffeur — model = {} @ {url}  (fast loop 10 Hz, Gemma ~2 Hz)", client.model());

    // Dual-rate driver: defaults = decide every 500 ms (System-2), hold if stale > 2 s.
    let mut driver = MickDriver::new(LlmBrain::new(client));
    let mut occy = GeometricPlanner::default();

    // Straight road, stopped car at x=30 — Gemma should drive up; KIRRA holds it ~4 m short.
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let objs = [PerceivedObject { id: 1, pos: Point { x_m: 30.0, y_m: 0.0 }, velocity_mps: 0.0, heading_rad: 0.0, vel: Point { x_m: 0.0, y_m: 0.0 } }];

    let mut ego = EgoState { pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 2.0, yaw_rate_rads: 0.0, stamp_ms: 0 };
    let mut accepted: Option<PlanOutput> = None;
    let mut slot_t = 0.0_f64;

    // Optional eval capture (default OFF): set KIRRA_MICK_EVAL_ENABLED=1 to log each
    // decision (intent → grounding → verdict) as JSONL for offline scoring of the brain.
    let mut eval = MickEvalLog::from_env();
    if eval.is_some() {
        println!("   (mick-eval capture ON → KIRRA_MICK_EVAL_PATH or kirra_mick_eval.jsonl)");
    }

    println!("   t(s)   ego.x      v   intent (System-2)        kirra-verdict");
    for tick in 1..=TICKS {
        let now_ms = tick as u64 * FAST_DT_MS;
        let w = world(ego, &corr, &objs);
        // drive_tick re-asks Gemma only when the System-2 interval has elapsed; otherwise it
        // grounds the cached intent. A model error / staleness fails closed to Hold.
        let plan = driver.drive_tick(&w, &mut occy, now_ms);
        let v = verdict(&plan, &corr, &objs);
        let admitted = matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp);

        // Eval capture (before `plan` is moved into the accepted slot): log the brain's
        // intent → Occy's grounding → KIRRA's verdict when there is a current intent.
        if let (Some(log), Some(intent)) = (eval.as_mut(), driver.current_intent()) {
            let _ = log.append(&MickDecisionRecord::new(tick as u64, now_ms, &intent, &plan, v));
        }

        // Fast/slow conformance: promote on admit, otherwise keep tracking the last slot.
        if admitted {
            accepted = Some(plan);
            slot_t = 0.0;
        }
        slot_t += FAST_DT_S;
        ego = match accepted.as_ref().and_then(|p| target_at(p, slot_t)) {
            Some((pose, vel)) => EgoState { pose, linear_x_mps: vel, yaw_rate_rads: 0.0, stamp_ms: now_ms },
            None => EgoState {
                pose: ego.pose, linear_x_mps: (ego.linear_x_mps - MRC_DECEL * FAST_DT_S).max(0.0),
                yaw_rate_rads: 0.0, stamp_ms: now_ms,
            },
        };

        let intent = driver.current_intent();
        println!("{:>6.1}  {:>6.2}  {:>5.2}   {:<22?}  {v:?}", now_ms as f64 / 1000.0, ego.pose.x_m, ego.linear_x_mps, intent);
    }
    println!("final ego.x = {:.2} (obstacle at x=30 — the chauffeur holds ~4 m short, never reaching it)", ego.pose.x_m);
}
