use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::backend::{
    BackendDescriptor, BackendError, InferenceBackend, ModelHandle, PrecisionMode, TensorBatch,
    TensorStorage,
};

/// Deterministic, zero-dependency backend for parko-core tests.
///
/// Returns a fixed output on every `run()` call. Safe to share across threads
/// (`Send + Sync`) via atomic call counting and immutable output data.
#[derive(Debug)]
pub struct MockBackend {
    /// Data returned as `TensorStorage::Owned` on every `run()` call.
    output_data: HashMap<String, Vec<f32>>,
    /// Descriptor returned by `descriptor()`.
    descriptor: BackendDescriptor,
    /// Incremented atomically on each `run()` call.
    call_count: AtomicU64,
}

impl MockBackend {
    /// Creates a `MockBackend` that returns `output_data` on every `run()` call.
    pub fn new(output_data: HashMap<String, Vec<f32>>, descriptor: BackendDescriptor) -> Self {
        Self {
            output_data,
            descriptor,
            call_count: AtomicU64::new(0),
        }
    }

    /// How many times `run()` has been called.
    pub fn call_count(&self) -> u64 {
        self.call_count.load(Ordering::SeqCst)
    }
}

impl InferenceBackend for MockBackend {
    fn load_model(&self, path: &str) -> Result<ModelHandle, BackendError> {
        let output_shapes = self
            .output_data
            .iter()
            .map(|(name, data)| (name.clone(), vec![data.len()]))
            .collect();

        Ok(ModelHandle {
            model_id: format!("mock::{}", path),
            input_shapes: HashMap::new(),
            output_shapes,
            expected_precision: PrecisionMode::FP32,
        })
    }

    fn run(
        &self,
        _model: &ModelHandle,
        _inputs: &TensorBatch,
    ) -> Result<TensorBatch<'static>, BackendError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);

        let named_tensors = self
            .output_data
            .iter()
            .map(|(name, data)| (name.clone(), TensorStorage::Owned(data.clone())))
            .collect();

        Ok(TensorBatch {
            named_tensors,
            metadata: HashMap::new(),
        })
    }

    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendCapabilities;

    fn make_backend() -> MockBackend {
        let mut output_data = HashMap::new();
        output_data.insert("scores".to_string(), vec![1.0_f32, 2.0, 3.0]);
        MockBackend::new(output_data, BackendDescriptor::Cpu)
    }

    fn empty_inputs() -> TensorBatch<'static> {
        TensorBatch {
            named_tensors: HashMap::new(),
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn test_mock_backend_run_returns_configured_output() {
        let backend = make_backend();
        let model = backend.load_model("dummy").unwrap();
        let output = backend.run(&model, &empty_inputs()).unwrap();
        let scores = output.named_tensors.get("scores").unwrap();
        assert_eq!(scores.as_slice(), &[1.0_f32, 2.0, 3.0]);
    }

    #[test]
    fn test_mock_backend_run_is_repeatable() {
        let backend = make_backend();
        let model = backend.load_model("dummy").unwrap();
        let out1 = backend.run(&model, &empty_inputs()).unwrap();
        let out2 = backend.run(&model, &empty_inputs()).unwrap();
        assert_eq!(
            out1.named_tensors.get("scores").unwrap().as_slice(),
            out2.named_tensors.get("scores").unwrap().as_slice(),
        );
    }

    #[test]
    fn test_mock_backend_call_count_increments() {
        let backend = make_backend();
        let model = backend.load_model("dummy").unwrap();
        assert_eq!(backend.call_count(), 0);
        backend.run(&model, &empty_inputs()).unwrap();
        assert_eq!(backend.call_count(), 1);
        backend.run(&model, &empty_inputs()).unwrap();
        assert_eq!(backend.call_count(), 2);
    }

    #[test]
    fn test_mock_backend_descriptor() {
        let mut data = HashMap::new();
        data.insert("out".to_string(), vec![0.0_f32]);
        let backend = MockBackend::new(data, BackendDescriptor::TensorRT);
        assert_eq!(backend.descriptor(), BackendDescriptor::TensorRT);
    }

    #[test]
    fn test_mock_backend_load_model_reflects_output_shape() {
        let backend = make_backend();
        let handle = backend.load_model("test_model.onnx").unwrap();
        assert_eq!(handle.model_id, "mock::test_model.onnx");
        assert_eq!(handle.output_shapes.get("scores"), Some(&vec![3usize]));
        assert_eq!(handle.expected_precision, PrecisionMode::FP32);
    }

    #[test]
    fn test_mock_backend_capabilities_is_default() {
        let backend = make_backend();
        assert_eq!(backend.capabilities(), BackendCapabilities::default());
    }

    #[test]
    fn test_mock_backend_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MockBackend>();
    }
}
