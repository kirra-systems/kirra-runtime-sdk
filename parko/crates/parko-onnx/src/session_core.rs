// crates/parko-onnx/src/session_core.rs
//
// Shared ORT inference core (PARK-021, A1). The `load_model` + `run` logic is
// IDENTICAL for every ort-backed backend — it operates on a committed
// `ort::Session` regardless of whether that session runs on the CPU provider
// (parko-onnx `OrtBackend`) or the TensorRT execution provider (parko-tensorrt
// `TrtBackend`). The ONLY difference between those backends is how the session is
// BUILT (thread/precision/EP config + which posture is logged), which stays in
// each backend's constructor. This module holds the part they share.
//
// EXTRACTION IS PURE: the bodies below are relocated verbatim from `OrtBackend`;
// the `model_id` prefix is parameterized so each backend keeps its own model_id
// string while the inference logic is single-sourced. parko-onnx behavior is
// unchanged (`OrtBackend` passes the original "ort_native_cpu" prefix).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ort::{
    session::Session,
    value::{Tensor, ValueType},
};

use parko_core::backend::{BackendError, ModelHandle, PrecisionMode, TensorBatch, TensorStorage};

/// Owns a committed `ort::Session` and runs the shared `load_model` / `run`
/// inference path. Each backend builds its own session (CPU vs TRT EP) and hands
/// it here; the inference logic below is single-sourced.
pub struct OrtRunCore {
    session: Arc<Mutex<Session>>,
    /// Backend-specific model_id prefix (e.g. "ort_native_cpu" for the CPU
    /// backend, "ort_trt" for the TensorRT backend) so each keeps its identity
    /// while sharing the logic.
    model_id_prefix: String,
}

impl OrtRunCore {
    /// Wrap a committed session. `model_id_prefix` is the backend's identity tag
    /// used in `load_model`'s `model_id`.
    pub fn new(session: Session, model_id_prefix: impl Into<String>) -> Self {
        Self {
            session: Arc::new(Mutex::new(session)),
            model_id_prefix: model_id_prefix.into(),
        }
    }

    /// Read input/output shapes off the session into a `ModelHandle`. Relocated
    /// verbatim from `OrtBackend::load_model`, plus the #G16 integrity gate below.
    pub fn load_model(&self, path: &str) -> Result<ModelHandle, BackendError> {
        // #G16 — model-integrity allow-list: SHA-256 the model file and reject a
        // substituted / corrupt artifact BEFORE handing back a runnable handle.
        // Enforcement is opt-in (KIRRA_MODEL_ALLOWLIST); when off this only
        // computes+logs the digest and acceptance is byte-identical to before.
        let integrity = parko_core::model_integrity::verify_model_file(
            path,
            &parko_core::model_integrity::ModelAllowList::from_env(),
        )?;
        tracing::info!(
            model_path = path,
            sha256 = integrity.sha256_hex,
            verified = integrity.verified,
            "ort load_model integrity check"
        );

        let session = self.session.lock().map_err(|e| {
            BackendError::InitializationError(format!("session lock poisoned: {}", e))
        })?;

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
            model_id: format!("{}_session_from_{}", self.model_id_prefix, path),
            input_shapes,
            output_shapes,
            expected_precision: PrecisionMode::FP32,
        })
    }

    /// Run inference. Relocated verbatim from `OrtBackend::run`.
    pub fn run(
        &self,
        model: &ModelHandle,
        inputs: &TensorBatch,
    ) -> Result<TensorBatch<'static>, BackendError> {
        let mut ort_inputs: Vec<(String, Tensor<f32>)> = Vec::new();

        for (name, expected_shape) in &model.input_shapes {
            let Some(storage) = inputs.named_tensors.get(name) else {
                return Err(BackendError::ExecutionFailure(format!(
                    "Missing input tensor '{}'",
                    name
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

            let tensor =
                Tensor::from_array((expected_shape.clone(), raw_slice.to_vec())).map_err(|e| {
                    BackendError::ExecutionFailure(format!("ort tensor error: {:?}", e))
                })?;

            ort_inputs.push((name.clone(), tensor));
        }

        let mut session = self
            .session
            .lock()
            .map_err(|e| BackendError::ExecutionFailure(format!("session lock poisoned: {}", e)))?;

        // Collect output names before run() borrows session mutably.
        let output_names: Vec<String> = session
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();

        let outputs = session
            .run(ort_inputs)
            .map_err(|e| BackendError::ExecutionFailure(format!("ort execution error: {:?}", e)))?;

        let mut output_named_tensors = HashMap::new();

        for name in &output_names {
            let ort_value = outputs.get(name.as_str()).ok_or_else(|| {
                BackendError::ExecutionFailure(format!("Missing output node '{}'", name))
            })?;

            let (_, raw_slice) = ort_value.try_extract_tensor::<f32>().map_err(|e| {
                BackendError::ExecutionFailure(format!("tensor extract error: {:?}", e))
            })?;

            output_named_tensors.insert(name.clone(), TensorStorage::Owned(raw_slice.to_vec()));
        }

        Ok(TensorBatch {
            named_tensors: output_named_tensors,
            metadata: HashMap::new(),
        })
    }
}
