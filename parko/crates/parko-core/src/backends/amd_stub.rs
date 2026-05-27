#![cfg(feature = "backend-amd")]

use std::collections::HashMap;

use crate::backend::{
    BackendCapabilities, BackendDescriptor, BackendError, InferenceBackend,
    ModelHandle, PrecisionMode, TensorBatch,
};

/// Zero-output stub for CI builds — no AMD Vitis hardware required.
/// Real implementation: PARK-030.
pub struct AmdStubBackend;

impl InferenceBackend for AmdStubBackend {
    fn load_model(&self, _path: &str) -> Result<ModelHandle, BackendError> {
        Ok(ModelHandle {
            model_id: "amd-stub".to_string(),
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
        BackendDescriptor::AmdVitis
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
    fn test_amd_stub_descriptor() {
        assert_eq!(AmdStubBackend.descriptor(), BackendDescriptor::AmdVitis);
    }

    #[test]
    fn test_amd_stub_capabilities_are_default() {
        assert_eq!(AmdStubBackend.capabilities(), BackendCapabilities::default());
    }
}
