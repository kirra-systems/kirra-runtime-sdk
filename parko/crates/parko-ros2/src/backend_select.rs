// parko/crates/parko-ros2/src/backend_select.rs
//
// Compile-time backend selection for the Parko node (PARK-021 #2 / ADR-0010).
//
// EXPLICIT, FAIL-CLOSED, NO SILENT SUBSTITUTION — mirrors the installer's
// `--target` ethos (`scripts/install-parko-backend.sh`) and
// `parko-core::backend_selector`'s "selection is explicit" rule.
//
// TWO explicit gates, both must agree:
//   1. COMPILE-TIME (authoritative): exactly ONE concrete backend compiles in,
//      chosen by Cargo feature —
//        (no backend feature) → MockBackend     (development only)
//        `onnx-backend`        → parko-onnx OrtBackend (CPU)
//        `tensorrt-backend`    → parko-tensorrt TrtBackend (Jetson)
//      TensorRT takes precedence when both backend features are on.
//   2. RUNTIME (operator declaration): if `PARKO_BACKEND` is set it MUST name the
//      compiled-in backend (`mock` / `onnx` / `tensorrt`), else the node refuses to
//      start. This is a fail-closed cross-check — it can never *switch* the backend
//      (only one is compiled in); it catches "deployed the wrong binary".
//
// A real backend whose runtime/EP is unavailable returns `Err` — the node then
// REFUSES to start rather than fall back to another backend.
//
// DELIBERATELY NOT `#[cfg(feature = "ros2")]`: this module compiles (and is
// verified) without a ROS 2 distro, e.g. `cargo build -p parko-ros2
// --features tensorrt-backend`. The ROS 2 node binary merely calls it.

use std::sync::Arc;

use parko_core::backend::BackendError;

/// The concrete backend type compiled into this build.
#[cfg(feature = "tensorrt-backend")]
pub type SelectedBackend = parko_tensorrt::TrtBackend;
#[cfg(all(feature = "onnx-backend", not(feature = "tensorrt-backend")))]
pub type SelectedBackend = parko_onnx::OrtBackend;
#[cfg(not(any(feature = "tensorrt-backend", feature = "onnx-backend")))]
pub type SelectedBackend = parko_core::backends::mock::MockBackend;

/// Human-readable label for the compiled-in backend (logged at startup).
#[cfg(feature = "tensorrt-backend")]
pub const SELECTED_BACKEND: &str = "tensorrt (parko-tensorrt)";
#[cfg(all(feature = "onnx-backend", not(feature = "tensorrt-backend")))]
pub const SELECTED_BACKEND: &str = "onnx-cpu (parko-onnx)";
#[cfg(not(any(feature = "tensorrt-backend", feature = "onnx-backend")))]
pub const SELECTED_BACKEND: &str = "mock (development-only)";

/// The `PARKO_BACKEND` token naming the compiled-in backend (the runtime
/// cross-check below requires an equal, case-insensitive value when set).
#[cfg(feature = "tensorrt-backend")]
pub const SELECTED_BACKEND_TOKEN: &str = "tensorrt";
#[cfg(all(feature = "onnx-backend", not(feature = "tensorrt-backend")))]
pub const SELECTED_BACKEND_TOKEN: &str = "onnx";
#[cfg(not(any(feature = "tensorrt-backend", feature = "onnx-backend")))]
pub const SELECTED_BACKEND_TOKEN: &str = "mock";

/// Fail-closed cross-check of the operator's `PARKO_BACKEND` declaration against
/// the compiled-in backend. Unset/empty → the compile-time gate is authoritative
/// (Ok). Set but mismatched → `Err` (refuse: wrong binary; no runtime switch).
pub fn verify_backend_env() -> Result<(), BackendError> {
    check_backend_declaration(std::env::var("PARKO_BACKEND").ok().as_deref())
}

/// Pure core of [`verify_backend_env`] — takes the declared value so it is
/// testable without mutating process env (invariant #13: no `set_var`).
fn check_backend_declaration(declared: Option<&str>) -> Result<(), BackendError> {
    match declared {
        Some(v) if !v.trim().is_empty() => {
            let want = v.trim().to_ascii_lowercase();
            if want != SELECTED_BACKEND_TOKEN {
                return Err(BackendError::InitializationError(format!(
                    "PARKO_BACKEND={want:?} but this binary was built with backend \
                     {SELECTED_BACKEND_TOKEN:?} ({SELECTED_BACKEND}) — refusing (fail-closed; \
                     rebuild with the matching --features, never a runtime substitution)"
                )));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// A production backend build must be given a real model path, not the dev
/// sentinel — reject it fail-closed rather than try to load `mock://…`.
#[cfg(any(feature = "tensorrt-backend", feature = "onnx-backend"))]
fn require_real_model_path(model_path: &str) -> Result<(), BackendError> {
    if model_path.is_empty() || model_path.starts_with("mock://") {
        return Err(BackendError::InitializationError(format!(
            "a production backend build ({SELECTED_BACKEND}) requires PARKO_MODEL_PATH to point at a \
             real model — got {model_path:?}"
        )));
    }
    Ok(())
}

/// Construct the compiled-in backend behind an `Arc`. FAIL-CLOSED on both gates:
/// the `PARKO_BACKEND` cross-check and the backend's own runtime/EP availability.
/// The caller must refuse to start on `Err` (never substitute another backend).
/// `warm_up` (the engine build for TensorRT) runs later, in the node's
/// `build_loop`, via the `InferenceBackend` trait hook.
pub fn select_backend(model_path: &str) -> Result<Arc<SelectedBackend>, BackendError> {
    verify_backend_env()?;
    construct_backend(model_path)
}

#[cfg(feature = "tensorrt-backend")]
fn construct_backend(model_path: &str) -> Result<Arc<SelectedBackend>, BackendError> {
    require_real_model_path(model_path)?;
    Ok(Arc::new(parko_tensorrt::TrtBackend::new(model_path)?))
}

#[cfg(all(feature = "onnx-backend", not(feature = "tensorrt-backend")))]
fn construct_backend(model_path: &str) -> Result<Arc<SelectedBackend>, BackendError> {
    require_real_model_path(model_path)?;
    Ok(Arc::new(parko_onnx::OrtBackend::new(model_path)?))
}

#[cfg(not(any(feature = "tensorrt-backend", feature = "onnx-backend")))]
fn construct_backend(_model_path: &str) -> Result<Arc<SelectedBackend>, BackendError> {
    use std::collections::HashMap;

    use parko_core::backend::BackendDescriptor;
    use parko_core::backends::mock::MockBackend;

    tracing::warn!(
        "parko-ros2: using MockBackend (no backend feature compiled in). DEVELOPMENT-ONLY — emits a \
         fixed zero command. Rebuild with --features ros2,onnx-backend (or tensorrt-backend) + set \
         PARKO_MODEL_PATH for a production backend."
    );
    let mut outputs: HashMap<String, Vec<f32>> = HashMap::new();
    outputs.insert("cmd_vel_linear".to_string(), vec![0.0]);
    outputs.insert("cmd_vel_angular".to_string(), vec![0.0]);
    Ok(Arc::new(MockBackend::new(outputs, BackendDescriptor::Cpu)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parko_backend_unset_or_empty_is_ok() {
        assert!(check_backend_declaration(None).is_ok());
        assert!(check_backend_declaration(Some("")).is_ok());
        assert!(check_backend_declaration(Some("   ")).is_ok());
    }

    #[test]
    fn parko_backend_matching_token_is_ok_case_insensitive() {
        let upper = SELECTED_BACKEND_TOKEN.to_uppercase();
        assert!(check_backend_declaration(Some(&upper)).is_ok());
        assert!(check_backend_declaration(Some(&format!("  {SELECTED_BACKEND_TOKEN}  "))).is_ok());
    }

    #[test]
    fn parko_backend_mismatch_is_fail_closed() {
        // Never the compiled-in token regardless of feature set.
        assert!(check_backend_declaration(Some("definitely-not-a-real-backend")).is_err());
    }
}
