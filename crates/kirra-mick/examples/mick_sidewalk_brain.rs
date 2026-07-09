//! **The sidewalk-courier brain: a local LLM authors sidewalk intents, KIRRA bounds them (ADR-0028).**
//! Mick's `LlmBrain::courier(...)` prompts a pedestrian-space persona offered ONLY the sidewalk
//! intents (`go_to` / `yield` / `cross_when_clear` / `creep_through` / `hold`) — no road maneuvers —
//! and is grammar-constrained to them. Each authored intent is grounded by Occy (courier planner)
//! and bounded by KIRRA (courier checker), so the model can never make the robot unsafe.
//!
//! This runs against a deterministic `MockModel` (no network), so it works anywhere — it stands in
//! for the choice a local Gemma would make per situation. On the Orin, swap one line:
//!     let brain = LlmBrain::courier(kirra_mick::OllamaClient::courier());   // real Gemma via Ollama
//!
//! Run: `cargo run -p kirra-mick --example mick_sidewalk_brain`

use kirra_core::FleetPosture;
use kirra_planner::{
    plan_for_intent, EgoState, GeometricPlanner, GeometricPlannerConfig, Goal, LlmBrain, MickBrain,
    MockModel, ObjectView, PlanInput, Pose, ProposalKind, WorldContext,
};
use kirra_trajectory::corridor::{MockCorridorSource, Point};
use kirra_trajectory::state::{PerceivedObject, TrajectoryVerdict};
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

const EGO_X: f64 = 2.0;

/// A scene: nearby objects (ego-relative ahead/left), and the goal ahead. Builds both the
/// `WorldContext` the brain sees and the `PlanInput` Occy grounds against.
struct Scene {
    objects: Vec<(f64, f64, f64)>, // (ahead_m, left_m, speed_mps)
    goal_ahead_m: f64,
}

impl Scene {
    fn world_context(&self) -> WorldContext {
        WorldContext {
            ego_speed_mps: 1.0,
            posture: "NOMINAL",
            goal_ahead_m: self.goal_ahead_m,
            goal_left_m: 0.0,
            may_change_left: false,
            may_change_right: false,
            objects: self
                .objects
                .iter()
                .enumerate()
                .map(|(i, &(a, l, s))| ObjectView {
                    id: i as u64,
                    ahead_m: a,
                    left_m: l,
                    speed_mps: s,
                })
                .collect(),
            available_turns: Vec::new(),
        }
    }
    fn perceived(&self) -> Vec<PerceivedObject> {
        self.objects
            .iter()
            .enumerate()
            .map(|(i, &(a, l, s))| PerceivedObject {
                id: i as u64,
                pos: Point {
                    x_m: EGO_X + a,
                    y_m: l,
                },
                velocity_mps: s,
                heading_rad: 0.0,
                vel: Point { x_m: 0.0, y_m: s }, // crossing agents move laterally
            })
            .collect()
    }
}

fn main() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let vcfg = VehicleConfig::courier();
    let mut occy = GeometricPlanner::new(GeometricPlannerConfig::courier());

    // One look at the prompt the local model actually receives (sidewalk persona, sidewalk intents).
    let demo = Scene {
        objects: vec![(3.0, 0.0, 0.0)],
        goal_ahead_m: 14.0,
    };
    println!("===== the courier prompt Gemma sees (excerpt) =====");
    let prompt = kirra_planner::build_courier_prompt(&demo.world_context());
    for line in prompt.lines().take(12) {
        println!("  {line}");
    }
    println!("  ...\n");

    // Scenarios: the situation, and the sidewalk intent the local model emits for it (MockModel
    // stands in for Gemma). Each is grounded + checked.
    let scenarios: &[(&str, &str, Scene)] = &[
        (
            "clear sidewalk ahead",
            r#"{"intent":"go_to","x_m":14.0,"y_m":0.0}"#,
            Scene {
                objects: vec![],
                goal_ahead_m: 14.0,
            },
        ),
        (
            "a pedestrian standing in the path",
            r#"{"intent":"yield","x_m":14.0,"y_m":0.0}"#,
            Scene {
                objects: vec![(5.0, 0.0, 0.0)],
                goal_ahead_m: 14.0,
            },
        ),
        (
            "a dense crowd around the robot",
            r#"{"intent":"creep_through","x_m":14.0,"y_m":0.0}"#,
            Scene {
                objects: vec![(2.5, 0.3, 0.2), (3.5, -0.4, 0.1), (4.0, 0.5, 0.0)],
                goal_ahead_m: 14.0,
            },
        ),
        (
            "at a crosswalk, a car approaching",
            r#"{"intent":"cross_when_clear","x_m":9.0,"y_m":0.0}"#,
            Scene {
                objects: vec![(4.0, -12.0, 6.0)],
                goal_ahead_m: 9.0,
            },
        ),
    ];

    println!("===== Mick(courier brain) → Occy(doer) → KIRRA(courier checker) =====");
    println!(
        "  {:<34} {:<16} {:<10} kirra",
        "situation", "Mick intent", "occy"
    );
    println!("  {}", "-".repeat(78));
    for (desc, reply, scene) in scenarios {
        // The local model authors the intent (MockModel == a fixed Gemma choice for this situation).
        let mut brain = LlmBrain::courier(MockModel::replying(*reply));
        let intent = match brain.decide(&scene.world_context()) {
            Ok(i) => i,
            Err(e) => {
                println!("  {desc:<34} <fail-closed: {e}>");
                continue;
            }
        };

        // Occy grounds it; KIRRA (courier profile) bounds it.
        let objs = scene.perceived();
        let world = PlanInput {
            ego: EgoState {
                pose: Pose {
                    x_m: EGO_X,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                linear_x_mps: 1.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal {
                target: Pose {
                    x_m: EGO_X + scene.goal_ahead_m,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
            },
            map: &corr,
            objects: &objs,
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
            signal_states: &[],
        };
        let plan = plan_for_intent(&mut occy, &intent, &world);
        let verdict = validate_trajectory_slow(
            &plan.trajectory,
            &corr,
            &objs,
            &vcfg,
            None,
            FleetPosture::Nominal,
        );

        let intent_tag = match intent {
            kirra_planner::MickIntent::GoTo { .. } => "go_to",
            kirra_planner::MickIntent::Yield { .. } => "yield",
            kirra_planner::MickIntent::CreepThrough { .. } => "creep_through",
            kirra_planner::MickIntent::CrossWhenClear { .. } => "cross_when_clear",
            kirra_planner::MickIntent::Hold => "hold",
            _ => "other",
        };
        let kind = match plan.kind {
            ProposalKind::Motion => "Motion",
            ProposalKind::SafeStop => "SafeStop",
        };
        let v = match verdict {
            TrajectoryVerdict::Accept => "Accept",
            TrajectoryVerdict::Clamp => "Clamp",
            TrajectoryVerdict::MRCFallback => "MRCFallback",
            other => {
                println!("{other:?}");
                "?"
            }
        };
        println!("  {desc:<34} {intent_tag:<16} {kind:<10} {v}");
    }

    println!(
        "\n  The courier brain authors only sidewalk intents (grammar-constrained); Occy grounds"
    );
    println!("  them and KIRRA (courier profile) bounds every one. Swap MockModel for");
    println!(
        "  OllamaClient::courier() to drive this with a local Gemma on the Orin — the model can"
    );
    println!("  never make the robot unsafe: its intent is grounded by Occy and bounded by KIRRA.");
}
