//! Emit the Q-1b doer-eval **artifacts** (Q1 scope §2/§4): the FP32 + int8-QDQ
//! ONNX models of the learned scorer, and the FP32-vs-int8 scorecard JSON — the
//! offline files the parko-side Orin runner loads and joins with on-target latency.
//!
//! Deterministic: seeded training + the fixed demo corpus ⇒ byte-identical output
//! every run (the artifact drift test pins this).
//!
//! **M-2**: also emits the v2 pair (`planner_v2_fp32.onnx` /
//! `planner_v2_int8_qdq.onnx`) and the v2 scorecard rows. The v2 model is NOT
//! trained here — it loads from the checked-in weights artifact
//! (`planner_v2_weights.bin`, regenerated only by `kirra-planner`'s `train_v2`
//! example), so this export stays a pure function of checked-in bytes.
//!
//! Run: `cargo run -p kirra-doer-eval --example export_artifacts [out_dir]`
//! Default out_dir: `artifacts/doer-eval` (repo root).

use std::fs;
use std::path::PathBuf;

use kirra_doer_eval::{
    demo_corpus, evaluate_corpus, onnx, quantize_over_corpus, quantize_v2_over_corpus, Scorecard,
    ScorecardRow,
};
use kirra_planner::{LearnedPlanner, LearnedPlannerV2, Teacher};

/// The one seed every Q-1 artifact derives from (same as the eval tests).
const SEED: u64 = 0xC0FFEE;

fn main() -> std::io::Result<()> {
    let out_dir = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("artifacts/doer-eval"), PathBuf::from);
    fs::create_dir_all(&out_dir)?;

    let corpus = demo_corpus();
    let fp32 = LearnedPlanner::trained(SEED, Teacher::SafetyAware);
    let int8 = quantize_over_corpus(&fp32, &corpus);

    // The two v1 model artifacts.
    let fp32_bytes = onnx::fp32_model(&fp32.scorer_weights());
    let qdq_bytes = onnx::int8_qdq_model(&int8.scorer_weights());
    fs::write(out_dir.join("planner_fp32.onnx"), &fp32_bytes)?;
    fs::write(out_dir.join("planner_int8_qdq.onnx"), &qdq_bytes)?;

    // The v2 pair (M-2): loaded from the CHECKED-IN weights artifact (never
    // trained here), exported through the chain writers. Resolved against the
    // repo-root artifacts dir so a custom out_dir still finds the weights.
    let weights_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../artifacts/doer-eval/planner_v2_weights.bin");
    let v2 = LearnedPlannerV2::from_bytes(&fs::read(&weights_path)?)
        .expect("valid checked-in v2 weights artifact");
    let v2_int8 = quantize_v2_over_corpus(&v2, &corpus);
    let v2_fp32_bytes = onnx::fp32_model_chain(&v2.scorer_weights());
    let v2_qdq_bytes = onnx::int8_qdq_model_chain(&v2_int8.scorer_weights());
    fs::write(out_dir.join("planner_v2_fp32.onnx"), &v2_fp32_bytes)?;
    fs::write(out_dir.join("planner_v2_int8_qdq.onnx"), &v2_qdq_bytes)?;

    // The scorecard the Orin runner joins with its latency rows: v1 rows first
    // (the small-model baseline), then the v2 rows (M-2).
    let fp32_summary = evaluate_corpus(&corpus, &fp32, &fp32);
    let int8_summary = evaluate_corpus(&corpus, &int8, &fp32);
    let v2_fp32_summary = evaluate_corpus(&corpus, &v2, &v2);
    let v2_int8_summary = evaluate_corpus(&corpus, &v2_int8, &v2);
    let card = Scorecard::new(vec![
        ScorecardRow::from_summary("fp32", &fp32_summary),
        ScorecardRow::from_summary("int8-ptq", &int8_summary),
        ScorecardRow::from_summary("v2-fp32", &v2_fp32_summary),
        ScorecardRow::from_summary("v2-int8-ptq", &v2_int8_summary),
    ]);
    fs::write(out_dir.join("scorecard.json"), card.to_json())?;

    println!(
        "wrote {}: planner_fp32.onnx ({} B), planner_int8_qdq.onnx ({} B), \
         planner_v2_fp32.onnx ({} B), planner_v2_int8_qdq.onnx ({} B), scorecard.json",
        out_dir.display(),
        fp32_bytes.len(),
        qdq_bytes.len(),
        v2_fp32_bytes.len(),
        v2_qdq_bytes.len(),
    );
    Ok(())
}
