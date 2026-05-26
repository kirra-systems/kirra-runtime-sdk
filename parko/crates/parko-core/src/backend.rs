use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors a backend may return.
///
/// Note: not `Clone`. Errors are meant to be propagated, not duplicated.
#[derive(Error, Debug)]
pub enum BackendError {
    #[error("Model initialization failed: {0}")]
    InitializationError(String),

    #[error("Inference execution failed: {0}")]
    ExecutionFailure(String),

    #[error("Tensor dimension mismatch. Expected {expected:?}, got {actual:?}")]
    DimensionMismatch {
        expected: Vec<usize>,
        actual: Vec<usize>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PrecisionMode {
    FP32,
    FP16,
    INT8,
}

#[derive(Debug, Clone)]
pub struct BackendCapabilities {
    pub precision_modes: Vec<PrecisionMode>,
    pub supports_zero_copy_inputs: bool,
    pub max_batch_size: usize,
    pub vendor_name: &'static str,
}

#[derive(Debug, Clone)]
pub struct ModelHandle {
    pub model_id: String,
    pub input_shapes: HashMap<String, Vec<usize>>,
    pub output_shapes: HashMap<String, Vec<usize>>,
    pub expected_precision: PrecisionMode,
}

/// Storage for a tensor's data. Either borrowed from caller memory or owned.
///
/// Not `Clone` or `PartialEq` — cloning would silently switch between a cheap
/// reference copy and a full data memcpy depending on variant, and equality
/// on float tensors is expensive and ill-defined for NaN.
#[derive(Debug)]
pub enum TensorStorage<'a> {
    Borrowed(&'a [f32]),
    Owned(Vec<f32>),
}

impl<'a> TensorStorage<'a> {
    pub fn as_slice(&self) -> &[f32] {
        match self {
            TensorStorage::Borrowed(slice) => slice,
            TensorStorage::Owned(vec) => vec.as_slice(),
        }
    }
}

#[derive(Debug)]
pub struct TensorBatch<'a> {
    pub named_tensors: HashMap<String, TensorStorage<'a>>,
    pub metadata: HashMap<String, String>,
}

/// A backend capable of running inference on loaded models.
///
/// Implementations must be `Send + Sync`; backends with non-`Sync` internals
/// (such as ONNX Runtime sessions) must use interior mutability to satisfy
/// this. See the parko-onnx backend for an example.
///
/// `run()` returns `TensorBatch<'static>` because outputs are always owned by
/// the caller — backends copy from their internal buffers into the returned
/// tensors. Input zero-copy via `Borrowed` is supported; output zero-copy is
/// not, and is a future API change if needed.
pub trait InferenceBackend: Send + Sync {
    fn load_model(&self, path: &str) -> Result<ModelHandle, BackendError>;

    fn run(
        &self,
        model: &ModelHandle,
        inputs: &TensorBatch,
    ) -> Result<TensorBatch<'static>, BackendError>;

    fn capabilities(&self) -> BackendCapabilities;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_storage_returns_pointer_to_original_buffer() {
        let buf = vec![1.23_f32, 4.56, 7.89];
        let storage = TensorStorage::Borrowed(&buf);
        assert_eq!(storage.as_slice().as_ptr(), buf.as_ptr());
        assert_eq!(storage.as_slice()[1], 4.56);
    }

    #[test]
    fn owned_storage_returns_slice_view_of_owned_data() {
        let storage = TensorStorage::Owned(vec![1.0, 2.0, 3.0]);
        assert_eq!(storage.as_slice(), &[1.0, 2.0, 3.0]);
    }
}
