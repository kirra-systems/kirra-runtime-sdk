#![cfg(feature = "backend-openvino")]

use std::collections::HashMap;

use crate::backend::{
    BackendCapabilities, BackendDescriptor, BackendError, InferenceBackend,
    ModelHandle, PrecisionMode, TensorBatch,
};

/// Zero-output stub for CI builds — no Intel OpenVINO hardware required.
/// Real implementation: PARK-029.
pub struct OpenVinoStubBackend;

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
        assert_eq!(OpenVinoStubBackend.descriptor(), BackendDescriptor::IntelOpenVino);
    }

    #[test]
    fn test_openvino_stub_capabilities_are_default() {
        assert_eq!(OpenVinoStubBackend.capabilities(), BackendCapabilities::default());
    }
}
