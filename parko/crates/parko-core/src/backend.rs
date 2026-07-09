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

    /// Slice-level shape mismatch on the zero-copy hot path (ADL-003).
    #[error("Shape mismatch: expected {expected}, got {got}")]
    ShapeMismatch { expected: usize, got: usize },

    #[error("I/O error: {0}")]
    Io(String),

    /// Model-integrity allow-list rejection (#G16): the loaded model's SHA-256
    /// digest is not in the operator's allow-list (or strict mode is on and no
    /// entry matches). Fail-closed — the model MUST NOT run.
    #[error("model integrity rejected: {path} has sha256 {sha256} (not in the allow-list)")]
    IntegrityRejected { path: String, sha256: String },

    #[error("Operation not supported by this backend")]
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PrecisionMode {
    FP32,
    FP16,
    INT8,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BackendCapabilities {
    pub supports_int8: bool,
    pub supports_fp16: bool,
    pub max_batch_size: Option<usize>,
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

/// Which silicon target a backend runs on.
///
/// `#[non_exhaustive]` — new targets will be added as hardware backends land
/// (PARK-020 TensorRT, PARK-027 QNN, PARK-028 TIDL, PARK-029 OpenVINO,
/// PARK-030 AMD). Matchers must use a wildcard arm.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BackendDescriptor {
    Cpu,
    /// NVIDIA GPU via the ONNX Runtime **CUDA** execution provider
    /// (parko-onnx `OrtBackend::new_cuda`, behind the `cuda` feature).
    Cuda,
    TensorRT,
    QualcommQnn,
    TiTidl,
    IntelOpenVino,
    AmdVitis,
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
///
/// The zero-copy hot-path contract (`run(&[f32], &mut [f32])`) specified in
/// ADL-003 is a target interface for future refactor. The current
/// `TensorBatch`-based `run()` is the live API used by all backends.
pub trait InferenceBackend: Send + Sync {
    fn load_model(&self, path: &str) -> Result<ModelHandle, BackendError>;

    fn run(
        &self,
        model: &ModelHandle,
        inputs: &TensorBatch,
    ) -> Result<TensorBatch<'static>, BackendError>;

    /// Returns the capability profile for this backend.
    ///
    /// Defaults to all-false, `max_batch_size: None`. Override in concrete
    /// backends to reflect actual hardware support. PARK-012 stub backends
    /// rely on `BackendCapabilities::default()`.
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }

    /// Identifies which silicon target this backend runs on.
    ///
    /// Defaults to `BackendDescriptor::Cpu` so existing impls compile without
    /// changes. Override in hardware backends when they land.
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor::Cpu
    }

    /// Startup lifecycle hook — do any first-use work NOW (at node startup, after
    /// `load_model`) so the first real command never pays it. The default is a
    /// no-op: most backends (CPU ORT, OpenVINO, the mock) need no warm-up.
    ///
    /// Hardware backends that build/cache engines override this — e.g. the
    /// TensorRT backend forces its multi-second per-model/shape engine build here
    /// (PARK-021 #2). Called through a shared `&self` because the runtime node
    /// holds the backend behind an `Arc`. FAIL-CLOSED: an `Err` means the backend
    /// could not be made ready, and the node must REFUSE to start rather than serve
    /// against an unbuilt engine.
    fn warm_up(&self, _model: &ModelHandle) -> Result<(), BackendError> {
        Ok(())
    }
}

/// The SINGLE source of inference thread count, shared by every inference
/// backend (parko-onnx `OrtBackend`, parko-openvino `OvBackend`).
///
/// WHY ONE TYPE: the #152 investigation found the cross-backend equivalence
/// drift came from an execution ASYMMETRY — ORT pinned to a single thread while
/// OpenVINO ran multi-threaded. Both backends reading this same value from one
/// place is the structural guard against that recurring: a comparison must
/// build both backends from the SAME `InferenceThreads`, so their thread counts
/// cannot diverge.
///
/// DEFAULT is 1 — inference is bitwise-reproducible on fixed hardware (the
/// production robot's one SoC): a single thread fixes the floating-point
/// accumulation order. Raising it trades that determinism for latency headroom,
/// which is why the active value is LOGGED at backend init (audit-relevant, not
/// a silent function of a config value). The other three settings
/// (fp32 / ACCURACY / LATENCY) are fixed production posture and are NOT
/// configurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InferenceThreads {
    pub num_threads: usize,
}

impl Default for InferenceThreads {
    /// Single-threaded: bitwise-reproducible inference.
    fn default() -> Self {
        Self { num_threads: 1 }
    }
}

impl InferenceThreads {
    #[must_use]
    pub fn new(num_threads: usize) -> Self {
        Self { num_threads }
    }

    /// True iff inference is bitwise-reproducible on fixed hardware (single
    /// thread → deterministic accumulation order). Recorded at backend init.
    #[must_use]
    pub fn bitwise_reproducible(self) -> bool {
        self.num_threads == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_threads_default_is_single_threaded_reproducible() {
        let t = InferenceThreads::default();
        assert_eq!(
            t.num_threads, 1,
            "default must be 1 (preserves the #152 experiment)"
        );
        assert!(
            t.bitwise_reproducible(),
            "single thread → bitwise reproducible"
        );
    }

    #[test]
    fn inference_threads_reproducible_flag_tracks_count() {
        assert!(InferenceThreads::new(1).bitwise_reproducible());
        assert!(!InferenceThreads::new(2).bitwise_reproducible());
        assert!(!InferenceThreads::new(8).bitwise_reproducible());
    }

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

    #[test]
    fn test_backend_descriptor_debug_roundtrip() {
        let variants = [
            BackendDescriptor::Cpu,
            BackendDescriptor::TensorRT,
            BackendDescriptor::QualcommQnn,
            BackendDescriptor::TiTidl,
            BackendDescriptor::IntelOpenVino,
            BackendDescriptor::AmdVitis,
        ];
        for variant in &variants {
            let s = format!("{:?}", variant);
            assert!(
                !s.is_empty(),
                "Debug output must be non-empty for {:?}",
                variant
            );
        }
    }

    #[test]
    fn test_backend_capabilities_default() {
        let caps = BackendCapabilities::default();
        assert!(!caps.supports_int8);
        assert!(!caps.supports_fp16);
        assert_eq!(caps.max_batch_size, None);
    }

    #[test]
    fn test_backend_error_display() {
        let shape_err = BackendError::ShapeMismatch {
            expected: 4,
            got: 2,
        };
        let msg = shape_err.to_string();
        assert!(
            msg.contains('4'),
            "Display must mention expected=4, got: {}",
            msg
        );
        assert!(
            msg.contains('2'),
            "Display must mention got=2, got: {}",
            msg
        );

        let io_err = BackendError::Io("disk full".into());
        assert!(
            io_err.to_string().contains("disk full"),
            "Io display must contain the message"
        );

        let unsupported = BackendError::Unsupported;
        assert!(
            !unsupported.to_string().is_empty(),
            "Unsupported display must be non-empty"
        );
    }
}
