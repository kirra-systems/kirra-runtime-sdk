//! ARTIFACT DRIFT GUARD (Q-1b): the checked-in `artifacts/doer-eval/*` files must
//! be byte-identical to what `examples/export_artifacts.rs` regenerates from the
//! current code. The export is deterministic (seeded training, fixed corpus, pure
//! encoder), so any divergence means someone changed the scorer / PTQ / encoder /
//! corpus without re-exporting — the checked-in artifact would silently misstate
//! the model. Pure CPU, no ORT — runs on CI.
//!
//! On failure, regenerate: `cargo run -p kirra-doer-eval --example export_artifacts`

use kirra_doer_eval::{
    demo_corpus, evaluate_corpus, onnx, quantize_over_corpus, quantize_v2_over_corpus, Scorecard,
    ScorecardRow,
};
use kirra_planner::{LearnedPlanner, LearnedPlannerV2, Teacher};

const SEED: u64 = 0xC0FFEE;

/// Repo-root artifacts dir, resolved from this crate's manifest dir (tests run
/// with CWD = crate dir, but CARGO_MANIFEST_DIR is stable regardless).
fn artifacts_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/doer-eval")
}

/// The checked-in v2 planner, loaded from its weights artifact — the drift
/// regeneration is a pure function of the CHECKED-IN bytes (never a retrain).
fn v2_from_artifact() -> LearnedPlannerV2 {
    let bytes = std::fs::read(artifacts_dir().join("planner_v2_weights.bin"))
        .expect("missing checked-in v2 weights artifact");
    LearnedPlannerV2::from_bytes(&bytes).expect("valid v2 weights artifact")
}

#[test]
fn checked_in_artifacts_match_regeneration() {
    let dir = artifacts_dir();
    let corpus = demo_corpus();
    let fp32 = LearnedPlanner::trained(SEED, Teacher::SafetyAware);
    let int8 = quantize_over_corpus(&fp32, &corpus);
    let v2 = v2_from_artifact();
    let v2_int8 = quantize_v2_over_corpus(&v2, &corpus);

    // Models: byte-identical.
    for (file, expected) in [
        (
            "planner_fp32.onnx",
            onnx::fp32_model(&fp32.scorer_weights()),
        ),
        (
            "planner_int8_qdq.onnx",
            onnx::int8_qdq_model(&int8.scorer_weights()),
        ),
        (
            "planner_v2_fp32.onnx",
            onnx::fp32_model_chain(&v2.scorer_weights()),
        ),
        (
            "planner_v2_int8_qdq.onnx",
            onnx::int8_qdq_model_chain(&v2_int8.scorer_weights()),
        ),
    ] {
        let on_disk = std::fs::read(dir.join(file))
            .unwrap_or_else(|e| panic!("missing checked-in artifact {file}: {e}"));
        assert_eq!(
            on_disk, expected,
            "{file} drifted from regeneration — re-run \
             `cargo run -p kirra-doer-eval --example export_artifacts`"
        );
    }

    // Scorecard: byte-identical JSON (v1 baseline rows + the M-2 v2 rows).
    let fp32_summary = evaluate_corpus(&corpus, &fp32, &fp32);
    let int8_summary = evaluate_corpus(&corpus, &int8, &fp32);
    let v2_fp32_summary = evaluate_corpus(&corpus, &v2, &v2);
    let v2_int8_summary = evaluate_corpus(&corpus, &v2_int8, &v2);
    let expected = Scorecard::new(vec![
        ScorecardRow::from_summary("fp32", &fp32_summary),
        ScorecardRow::from_summary("int8-ptq", &int8_summary),
        ScorecardRow::from_summary("v2-fp32", &v2_fp32_summary),
        ScorecardRow::from_summary("v2-int8-ptq", &v2_int8_summary),
    ])
    .to_json();
    let on_disk = std::fs::read_to_string(dir.join("scorecard.json"))
        .expect("missing checked-in scorecard.json");
    assert_eq!(
        on_disk, expected,
        "scorecard.json drifted — re-run the export example"
    );
}
