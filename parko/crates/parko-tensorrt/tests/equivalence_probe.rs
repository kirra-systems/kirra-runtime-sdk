//! DECISION-AGREEMENT PROBE — PARK-021 jetson-gated item #4 (cross-backend
//! equivalence). The positive probe (`positive_probe.rs`) proves the TensorRT
//! backend LOADS and RUNS on real silicon; this probe asks the safety-relevant
//! follow-up: does it AGREE with the CPU baseline?
//!
//! GPU TensorRT is NOT bitwise-identical to single-thread CPU ORT (different
//! kernels, fusion, accumulation order — and TF32 is unenforceable from the EP, see
//! `TrtBackend` docs). So the contract is NOT bitwise logits. It is **decision
//! agreement**: the governed DECISION (here, MNIST argmax) must match the CPU
//! reference, and the raw per-logit drift is MEASURED and reported (indicative —
//! the production bound is a hardware-measured decision-agreement tolerance on the
//! governed command, not on logits).
//!
//! GATING — identical to `positive_probe.rs`: self-SKIPS wherever the TensorRT EP
//! is unavailable (no ORT dylib, or `with_config` Errs on a CPU-only ORT), so it is
//! inert in the GPU-less sandbox and the CPU-only fail-closed CI job. Both backends
//! dlopen the SAME ORT runtime (`ORT_DYLIB_PATH`): TRT EP for the candidate, CPU EP
//! for the reference. `PARKO_TRT_REQUIRE_EP=1` turns the skips into hard failures
//! for use as a strict installer/CI gate. Run on hardware:
//!
//!   ORT_DYLIB_PATH=<venv>/…/libonnxruntime.so.1.23.0 \
//!     cargo test -p parko-tensorrt --test equivalence_probe -- --nocapture

use std::collections::HashMap;

use parko_core::backend::{BackendDescriptor, InferenceBackend, TensorBatch, TensorStorage};
use parko_onnx::OrtBackend;
use parko_tensorrt::{TrtBackend, TrtConfig};

/// `PARKO_TRT_REQUIRE_EP` truthy → the TensorRT EP is REQUIRED: a would-be skip
/// becomes a hard failure (strict gate). Unset everywhere else (self-skip).
fn require_ep() -> bool {
    std::env::var("PARKO_TRT_REQUIRE_EP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// argmax over a logit slice — the "decision" the governor would act on.
fn argmax(scores: &[f32]) -> usize {
    scores
        .iter()
        .enumerate()
        .fold(0usize, |best, (i, &s)| if s > scores[best] { i } else { best })
}

/// On a TensorRT-enabled ORT runtime, the TRT backend's MNIST DECISION (argmax)
/// must match the CPU baseline, and the measured per-logit drift is reported.
/// Self-skips where the TRT EP is unavailable.
#[test]
fn trt_decision_agrees_with_cpu_baseline() {
    let model_path = "tests/data/mnist-12.onnx";

    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).exists() {
        assert!(
            !require_ep(),
            "STRICT (PARKO_TRT_REQUIRE_EP): no loadable ORT runtime at ORT_DYLIB_PATH ({dylib:?}) — \
             refusing (fail-closed).",
        );
        eprintln!(
            "SKIP: ORT runtime lib not present ({dylib:?}) — the equivalence probe needs a \
             TensorRT-enabled ORT lib (Jetson / GPU job only)."
        );
        return;
    }

    let cache = std::env::temp_dir().join("parko_trt_equivalence_probe_cache");
    let cfg = TrtConfig { engine_cache_path: cache.to_string_lossy().into_owned() };

    // Candidate: the TensorRT backend. If the TRT EP isn't available this Errs —
    // the fail-closed path proven by fail_closed.rs — so SKIP (or fail, if strict).
    let trt = match TrtBackend::with_config(model_path, &cfg) {
        Ok(b) => b,
        Err(e) => {
            assert!(
                !require_ep(),
                "STRICT (PARKO_TRT_REQUIRE_EP): TensorRT EP unavailable ({e:?}) — refusing \
                 (fail-closed).",
            );
            eprintln!(
                "SKIP: TensorRT EP unavailable ({e:?}) — no usable TRT provider (expected on a \
                 CPU-only ORT / non-GPU box). The equivalence probe asserts only on a Jetson/TRT ORT."
            );
            return;
        }
    };
    assert_eq!(
        trt.descriptor(),
        BackendDescriptor::TensorRT,
        "the candidate must be the TensorRT backend",
    );

    // Reference: the single-thread CPU ORT baseline (same dlopened runtime, CPU EP).
    let cpu = OrtBackend::new(model_path).expect("failed to construct the CPU ORT baseline backend");

    let model_trt = trt.load_model(model_path).expect("TRT model introspection failed");
    let model_cpu = cpu.load_model(model_path).expect("CPU model introspection failed");

    let input_name = "Input3";
    let output_name = "Plus214_Output_0";
    let total: usize = model_cpu
        .input_shapes
        .get(input_name)
        .expect("MNIST input node 'Input3' not found")
        .iter()
        .product();

    // Same fixed input through both backends (Parko's sensor mappings emit fixed
    // shapes; a zero image is deterministic and its argmax is well-separated).
    let flat = vec![0.0f32; total];
    let mut named = HashMap::new();
    named.insert(input_name.to_string(), TensorStorage::Borrowed(&flat));
    let batch = TensorBatch { named_tensors: named, metadata: HashMap::new() };

    let out_trt = trt.run(&model_trt, &batch).expect("TensorRT run() failed");
    let out_cpu = cpu.run(&model_cpu, &batch).expect("CPU baseline run() failed");

    let a = out_trt.named_tensors.get(output_name).expect("missing TRT output").as_slice();
    let b = out_cpu.named_tensors.get(output_name).expect("missing CPU output").as_slice();
    assert_eq!(a.len(), b.len(), "output length must match across backends");
    assert_eq!(a.len(), 10, "expected 10-class MNIST output");
    for (i, s) in a.iter().enumerate() {
        assert!(s.is_finite(), "non-finite TensorRT score at class {i}: {s}");
    }

    // MEASURED drift (indicative, NOT bitwise — reported, not gated on a logit bound).
    let max_drift = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);

    // The CONTRACT: the governed DECISION agrees with the CPU reference.
    let (d_trt, d_cpu) = (argmax(a), argmax(b));
    assert_eq!(
        d_trt, d_cpu,
        "decision-agreement FAILED — TensorRT argmax (class {d_trt}) != CPU baseline (class {d_cpu}); \
         max per-logit drift {max_drift:e}. TRT={a:?} CPU={b:?}",
    );

    println!(
        "DECISION-AGREEMENT PROBE PASSED — TRT and CPU agree on class {d_trt}; \
         measured max per-logit drift {max_drift:e} (indicative, GPU≠bitwise). TRT={a:?} CPU={b:?}"
    );
}
