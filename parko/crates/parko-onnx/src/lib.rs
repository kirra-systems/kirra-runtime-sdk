// crates/parko-onnx/src/lib.rs

use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;

use parko_core::backend::{
    BackendCapabilities, BackendDescriptor, BackendError, InferenceBackend, InferenceThreads,
    ModelHandle, TensorBatch,
};

pub mod lineage_load;
pub mod session_core;
use session_core::OrtRunCore;

pub struct OrtBackend {
    core: OrtRunCore,
    /// Which execution provider this instance actually runs on. Set per
    /// constructor: `Cpu` for `new`/`with_threads`, `Cuda` for a successful
    /// `new_cuda`/`with_cuda_config`, and honestly back to `Cpu` for the opt-in
    /// CUDA→CPU degraded fallback. Returned by [`InferenceBackend::descriptor`].
    descriptor: BackendDescriptor,
}

impl OrtBackend {
    /// Construct with the default execution posture (single-threaded,
    /// bitwise-reproducible). The thread count is the only configurable knob;
    /// see [`OrtBackend::with_threads`].
    pub fn new(model_path: &str) -> Result<Self, BackendError> {
        Self::with_threads(model_path, InferenceThreads::default())
    }

    /// Construct with an explicit [`InferenceThreads`]. `num_threads` is the
    /// sole configurable setting; the optimization level (`Disable`, the
    /// determinism posture mirrored by parko-openvino's ACCURACY mode) is
    /// fixed. The thread count MUST come from the same `InferenceThreads` the
    /// OpenVINO backend reads — see `parko_core::InferenceThreads`.
    pub fn with_threads(
        model_path: &str,
        threads: InferenceThreads,
    ) -> Result<Self, BackendError> {
        let session = Session::builder()
            .map_err(|e| BackendError::InitializationError(format!("ort builder error: {:?}", e)))?
            .with_intra_threads(threads.num_threads)
            .map_err(|e| BackendError::InitializationError(format!("ort intra_threads error: {:?}", e)))?
            .with_optimization_level(GraphOptimizationLevel::Disable)
            .map_err(|e| BackendError::InitializationError(format!("ort opt_level error: {:?}", e)))?
            .commit_from_file(model_path)
            .map_err(|e| BackendError::InitializationError(format!("ort session init error: {:?}", e)))?;

        // Record the execution posture (determinism status is audit-relevant).
        tracing::info!(
            backend = "ort",
            num_threads = threads.num_threads,
            optimization = "disabled",
            bitwise_reproducible = threads.bitwise_reproducible(),
            "OrtBackend execution posture"
        );

        // The CPU backend keeps its model_id identity ("ort_native_cpu"); the
        // shared core single-sources the load_model/run logic.
        Ok(Self {
            core: OrtRunCore::new(session, "ort_native_cpu"),
            descriptor: BackendDescriptor::Cpu,
        })
    }

    /// EP-04: lineage-SUPERVISED construction — the live consumer of
    /// [`parko_core::model_lineage`]. Because an `ort::Session` is committed
    /// from its model path at construction, the rollback seam is here: the
    /// [`lineage_load::LineageLoader`] resolves WHICH artifact to build from
    /// (requested on a clean verify; the re-verified last-good on an integrity
    /// rejection; an `Err` when nothing trustworthy is loadable), and only then
    /// is the session built. Reads the allow-list from env
    /// (`KIRRA_MODEL_ALLOWLIST`(+`_STRICT`)) exactly like the per-`load_model`
    /// #G16 gate; use [`OrtBackend::with_lineage_and_allowlist`] to inject one.
    ///
    /// Returns the backend PLUS the [`lineage_load::ResolvedLoad`] so the
    /// caller can log/escalate a `Rollback` (a rollback is a red flag even
    /// though a good model is running).
    pub fn new_with_lineage(
        loader: &mut lineage_load::LineageLoader,
        requested_path: &str,
    ) -> Result<(Self, lineage_load::ResolvedLoad), BackendError> {
        Self::with_lineage_and_allowlist(
            loader,
            requested_path,
            &parko_core::model_integrity::ModelAllowList::from_env(),
        )
    }

    /// [`OrtBackend::new_with_lineage`] with an injected allow-list (testable
    /// without env mutation — the parko/kirra no-`set_var` invariant).
    pub fn with_lineage_and_allowlist(
        loader: &mut lineage_load::LineageLoader,
        requested_path: &str,
        allow: &parko_core::model_integrity::ModelAllowList,
    ) -> Result<(Self, lineage_load::ResolvedLoad), BackendError> {
        let resolved = loader.resolve(requested_path, allow)?;
        let backend = Self::new(&resolved.path)?;
        Ok((backend, resolved))
    }
}

impl InferenceBackend for OrtBackend {
    fn load_model(&self, path: &str) -> Result<ModelHandle, BackendError> {
        self.core.load_model(path)
    }

    fn run(&self, model: &ModelHandle, inputs: &TensorBatch)
        -> Result<TensorBatch<'static>, BackendError>
    {
        self.core.run(model, inputs)
    }

    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> BackendCapabilities {
        // CPU ONNX Runtime baseline — update when quantized models are tested (PARK-009, ADL-007).
        // TODO(cuda): the CUDA EP enables FP16/INT8 inference; revisit
        // supports_fp16 / supports_int8 for the `new_cuda` path once measured on
        // GPU hardware. Conservatively reported false (full-precision posture)
        // until validated, so this stays correct for both EPs today.
        BackendCapabilities {
            supports_int8: false,
            supports_fp16: false,
            max_batch_size: None,
        }
    }
}

// ---------------------------------------------------------------------------
// CUDA execution provider (NVIDIA GPU) — behind the `cuda` feature
// ---------------------------------------------------------------------------
//
// Mirrors the CPU constructor but builds the session on ort's CUDA EP, then
// hands it to the SAME `OrtRunCore` (load_model/run is single-sourced — no
// duplication). The CPU constructor and its "ort_native_cpu" identity are
// untouched.
//
// FAIL-CLOSED (the load-bearing safety property): the CUDA EP is registered with
// `.error_on_failure()`. ort's default is to SILENTLY fall back to CPU when an
// EP can't register — exactly the silent-degradation hazard a safety path must
// reject. With `error_on_failure`, a missing GPU/driver/CUDA-provider lib makes
// construction return `BackendError::InitializationError` — never a quiet CPU
// run. An explicit, WARN-logged CPU fallback is available only by opt-in
// (`CudaConfig::allow_cpu_fallback`, default false).
//
// DETERMINISM HONESTY: GPU CUDA is not bitwise-reproducible the way single-thread
// CPU is, so this path does NOT read `InferenceThreads` (a CPU concept).

#[cfg(feature = "cuda")]
use ort::ep::CUDA;

/// Configuration for [`OrtBackend::with_cuda_config`].
#[cfg(feature = "cuda")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CudaConfig {
    /// CUDA device ordinal (0 = first GPU).
    pub device_id: i32,
    /// EP-init-failure policy. **Fail-closed by default (`false`)**: a CUDA-EP
    /// registration failure returns `Err` with no CPU fallback. Set `true` to
    /// opt into an explicit, `tracing::WARN`-logged degrade-to-CPU.
    pub allow_cpu_fallback: bool,
}

#[cfg(feature = "cuda")]
impl Default for CudaConfig {
    fn default() -> Self {
        // Device 0, FAIL-CLOSED (no silent CPU fallback).
        Self { device_id: 0, allow_cpu_fallback: false }
    }
}

/// Build a session on the CUDA EP, registered `error_on_failure` (fail-closed).
/// Returns `Err` if the EP cannot register against the dlopened ORT runtime
/// (no GPU / driver / CUDA provider) — never a silent CPU run.
#[cfg(feature = "cuda")]
fn build_cuda_session(model_path: &str, cfg: &CudaConfig) -> Result<Session, BackendError> {
    let cuda_ep = CUDA::default()
        .with_device_id(cfg.device_id)
        .build()
        .error_on_failure();

    Session::builder()
        .map_err(|e| BackendError::InitializationError(format!("ort builder error: {e:?}")))?
        .with_execution_providers([cuda_ep])
        .map_err(|e| BackendError::InitializationError(format!(
            "CUDA EP registration failed (fail-closed; no silent CPU fallback). The dlopened \
             ONNX Runtime lacks a usable CUDA provider, or no GPU/driver is present: {e:?}"
        )))?
        .commit_from_file(model_path)
        .map_err(|e| BackendError::InitializationError(format!("ort session init error: {e:?}")))
}

#[cfg(feature = "cuda")]
impl OrtBackend {
    /// Construct a CUDA (NVIDIA GPU) backend with the default config — device 0,
    /// **fail-closed** (no CPU fallback). Mirrors [`OrtBackend::new`] for the
    /// CPU path. Requires a CUDA-enabled ONNX Runtime + GPU at runtime.
    pub fn new_cuda(model_path: &str) -> Result<Self, BackendError> {
        Self::with_cuda_config(model_path, &CudaConfig::default())
    }

    /// Construct a CUDA backend with an explicit [`CudaConfig`]. On a successful
    /// CUDA-EP registration: descriptor `Cuda`, model_id prefix `"ort_cuda"`. On
    /// failure: fail-closed `Err` by default, or — if `cfg.allow_cpu_fallback` —
    /// a `tracing::WARN` "CUDA EP unavailable, degraded to CPU" and a plain CPU
    /// session (descriptor honestly `Cpu`, model_id `"ort_cuda_degraded_cpu"` so
    /// audit distinguishes a degraded run from the real CPU backend).
    pub fn with_cuda_config(model_path: &str, cfg: &CudaConfig) -> Result<Self, BackendError> {
        match build_cuda_session(model_path, cfg) {
            Ok(session) => {
                tracing::info!(
                    backend = "ort_cuda",
                    device_id = cfg.device_id,
                    execution_provider = "CUDA",
                    bitwise_reproducible = false,
                    "OrtBackend CUDA execution posture (NVIDIA GPU EP; not bitwise-reproducible)"
                );
                Ok(Self {
                    core: OrtRunCore::new(session, "ort_cuda"),
                    descriptor: BackendDescriptor::Cuda,
                })
            }
            Err(e) => {
                if !cfg.allow_cpu_fallback {
                    // Default, safety posture: fail-closed.
                    return Err(e);
                }
                // Opt-in degraded path. Surfaced loudly; honestly reports CPU.
                tracing::warn!(
                    backend = "ort_cuda",
                    device_id = cfg.device_id,
                    cause = %format!("{e:?}"),
                    "CUDA EP unavailable, degraded to CPU"
                );
                // A plain CPU session (mirrors the CPU constructor's build; the
                // CPU constructor itself is left untouched). Distinct model_id so
                // a degraded-CUDA run is never mistaken for the native CPU backend.
                let threads = InferenceThreads::default();
                let session = Session::builder()
                    .map_err(|e| BackendError::InitializationError(format!("ort builder error: {e:?}")))?
                    .with_intra_threads(threads.num_threads)
                    .map_err(|e| BackendError::InitializationError(format!("ort intra_threads error: {e:?}")))?
                    .with_optimization_level(GraphOptimizationLevel::Disable)
                    .map_err(|e| BackendError::InitializationError(format!("ort opt_level error: {e:?}")))?
                    .commit_from_file(model_path)
                    .map_err(|e| BackendError::InitializationError(format!("ort session init error: {e:?}")))?;
                Ok(Self {
                    core: OrtRunCore::new(session, "ort_cuda_degraded_cpu"),
                    descriptor: BackendDescriptor::Cpu,
                })
            }
        }
    }
}
