//! **Watch real Gemma drive — bounded by KIRRA.** Runs the closed loop with a LIVE
//! `LlmBrain<OllamaClient>`: each tick a local Gemma proposes a typed intent, Occy grounds
//! it, KIRRA judges it, and the ego advances (admitted → conform; rejected → MRC/hold).
//! The per-tick trace prints what the model chose and what the governor did with it.
//!
//! Run it:
//!   ollama pull gemma3:4b           # one-time
//!   cargo run -p kirra-mick --example mick_chauffeur
//!
//! No Ollama running? The brain fails closed every tick — you'll see HOLD throughout,
//! which is exactly the safe behavior. The model can never make the car unsafe: its intent
//! is grounded by Occy and bounded by KIRRA; this binary only shows the loop.

use kirra_core::FleetPosture;
use kirra_mick::OllamaClient;
use kirra_planner::{
    mick_drive_once, EgoState, GeometricPlanner, Goal, LlmBrain, PlanInput, PlanOutput, Pose,
};
use kirra_ros2_adapter::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_ros2_adapter::state::{PerceivedObject, TrajectoryVerdict};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

const TICK_DT: f64 = 0.5;
const TICKS: usize = 12;

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

fn verdict(plan: &PlanOutput, corr: &dyn CorridorSource, objs: &[PerceivedObject]) -> TrajectoryVerdict {
    validate_trajectory_slow(&plan.trajectory, corr, objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal)
}

fn advance(ego: EgoState, plan: &PlanOutput, admitted: bool, t_ms: u64) -> EgoState {
    if !admitted {
        return EgoState {
            pose: ego.pose,
            linear_x_mps: (ego.linear_x_mps - 3.0 * TICK_DT).max(0.0),
            yaw_rate_rads: 0.0,
            stamp_ms: t_ms,
        };
    }
    let tp = plan.trajectory.iter().find(|p| p.time_from_start_s >= TICK_DT).or_else(|| plan.trajectory.last());
    match tp {
        Some(p) => EgoState { pose: p.pose, linear_x_mps: p.velocity_mps, yaw_rate_rads: 0.0, stamp_ms: t_ms },
        None => ego,
    }
}

fn main() {
    let client = OllamaClient::new();
    println!("Mick chauffeur — model = {} @ {}", client.model(), std::env::var("KIRRA_OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".into()));
    let mut mick = LlmBrain::new(client);
    let mut occy = GeometricPlanner::default();

    // A straight road with a stopped car at x=30 — we should see Gemma drive up and the
    // system hold short / KIRRA bound it before the obstacle.
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let objs = [PerceivedObject { id: 1, pos: Point { x_m: 30.0, y_m: 0.0 }, velocity_mps: 0.0, heading_rad: 0.0, vel: Point { x_m: 0.0, y_m: 0.0 } }];

    let mut ego = EgoState { pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 2.0, yaw_rate_rads: 0.0, stamp_ms: 0 };
    println!("  tick     ego.x       v  kirra-verdict");
    for tick in 1..=TICKS {
        let w = world(ego, &corr, &objs);
        // mick_drive_once asks Gemma; a model error fails closed to a safe stop.
        let plan = mick_drive_once(&mut mick, &w, &mut occy);
        let v = verdict(&plan, &corr, &objs);
        let admitted = matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp);
        println!("{tick:>4}  {:>8.2}  {:>6.2}  {v:?}", ego.pose.x_m, ego.linear_x_mps);
        ego = advance(ego, &plan, admitted, tick as u64 * 500);
    }
    println!("final ego.x = {:.2} (obstacle at x=30 — the chauffeur never reaches it)", ego.pose.x_m);
}
