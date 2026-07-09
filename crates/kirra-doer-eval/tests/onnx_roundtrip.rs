//! ROUND-TRIP VERIFICATION of the ONNX export (Q-1b): the emitted bytes are
//! loaded through the REAL ONNX Runtime (`parko-onnx`'s `OrtBackend`, CPU) and
//! must reproduce the Rust scorer's outputs — FP32 model vs [`Mlp::forward`]'s
//! scores, QDQ model vs the in-Rust [`QuantizedLearnedPlanner`]'s scores — on
//! every feature vector in the demo corpus.
//!
//! GATING (the `parko-tensorrt` probe idiom): self-SKIPS when no loadable ORT
//! runtime is present (`ORT_DYLIB_PATH` unset or missing) — CI without the dylib
//! stays green. STRICT MODE: `KIRRA_DOER_EVAL_REQUIRE_ORT=1` turns a would-be
//! skip into a hard failure, for lanes that must actually verify (the Orin).

use std::collections::HashMap;

use kirra_doer_eval::{demo_corpus, onnx, quantize_over_corpus, quantize_v2_over_corpus};
use kirra_planner::{LearnedPlanner, LearnedPlannerV2, ScoredPlanner, Teacher};
use parko_core::backend::{InferenceBackend, TensorBatch, TensorStorage};
use parko_onnx::OrtBackend;

const SEED: u64 = 0xC0FFEE;

fn require_ort() -> bool {
    std::env::var("KIRRA_DOER_EVAL_REQUIRE_ORT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// `Some(())` if a loadable ORT runtime is present; `None` → skip (or panic in
/// strict mode). ort PANICS on a missing dylib, so the FILE presence is the guard.
fn ort_available() -> Option<()> {
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).is_file() {
        assert!(
            !require_ort(),
            "STRICT (KIRRA_DOER_EVAL_REQUIRE_ORT): no loadable ORT runtime at \
             ORT_DYLIB_PATH ({dylib:?}) — refusing to skip the round-trip verification."
        );
        eprintln!("SKIP: no ORT runtime ({dylib:?}) — ONNX round-trip needs a real ORT lib.");
        return None;
    }
    Some(())
}

/// Run one feature vector through an ONNX model via the real ORT CPU backend.
fn ort_scores(backend: &OrtBackend, model_path: &str, features: &[f64]) -> Vec<f64> {
    let model = backend
        .load_model(model_path)
        .expect("introspect exported model");
    let x: Vec<f32> = features.iter().map(|&v| v as f32).collect();
    let mut named = HashMap::new();
    named.insert(onnx::INPUT_NAME.to_string(), TensorStorage::Borrowed(&x));
    let out = backend
        .run(
            &model,
            &TensorBatch {
                named_tensors: named,
                metadata: HashMap::new(),
            },
        )
        .expect("exported model must run");
    out.named_tensors[onnx::OUTPUT_NAME]
        .as_slice()
        .iter()
        .map(|&v| f64::from(v))
        .collect()
}

fn argmax(v: &[f64]) -> usize {
    let mut best = 0;
    for (k, &val) in v.iter().enumerate() {
        if val > v[best] {
            best = k;
        }
    }
    best
}

/// The FP32 export reproduces the Rust scorer through the real ONNX Runtime:
/// scores match within f32-cast tolerance on every corpus feature vector.
#[test]
fn fp32_export_matches_rust_scorer_through_real_ort() {
    if ort_available().is_none() {
        return;
    }
    let corpus = demo_corpus();
    let fp32 = LearnedPlanner::trained(SEED, Teacher::SafetyAware);

    let dir = std::env::temp_dir().join("kirra_doer_eval_roundtrip");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("planner_fp32.onnx");
    std::fs::write(&path, onnx::fp32_model(&fp32.scorer_weights())).unwrap();
    let path = path.to_string_lossy().into_owned();

    let backend = OrtBackend::new(&path).expect("ORT loads the exported FP32 model");
    for sc in &corpus {
        let input = sc.plan_input();
        let (features, rust_scores) = fp32.features_and_scores(&input);
        let onnx_scores = ort_scores(&backend, &path, &features);
        assert_eq!(onnx_scores.len(), rust_scores.len());
        for (k, (a, b)) in rust_scores.iter().zip(onnx_scores.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-4,
                "{}: fp32 score {k} diverged — rust {a} vs onnx {b}",
                sc.name
            );
        }
        assert_eq!(
            argmax(&rust_scores),
            argmax(&onnx_scores),
            "{}: fp32 argmax must agree",
            sc.name
        );
    }
    println!("ROUND-TRIP PASSED: FP32 export matches the Rust scorer through real ORT");
}

/// The QDQ export reproduces the in-Rust int8 planner through the real ONNX
/// Runtime — the SAME codes + scales, so the scores agree within f32/rounding
/// tolerance and the argmax (the doer's actual decision) is identical.
#[test]
fn qdq_export_matches_rust_int8_planner_through_real_ort() {
    if ort_available().is_none() {
        return;
    }
    let corpus = demo_corpus();
    let fp32 = LearnedPlanner::trained(SEED, Teacher::SafetyAware);
    let int8 = quantize_over_corpus(&fp32, &corpus);

    let dir = std::env::temp_dir().join("kirra_doer_eval_roundtrip");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("planner_int8_qdq.onnx");
    std::fs::write(&path, onnx::int8_qdq_model(&int8.scorer_weights())).unwrap();
    let path = path.to_string_lossy().into_owned();

    let backend = OrtBackend::new(&path).expect("ORT loads the exported QDQ model");
    for sc in &corpus {
        let input = sc.plan_input();
        let (features, _) = fp32.features_and_scores(&input);
        let rust_scores = int8.scores(&input);
        let onnx_scores = ort_scores(&backend, &path, &features);
        for (k, (a, b)) in rust_scores.iter().zip(onnx_scores.iter()).enumerate() {
            assert!(
                (a - b).abs() < 2e-2,
                "{}: qdq score {k} diverged — rust-int8 {a} vs onnx-qdq {b}",
                sc.name
            );
        }
        assert_eq!(
            argmax(&rust_scores),
            argmax(&onnx_scores),
            "{}: the int8 DECISION (argmax) must be identical in-Rust vs QDQ-ONNX",
            sc.name
        );
        // And the decision equals what the harness scored in the Q-1a eval.
        assert_eq!(
            argmax(&onnx_scores),
            int8.chosen_index(&input),
            "{}: QDQ-ONNX argmax equals the Q-1a quantized planner's choice",
            sc.name
        );
    }
    println!("ROUND-TRIP PASSED: QDQ export matches the in-Rust int8 planner through real ORT");
}

/// The checked-in v2 planner, loaded from its weights artifact (M-2: the chain
/// exporters round-trip the SHIPPED model, not a test-time retrain).
fn v2_from_artifact() -> LearnedPlannerV2 {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../artifacts/doer-eval/planner_v2_weights.bin");
    let bytes = std::fs::read(path).expect("missing checked-in v2 weights artifact");
    LearnedPlannerV2::from_bytes(&bytes).expect("valid v2 weights artifact")
}

/// M-2: the FP32 CHAIN export reproduces the v2 Rust scorer through real ORT.
/// Tolerance is looser than v1's (1e-3 vs 1e-4): three f32 matmul layers of
/// width 256 accumulate more cast error than v1's 4×8, and the scores span ~5.
#[test]
fn v2_fp32_chain_export_matches_rust_scorer_through_real_ort() {
    if ort_available().is_none() {
        return;
    }
    let corpus = demo_corpus();
    let v2 = v2_from_artifact();

    let dir = std::env::temp_dir().join("kirra_doer_eval_roundtrip");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("planner_v2_fp32.onnx");
    std::fs::write(&path, onnx::fp32_model_chain(&v2.scorer_weights())).unwrap();
    let path = path.to_string_lossy().into_owned();

    let backend = OrtBackend::new(&path).expect("ORT loads the exported v2 FP32 chain model");
    for sc in &corpus {
        let input = sc.plan_input();
        let (features, rust_scores) = v2.features_and_scores(&input);
        let onnx_scores = ort_scores(&backend, &path, &features);
        assert_eq!(onnx_scores.len(), rust_scores.len());
        for (k, (a, b)) in rust_scores.iter().zip(onnx_scores.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-3,
                "{}: v2 fp32 score {k} diverged — rust {a} vs onnx {b}",
                sc.name
            );
        }
        assert_eq!(
            argmax(&rust_scores),
            argmax(&onnx_scores),
            "{}: v2 fp32 argmax must agree",
            sc.name
        );
    }
    println!("ROUND-TRIP PASSED: v2 FP32 chain export matches the Rust scorer through real ORT");
}

/// M-2 CI exit criterion (`DOER_MODEL_SCALEUP.md` §4): the v2 QDQ chain export
/// reproduces the in-Rust int8 planner through real ORT — same codes + scales,
/// IDENTICAL argmax (the doer's actual decision) on every corpus scene.
#[test]
fn v2_qdq_chain_export_matches_rust_int8_planner_through_real_ort() {
    if ort_available().is_none() {
        return;
    }
    let corpus = demo_corpus();
    let v2 = v2_from_artifact();
    let v2_int8 = quantize_v2_over_corpus(&v2, &corpus);

    let dir = std::env::temp_dir().join("kirra_doer_eval_roundtrip");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("planner_v2_int8_qdq.onnx");
    std::fs::write(&path, onnx::int8_qdq_model_chain(&v2_int8.scorer_weights())).unwrap();
    let path = path.to_string_lossy().into_owned();

    let backend = OrtBackend::new(&path).expect("ORT loads the exported v2 QDQ chain model");
    for sc in &corpus {
        let input = sc.plan_input();
        let (features, _) = v2.features_and_scores(&input);
        let rust_scores = v2_int8.scores(&input);
        let onnx_scores = ort_scores(&backend, &path, &features);
        for (k, (a, b)) in rust_scores.iter().zip(onnx_scores.iter()).enumerate() {
            assert!(
                (a - b).abs() < 5e-2,
                "{}: v2 qdq score {k} diverged — rust-int8 {a} vs onnx-qdq {b}",
                sc.name
            );
        }
        assert_eq!(
            argmax(&rust_scores),
            argmax(&onnx_scores),
            "{}: the v2 int8 DECISION (argmax) must be identical in-Rust vs QDQ-ONNX",
            sc.name
        );
        assert_eq!(
            argmax(&onnx_scores),
            v2_int8.chosen_index(&input),
            "{}: v2 QDQ-ONNX argmax equals the quantized planner's choice",
            sc.name
        );
    }
    println!(
        "ROUND-TRIP PASSED: v2 QDQ chain export matches the in-Rust int8 planner through real ORT"
    );
}
