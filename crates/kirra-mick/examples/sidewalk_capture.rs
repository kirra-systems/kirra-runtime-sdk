//! **Capture the courier's slow-loop decisions into the learning loop (ADR-0028 / E).** Runs a
//! sidewalk-courier drive (Mick intent → Occy plan → KIRRA verdict) and writes, per tick, a
//! `kirra_capture_schema::CaptureRecord` (`SlowLoopTrajectory` source) plus the matching
//! `BusMessage` — exactly what `kirra-collector` joins into a Parquet supervised dataset. Where
//! the PR-#560 wiring captured the FAST-loop command gateway, this captures the SLOW-loop
//! trajectory verdict (the courier's actual proposal-vs-checker signal).
//!
//!   cargo run -p kirra-mick --example sidewalk_capture            # writes sidewalk.capture.jsonl + sidewalk.bag.json
//!   cargo run -p kirra-collector -- --capture sidewalk.capture.jsonl \
//!       --bag-json sidewalk.bag.json --out dataset/ --window-ms 100
//!
//! The collector depends on `kirra-capture-schema` only (never the verifier), so it is
//! mechanically incapable of reaching the verdict path. Tune the DOER from this data — never the
//! checker's envelope.

use std::io::Write;

use kirra_capture_schema::{CaptureOutcome, CaptureRecord, CaptureSource, PoseSnapshot, TrajectoryCaptureExt};
use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, GeometricPlannerConfig, Goal,
    MickIntent, PlanInput, Pose, ProposalKind, TrajectoryPoint,
};
use kirra_trajectory::corridor::{MockCorridorSource, Point};
use kirra_trajectory::state::{PerceivedObject, TrajectoryVerdict};
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

const DT_MS: u64 = 200; // 5 Hz slow loop
const DT_S: f64 = 0.2;
const CREEP_CRUISE_MPS: f64 = 1.5;
const STOP_EPS: f64 = 0.05;
const DOER_VERSION: &str = "occy-courier-v0";

fn committed_speed(traj: &[TrajectoryPoint]) -> f64 {
    traj.iter().find(|p| p.time_from_start_s >= 0.4).or_else(|| traj.last()).map_or(0.0, |p| p.velocity_mps)
}

fn obj(id: u64, x: f64, y: f64, vx: f64, vy: f64) -> PerceivedObject {
    PerceivedObject { id, pos: Point { x_m: x, y_m: y }, velocity_mps: vx.hypot(vy), heading_rad: vy.atan2(vx), vel: Point { x_m: vx, y_m: vy } }
}

fn pose_snap(p: &Pose) -> PoseSnapshot { PoseSnapshot { x_m: p.x_m, y_m: p.y_m, heading_rad: p.heading_rad } }

/// Run one courier phase, appending a SlowLoopTrajectory CaptureRecord + BusMessage per tick.
#[allow(clippy::too_many_arguments)]
fn run_phase(
    records: &mut Vec<CaptureRecord>,
    bus: &mut Vec<serde_json::Value>,
    seq: &mut u64,
    clock_ms: &mut u64,
    goal_x: f64,
    ego_start_x: f64,
    ticks: usize,
    intent: MickIntent,
    agents_at: impl Fn(usize) -> Vec<PerceivedObject>,
) {
    let corridor = MockCorridorSource::straight_5m_half_width(100.0);
    let vcfg = VehicleConfig::courier();
    let mut occy = GeometricPlanner::new(GeometricPlannerConfig { cruise_speed_mps: CREEP_CRUISE_MPS, ..GeometricPlannerConfig::courier() });
    let mut ego_x = ego_start_x;
    let mut speed = 0.0;

    for tick in 0..ticks {
        let agents = agents_at(tick);
        let world = PlanInput {
            ego: EgoState { pose: Pose { x_m: ego_x, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: speed, yaw_rate_rads: 0.0, stamp_ms: *clock_ms },
            goal: Goal { target: Pose { x_m: goal_x, y_m: 0.0, heading_rad: 0.0 } },
            map: &corridor, objects: &agents, controls: &[], lane_boundaries: &[], motion: &[], predicted_paths: &[],
            cedes_to_ego_ids: &[], lane_change_to_m: None, no_overtake_ids: &[], drivable: None,
            posture: FleetPosture::Nominal, target_speed_mps: None,
            request_overtake: false, request_pull_over: false, lane_graph: None, signal_states: &[],
        };
        let plan = plan_for_intent(&mut occy, &intent, &world);
        let verdict = validate_trajectory_slow(&plan.trajectory, &corridor, &agents, &vcfg, None, FleetPosture::Nominal);

        // Map the slow-loop verdict onto the capture outcome.
        let (outcome, deny_code, mrc) = match verdict {
            TrajectoryVerdict::Accept => (CaptureOutcome::Allow, None, false),
            TrajectoryVerdict::Clamp => (CaptureOutcome::ClampLinear, None, false),
            _ => (CaptureOutcome::Deny, Some("TRAJECTORY_MRC_FALLBACK".to_string()), true),
        };
        let traj = &plan.trajectory;
        let target = traj.iter().map(|p| p.velocity_mps).fold(0.0_f64, f64::max);
        records.push(CaptureRecord {
            decision_seq: *seq,
            t_mono_ns: u128::from(*clock_ms) * 1_000_000,
            t_wall_ms: *clock_ms,
            source: CaptureSource::SlowLoopTrajectory,
            proposed: None,
            traj: Some(TrajectoryCaptureExt {
                asset_id: "courier".to_string(),
                trajectory_id: *seq,
                objects_ms: *clock_ms,
                point_count: traj.len(),
                object_count: agents.len(),
                first_pose: traj.first().map(|p| pose_snap(&p.pose)),
                last_pose: traj.last().map(|p| pose_snap(&p.pose)),
                target_speed_mps: (!traj.is_empty()).then_some(target),
            }),
            outcome,
            deny_code,
            safe_value: None,
            mrc,
            posture: "NOMINAL".to_string(),
            derate_enabled: false,
        });
        bus.push(serde_json::json!({
            "t_wall_ms": *clock_ms, "doer_version": DOER_VERSION, "asset_id": "courier",
            "trajectory_id": *seq, "objects_ms": *clock_ms, "bulk_ref": format!("mem://sidewalk#{seq}"),
        }));

        // Advance the ego by the admitted speed; step the clock + sequence.
        let admitted = matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp) && plan.kind == ProposalKind::Motion;
        speed = if admitted { committed_speed(traj) } else { 0.0 };
        if speed <= STOP_EPS { speed = 0.0; }
        ego_x += speed * DT_S;
        *seq += 1;
        *clock_ms += DT_MS;
    }
}

fn main() {
    let mut records: Vec<CaptureRecord> = Vec::new();
    let mut bus: Vec<serde_json::Value> = Vec::new();
    let mut seq = 0u64;
    let mut clock_ms = 0u64;

    // Phase 1 — Yield: a pedestrian stands in the path, then steps aside.
    run_phase(&mut records, &mut bus, &mut seq, &mut clock_ms, 16.0, 2.0, 60,
        MickIntent::Yield { x_m: 16.0, y_m: 0.0 },
        |tick| { let y = if tick < 30 { 0.0 } else { 0.30 * (tick as f64 - 30.0) * DT_S * 5.0 }; vec![obj(1, 9.0, y, 0.0, 0.0)] });

    // Phase 2 — CrossWhenClear: a car crosses the courier's line, then a gap.
    run_phase(&mut records, &mut bus, &mut seq, &mut clock_ms, 10.0, 2.0, 60,
        MickIntent::CrossWhenClear { x_m: 10.0, y_m: 0.0 },
        |tick| vec![obj(2, 6.0, -12.0 + 6.0 * (tick as f64 * DT_S), 0.0, 6.0)]);

    // Write the capture JSONL + the matching bus recording.
    let cap_path = "sidewalk.capture.jsonl";
    let bag_path = "sidewalk.bag.json";
    let mut cap = std::fs::File::create(cap_path).expect("create capture jsonl");
    for r in &records {
        writeln!(cap, "{}", serde_json::to_string(r).unwrap()).unwrap();
    }
    std::fs::write(bag_path, serde_json::to_string(&bus).unwrap()).expect("write bag json");

    let (allow, clamp, deny) = records.iter().fold((0, 0, 0), |(a, c, d), r| match r.outcome {
        CaptureOutcome::Allow => (a + 1, c, d),
        CaptureOutcome::ClampLinear | CaptureOutcome::ClampSteering => (a, c + 1, d),
        CaptureOutcome::Deny => (a, c, d + 1),
    });
    println!("=== sidewalk courier capture ({} slow-loop trajectory records) ===", records.len());
    println!("  outcomes:  ALLOW {allow}   CLAMP {clamp}   DENY {deny}");
    println!("  capture:   {cap_path}");
    println!("  bus:       {bag_path}");
    println!("  → build the dataset:");
    println!("    cargo run -p kirra-collector -- --capture {cap_path} --bag-json {bag_path} --out dataset/ --window-ms 100");
    println!("  Every record is the courier's (intent→trajectory→KIRRA verdict) decision — the");
    println!("  supervised signal for tuning the DOER. The collector links kirra-capture-schema only.");
}
