#![cfg(feature = "backend-qnn")]

use std::collections::HashMap;

use crate::backend::{
    BackendCapabilities, BackendDescriptor, BackendError, InferenceBackend,
    ModelHandle, PrecisionMode, TensorBatch,
};

/// Zero-output stub for CI builds — no Qualcomm QNN hardware required.
/// Real implementation: PARK-027.
pub struct QnnStubBackend;

impl InferenceBackend for QnnStubBackend {
    fn load_model(&self, _path: &str) -> Result<ModelHandle, BackendError> {
        Ok(ModelHandle {
            model_id: "qnn-stub".to_string(),
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
        BackendDescriptor::QualcommQnn
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
    fn test_qnn_stub_descriptor() {
        assert_eq!(QnnStubBackend.descriptor(), BackendDescriptor::QualcommQnn);
    }

    #[test]
    fn test_qnn_stub_capabilities_are_default() {
        assert_eq!(QnnStubBackend.capabilities(), BackendCapabilities::default());
    }
}
