#![cfg(feature = "backend-tidl")]

use std::collections::HashMap;

use crate::backend::{
    BackendCapabilities, BackendDescriptor, BackendError, InferenceBackend,
    ModelHandle, PrecisionMode, TensorBatch,
};

/// Zero-output stub for CI builds — no TI TIDL hardware required.
/// Real implementation: PARK-028.
pub struct TidlStubBackend;

impl InferenceBackend for TidlStubBackend {
    fn load_model(&self, _path: &str) -> Result<ModelHandle, BackendError> {
        Ok(ModelHandle {
            model_id: "tidl-stub".to_string(),
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
        BackendDescriptor::TiTidl
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
    fn test_tidl_stub_descriptor() {
        assert_eq!(TidlStubBackend.descriptor(), BackendDescriptor::TiTidl);
    }

    #[test]
    fn test_tidl_stub_capabilities_are_default() {
        assert_eq!(TidlStubBackend.capabilities(), BackendCapabilities::default());
    }
}
