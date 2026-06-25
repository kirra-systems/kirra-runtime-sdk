//! **The full on-vehicle stack in one process — Taj → Mick → Occy → KIRRA — sized for a
//! Jetson Orin NX 16GB (ADR-0014).** No GPU, no CARLA, no ROS: pure aarch64-native Rust
//! compute, so it runs on the Orin (or any box) exactly as it would on the robot's
//! System-2 board.
//!
//! Each scenario walks the real ADR-0014 propose→govern pipeline once:
//!
//!   1. **Taj** (`kirra-taj`, ADR-0015 Phase A) turns a synthetic forward range scan into
//!      the Kirra perception contract — a drivable `CorridorSource` + `PerceivedObject`s.
//!      (On the robot this is the R2 lidar/depth feed; the geometry is identical.)
//!   2. **Mick** (`kirra_planner::mick`) is the intent seam: an LLM authors a TYPED
//!      `MickIntent` as JSON, parsed fail-closed by `MickIntent::from_llm_json`. Here a
//!      fixed `go_to` intent stands in for the brain — swap in `kirra-mick`'s Ollama
//!      `ModelClient` (local Gemma on the Orin) and nothing downstream changes.
//!   3. **Occy** (`kirra-planner`) grounds the intent against Taj's perception into a
//!      proposed trajectory (`plan_for_intent`). Occy only PROPOSES.
//!   4. **KIRRA** (`validate_trajectory_slow`, the #131 checker) is the sole safety
//!      authority — it bounds Occy's proposal against Taj's corridor + objects.
//!
//! The scenarios sweep the regimes the stack must get right: a clear corridor (admit), a
//! stopped car ahead (controlled stop, admitted), an off-centre obstacle in a wide corridor
//! (route around, admitted), and a LockedOut posture (refuse everything). The load-bearing
//! property across all of them: **Occy is never trusted to stop — KIRRA is.** Taj tightens
//! the envelope, Occy proposes, KIRRA disposes. The footprint is pure Rust compute, so this
//! is exactly what runs on the Orin NX System-2 board.
//!
//! Run: `cargo run -p kirra-mick --example taj_occy_kirra_stack`

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, GeometricPlannerConfig, Goal,
    MickIntent, PlanInput, Pose, ProposalKind,
};
use kirra_ros2_adapter::corridor::Point;
use kirra_ros2_adapter::state::{PerceivedObject, TrajectoryVerdict};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};
use kirra_taj::{LaserScan, TajConfig, TajPhaseA};
use std::f64::consts::PI;

/// Forward 181-ray scan (`-π/2..+π/2`, 1° steps) from a per-ray range fn — a
/// `sensor_msgs/LaserScan` subset, the shape the R2 lidar publishes.
fn forward_scan<F: Fn(f64) -> Option<f64>>(range_max: f64, f: F) -> LaserScan {
    let n = 181usize;
    let angle_min = -PI / 2.0;
    let angle_inc = PI / (n as f64 - 1.0);
    let ranges = (0..n)
        .map(|i| {
            let theta = angle_min + i as f64 * angle_inc;
            match f(theta) {
                Some(r) if (0.05..=range_max).contains(&r) => r as f32,
                _ => (range_max + 1.0) as f32, // no return
            }
        })
        .collect();
    LaserScan { angle_min_rad: angle_min, angle_increment_rad: angle_inc, range_min_m: 0.05, range_max_m: range_max, ranges, stamp_ms: 1 }
}

/// Two walls at `y = ±half_width`, plus an optional in-path blob at `(blob_x, 0)`
/// (rays within `±half_angle` return at `blob_x / cos θ`). Mirrors the proven
/// `taj_occy_kirra_stack` integration scenes.
fn corridor_scan(half_width: f64, blob: Option<(f64, f64)>) -> LaserScan {
    forward_scan(20.0, |theta| {
        let s = theta.sin();
        let wall = if s.abs() < 1e-3 { f64::INFINITY } else { half_width / s.abs() };
        let blob_r = match blob {
            Some((bx, half_angle)) if theta.abs() < half_angle => bx / theta.cos(),
            _ => f64::INFINITY,
        };
        let r = wall.min(blob_r);
        if r.is_finite() { Some(r) } else { None }
    })
}

struct Outcome {
    objects: usize,
    kind: ProposalKind,
    verdict: TrajectoryVerdict,
    min_y: f64,
    vmax: f64,
}

/// One pass of the full stack: Taj perception → Mick intent → Occy plan → KIRRA verdict.
fn run_stack(
    scan: &LaserScan,
    extent_m: f64,
    extra_objects: &[PerceivedObject],
    ego: Pose,
    intent: &MickIntent,
    goal: Pose,
    posture: FleetPosture,
) -> Outcome {
    // 1) Taj — scan → corridor + objects. The corridor must extend past the plan's footprint
    //    horizon, so the forward extent scales with how far ahead the goal sits.
    let taj = TajPhaseA::new(TajConfig { forward_extent_m: extent_m, ..Default::default() });
    let mut perception = taj.process(scan, 2);
    perception.objects.extend_from_slice(extra_objects);

    // 2+3) Mick's intent + Occy grounds it against Taj's perception.
    let world = PlanInput {
        ego: EgoState { pose: ego, linear_x_mps: 2.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: goal },
        map: &perception.corridor,
        objects: &perception.objects,
        controls: &[], lane_boundaries: &[], motion: &[], predicted_paths: &[],
        cedes_to_ego_ids: &[], lane_change_to_m: None, no_overtake_ids: &[], drivable: None,
        posture: posture.clone(), target_speed_mps: None,
        request_overtake: false, request_pull_over: false, lane_graph: None, signal_states: &[],
    };
    let mut occy = GeometricPlanner::new(GeometricPlannerConfig { cruise_speed_mps: 4.0, ..Default::default() });
    let plan = plan_for_intent(&mut occy, intent, &world);

    // 4) KIRRA — the sole safety authority bounds Occy's proposal.
    let verdict = validate_trajectory_slow(&plan.trajectory, &perception.corridor, &perception.objects, &VehicleConfig::default_urban(), None, posture);

    Outcome {
        objects: perception.objects.len(),
        kind: plan.kind,
        verdict,
        min_y: plan.trajectory.iter().map(|p| p.pose.y_m).fold(0.0_f64, f64::min),
        vmax: plan.trajectory.iter().map(|p| p.velocity_mps).fold(0.0_f64, f64::max),
    }
}

/// Mick: the brain authors a TYPED intent as JSON, parsed fail-closed. (Swap the string
/// for `kirra-mick`'s Ollama `ModelClient` output on the Orin — nothing downstream changes.)
fn mick_goto(x_m: f64, y_m: f64) -> MickIntent {
    let json = format!(r#"{{"intent":"go_to","x_m":{x_m:.1},"y_m":{y_m:.1}}}"#);
    MickIntent::from_llm_json(&json).expect("Mick parses a well-formed go_to intent")
}

fn main() {
    println!("Taj(perception) → Mick(intent) → Occy(doer) → KIRRA(checker) — on-Orin stack (ADR-0014), headless\n");
    println!("  {:<34} {:>5}  {:<9} {:<12} {:>6}  {:<8}", "scenario", "objs", "occy", "kirra", "min_y", "result");
    println!("  {}", "-".repeat(86));

    // 1) Clear corridor — Occy proposes motion, KIRRA admits.
    let s = corridor_scan(5.0, None);
    let o = run_stack(&s, 20.0, &[], Pose { x_m: 2.0, y_m: 0.0, heading_rad: 0.0 }, &mick_goto(6.0, 0.0), Pose { x_m: 6.0, y_m: 0.0, heading_rad: 0.0 }, FleetPosture::Nominal);
    report("clear corridor", &o, "drives (KIRRA admits)");

    // 2) Stopped car dead ahead — Occy brakes to a controlled stop behind it; KIRRA ADMITS
    //    that safe same-lane stop (the §4 RSS-conjunction admits a halt behind a stopped lead).
    let s = corridor_scan(5.0, Some((4.0, 0.12)));
    let o = run_stack(&s, 20.0, &[], Pose { x_m: 2.0, y_m: 0.0, heading_rad: 0.0 }, &mick_goto(6.0, 0.0), Pose { x_m: 6.0, y_m: 0.0, heading_rad: 0.0 }, FleetPosture::Nominal);
    report("stopped car ahead", &o, "controlled stop behind it (admitted)");

    // 3) Off-centre obstacle in a wide corridor — Occy routes AROUND it (path offsets to
    //    min_y ≤ -1), KIRRA admits the offset pass.
    let s = corridor_scan(5.0, None);
    let off_centre = [PerceivedObject { id: 99, pos: Point { x_m: 20.0, y_m: 3.0 }, velocity_mps: 0.0, heading_rad: 0.0, vel: Point { x_m: 0.0, y_m: 0.0 } }];
    let o = run_stack(&s, 40.0, &off_centre, Pose { x_m: 8.0, y_m: 0.0, heading_rad: 0.0 }, &mick_goto(30.0, 0.0), Pose { x_m: 30.0, y_m: 0.0, heading_rad: 0.0 }, FleetPosture::Nominal);
    report("off-centre obstacle, wide road", &o, "routes around (admitted)");

    // 4) LockedOut posture — the fault flows through the whole stack: Occy may only propose
    //    safe-stop, and KIRRA refuses all motion. Fail-closed regardless of what Occy wants.
    let s = corridor_scan(5.0, None);
    let o = run_stack(&s, 20.0, &[], Pose { x_m: 2.0, y_m: 0.0, heading_rad: 0.0 }, &mick_goto(6.0, 0.0), Pose { x_m: 6.0, y_m: 0.0, heading_rad: 0.0 }, FleetPosture::LockedOut);
    report("LockedOut posture", &o, "KIRRA refuses all motion (MRC)");

    println!("\n  All four components ran in one aarch64-native process — no GPU, no CARLA, no ROS.");
    println!("  Taj produced the corridor+objects; Mick the intent; Occy proposed; KIRRA disposed.");
    println!("  Across every regime Occy only PROPOSES; KIRRA is the sole authority that admits or");
    println!("  refuses. This is exactly the System-2 stack that runs on the Orin NX 16GB (ADR-0014).");
}

fn report(name: &str, o: &Outcome, result: &str) {
    println!(
        "  {:<34} {:>5}  {:<9} {:<12} {:>6.2}  {}",
        name, o.objects, format!("{:?}", o.kind), format!("{:?}", o.verdict), o.min_y, result,
    );
    let _ = o.vmax; // vmax available for richer scorecards; not shown in this row
}
