//! BEHAVIOR GATE for the checked-in v2 weights artifact (M-1, scope §2/§5.3):
//! CI validates what the artifact DOES, never retrains it (cross-arch float
//! reproducibility is not guaranteed; regeneration is documented in
//! `examples/train_v2.rs`). Deliberately NOT byte-drift-gated — the ONNX
//! artifacts are pinned byte-exact because their generator is pure; a trained
//! net is pinned by its measured behavior instead.
//!
//! Gates:
//!   1. The artifact parses, is SafetyAware, and has the scoped full shape.
//!   2. TEACHER-SCORE REGRET on a HELD-OUT probe grid (parameter sweep,
//!      disjoint from the random training stream by construction) stays under
//!      a ceiling.
//!
//! On failure after an intentional model change: retrain via
//!   cargo run --release -p kirra-planner --example train_v2
//! and re-measure the ceilings (they are pinned with margin above measured).

use kirra_core::corridor::{MockCorridorSource, Point};
use kirra_core::trajectory::PerceivedObject;
use kirra_core::FleetPosture;
use kirra_planner::{
    teacher_candidate_score, teacher_choice, EgoState, Goal, LearnedPlannerV2, PlanInput, Pose,
    ScoredPlanner, ScorerConfigV2, Teacher,
};

fn artifact_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../artifacts/doer-eval/planner_v2_weights.bin")
}

fn load() -> LearnedPlannerV2 {
    let bytes = std::fs::read(artifact_path()).expect(
        "checked-in v2 weights artifact missing — regenerate with \
         `cargo run --release -p kirra-planner --example train_v2`",
    );
    LearnedPlannerV2::from_bytes(&bytes).expect("valid v2 weights artifact")
}

fn world<'a>(
    map: &'a MockCorridorSource,
    objects: &'a [PerceivedObject],
    ego_speed: f64,
) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: 5.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            linear_x_mps: ego_speed,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal {
            target: Pose {
                x_m: 40.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
        },
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
        signal_states: &[],
    }
}

fn car(x: f64, y: f64) -> PerceivedObject {
    PerceivedObject {
        id: 1,
        pos: Point { x_m: x, y_m: y },
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }
}

#[test]
fn artifact_parses_with_the_scoped_identity() {
    let p = load();
    assert_eq!(
        p.teacher(),
        Teacher::SafetyAware,
        "the shipped doer is the safety-aware one"
    );
    assert_eq!(
        p.config(),
        &ScorerConfigV2::full(),
        "the shipped doer is the FULL config"
    );
}

/// TEACHER-SCORE REGRET on a held-out parameter grid: ego speed × hazard
/// layout, 36 scenes none of which is a sample of the random training stream.
///
/// Why regret and not exact-argmax agreement: with 256 grid candidates the
/// teacher's top-1 sits on near-tie plateaus (adjacent offsets/speeds differ by
/// ~1e-2 in teacher score) while a distilled net's regression error is ~1e-1 —
/// exact top-1 matching measured 0/36 on a model whose every pick the teacher
/// itself scored within ~0.2 of optimal. The robust gate is "the net's pick,
/// scored BY THE TEACHER, costs ≤ ε versus the teacher's own choice."
///
/// Ceilings pinned with margin above the measured artifact (mean 0.300, worst
/// 1.402 at check-in; the worst case is the net picking a slower/stopping
/// candidate where the teacher threaded a multi-object scene at speed — a
/// conservative miss, not a hazard pick, which would cost ≥ 5.0).
#[test]
fn artifact_regret_vs_its_teacher_stays_bounded_on_held_out_scenes() {
    let p = load();
    let cfg = p.config().clone();
    let corr = MockCorridorSource::straight_5m_half_width(100.0);

    let mut sum = 0.0f64;
    let mut worst = 0.0f64;
    let mut n = 0usize;
    for &ego_speed in &[0.5, 1.5, 2.5, 3.5] {
        // Hazard layouts: clear road + stopped cars at a distance × lateral sweep.
        let layouts: [Vec<PerceivedObject>; 9] = [
            vec![],
            vec![car(13.0, 0.0)],
            vec![car(19.0, 0.0)],
            vec![car(26.0, 0.0)],
            vec![car(33.0, 0.0)],
            vec![car(19.0, 2.0)],
            vec![car(19.0, -2.0)],
            vec![car(15.0, 0.0), car(28.0, 1.0)],
            vec![car(12.0, -1.0), car(22.0, 0.0), car(30.0, 2.0)],
        ];
        for objs in &layouts {
            let w = world(&corr, objs, ego_speed);
            let net_c = p.chosen_index(&w);
            let tea_c = teacher_choice(&cfg, &w, Teacher::SafetyAware);
            let regret = teacher_candidate_score(&cfg, &w, tea_c, Teacher::SafetyAware)
                - teacher_candidate_score(&cfg, &w, net_c, Teacher::SafetyAware);
            sum += regret;
            worst = worst.max(regret);
            n += 1;
        }
    }
    let mean = sum / n as f64;
    println!("held-out teacher regret over {n} scenes: mean {mean:.3}, worst {worst:.3}");
    const MEAN_CEILING: f64 = 0.5;
    const WORST_CEILING: f64 = 2.0;
    assert!(
        mean <= MEAN_CEILING,
        "mean held-out teacher regret {mean:.3} exceeded {MEAN_CEILING} — did the model or \
         encoder change without retraining the artifact?"
    );
    assert!(
        worst <= WORST_CEILING,
        "worst held-out teacher regret {worst:.3} exceeded {WORST_CEILING} (teacher-score units; \
         collision = 5.0) — a single catastrophic pick, not plateau noise"
    );
}
