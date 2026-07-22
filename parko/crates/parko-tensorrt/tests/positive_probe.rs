//! POSITIVE PROBE — the on-hardware counterpart to `fail_closed.rs` (PARK-021 #6).
//!
//! `fail_closed.rs` proves the SAFETY property on a CPU-only ORT runtime: the
//! TensorRT EP can't register, so `with_config` must `Err` (no silent CPU run).
//! This probe proves the COMPLEMENT on real NVIDIA silicon: when the dlopened
//! ONNX Runtime genuinely carries a usable TensorRT provider (a TensorRT-enabled
//! JetPack/Jetson ORT build, e.g. onnxruntime-gpu 1.23.x on a Jetson Orin),
//! `with_config` must SUCCEED, report the `TensorRT` descriptor, and run real
//! inference. That is the unanswered half of PARK-021 jetson-gated item #6 —
//! "verify the JetPack ORT build actually carries the TensorRT EP so `with_config`
//! succeeds there".
//!
//! GATING (mirrors the `parko-onnx` CUDA-EP test idiom): this self-SKIPS — never
//! fails — anywhere the TensorRT EP is not available. So it is safe in exactly the
//! environments the fail-closed test targets:
//!   * the GPU-less sandbox / default workspace job (no ORT dylib)       → skip
//!   * the parko-tensorrt CI job's CPU-only ORT (`with_config` Errs)     → skip
//!
//! It only ASSERTS where a TensorRT-enabled ORT is present (the Jetson). The
//! existing CI fail-closed job runs `--test fail_closed` specifically, so adding
//! this file does not change CI behaviour; run it on hardware with:
//!
//!   ORT_DYLIB_PATH=<venv>/onnxruntime/capi/libonnxruntime.so.1.23.0 \
//!     cargo test -p parko-tensorrt --test positive_probe -- --nocapture
//!
//! STRICT MODE — `PARKO_TRT_REQUIRE_EP=1`: the two self-skip branches become hard
//! FAILURES instead. This is what makes the probe usable as a fail-closed installer
//! gate (`scripts/install-parko-backend.sh`): there, "TRT EP absent" must REFUSE
//! (nonzero exit), never pass quietly. Unset (the default) keeps the CI/sandbox-safe
//! self-skip, so this file stays inert in the GPU-less jobs.

use std::collections::HashMap;

use parko_core::backend::{BackendDescriptor, InferenceBackend, TensorBatch, TensorStorage};
use parko_tensorrt::{TrtBackend, TrtConfig};

/// `PARKO_TRT_REQUIRE_EP` truthy → the TensorRT EP is REQUIRED: a would-be skip
/// (no ORT lib, or EP unavailable) becomes a hard failure. Used by the installer's
/// fail-closed backend-load validation; unset everywhere else (self-skip).
fn require_ep() -> bool {
    std::env::var("PARKO_TRT_REQUIRE_EP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// On a TensorRT-enabled ORT runtime, `TrtBackend::with_config` must SUCCEED
/// (the TRT EP registers), report the `TensorRT` descriptor, and produce finite
/// MNIST outputs. Self-skips where the TRT EP is unavailable.
#[test]
fn trt_backend_loads_and_runs_when_tensorrt_ep_available() {
    let model_path = "tests/data/mnist-12.onnx";

    // No loadable ORT runtime at all → "no runtime", not the positive signal.
    // ort PANICS on a missing dylib, so require the FILE to exist before we try
    // (the same guard fail_closed.rs uses; `.cargo/config.toml` sets the env var
    // unconditionally, so the path — not the var — is the reliable signal).
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).exists() {
        assert!(
            !require_ep(),
            "STRICT (PARKO_TRT_REQUIRE_EP): no loadable ORT runtime at ORT_DYLIB_PATH ({dylib:?}) — \
             refusing (fail-closed). The acquire step must install a TensorRT-enabled ORT and export \
             ORT_DYLIB_PATH before validation.",
        );
        eprintln!(
            "SKIP: ORT runtime lib not present ({dylib:?}) — the positive probe needs a \
             TensorRT-enabled ORT lib (Jetson / GPU job only)."
        );
        return;
    }

    // Engine cache in a temp dir so the probe is self-contained and repeatable.
    let cache = std::env::temp_dir().join("parko_trt_positive_probe_cache");
    let cfg = TrtConfig {
        engine_cache_path: cache.to_string_lossy().into_owned(),
    };

    // Construct the TRT backend fail-closed. If the TensorRT EP isn't available
    // (CPU-only ORT / no GPU / no TRT provider) this returns Err — that is the
    // fail-closed path proven by fail_closed.rs, so here we SKIP rather than fail.
    let backend = match TrtBackend::with_config(model_path, &cfg) {
        Ok(b) => b,
        Err(e) => {
            assert!(
                !require_ep(),
                "STRICT (PARKO_TRT_REQUIRE_EP): TensorRT EP unavailable ({e:?}) — refusing \
                 (fail-closed). A loadable ORT runtime is present but carries no usable TensorRT \
                 provider; the selected backend did NOT load and must not be claimed valid.",
            );
            eprintln!(
                "SKIP: TensorRT EP unavailable ({e:?}) — this runtime has no usable TRT \
                 provider (expected on a CPU-only ORT / non-GPU box; the fail-closed test \
                 asserts this). The positive probe asserts only on a Jetson/TRT ORT."
            );
            return;
        }
    };

    // --- From here the TRT EP IS available: everything below MUST hold. ---

    assert_eq!(
        backend.descriptor(),
        BackendDescriptor::TensorRT,
        "a constructed TRT backend must report the TensorRT descriptor",
    );

    // The posture this backend logged at init is the audited full-precision anchor.
    let posture = backend.posture();
    assert!(
        !posture.fp16,
        "safety path runs full precision — fp16 must be off"
    );
    assert!(
        !posture.int8,
        "safety path runs full precision — int8 must be off"
    );

    let model = backend
        .load_model(model_path)
        .expect("failed to introspect MNIST model on the TensorRT backend");

    let input_name = "Input3";
    let output_name = "Plus214_Output_0";

    let input_shape = model
        .input_shapes
        .get(input_name)
        .expect("MNIST input node 'Input3' not found");
    assert_eq!(
        input_shape,
        &vec![1, 1, 28, 28],
        "MNIST input shape mismatch"
    );

    let total_elems: usize = input_shape.iter().product();
    let flat_image = vec![0.0f32; total_elems];

    let mut named = HashMap::new();
    named.insert(input_name.to_string(), TensorStorage::Borrowed(&flat_image));
    let batch = TensorBatch {
        named_tensors: named,
        metadata: HashMap::new(),
    };

    // First run on TRT may build+cache an engine (slow) before inferring; both
    // must complete without falling back or erroring.
    let output = backend
        .run(&model, &batch)
        .expect("TensorRT backend run() failed — EP registered but inference did not complete");

    let scores = output
        .named_tensors
        .get(output_name)
        .expect("missing MNIST output tensor")
        .as_slice();
    assert_eq!(scores.len(), 10, "expected 10-class MNIST output");
    for (i, s) in scores.iter().enumerate() {
        assert!(s.is_finite(), "non-finite TensorRT score at class {i}: {s}");
    }

    println!(
        "POSITIVE PROBE PASSED — TensorRT EP available; with_config Ok, descriptor=TensorRT, \
         MNIST inference produced 10 finite scores: {scores:?}"
    );
}
