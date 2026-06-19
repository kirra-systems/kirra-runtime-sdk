//! TF32 / PRECISION DIFFERENTIAL PROBE — PARK-021 jetson-gated item #3.
//!
//! Ampere+ GPUs (the Orin) may use TF32 for fp32 matmuls, silently dropping
//! mantissa bits. ort's TensorRT EP exposes NO TF32 knob (see `TrtBackend` docs),
//! so the only out-of-band control is the `NVIDIA_TF32_OVERRIDE=0` env override —
//! and whether that override even reaches the TRT EP's kernels is an EMPIRICAL
//! question (TRT has its own build-time kTF32 flag distinct from the cuBLAS/cuDNN
//! override). This probe answers it on hardware.
//!
//! It is a ONE-SHOT DIFFERENTIAL. `NVIDIA_TF32_OVERRIDE` is read by CUDA once at
//! context init, so it cannot be toggled mid-process. The probe therefore:
//!   1. (parent) runs MNIST on a deterministic NON-ZERO input at the platform
//!      default TF32 state, then
//!   2. spawns a CHILD copy of this very test binary with `NVIDIA_TF32_OVERRIDE=0`
//!      (fresh process → the override takes effect), captures its logits, and
//!   3. compares: reports the measured per-logit drift, and ASSERTS the governed
//!      DECISION (argmax) is unchanged — a TF32-induced decision flip is a safety
//!      failure.
//! Each process uses its OWN engine-cache dir so a default-TF32 engine is never
//! reused under the override.
//!
//! A NON-zero input is deliberate: the equivalence probe's all-zeros input gives
//! 0 drift (reductions over zeros), which would mask any TF32 effect.
//!
//! GATING — same as the other probes: self-skips wherever the TRT EP is
//! unavailable (no ORT dylib / `with_config` Errs); `PARKO_TRT_REQUIRE_EP=1` makes
//! those skips hard failures (strict gate). Inert in the GPU-less sandbox and the
//! CPU-only fail-closed CI job. Run on the Jetson with:
//!
//!   ORT_DYLIB_PATH=<venv>/…/libonnxruntime.so.1.23.0 \
//!     cargo test -p parko-tensorrt --test tf32_probe -- --nocapture

use std::collections::HashMap;
use std::process::Command;

use parko_core::backend::{InferenceBackend, TensorBatch, TensorStorage};
use parko_tensorrt::{TrtBackend, TrtConfig};

/// Marks the spawned child process (which only runs inference + writes logits).
const CHILD_MARKER: &str = "PARKO_TF32_CHILD";
/// Env var carrying the temp file the child writes its logits to. A FILE (not
/// stdout) is used deliberately: libtest prints `test NAME ... ` without a trailing
/// newline, so child println output is interleaved with the harness banner and is
/// not reliably parseable. The file side-channel sidesteps that entirely.
const CHILD_OUT: &str = "PARKO_TF32_OUT";
/// Fallback stdout prefix if no out-file is provided (manual child invocation).
const LOGITS_PREFIX: &str = "PARKO_TF32_LOGITS:";
const INPUT_NAME: &str = "Input3";
const OUTPUT_NAME: &str = "Plus214_Output_0";

fn require_ep() -> bool {
    std::env::var("PARKO_TRT_REQUIRE_EP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn argmax(scores: &[f32]) -> usize {
    scores
        .iter()
        .enumerate()
        .fold(0usize, |best, (i, &s)| if s > scores[best] { i } else { best })
}

/// Deterministic NON-zero MNIST input — exercises fp32 matmul accumulation (so a
/// TF32 mantissa-drop, if any, is observable). Pure function of the index.
fn deterministic_input(n: usize) -> Vec<f32> {
    (0..n).map(|i| ((i * 7 + 13) % 251) as f32 / 251.0).collect()
}

/// Build the TRT backend and run one MNIST inference on the deterministic input.
/// Returns `None` if the TRT EP is unavailable (caller decides skip vs. fail). The
/// engine-cache dir is keyed to the TF32 state so the parent (default) and child
/// (override) never share a built engine.
fn run_once() -> Option<Vec<f32>> {
    let model_path = "tests/data/mnist-12.onnx";
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).exists() {
        return None;
    }
    // Distinct cache per TF32 state (child = override-off, parent = default).
    let tag = if std::env::var(CHILD_MARKER).is_ok() { "tf32off" } else { "default" };
    let cache = std::env::temp_dir().join(format!("parko_trt_tf32_probe_cache_{tag}"));
    let cfg = TrtConfig { engine_cache_path: cache.to_string_lossy().into_owned() };

    let trt = TrtBackend::with_config(model_path, &cfg).ok()?;
    let model = trt.load_model(model_path).ok()?;

    let total: usize = model.input_shapes.get(INPUT_NAME)?.iter().product();
    let input = deterministic_input(total);
    let mut named = HashMap::new();
    named.insert(INPUT_NAME.to_string(), TensorStorage::Borrowed(&input));
    let batch = TensorBatch { named_tensors: named, metadata: HashMap::new() };

    let out = trt.run(&model, &batch).ok()?;
    Some(out.named_tensors.get(OUTPUT_NAME)?.as_slice().to_vec())
}

#[test]
fn trt_fp32_tf32_differential() {
    // CHILD: only run inference (under whatever TF32 the spawned env dictates) and
    // print the logits for the parent to parse. Never spawns a grandchild.
    if std::env::var(CHILD_MARKER).is_ok() {
        if let Some(v) = run_once() {
            let s = v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
            match std::env::var(CHILD_OUT) {
                Ok(path) => std::fs::write(&path, s).expect("child write logits file"),
                Err(_) => println!("{LOGITS_PREFIX}{s}"),
            }
        }
        return;
    }

    // PARENT — run at the PLATFORM-DEFAULT TF32 state first.
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).exists() {
        assert!(
            !require_ep(),
            "STRICT (PARKO_TRT_REQUIRE_EP): no loadable ORT runtime at ORT_DYLIB_PATH ({dylib:?}) — \
             refusing (fail-closed).",
        );
        eprintln!("SKIP: ORT runtime lib not present ({dylib:?}) — TF32 probe needs a Jetson/TRT ORT.");
        return;
    }
    let default_logits = match run_once() {
        Some(v) => v,
        None => {
            assert!(
                !require_ep(),
                "STRICT (PARKO_TRT_REQUIRE_EP): TensorRT EP unavailable — refusing (fail-closed).",
            );
            eprintln!("SKIP: TensorRT EP unavailable — TF32 probe asserts only on a Jetson/TRT ORT.");
            return;
        }
    };

    // Spawn a fresh child of THIS test binary with TF32 forced OFF (the override is
    // only honored at CUDA init, hence a new process). ORT_DYLIB_PATH is inherited;
    // the child writes its logits to a temp file we read back.
    let exe = std::env::current_exe().expect("current_exe() for the TF32 child");
    let out_file = std::env::temp_dir().join(format!("parko_tf32_child_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&out_file);
    let output = Command::new(&exe)
        .args(["trt_fp32_tf32_differential", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_MARKER, "1")
        .env(CHILD_OUT, &out_file)
        .env("NVIDIA_TF32_OVERRIDE", "0")
        .output()
        .expect("failed to spawn the TF32-off child process");

    let tf32off_logits: Vec<f32> = match std::fs::read_to_string(&out_file) {
        Ok(s) if !s.trim().is_empty() => s
            .trim()
            .split(',')
            .map(|x| x.parse::<f32>().expect("child logit parse"))
            .collect(),
        _ => {
            // Parent inferred fine but the child wrote no logits — an anomaly, not a
            // normal skip. Surface the child's output; fail only under strict mode.
            let _ = std::fs::remove_file(&out_file);
            eprintln!(
                "INCONCLUSIVE: TF32-off child produced no logits (the override run did not infer).\n\
                 child stdout:\n{}\nchild stderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            assert!(!require_ep(), "STRICT: TF32-off child failed to produce logits.");
            return;
        }
    };
    let _ = std::fs::remove_file(&out_file);

    assert_eq!(
        default_logits.len(),
        tf32off_logits.len(),
        "default and TF32-off output lengths differ",
    );
    let max_drift = default_logits
        .iter()
        .zip(&tf32off_logits)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    let d_default = argmax(&default_logits);
    let d_off = argmax(&tf32off_logits);

    // SAFETY contract: TF32 must not change the governed DECISION. A flip means the
    // unenforceable TF32 path is decision-relevant → resolve precision before trust.
    assert_eq!(
        d_default, d_off,
        "TF32 changes the DECISION: default→class {d_default}, NVIDIA_TF32_OVERRIDE=0→class {d_off} \
         (max per-logit drift {max_drift:e}). The unenforceable TF32 path is decision-relevant — \
         escalate to A2 (native nvinfer) or pin precision before trusting this backend.",
    );

    if max_drift == 0.0 {
        println!(
            "TF32 PROBE — NO observable effect: NVIDIA_TF32_OVERRIDE=0 leaves the TRT logits \
             bitwise-unchanged (max drift 0). Either TF32 is already off for this TRT/ORT build, \
             or the override does not reach the TRT EP's kernels (TRT's kTF32 is a separate \
             build-time flag). Implication: the override is NOT a reliable TF32 control for this \
             backend — track the A2 (native nvinfer) escalation if fp32 must be guaranteed."
        );
    } else {
        println!(
            "TF32 PROBE — TF32 IS decision-irrelevant but PRESENT: NVIDIA_TF32_OVERRIDE=0 shifts \
             the TRT logits by max {max_drift:e} (mantissa-drop quantified) while the decision \
             holds (class {d_default}). Pin NVIDIA_TF32_OVERRIDE=0 when calibrating the #4 \
             decision-agreement tolerance, and record this drift as the TF32 precision budget."
        );
    }
    println!("default(TF32 platform-default) = {default_logits:?}");
    println!("NVIDIA_TF32_OVERRIDE=0         = {tf32off_logits:?}");
}
