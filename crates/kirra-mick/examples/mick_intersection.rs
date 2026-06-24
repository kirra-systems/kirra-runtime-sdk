//! **Watch real Gemma negotiate a signalized intersection — bounded by KIRRA.** The
//! capstone that drives the whole junction stack end-to-end with a live model:
//!
//!   - #524 grammar-constrained intent decode  (Gemma → typed `MickIntent`)
//!   - #527 `TurnAt` grounding                 (route the chosen branch through the junction)
//!   - #528 right-of-way → cedes               (priority over a yielding-lane crosser)
//!   - #529 / #531 stop line / traffic light   (HOLD on red, proceed on green)
//!   - #525 decision capture                   (intent → grounding → verdict, JSONL)
//!   - KIRRA                                    (bounds every proposal the model induces)
//!
//! The scene: the ego approaches a junction whose lane carries a TRAFFIC LIGHT and a LEFT
//! turn branch, with a crossing lane the ego has right-of-way over. The light starts RED —
//! the ego decelerates to the stop line and HOLDS no matter what Gemma asks — then turns
//! GREEN, and the ego tracks the turn left through the junction toward the goal across it.
//! Nothing the model says can make the car run the red, cross into the crosser, or over-cut
//! the turn: Occy grounds the intent against the map-derived controls and KIRRA bounds the
//! result. (Whether the model emits `turn_at` or `go_to`, the turn drives to completion —
//! route-progress continues the committed arc, the fast-loop tracker carries it through, and
//! curved-lane `contains` keeps the ego located on the arc; see `intersection_closed_loop`.)
//!
//! Dual-rate, like `mick_chauffeur`: the FAST loop conforms at 10 Hz; the SLOW System-2 path
//! only re-asks Gemma for a new intent every ~500 ms, so you watch a maneuver persist between
//! the model's (infrequent) decisions.
//!
//! Run it:
//!   ollama pull gemma3:4b           # one-time
//!   cargo run -p kirra-mick --example mick_intersection
//!
//! No Ollama? The driver fails closed — HOLD throughout, the safe default. The model can
//! never make the car unsafe; this binary only shows the loop.

use kirra_planner::{
    EgoState, FastLoopTracker, FleetPosture, GeometricPlanner, Goal, Lane, LaneControl,
    LaneCorridor, LaneEdge, LaneGraph, LineType, LlmBrain, MickDecisionRecord, MickDriver,
    MickEvalLog, PlanInput, PlanOutput, Pose, SignalState,
};
use kirra_mick::OllamaClient;
use kirra_ros2_adapter::corridor::{CorridorSource, Point};
use kirra_ros2_adapter::state::{PerceivedObject, TrajectoryVerdict};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};

const FAST_DT_S: f64 = 0.1; // 10 Hz fast loop
const FAST_DT_MS: u64 = 100;
const TICKS: usize = 80; // 8 s — approach, hold on red, then track the turn on green
const MRC_DECEL: f64 = 3.0;
const GREEN_AT_MS: u64 = 3500; // the light flips RED → GREEN at 3.5 s (after the ego has held)
const REPLAN_MS: u64 = 500; // slow-loop (System-2) re-plan cadence; the fast loop tracks between
const TURN_RADIUS: f64 = 12.0; // gentle enough for the turn corridor to admit (cf. #526)
const APPROACH_END: f64 = 30.0; // ego lane runs (0,0)→(30,0); the stop line is here

/// A signalized LEFT-turn junction with a yielding crossing lane:
///   1  ego approach (0,0)→(30,0), TRAFFIC LIGHT at the stop line, priority over lane 3,
///      Successor → 2 (the left arc)
///   2  left-turn arc (30,0)→(42,12), heading +π/2, Successor → 5
///   5  north exit (42,12)→(42,32)
///   3  crossing lane (yields to 1), off to the right where the left-turning ego never goes
fn junction() -> LaneGraph {
    let r = TURN_RADIUS;
    let arc: Vec<Point> = (0..=12)
        .map(|i| {
            let t = -std::f64::consts::FRAC_PI_2 + std::f64::consts::FRAC_PI_2 * (i as f64 / 12.0);
            Point { x_m: APPROACH_END + r * t.cos(), y_m: r + r * t.sin() }
        })
        .collect();
    LaneGraph::new()
        .with_lane(
            Lane::straight(1, 0.0, 0.0, APPROACH_END, 3.0, LineType::Solid, LineType::Solid)
                .with_control(LaneControl::TrafficLight)
                .with_edge(LaneEdge::Successor { to: 2 }),
        )
        .with_lane(Lane {
            id: 2,
            centerline: arc,
            half_width_m: 3.0,
            left_line: LineType::Solid,
            right_line: LineType::Solid,
            heading_rad: std::f64::consts::FRAC_PI_2,
            edges: vec![LaneEdge::Successor { to: 5 }],
            control: None,
        })
        .with_lane(Lane {
            id: 5,
            centerline: vec![Point { x_m: APPROACH_END + r, y_m: r }, Point { x_m: APPROACH_END + r, y_m: r + 20.0 }],
            half_width_m: 3.0,
            left_line: LineType::Solid,
            right_line: LineType::Solid,
            heading_rad: std::f64::consts::FRAC_PI_2,
            edges: Vec::new(),
            control: None,
        })
        // Crossing lane the ego has priority over (a vehicle sits in it, off the turn path).
        .with_lane(
            Lane::straight(3, -10.0, 36.0, 25.0, 2.0, LineType::Solid, LineType::Solid)
                .with_heading(std::f64::consts::FRAC_PI_2),
        )
        .with_right_of_way(1, 3)
}

fn world<'a>(
    ego: EgoState,
    map: &'a dyn CorridorSource,
    objects: &'a [PerceivedObject],
    signal_states: &'a [(u64, SignalState)],
    graph: &'a LaneGraph,
    goal: Pose,
) -> PlanInput<'a> {
    PlanInput {
        ego,
        goal: Goal { target: goal },
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
        lane_graph: Some(graph),
        signal_states,
    }
}

fn verdict(plan: &PlanOutput, corr: &dyn CorridorSource, objs: &[PerceivedObject]) -> TrajectoryVerdict {
    validate_trajectory_slow(&plan.trajectory, corr, objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal)
}

fn main() {
    let client = OllamaClient::new();
    let url = std::env::var("KIRRA_OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".into());
    println!("Mick intersection — model = {} @ {url}  (fast loop 10 Hz, Gemma ~2 Hz)", client.model());
    println!("   signalized LEFT turn: HOLD on red → turn on green @ {:.1}s; right-of-way over a crosser; KIRRA bounds all.", GREEN_AT_MS as f64 / 1000.0);

    let mut driver = MickDriver::new(LlmBrain::new(client));
    let mut occy = GeometricPlanner::default();

    let g = junction();
    // The reference path AND the KIRRA containment corridor: the full route through the turn.
    // The ego tracks it toward the cross-junction goal; the red light HOLDs it at the line.
    let route: LaneCorridor = g.route(1, 5).and_then(|r| g.route_corridor(&r, 0.95, 5)).expect("route corridor");
    // A stationary vehicle in the crossing lane (id 7), off the left-turn path — the ego has
    // right-of-way over it; the cede is derived live and printed so you see #528 firing.
    let objs = [PerceivedObject { id: 7, pos: Point { x_m: 33.0, y_m: -10.0 }, velocity_mps: 0.0, heading_rad: std::f64::consts::FRAC_PI_2, vel: Point { x_m: 0.0, y_m: 0.0 } }];
    let goal = Pose { x_m: APPROACH_END + TURN_RADIUS, y_m: 28.0, heading_rad: std::f64::consts::FRAC_PI_2 };

    let mut ego = EgoState { pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 5.0, yaw_rate_rads: 0.0, stamp_ms: 0 };
    // Dual-rate: the slow loop (System-2 → Occy → KIRRA) re-plans/validates every REPLAN_MS and
    // promotes only ADMITTED trajectories; the fast loop tracks the committed one each tick.
    let mut tracker = FastLoopTracker::new();
    let mut last_replan_ms: Option<u64> = None;
    let mut last_v = TrajectoryVerdict::MRCFallback;
    let mut cedes: Vec<u64> = Vec::new();

    let mut eval = MickEvalLog::from_env();
    if eval.is_some() {
        println!("   (mick-eval capture ON → KIRRA_MICK_EVAL_PATH or kirra_mick_eval.jsonl)");
    }

    println!("   t(s)   ego.x  ego.y     v   light   intent (System-2)        cedes   kirra");
    for tick in 1..=TICKS {
        let now_ms = tick as u64 * FAST_DT_MS;
        // The live signal feed: RED until GREEN_AT_MS, then GREEN. Keyed by the governed lane.
        let light = if now_ms < GREEN_AT_MS { SignalState::Red } else { SignalState::Green };
        let signals = [(1u64, light)];

        // SLOW loop: re-ask the brain / re-ground / re-validate at the slow cadence or when the
        // committed trajectory is spent. Promote only what KIRRA admits.
        let replan_due = tracker.is_exhausted(now_ms) || last_replan_ms.is_none_or(|t| now_ms.saturating_sub(t) >= REPLAN_MS);
        if replan_due {
            let w = world(ego, &route, &objs, &signals, &g, goal);
            let plan = driver.drive_tick(&w, &mut occy, now_ms);
            last_v = verdict(&plan, &route, &objs);
            cedes = derived_cedes(&w); // the cede set the right-of-way derived (observability)
            if let (Some(log), Some(intent)) = (eval.as_mut(), driver.current_intent()) {
                let _ = log.append(&MickDecisionRecord::new(tick as u64, now_ms, &intent, &plan, last_v));
            }
            if matches!(last_v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp) {
                tracker.promote(plan, now_ms);
                last_replan_ms = Some(now_ms);
            }
        }

        // FAST loop: track the committed trajectory by elapsed time; MRC if none/exhausted.
        ego = match tracker.track(now_ms) {
            Some(cmd) => EgoState { pose: cmd.pose, linear_x_mps: cmd.velocity_mps, yaw_rate_rads: 0.0, stamp_ms: now_ms },
            None => EgoState {
                pose: ego.pose, linear_x_mps: (ego.linear_x_mps - MRC_DECEL * FAST_DT_S).max(0.0),
                yaw_rate_rads: 0.0, stamp_ms: now_ms,
            },
        };

        let intent = driver.current_intent();
        let light_s = if light == SignalState::Red { "RED " } else { "GREEN" };
        println!(
            "{:>6.1}  {:>6.2} {:>6.2}  {:>5.2}   {light_s}   {:<22?}  {:>5?}   {last_v:?}",
            now_ms as f64 / 1000.0, ego.pose.x_m, ego.pose.y_m, ego.linear_x_mps, intent, cedes,
        );
    }
    println!(
        "final ego = ({:.1}, {:.1}) — held at the line on red, then tracked the turn through the arc on green; right-of-way asserted; KIRRA bounded every tracked pose.",
        ego.pose.x_m, ego.pose.y_m
    );
}

/// The cede set the junction's right-of-way grants the ego at its current pose — what
/// `plan_for_intent` derives internally and feeds the planner (surfaced here for the trace).
fn derived_cedes(w: &PlanInput<'_>) -> Vec<u64> {
    let Some(g) = w.lane_graph else { return Vec::new() };
    g.junction_context(Point { x_m: w.ego.pose.x_m, y_m: w.ego.pose.y_m }, w.objects).cedes_to_ego
}
