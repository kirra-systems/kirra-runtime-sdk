use std::collections::HashMap;

use parko_core::backend::{BackendCapabilities, BackendDescriptor, InferenceBackend, TensorBatch, TensorStorage};
use parko_onnx::OrtBackend;

/// `PARKO_ONNX_REQUIRE_ORT` truthy → a loadable ORT runtime is REQUIRED: a
/// would-be skip becomes a hard failure. Set in the dedicated ORT-provisioned
/// CI job so these tests keep gating with teeth there; unset everywhere else
/// (self-skip). Mirrors `PARKO_TRT_REQUIRE_EP` / `KIRRA_DOER_EVAL_REQUIRE_ORT`.
fn require_ort() -> bool {
    std::env::var("PARKO_ONNX_REQUIRE_ORT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// `Some(())` iff a loadable ORT runtime is present (`ORT_DYLIB_PATH` names an
/// existing regular file — symlinks resolve); `None` → skip (or panic in strict
/// mode). ort PANICS on a
/// missing dylib — and since `ort = { default-features = false }` dropped the
/// build-time `download-binaries` provisioning, NO lane gets a dylib implicitly
/// any more. The guard matters beyond the dedicated job because cargo absorbs
/// path-depped parko crates as implicit ROOT-workspace members (a path dep under
/// the workspace directory is force-absorbed; `exclude` cannot prevent it), so
/// root `cargo test --workspace` runs THESE tests in lanes with no ORT installed.
fn ort_available() -> Option<()> {
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    // `is_file()` (not `exists()`): a directory here is a misconfiguration that
    // would sail past the guard straight into ort's panicking dlopen.
    if dylib.is_empty() || !std::path::Path::new(&dylib).is_file() {
        assert!(
            !require_ort(),
            "STRICT (PARKO_ONNX_REQUIRE_ORT): no loadable ORT runtime at ORT_DYLIB_PATH \
             ({dylib:?}) — refusing to skip (the ORT-provisioned job must install it)."
        );
        eprintln!("SKIP: no ORT runtime ({dylib:?}) — parko-onnx tests need a real ORT lib.");
        return None;
    }
    Some(())
}

/// #G16 — the model-integrity allow-list rejects a SUBSTITUTED model and accepts
/// the pinned one, exercised against the REAL mnist artifact. No ORT and no env
/// mutation: `verify_model_file` (which `OrtRunCore::load_model` now calls for both
/// the CPU and TensorRT backends) is driven directly with explicit policies.
#[test]
fn model_integrity_allowlist_gates_the_real_mnist_artifact() {
    use parko_core::model_integrity::{sha256_file, verify_model_file, ModelAllowList};
    let model_path = "tests/data/mnist-12.onnx";

    let real_digest = sha256_file(std::path::Path::new(model_path)).expect("hash mnist");

    // (a) Enforcing with a WRONG digest → the real model is rejected (fail-closed).
    let wrong = ModelAllowList::from_parts(["0".repeat(64)], false);
    let err = verify_model_file(model_path, &wrong).unwrap_err();
    assert!(
        matches!(err, parko_core::backend::BackendError::IntegrityRejected { .. }),
        "an unlisted (substituted) model must be rejected, got {err:?}"
    );

    // (b) Enforcing with the CORRECT digest → accepted and marked verified.
    let right = ModelAllowList::from_parts([real_digest.clone()], false);
    let v = verify_model_file(model_path, &right).expect("pinned model accepted");
    assert!(v.verified && v.sha256_hex == real_digest);

    // (c) No allow-list configured → accepted but unverified (byte-identical to
    //     the pre-#G16 behaviour; the digest is still computed for audit).
    let off = ModelAllowList::from_parts(Vec::<String>::new(), false);
    assert!(!verify_model_file(model_path, &off).unwrap().verified);
}

#[test]
fn mnist_end_to_end_inference() {
    if ort_available().is_none() {
        return;
    }
    let model_path = "tests/data/mnist-12.onnx";

    let backend = OrtBackend::new(model_path)
        .expect("failed to construct OrtBackend");

    let model = backend
        .load_model(model_path)
        .expect("failed to introspect MNIST model");

    let input_name = "Input3";
    let output_name = "Plus214_Output_0";

    let input_shape = model
        .input_shapes
        .get(input_name)
        .expect("MNIST input node 'Input3' not found");
    let output_shape = model
        .output_shapes
        .get(output_name)
        .expect("MNIST output node 'Plus214_Output_0' not found");

    assert_eq!(input_shape, &vec![1, 1, 28, 28], "MNIST input shape mismatch");
    assert_eq!(output_shape, &vec![1, 10], "MNIST output shape mismatch");

    let total_elems: usize = input_shape.iter().product();
    let flat_image = vec![0.0f32; total_elems];

    let mut named = HashMap::new();
    named.insert(
        input_name.to_string(),
        TensorStorage::Borrowed(&flat_image),
    );

    let batch = TensorBatch {
        named_tensors: named,
        metadata: HashMap::new(),
    };

    let output = backend
        .run(&model, &batch)
        .expect("OrtBackend run() failed");

    let storage = output
        .named_tensors
        .get(output_name)
        .expect("missing MNIST output tensor");

    let scores = storage.as_slice();
    assert_eq!(scores.len(), 10, "expected 10-class output");

    for (i, s) in scores.iter().enumerate() {
        assert!(s.is_finite(), "non-finite score at index {}: {}", i, s);
    }

    // Golden-output regression pin. Finiteness + shape prove the inference path
    // *runs*; they do NOT prove it computes the right thing — a transposed input,
    // a wrong-weights load, or an ABI/layout drift in a future ONNX Runtime can
    // still emit ten finite scores. With an all-zeros input the MNIST-12 graph is
    // deterministic (the output is the trailing-layer bias propagated through the
    // fixed weights), so the logits are a stable fingerprint of correct numerics.
    //
    // Captured from a known-green CI run (ORT 1.23.2, CPU EP — the exact pair this
    // job installs). The tolerance is wide enough to absorb last-ULP CPU/version
    // float drift yet far tighter than any real numerics regression: a genuine
    // layout/weights fault shifts these logits by O(0.1+), orders of magnitude
    // past 1e-2, while the values themselves sit in [-0.13, 0.14]. This pins ORT's
    // numerics INDEPENDENTLY of the parko-openvino cross-backend equivalence test
    // (which only catches an ORT/OV *divergence*), so an identical drift in both —
    // or a skipped OpenVINO job — can no longer pass silently here.
    const GOLDEN: [f32; 10] = [
        -0.044856027, 0.007791661, 0.06810082, 0.02999374, -0.12640963, 0.14021875,
        -0.055284902, -0.049383815, 0.08432205, -0.054540414,
    ];
    const TOL: f32 = 1e-2;
    for (i, (got, want)) in scores.iter().zip(GOLDEN.iter()).enumerate() {
        assert!(
            (got - want).abs() <= TOL,
            "MNIST logit regression at class {i}: got {got}, golden {want}, |Δ|>{TOL} \
             (an all-zeros-input numerics drift — suspect input layout / weights / ORT ABI)"
        );
    }

    println!("MNIST inference successful. Output: {:?}", scores);
}

#[test]
fn test_ort_backend_descriptor_is_cpu() {
    if ort_available().is_none() {
        return;
    }
    let model_path = "tests/data/mnist-12.onnx";
    let backend = OrtBackend::new(model_path).expect("failed to construct OrtBackend");
    assert_eq!(backend.descriptor(), BackendDescriptor::Cpu);
}

#[test]
fn test_ort_backend_capabilities() {
    if ort_available().is_none() {
        return;
    }
    let model_path = "tests/data/mnist-12.onnx";
    let backend = OrtBackend::new(model_path).expect("failed to construct OrtBackend");
    let caps = backend.capabilities();
    assert!(!caps.supports_int8, "CPU ONNX baseline does not support INT8");
    assert!(!caps.supports_fp16, "CPU ONNX baseline does not support FP16");
    assert_eq!(caps.max_batch_size, None, "CPU ONNX baseline has no batch-size limit");
    assert_eq!(
        caps,
        BackendCapabilities { supports_int8: false, supports_fp16: false, max_batch_size: None },
        "capabilities must match documented CPU ONNX baseline"
    );
}

// ---------------------------------------------------------------------------
// CUDA execution provider tests — behind the `cuda` feature
// ---------------------------------------------------------------------------
//
// These are NOT run by the default CI (it does not pass `--features cuda`). They
// run in a dedicated GPU CI job (`cargo test -p parko-onnx --features cuda` on
// NVIDIA silicon with a CUDA-enabled ONNX Runtime) — same gating spirit as the
// ORT-dylib / TensorRT GPU gating (#144). The cross-check self-skips (does not
// fail) when no CUDA provider/GPU is present, so `--features cuda` on a non-GPU
// box is clean.

#[cfg(feature = "cuda")]
mod cuda_ep {
    use super::*;
    use parko_onnx::CudaConfig;

    /// GPU-FREE: the default CUDA config is fail-closed (no silent CPU fallback),
    /// device 0. Runs anywhere `--features cuda` builds — constructs no session.
    #[test]
    fn cuda_config_default_is_fail_closed() {
        let cfg = CudaConfig::default();
        assert!(!cfg.allow_cpu_fallback,
            "CUDA must default to fail-closed — no silent CPU fallback");
        assert_eq!(cfg.device_id, 0, "default CUDA device is 0");
    }

    /// GPU-GATED: a constructed CUDA backend reports the `Cuda` descriptor and
    /// its MNIST output matches the CPU backend within tolerance. Self-skips
    /// (no failure) when CUDA is unavailable — `new_cuda` fail-closes there.
    #[test]
    fn cuda_descriptor_and_mnist_matches_cpu() {
        let model_path = "tests/data/mnist-12.onnx";

        // Construct the CUDA backend fail-closed. If CUDA isn't available
        // (no GPU / driver / CUDA-enabled ORT lib) this returns Err — skip.
        let cuda = match OrtBackend::new_cuda(model_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("SKIP: CUDA EP unavailable (no GPU / CUDA ORT provider) — {e:?}. \
                           This test runs only in the GPU CI job.");
                return;
            }
        };
        assert_eq!(cuda.descriptor(), BackendDescriptor::Cuda,
            "the CUDA backend must report the Cuda descriptor");

        let cpu = OrtBackend::new(model_path).expect("CPU backend");

        // Same MNIST input through both EPs.
        let model_cpu = cpu.load_model(model_path).expect("cpu load");
        let model_cuda = cuda.load_model(model_path).expect("cuda load");

        let input_name = "Input3";
        let output_name = "Plus214_Output_0";
        let total: usize = model_cpu.input_shapes.get(input_name).unwrap().iter().product();
        let img = vec![0.0f32; total];

        let batch = |img: &'static [f32]| {
            let mut m = HashMap::new();
            m.insert(input_name.to_string(), TensorStorage::Borrowed(img));
            TensorBatch { named_tensors: m, metadata: HashMap::new() }
        };
        // Leak a stable slice for the 'static Borrowed lifetime (test-only).
        let img: &'static [f32] = Box::leak(img.into_boxed_slice());

        let out_cpu = cpu.run(&model_cpu, &batch(img)).expect("cpu run");
        let out_cuda = cuda.run(&model_cuda, &batch(img)).expect("cuda run");

        let a = out_cpu.named_tensors.get(output_name).unwrap().as_slice();
        let b = out_cuda.named_tensors.get(output_name).unwrap().as_slice();
        assert_eq!(a.len(), b.len(), "output length must match across EPs");

        // GPU vs CPU is NOT bitwise identical — compare within tolerance.
        const TOL: f32 = 1e-3;
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert!(y.is_finite(), "non-finite CUDA score at {i}: {y}");
            assert!((x - y).abs() <= TOL,
                "CUDA vs CPU mismatch at class {i}: cpu={x}, cuda={y}, |Δ|>{TOL}");
        }
        println!("CUDA matches CPU within {TOL}. cpu={a:?} cuda={b:?}");
    }
}
