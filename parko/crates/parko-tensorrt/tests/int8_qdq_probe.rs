//! INT8-QDQ PROBE (Q-1b) — on-hardware validation that the TensorRT backend runs
//! the exported **explicit-quantization (QDQ)** planner model under the opt-in
//! `TrtPrecision::Int8Qdq` posture (`parko/QUANTIZATION_Q1_SCOPE.md` §2).
//!
//! The model artifact is produced by the ROOT workspace
//! (`cargo run -p kirra-doer-eval --example export_artifacts`) and checked in at
//! `artifacts/doer-eval/planner_int8_qdq.onnx` — the cross-workspace file seam.
//! Its QDQ nodes carry the in-Rust PTQ calibration (scales + int8 codes), so
//! TensorRT INT8 needs no separate calibration table: one calibration, every
//! backend (design note §6). Override the path with `KIRRA_PLANNER_QDQ_ONNX`.
//!
//! GATING — the standard probe idiom (see `positive_probe.rs`): self-SKIPS
//! wherever the TensorRT EP is unavailable (GPU-less sandbox, CPU-only ORT CI
//! job); `PARKO_TRT_REQUIRE_EP=1` turns a skip into a hard failure (the Orin
//! strict lane). Run on hardware:
//!
//!   ORT_DYLIB_PATH=<...>/libonnxruntime.so PARKO_TRT_REQUIRE_EP=1 \
//!     cargo test -p parko-tensorrt --test int8_qdq_probe -- --nocapture

use std::collections::HashMap;

use parko_core::backend::{InferenceBackend, TensorBatch, TensorStorage};
use parko_tensorrt::{TrtBackend, TrtConfig, TrtPrecision};

fn require_ep() -> bool {
    std::env::var("PARKO_TRT_REQUIRE_EP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// The checked-in QDQ artifact, relative to this crate (tests run with CWD =
/// the crate dir); env-overridable for out-of-tree artifacts.
fn qdq_model_path() -> String {
    std::env::var("KIRRA_PLANNER_QDQ_ONNX")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "../../../artifacts/doer-eval/planner_int8_qdq.onnx".to_string())
}

/// Under the Int8Qdq posture on a TensorRT-enabled ORT, the exported QDQ planner
/// model must build an engine (warm-up), run, and produce 4 finite, stable scores.
#[test]
fn trt_int8_qdq_builds_and_runs_the_exported_planner() {
    let model_path = qdq_model_path();
    assert!(
        std::path::Path::new(&model_path).exists(),
        "QDQ artifact missing at {model_path} — regenerate with \
         `cargo run -p kirra-doer-eval --example export_artifacts` (root workspace)",
    );

    // No loadable ORT runtime at all → skip (or refuse, strict).
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).exists() {
        assert!(
            !require_ep(),
            "STRICT (PARKO_TRT_REQUIRE_EP): no loadable ORT runtime at ORT_DYLIB_PATH \
             ({dylib:?}) — refusing (fail-closed).",
        );
        eprintln!("SKIP: ORT runtime lib not present ({dylib:?}) — int8-QDQ probe is Jetson-gated.");
        return;
    }

    let cache = std::env::temp_dir().join("parko_trt_int8_qdq_probe_cache");
    let cfg = TrtConfig { engine_cache_path: cache.to_string_lossy().into_owned() };

    let backend = match TrtBackend::with_precision(&model_path, &cfg, TrtPrecision::Int8Qdq) {
        Ok(b) => b,
        Err(e) => {
            assert!(
                !require_ep(),
                "STRICT (PARKO_TRT_REQUIRE_EP): TensorRT EP unavailable ({e:?}) — refusing \
                 (fail-closed); the INT8 row did NOT validate.",
            );
            eprintln!("SKIP: TensorRT EP unavailable ({e:?}) — expected off-Jetson.");
            return;
        }
    };

    // --- TRT EP available: everything below MUST hold. ---

    // The posture is the audited opt-in INT8 record (and honestly not full precision).
    let posture = backend.posture();
    assert!(posture.int8, "Int8Qdq posture must set int8");
    assert!(!posture.fp16, "Int8Qdq posture must not also set fp16");
    assert!(!posture.full_precision_guaranteed());

    let model = backend.load_model(&model_path).expect("introspect the QDQ planner model");
    let in_shape = model.input_shapes.get("features").expect("input 'features' missing");
    assert_eq!(in_shape, &vec![1, 4], "planner scorer input is [1,4]");

    // Warm-up: force the INT8 engine build now (fail-closed on failure).
    let report = backend.warm_up_report(&model).expect("INT8 engine build must succeed");
    eprintln!(
        "int8 engine warm-up: {} ms, engine_sha={:?}",
        report.warmed_ms, report.engine_sha256
    );

    // A real featurized scene (ego 2 m/s, goal 35 m, hazard 18 m ahead, present).
    let features = [0.25f32, 0.7, 0.36, 1.0];
    let mut named = HashMap::new();
    named.insert("features".to_string(), TensorStorage::Borrowed(&features));
    let batch = TensorBatch { named_tensors: named, metadata: HashMap::new() };

    let run = |b: &TrtBackend| -> Vec<f32> {
        let out = b.run(&model, &batch).expect("INT8 QDQ inference must run");
        out.named_tensors["scores"].as_slice().to_vec()
    };
    let scores = run(&backend);
    assert_eq!(scores.len(), 4, "planner scorer emits 4 vocabulary scores");
    for (k, s) in scores.iter().enumerate() {
        assert!(s.is_finite(), "non-finite INT8 score at candidate {k}: {s}");
    }
    // Fixed engine + fixed input ⇒ the DECISION (argmax) must be stable.
    let again = run(&backend);
    let arg = |v: &[f32]| (0..v.len()).max_by(|&a, &b| v[a].total_cmp(&v[b])).unwrap();
    assert_eq!(arg(&scores), arg(&again), "argmax must be stable run-to-run");

    println!(
        "INT8-QDQ PROBE PASSED — engine built ({} ms), scores {scores:?}, argmax {}",
        report.warmed_ms,
        arg(&scores)
    );
}
