//! **A deterministic closed-loop drive session with Occy as the DOER and KIRRA as the CHECKER.**
//!
//! Where `governor_drive_session.py` uses a toy controller and the HTTP governor, this runs the
//! REAL planner: each slow tick Occy grounds a Mick intent into a trajectory (`plan_for_intent`),
//! KIRRA validates it (`validate_trajectory_slow`), and only an ADMITTED trajectory is committed to
//! the fast-loop tracker that carries the ego forward — exactly the dual-rate loop the AV stack
//! uses, with no LLM and no GPU. Every decision is recorded (`MickDecisionRecord`) and scored
//! (`MickEvalSummary`), and a per-tick divergence line (proposed peak speed → checker verdict) is
//! printed, so Occy's performance is *measured* against the checker over a continuous drive.
//!
//! The ego drives a route that goes straight, then through a curve, to an exit — so the verdict
//! mix reflects real geometry. The clear straight admits; a fraction of Occy's continuous
//! curve-following proposals are REFUSED (→ MRC fallback) — a closed-loop divergence the discrete
//! scenario tests don't surface, and exactly the tuning hotspot this harness exists to find. The
//! verdict split is the tuning signal. Tune the DOER from this, never the checker's envelope.
//!
//! Run: `cargo run -p kirra-mick --example drive_session`

use kirra_planner::{
    plan_for_intent, EgoState, FastLoopTracker, FleetPosture, GeometricPlanner,
    GeometricPlannerConfig, Goal, Lane, LaneEdge, LaneGraph, LineType, MickDecisionRecord,
    MickEvalSummary, MickIntent, PlanInput, Pose, ProposalKind,
};
use kirra_trajectory::corridor::{CorridorSource, Point};
use kirra_trajectory::state::TrajectoryVerdict;
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

const FAST_DT_MS: u64 = 100; // 10 Hz fast loop
const FAST_DT_S: f64 = 0.1;
const REPLAN_MS: u64 = 300; // slow-loop (Occy) replan cadence
const TICKS: usize = 120; // 12 s drive
const MRC_DECEL: f64 = 3.0;

const R: f64 = 22.0;

/// A route graph that runs straight (0,0)→(30,0), curves LEFT through an arc, then exits north —
/// the route Occy grounds (`RouteTo`) and KIRRA bounds.
fn route_graph() -> LaneGraph {
    let arc: Vec<Point> = (0..=12)
        .map(|i| {
            let t = -std::f64::consts::FRAC_PI_2 + std::f64::consts::FRAC_PI_2 * (i as f64 / 12.0);
            Point {
                x_m: 30.0 + R * t.cos(),
                y_m: R + R * t.sin(),
            }
        })
        .collect();
    LaneGraph::new()
        .with_lane(
            Lane::straight(1, 0.0, 0.0, 30.0, 3.0, LineType::Solid, LineType::Solid)
                .with_edge(LaneEdge::Successor { to: 2 }),
        )
        .with_lane(Lane {
            id: 2,
            centerline: arc,
            half_width_m: 3.0,
            left_line: LineType::Solid,
            right_line: LineType::Solid,
            heading_rad: std::f64::consts::FRAC_PI_2,
            edges: vec![LaneEdge::Successor { to: 3 }],
            control: None,
        })
        .with_lane(Lane {
            id: 3,
            centerline: vec![
                Point {
                    x_m: 30.0 + R,
                    y_m: R,
                },
                Point {
                    x_m: 30.0 + R,
                    y_m: R + 20.0,
                },
            ],
            half_width_m: 3.0,
            left_line: LineType::Solid,
            right_line: LineType::Solid,
            heading_rad: std::f64::consts::FRAC_PI_2,
            edges: Vec::new(),
            control: None,
        })
}

fn world<'a>(
    ego: EgoState,
    map: &'a dyn CorridorSource,
    g: &'a LaneGraph,
    goal: Pose,
) -> PlanInput<'a> {
    PlanInput {
        ego,
        goal: Goal { target: goal },
        map,
        objects: &[],
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

fn main() {
    let g = route_graph();
    let route = g.route(1, 3).expect("route 1→3");
    let corr = g.route_corridor(&route, 0.95, 0).expect("route corridor"); // KIRRA's validation corridor
                                                                           // Exit lane tops out at (30+R, R+20) = (52, 42); aim a couple of metres short of the top.
    let goal = Pose {
        x_m: 30.0 + R,
        y_m: R + 16.0,
        heading_rad: std::f64::consts::FRAC_PI_2,
    };
    let cfg = GeometricPlannerConfig {
        cruise_speed_mps: 10.0,
        ..Default::default()
    };
    let mut occy = GeometricPlanner::new(cfg);
    let vcfg = VehicleConfig::default_urban();

    let mut ego = EgoState {
        pose: Pose {
            x_m: 2.0,
            y_m: 0.0,
            heading_rad: 0.0,
        },
        linear_x_mps: 6.0,
        yaw_rate_rads: 0.0,
        stamp_ms: 0,
    };
    let mut tracker = FastLoopTracker::new();
    let mut last_replan: Option<u64> = None;
    let mut records: Vec<MickDecisionRecord> = Vec::new();

    println!("Occy(doer) → KIRRA(checker) closed-loop drive — straight, curve, exit");
    println!("   t(s)  ego.x  ego.y    v    proposed_vmax   verdict");
    for tick in 1..=TICKS {
        let now = tick as u64 * FAST_DT_MS;

        // SLOW loop: Occy proposes, KIRRA checks, admit→commit.
        if tracker.is_exhausted(now)
            || last_replan.is_none_or(|t| now.saturating_sub(t) >= REPLAN_MS)
        {
            let w = world(ego, &corr, &g, goal);
            let intent = MickIntent::RouteTo {
                x_m: goal.x_m,
                y_m: goal.y_m,
            };
            let plan = plan_for_intent(&mut occy, &intent, &w);
            let verdict = validate_trajectory_slow(
                &plan.trajectory,
                &corr,
                &[],
                &vcfg,
                None,
                FleetPosture::Nominal,
            );
            let vmax = plan
                .trajectory
                .iter()
                .map(|p| p.velocity_mps)
                .fold(0.0_f64, f64::max);
            records.push(MickDecisionRecord::new(
                tick as u64,
                now,
                &intent,
                &plan,
                verdict,
            ));
            println!(
                "  {:>5.1} {:>6.2} {:>6.2} {:>5.2}    {:>9.2}     {verdict:?}",
                now as f64 / 1000.0,
                ego.pose.x_m,
                ego.pose.y_m,
                ego.linear_x_mps,
                vmax
            );
            if matches!(
                verdict,
                TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
            ) && plan.kind == ProposalKind::Motion
            {
                tracker.promote(plan, now);
                last_replan = Some(now);
            }
        }

        // FAST loop: track the committed trajectory; MRC-decel if none.
        ego = match tracker.track(now) {
            Some(cmd) => EgoState {
                pose: cmd.pose,
                linear_x_mps: cmd.velocity_mps,
                yaw_rate_rads: 0.0,
                stamp_ms: now,
            },
            None => EgoState {
                pose: ego.pose,
                linear_x_mps: (ego.linear_x_mps - MRC_DECEL * FAST_DT_S).max(0.0),
                yaw_rate_rads: 0.0,
                stamp_ms: now,
            },
        };
        if (ego.pose.x_m - goal.x_m).hypot(ego.pose.y_m - goal.y_m) < 2.0 {
            println!("  reached the exit at t={:.1}s", now as f64 / 1000.0);
            break;
        }
    }

    let summary = MickEvalSummary::from_records(&records);
    println!("\n{summary}");
    println!(
        "  → Occy as the doer; the verdict mix is the tuning scorecard. Here the STRAIGHT admits"
    );
    println!(
        "    (clamp = a minor derate) while a fraction of the CURVE proposals are REFUSED — the"
    );
    println!("    closed-loop tuning HOTSPOT (continuous curve-following the discrete scenario tests miss).");
    println!(
        "    Every refused proposal fell back to MRC; KIRRA bounded every committed pose. Tune the"
    );
    println!("    DOER's curve speed/line from this, never the checker's envelope.");
}
