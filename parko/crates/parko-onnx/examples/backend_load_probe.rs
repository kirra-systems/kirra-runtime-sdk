//! #44 — fail-closed ORT-CPU backend-load probe for the production image.
//!
//! FLAGGED FOR REVIEW: this is the one small code addition the #44 task allows
//! ("No src/ changes except (if unavoidable) a tiny load-probe; flag it"). A
//! slim runtime image carries no `cargo`/source, so the existing
//! `tests/test_onnx_backend.rs` cannot run there; this example reuses the SAME
//! load path (`OrtBackend::new` + `load_model`) as a standalone probe binary the
//! container entrypoint runs before exec'ing the node.
//!
//! Contract: exit 0 ONLY if the ORT CPU runtime actually loaded the model; any
//! failure exits non-zero so the production entrypoint REFUSES to start (no
//! MockBackend fallback in the production image). This generalizes the
//! installer's `PARKO_BACKEND_PROBE` gate.
//!
//! Model path: `argv[1]` or `$PARKO_MODEL_PATH` (the image bakes a known-good
//! mnist sample as the default so the probe always has a model to load).

use parko_core::backend::InferenceBackend;
use parko_onnx::OrtBackend;
use std::process::ExitCode;

fn main() -> ExitCode {
    let model_path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("PARKO_MODEL_PATH").ok())
        .unwrap_or_default();
    if model_path.is_empty() {
        eprintln!("PARKO_BACKEND_PROBE: FAIL — no model path (argv[1] or PARKO_MODEL_PATH)");
        return ExitCode::FAILURE;
    }

    // OrtBackend::new dlopens libonnxruntime.so via ORT_DYLIB_PATH — this is the
    // load that fails closed if the ORT CPU runtime is missing/mismatched.
    let backend = match OrtBackend::new(model_path.as_str()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("PARKO_BACKEND_PROBE: FAIL — OrtBackend::new('{model_path}'): {e:?}");
            return ExitCode::FAILURE;
        }
    };

    match backend.load_model(model_path.as_str()) {
        Ok(_) => {
            println!(
                "PARKO_BACKEND_PROBE: OK — ORT CPU runtime loaded and model introspected ({model_path})"
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("PARKO_BACKEND_PROBE: FAIL — load_model('{model_path}'): {e:?}");
            ExitCode::FAILURE
        }
    }
}
