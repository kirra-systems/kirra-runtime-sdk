//! ADMISSIBILITY GATE for the checked-in v2 weights artifact (M-1 exit
//! criteria, `parko/DOER_MODEL_SCALEUP.md` §4): the shipped safety-aware doer,
//! loaded from bytes, must be admitted by the UNCHANGED checker on the demo
//! corpus — the load-bearing floor. Behavior-gated, never retrained in CI.

use kirra_doer_eval::{demo_corpus, evaluate_corpus};
use kirra_planner::LearnedPlannerV2;

fn load() -> LearnedPlannerV2 {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../artifacts/doer-eval/planner_v2_weights.bin");
    let bytes = std::fs::read(path).expect(
        "checked-in v2 weights artifact missing — regenerate with \
         `cargo run --release -p kirra-planner --example train_v2`",
    );
    LearnedPlannerV2::from_bytes(&bytes).expect("valid v2 weights artifact")
}

/// The shipped artifact through the unchanged harness: fully self-consistent
/// (agreement 1.0 against itself) and — the gate — ADMITTED by the checker on
/// every demo scenario. A model/encoder change that breaks this must retrain
/// and re-measure, not relax the floor.
#[test]
fn shipped_v2_artifact_is_checker_admissible_on_the_corpus() {
    let p = load();
    let corpus = demo_corpus();
    let s = evaluate_corpus(&corpus, &p, &p);

    assert_eq!(
        s.quality.argmax_agreement_rate(),
        1.0,
        "self-agreement is definitionally 1.0"
    );
    assert_eq!(
        s.admissibility.admissibility_rate(),
        1.0,
        "the shipped safety-aware v2 doer must be admitted on every demo scenario: {:?}",
        s.admissibility
    );
    assert_eq!(
        s.admissibility.mrc, 0,
        "no MRC refusals for the shipped doer"
    );
}
