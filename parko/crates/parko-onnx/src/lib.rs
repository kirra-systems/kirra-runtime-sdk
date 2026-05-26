// crates/parko-onnx/src/lib.rs

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ort::{
    session::Session,
    value::{Tensor, ValueType},
};

use parko_core::backend::{
    BackendCapabilities, BackendError, InferenceBackend, ModelHandle, PrecisionMode,
    TensorBatch, TensorStorage,
};

pub struct OrtBackend {
    session: Arc<Mutex<Session>>,
}

impl OrtBackend {
    pub fn new(model_path: &str) -> Result<Self, BackendError> {
        let session = Session::builder()
            .map_err(|e| BackendError::InitializationError(format!("ort builder error: {:?}", e)))?
            .commit_from_file(model_path)
            .map_err(|e| BackendError::InitializationError(format!("ort session init error: {:?}", e)))?;

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
        })
    }
}

impl InferenceBackend for OrtBackend {
    fn load_model(&self, path: &str) -> Result<ModelHandle, BackendError> {
        let session = self.session.lock()
            .map_err(|e| BackendError::InitializationError(format!("session lock poisoned: {}", e)))?;

        let mut input_shapes = HashMap::new();
        let mut output_shapes = HashMap::new();

        for input in session.inputs() {
            let shape: Vec<usize> = if let ValueType::Tensor { shape, .. } = input.dtype() {
                shape.iter().map(|&d| d.max(1) as usize).collect()
            } else {
                vec![]
            };
            input_shapes.insert(input.name().to_string(), shape);
        }

        for output in session.outputs() {
            let shape: Vec<usize> = if let ValueType::Tensor { shape, .. } = output.dtype() {
                shape.iter().map(|&d| d.max(1) as usize).collect()
            } else {
                vec![]
            };
            output_shapes.insert(output.name().to_string(), shape);
        }

        Ok(ModelHandle {
            model_id: format!("ort_native_cpu_session_from_{}", path),
            input_shapes,
            output_shapes,
            expected_precision: PrecisionMode::FP32,
        })
    }

    fn run(&self, model: &ModelHandle, inputs: &TensorBatch)
        -> Result<TensorBatch<'static>, BackendError>
    {
        let mut ort_inputs: Vec<(String, Tensor<f32>)> = Vec::new();

        for (name, expected_shape) in &model.input_shapes {
            let Some(storage) = inputs.named_tensors.get(name) else {
                return Err(BackendError::ExecutionFailure(format!(
                    "Missing input tensor '{}'", name
                )));
            };

            let raw_slice = storage.as_slice();
            let expected_len: usize = expected_shape.iter().product();

            if raw_slice.len() != expected_len {
                return Err(BackendError::DimensionMismatch {
                    expected: expected_shape.clone(),
                    actual: vec![raw_slice.len()],
                });
            }

            let tensor = Tensor::from_array((expected_shape.clone(), raw_slice.to_vec()))
                .map_err(|e| BackendError::ExecutionFailure(format!("ort tensor error: {:?}", e)))?;

            ort_inputs.push((name.clone(), tensor));
        }

        let mut session = self.session.lock()
            .map_err(|e| BackendError::ExecutionFailure(format!("session lock poisoned: {}", e)))?;

        // Collect output names before run() borrows session mutably.
        let output_names: Vec<String> = session.outputs().iter()
            .map(|o| o.name().to_string())
            .collect();

        let outputs = session.run(ort_inputs)
            .map_err(|e| BackendError::ExecutionFailure(format!("ort execution error: {:?}", e)))?;

        let mut output_named_tensors = HashMap::new();

        for name in &output_names {
            let ort_value = outputs.get(name.as_str())
                .ok_or_else(|| BackendError::ExecutionFailure(format!(
                    "Missing output node '{}'", name
                )))?;

            let (_, raw_slice) = ort_value.try_extract_tensor::<f32>()
                .map_err(|e| BackendError::ExecutionFailure(format!("tensor extract error: {:?}", e)))?;

            output_named_tensors.insert(
                name.clone(),
                TensorStorage::Owned(raw_slice.to_vec()),
            );
        }

        Ok(TensorBatch {
            named_tensors: output_named_tensors,
            metadata: HashMap::new(),
        })
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            precision_modes: vec![PrecisionMode::FP32],
            supports_zero_copy_inputs: false,
            max_batch_size: 1,
            vendor_name: "ort-cpu",
        }
    }
}
