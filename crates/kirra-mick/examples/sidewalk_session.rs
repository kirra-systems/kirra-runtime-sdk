//! **A deterministic sidewalk-courier drive exercising the ADR-0027 behaviors end to end.**
//! Mick authors a sidewalk intent each tick, Occy grounds it, KIRRA (the COURIER profile)
//! bounds it, and only an admitted plan carries the ego forward — so the new `Yield` and
//! `CrossWhenClear` primitives are shown COMPOSING in a continuous drive, not just in unit tests.
//!
//! Two scenarios:
//!   1. **Yield** — the courier creeps down a sidewalk toward a goal; a pedestrian steps across
//!      its path. The courier slows, stops a standoff short, HOLDS while the pedestrian is in the
//!      way, then resumes once clear. (`MickIntent::Yield`)
//!   2. **CrossWhenClear** — the courier reaches a crosswalk; a car crosses its line. It waits at
//!      the curb while the car is closing, then steps off once the gap is clear. (`MickIntent::CrossWhenClear`)
//!
//! No GPU, no ROS — pure aarch64-native compute, the same path that runs on the Orin.
//!
//! Run: `cargo run -p kirra-mick --example sidewalk_session`

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, GeometricPlannerConfig, Goal,
    MickIntent, PlanInput, Pose, ProposalKind, TrajectoryPoint,
};
use kirra_ros2_adapter::corridor::{MockCorridorSource, Point};
use kirra_ros2_adapter::state::{PerceivedObject, TrajectoryVerdict};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

const DT: f64 = 0.2; // 5 Hz
const CREEP_CRUISE_MPS: f64 = 1.5; // sidewalk creep
const STOP_EPS: f64 = 0.05;

/// The committed speed: the validated trajectory's planned velocity ~0.4 s in (what the fast loop
/// would track), so a capped/creeping plan carries the right speed and a stop carries zero.
fn committed_speed(traj: &[TrajectoryPoint]) -> f64 {
    traj.iter()
        .find(|p| p.time_from_start_s >= 0.4)
        .or_else(|| traj.last())
        .map_or(0.0, |p| p.velocity_mps)
}

fn obj(id: u64, x: f64, y: f64, vx: f64, vy: f64) -> PerceivedObject {
    PerceivedObject {
        id,
        pos: Point { x_m: x, y_m: y },
        velocity_mps: vx.hypot(vy),
        heading_rad: vy.atan2(vx),
        vel: Point { x_m: vx, y_m: vy },
    }
}

/// Run one temporal scenario: each tick build the world, ground `intent_at(tick, goal)`, let KIRRA
/// (courier profile) judge it, commit the admitted speed onto the ego (along +X), advance the
/// agents. Returns `(moved_ticks, held_ticks, reached, all_admitted)`.
fn run_scenario(
    label: &str,
    goal_x: f64,
    ego_start_x: f64,
    ticks: usize,
    intent: MickIntent,
    agents_at: impl Fn(usize) -> Vec<PerceivedObject>,
) {
    let corridor = MockCorridorSource::straight_5m_half_width(100.0);
    let vcfg = VehicleConfig::courier(); // the checker judges a small robot, not a car
    // The DOER's robot-scale planner preset (ADR-0027 / step B): stop ~1 m short of a person,
    // route around with the courier clearance, small footprint — overridden cruise to creep.
    let mut occy = GeometricPlanner::new(GeometricPlannerConfig { cruise_speed_mps: CREEP_CRUISE_MPS, ..GeometricPlannerConfig::courier() });

    let mut ego_x = ego_start_x;
    let mut speed = 0.0;
    let (mut moved, mut held, mut divergences) = (0usize, 0usize, 0usize);
    let mut reached = false;

    println!("== {label} ==");
    println!("  t(s)  ego_x  speed  occy        kirra        agents");
    for tick in 0..ticks {
        let now = (tick as f64 * DT * 1000.0) as u64;
        let agents = agents_at(tick);

        let world = PlanInput {
            ego: EgoState { pose: Pose { x_m: ego_x, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: speed, yaw_rate_rads: 0.0, stamp_ms: now },
            goal: Goal { target: Pose { x_m: goal_x, y_m: 0.0, heading_rad: 0.0 } },
            map: &corridor,
            objects: &agents,
            controls: &[], lane_boundaries: &[], motion: &[], predicted_paths: &[],
            cedes_to_ego_ids: &[], lane_change_to_m: None, no_overtake_ids: &[], drivable: None,
            posture: FleetPosture::Nominal, target_speed_mps: None,
            request_overtake: false, request_pull_over: false, lane_graph: None, signal_states: &[],
        };
        let plan = plan_for_intent(&mut occy, &intent, &world);
        let verdict = validate_trajectory_slow(&plan.trajectory, &corridor, &agents, &vcfg, None, FleetPosture::Nominal);

        let admitted = matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp) && plan.kind == ProposalKind::Motion;
        let cmd_v = if admitted { committed_speed(&plan.trajectory) } else { 0.0 };
        // Occy proposed motion but KIRRA refused it → a doer/checker DIVERGENCE (the courier still
        // holds, fail-safe; this is the tuning signal, not a fault — KIRRA backstops the doer).
        if plan.kind == ProposalKind::Motion && !admitted { divergences += 1; }
        if cmd_v > STOP_EPS { moved += 1; } else { held += 1; }

        speed = cmd_v;
        ego_x += cmd_v * DT;

        // Compact trace: print transitions + a few samples.
        if tick % 4 == 0 || (cmd_v <= STOP_EPS) != (speed <= STOP_EPS) {
            let near = agents.iter().min_by(|a, b| a.pos.x_m.total_cmp(&b.pos.x_m))
                .map(|a| format!("nearest@({:.1},{:.1})", a.pos.x_m, a.pos.y_m)).unwrap_or_else(|| "—".into());
            println!("  {:>4.1} {:>6.2} {:>6.2}  {:<11} {:<12} {near}",
                now as f64 / 1000.0, ego_x, cmd_v, format!("{:?}", plan.kind), format!("{verdict:?}"));
        }
        if ego_x >= goal_x - 0.6 {
            reached = true;
            println!("  reached the goal at t={:.1}s", now as f64 / 1000.0);
            break;
        }
    }

    println!("  scorecard: moved {moved} ticks, held {held} ticks (gave way / waited), reached_goal={reached}, \
              doer/checker divergences={divergences} (KIRRA backstopped; every committed pose was admitted)");
    println!();
}

fn main() {
    println!("Sidewalk courier — Mick(intent) → Occy(doer) → KIRRA(courier checker), ADR-0027\n");

    // 1) YIELD: a pedestrian STANDS in the courier's path at x=9 (in the personal-space band) for
    //    ~6 s, then steps aside. The courier creeps in under the yield cap, stops ~1 m short, HOLDS
    //    while the person is there, then resumes once they clear and reaches the goal.
    run_scenario(
        "Yield to a pedestrian in the path",
        16.0, 2.0, 110,
        MickIntent::Yield { x_m: 16.0, y_m: 0.0 },
        |tick| {
            // Stationary in the path until t=6 s, then steps aside (+Y) out of the band.
            let y = if tick < 30 { 0.0 } else { 0.30 * (tick as f64 - 30.0) * DT * 5.0 };
            vec![obj(1, 9.0, y, 0.0, 0.0)]
        },
    );

    // 2) CROSS-WHEN-CLEAR: the courier is at the curb (x=2), crossing an ~8 m road to x=10. A car
    //    crosses its line at x=6, reaching y=0 at ~t=10 then receding. The courier waits, then crosses.
    run_scenario(
        "Cross a crosswalk when clear",
        10.0, 2.0, 110,
        MickIntent::CrossWhenClear { x_m: 10.0, y_m: 0.0 },
        |tick| vec![obj(2, 6.0, -12.0 + 6.0 * (tick as f64 * DT), 0.0, 6.0)], // car crossing +Y at 6 m/s
    );

    println!("  Mick proposes a sidewalk intent; Occy grounds it; KIRRA (courier profile) bounds it.");
    println!("  Yield: the courier gives way to a pedestrian, then resumes. CrossWhenClear: it waits");
    println!("  at the curb for the car, then steps off. Every committed pose cleared the checker.");
}
