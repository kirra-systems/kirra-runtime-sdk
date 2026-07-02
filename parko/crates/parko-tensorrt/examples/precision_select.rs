//! Q-2 PRECISION SELECTION DEMO — walk the evidence-derived precision ladder
//! against the real TensorRT backend (`parko_core::precision`; design note §5).
//!
//! Reads `KIRRA_PRECISION_LADDER` (e.g. `int8,fp32`; default `fp32` — engaging a
//! reduced-precision artifact must be a deliberate, evidence-backed act), then
//! walks the ladder with the FULL operational proof per rung: construct
//! `TrtBackend` at that precision, introspect the artifact, and run the
//! fail-closed `warm_up` (engine build). The first rung that proves out wins;
//! rejected rungs are printed (a degradation is never silent).
//!
//! EVIDENCE NOTE (Q1B_ORIN.md "Measured results"): on the Orin NX, INT8 measured
//! *slower* than FP32 for this planner scorer, so the evidence-derived ladder for
//! THIS deployment is plain `fp32`. Running with `KIRRA_PRECISION_LADDER=int8,fp32`
//! demonstrates the mechanism (int8 builds and wins the walk) — it is a mechanism
//! demo, not the recommended config for this model.
//!
//! GATING: prints SKIPPED and exits 0 where the TensorRT EP is unavailable;
//! `PARKO_TRT_REQUIRE_EP=1` makes that a nonzero exit (the Orin strict lane).
//!
//! Run on the Orin:
//!   ORT_DYLIB_PATH=<...>/libonnxruntime.so KIRRA_PRECISION_LADDER=int8,fp32 \
//!     PARKO_TRT_REQUIRE_EP=1 cargo run -p parko-tensorrt --example precision_select

use parko_core::backend::{InferenceBackend, PrecisionMode};
use parko_core::precision::{select_by_ladder, PrecisionLadder};
use parko_tensorrt::{TrtBackend, TrtConfig, TrtPrecision};

fn require_ep() -> bool {
    std::env::var("PARKO_TRT_REQUIRE_EP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn skip_or_refuse(reason: &str) -> ! {
    if require_ep() {
        eprintln!("REFUSED (PARKO_TRT_REQUIRE_EP): {reason}");
        std::process::exit(1);
    }
    println!("SKIPPED: {reason}");
    std::process::exit(0);
}

fn main() {
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).exists() {
        skip_or_refuse(&format!("no loadable ORT runtime at ORT_DYLIB_PATH ({dylib:?})"));
    }

    let artifacts = std::env::var("KIRRA_DOER_ARTIFACTS").unwrap_or_else(|_| {
        format!("{}/../../../artifacts/doer-eval", env!("CARGO_MANIFEST_DIR"))
    });

    // A malformed ladder is a MISCONFIGURATION → a controlled nonzero-exit
    // refusal, unconditionally (this is not the hardware skip lane — a config
    // error must never read as "skipped").
    let ladder = match PrecisionLadder::from_env() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("REFUSED: invalid KIRRA_PRECISION_LADDER: {e}");
            std::process::exit(1);
        }
    };
    if ladder.fp32_anchored() {
        println!(
            "note: fp32 was absent from the configured ladder — appended as the \
             availability anchor (visible, per design note §5)"
        );
    }
    println!("ladder: {:?}", ladder.rungs());

    // The per-rung operational proof: artifact + posture for the precision,
    // construct, introspect, fail-closed warm-up (engine build). Only rungs the
    // Q-1 pipeline produced artifacts for are constructible; FP16 shares the
    // fp32 artifact (precision is an engine posture, not a different file).
    let selection = select_by_ladder(&ladder, |rung| {
        let (model_path, trt_precision) = match rung {
            PrecisionMode::FP32 => (format!("{artifacts}/planner_fp32.onnx"), TrtPrecision::Full),
            PrecisionMode::FP16 => (format!("{artifacts}/planner_fp32.onnx"), TrtPrecision::Fp16),
            PrecisionMode::INT8 => {
                (format!("{artifacts}/planner_int8_qdq.onnx"), TrtPrecision::Int8Qdq)
            }
            // PrecisionMode is #[non_exhaustive]; a future mode with no artifact
            // mapping here must fail the rung, not the process.
            other => {
                return Err(parko_core::backend::BackendError::InitializationError(
                    format!("no artifact mapping for precision {other:?}"),
                ))
            }
        };
        // Distinct engine cache per precision — engines are precision-specific.
        let cache = std::env::temp_dir().join(format!("kirra_precision_select_{rung:?}"));
        let cfg = TrtConfig { engine_cache_path: cache.to_string_lossy().into_owned() };
        let backend = TrtBackend::with_precision(&model_path, &cfg, trt_precision)?;
        let model = backend.load_model(&model_path)?;
        backend.warm_up(&model)?; // fail-closed engine build = the proof
        Ok((backend, model))
    });

    match selection {
        Ok(sel) => {
            for (rung, err) in &sel.rejected {
                println!("rejected {rung:?}: {err}");
            }
            let (backend, model) = &sel.backend;
            println!(
                "SELECTED {:?} (degraded={}) — model {}, engine_sha={}",
                sel.precision,
                sel.degraded(),
                model.model_id,
                backend.engine_sha().unwrap_or("-"),
            );
        }
        Err(e) => skip_or_refuse(&format!("ladder exhausted: {e}")),
    }
}
