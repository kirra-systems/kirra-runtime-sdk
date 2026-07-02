//! Emit the FP32-vs-int8 doer-eval **scorecard** over the demo corpus — the
//! cross-workspace seam (Q1 scope §4). Prints the versioned JSON the parko-side
//! Q-1b runner reads and joins with the on-target latency row.
//!
//! Run: `cargo run -p kirra-doer-eval --example scorecard`

use kirra_doer_eval::{
    demo_corpus, evaluate_corpus, quantize_over_corpus, Scorecard, ScorecardRow,
};
use kirra_planner::{LearnedPlanner, Teacher};

fn main() {
    let corpus = demo_corpus();

    // FP32 reference planner, and its int8 PTQ calibrated over the same corpus.
    let fp32 = LearnedPlanner::trained(0xC0FFEE, Teacher::SafetyAware);
    let int8 = quantize_over_corpus(&fp32, &corpus);

    // FP32 scored against itself (agreement = 1.0 by construction) and int8 vs FP32.
    let fp32_summary = evaluate_corpus(&corpus, &fp32, &fp32);
    let int8_summary = evaluate_corpus(&corpus, &int8, &fp32);

    let card = Scorecard::new(vec![
        ScorecardRow::from_summary("fp32", &fp32_summary),
        ScorecardRow::from_summary("int8-ptq", &int8_summary),
    ]);

    println!("{}", card.to_json());
}
