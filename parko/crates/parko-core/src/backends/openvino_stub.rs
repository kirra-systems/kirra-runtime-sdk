#![cfg(feature = "backend-openvino")]

//! PARK-029 — Intel OpenVINO backend (stub on this build host).
//!
//! Status on the build host where this stub compiles: **OpenVINO runtime
//! is NOT installed**. Until the runtime is on hand, `OpenVinoStubBackend`
//! provides:
//!
//! - A constructor `new(model_path, device) -> Result<Self, BackendError>`
//!   that returns `BackendError::InitializationError(...)` — explicit
//!   fail-closed so any caller that expected a working backend learns
//!   immediately, not at first inference.
//! - A no-op `InferenceBackend` impl returning empty tensors. This is the
//!   pre-PARK-029 CI shape; it lets the feature flag compile and the
//!   trait contract get exercised on hardware-less CI runners.
//! - A free-function `descriptor()` returning `BackendDescriptor::IntelOpenVino`
//!   that does not require constructing the backend — useful for capability
//!   reporting and feature-flag tests.
//!
//! When OpenVINO runtime lands on a build host, replace `new()` with a
//! real implementation backed by the `openvino` crate (binding the
//! OpenVINO C API). Target devices: `"CPU"`, `"GPU"` (iGPU), `"MYRIAD"`
//! (Neural Compute Stick 2 / VPU), `"AUTO"`. Validates Kirra's
//! vendor-neutral multi-silicon architecture (second silicon backend
//! after `parko-onnx`'s `OrtBackend`).

use std::collections::HashMap;

use crate::backend::{
    BackendCapabilities, BackendDescriptor, BackendError, InferenceBackend, ModelHandle,
    PrecisionMode, TensorBatch,
};

/// Backend descriptor for OpenVINO. Free function so callers can read the
/// descriptor without constructing a backend (which on this host always
/// returns an error from `OpenVinoStubBackend::new`).
pub fn descriptor() -> BackendDescriptor {
    BackendDescriptor::IntelOpenVino
}

/// Stub-on-this-host implementation of the OpenVINO backend.
pub struct OpenVinoStubBackend;

impl OpenVinoStubBackend {
    /// Attempt to construct an OpenVINO-backed inference engine.
    ///
    /// On a build host without the OpenVINO runtime installed, this returns
    /// `BackendError::InitializationError` with a message indicating where
    /// to install OpenVINO from. The error path is deliberately fail-closed:
    /// callers that hold an `OpenVinoStubBackend` value have, by construction,
    /// verified the runtime is present.
    ///
    /// `device` accepts the same string identifiers the OpenVINO C API uses:
    /// `"CPU"`, `"GPU"` (iGPU), `"MYRIAD"` (Neural Compute Stick 2 / VPU),
    /// `"AUTO"`. The argument is accepted but ignored by the stub.
    pub fn new(_model_path: &str, _device: &str) -> Result<Self, BackendError> {
        Err(BackendError::InitializationError(
            "OpenVINO runtime not installed on this build host. \
             Install from https://docs.openvino.ai/ and rebuild. \
             (PARK-029: stub on hosts without runtime)"
                .to_string(),
        ))
    }
}

impl InferenceBackend for OpenVinoStubBackend {
    fn load_model(&self, _path: &str) -> Result<ModelHandle, BackendError> {
        Ok(ModelHandle {
            model_id: "openvino-stub".to_string(),
            input_shapes: HashMap::new(),
            output_shapes: HashMap::new(),
            expected_precision: PrecisionMode::FP32,
        })
    }

    fn run(
        &self,
        _model: &ModelHandle,
        _inputs: &TensorBatch,
    ) -> Result<TensorBatch<'static>, BackendError> {
        Ok(TensorBatch {
            named_tensors: HashMap::new(),
            metadata: HashMap::new(),
        })
    }

    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor::IntelOpenVino
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendCapabilities, BackendDescriptor};

    #[test]
    fn test_openvino_stub_descriptor() {
        assert_eq!(
            OpenVinoStubBackend.descriptor(),
            BackendDescriptor::IntelOpenVino
        );
    }

    #[test]
    fn test_openvino_stub_capabilities_are_default() {
        assert_eq!(
            OpenVinoStubBackend.capabilities(),
            BackendCapabilities::default()
        );
    }

    /// Free-function descriptor() returns the right variant without needing
    /// a backend instance. Useful for the BackendSelector capability table.
    #[test]
    fn test_openvino_backend_descriptor_free_fn() {
        assert_eq!(descriptor(), BackendDescriptor::IntelOpenVino);
    }

    /// On a build host without the OpenVINO runtime, `new()` must return
    /// `BackendError::InitializationError` — never panic, never silently
    /// succeed. Future hosts with the runtime installed will replace this
    /// stub with a real impl whose `new()` may succeed.
    #[test]
    fn test_openvino_backend_unavailable_without_runtime() {
        let result = OpenVinoStubBackend::new("model.xml", "CPU");
        match result {
            Err(BackendError::InitializationError(msg)) => {
                assert!(
                    msg.contains("OpenVINO"),
                    "InitializationError should mention OpenVINO; got {msg:?}"
                );
            }
            Err(other) => {
                panic!("Expected InitializationError (runtime not installed), got: {other:?}")
            }
            Ok(_) => panic!(
                "Stub on a runtime-less host must not succeed. If OpenVINO is now \
                 installed on this host, replace the stub body with the real impl."
            ),
        }
    }
}
