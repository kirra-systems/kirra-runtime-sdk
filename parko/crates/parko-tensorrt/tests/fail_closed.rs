//! FAIL-CLOSED — the safety-relevant test, and the reason parko-tensorrt has a
//! dedicated CI job. It REQUIRES an ONNX Runtime shared lib (ORT_DYLIB_PATH set,
//! same as the parko-onnx job). CI installs the Microsoft CPU-only build, which
//! has NO usable TensorRT provider. So registering the TRT EP (with
//! `error_on_failure`) must make `TrtBackend::with_config` return `Err` — proving
//! "no silent CPU fallback" on the very environment that lacks TensorRT.
//!
//! It does NOT run in the default workspace test job (that job excludes
//! parko-tensorrt because no ORT lib is installed there); it does NOT run in the
//! GPU-less sandbox. It runs in the parko-tensorrt CI job that installs the CPU
//! ORT runtime. See ci.yml.

use parko_core::backend::BackendError;
use parko_tensorrt::{TrtBackend, TrtConfig};

/// On a CPU-only ORT runtime, constructing the TRT backend must FAIL (the TRT EP
/// can't register and `error_on_failure` propagates it) — never a silent CPU run.
#[test]
fn trt_backend_fails_closed_when_tensorrt_ep_unavailable() {
    // Skip cleanly unless a loadable ORT runtime is actually present — the
    // assertion is meaningful only where libonnxruntime.so can be dlopened (ort
    // PANICS on a missing dylib, which is "no runtime", not the fail-closed
    // signal). NOTE: `.cargo/config.toml` sets ORT_DYLIB_PATH unconditionally, so
    // the env var alone is not a reliable signal — the FILE must exist. The CI
    // parko-tensorrt job installs the CPU-only ORT lib at this path, so it runs
    // the assertion; the GPU-less sandbox lacks the file and skips.
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).exists() {
        eprintln!("SKIP: ORT runtime lib not present ({dylib:?}) — fail-closed test needs a loadable ORT lib (CI parko-tensorrt job only)");
        return;
    }

    // Any path: construction must fail at EP registration, before model load.
    let cfg = TrtConfig {
        engine_cache_path: "/tmp/parko_trt_cache".to_string(),
    };
    let result = TrtBackend::with_config("tests/data/any_model.onnx", &cfg);

    let err = result.err().expect(
        "TrtBackend::with_config MUST fail on a CPU-only ORT runtime — \
         the TensorRT EP is unavailable and fail-closed must reject, never silently run CPU",
    );

    // Mapped to an InitializationError; the message names the fail-closed reason.
    match err {
        BackendError::InitializationError(msg) => {
            assert!(
                msg.contains("TensorRT")
                    || msg.contains("fail-closed")
                    || msg.contains("registration"),
                "error must identify the TensorRT EP fail-closed cause, got: {msg}"
            );
        }
        other => panic!("expected InitializationError (fail-closed), got: {other:?}"),
    }
}
