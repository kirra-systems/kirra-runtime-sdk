// parko/crates/parko-ros2/src/backend_select.rs
//
// Compile-time backend selection for the Parko node (PARK-021 #2 / ADR-0010).
//
// EXPLICIT, FAIL-CLOSED, NO SILENT SUBSTITUTION — mirrors the installer's
// `--target` ethos (`scripts/install-parko-backend.sh`) and
// `parko-core::backend_selector`'s "selection is explicit" rule. Exactly ONE
// concrete backend compiles in, chosen by Cargo feature:
//
//   (no backend feature)            → MockBackend     (development only)
//   `onnx-backend`                  → parko-onnx OrtBackend (CPU)
//   `tensorrt-backend`              → parko-tensorrt TrtBackend (Jetson)
//
// TensorRT takes precedence when both backend features are on. A real backend
// whose runtime/EP is unavailable returns `Err` — the node then REFUSES to start
// rather than fall back to another backend.
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

/// Construct the compiled-in backend behind an `Arc`. FAIL-CLOSED: returns `Err`
/// if a real backend's runtime/EP is unavailable; the caller must refuse to start
/// (never substitute another backend). `warm_up` (the engine build for TensorRT)
/// runs later, in the node's `build_loop`, via the `InferenceBackend` trait hook.
#[cfg(feature = "tensorrt-backend")]
pub fn select_backend(model_path: &str) -> Result<Arc<SelectedBackend>, BackendError> {
    require_real_model_path(model_path)?;
    Ok(Arc::new(parko_tensorrt::TrtBackend::new(model_path)?))
}

#[cfg(all(feature = "onnx-backend", not(feature = "tensorrt-backend")))]
pub fn select_backend(model_path: &str) -> Result<Arc<SelectedBackend>, BackendError> {
    require_real_model_path(model_path)?;
    Ok(Arc::new(parko_onnx::OrtBackend::new(model_path)?))
}

#[cfg(not(any(feature = "tensorrt-backend", feature = "onnx-backend")))]
pub fn select_backend(_model_path: &str) -> Result<Arc<SelectedBackend>, BackendError> {
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
