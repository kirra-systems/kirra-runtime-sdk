// crates/parko-tensorrt/src/lib.rs
//
// PARK-021 — TensorRT backend (A1: distinct crate using ort's TensorRT execution
// provider). Runs Parko accelerated on the ROSOrin's Jetson. CI-BUILDABLE ONLY:
// it compiles with no GPU/CUDA/TRT libraries present (ort's `load-dynamic` pulls
// `ort-sys/disable-linking`), but real inference requires a TensorRT-enabled ORT
// runtime on NVIDIA silicon — see the PARK-021 Jetson-gated list at the bottom.
//
// REUSE: the load_model/run inference path is IDENTICAL to parko-onnx and is
// single-sourced via `parko_onnx::session_core::OrtRunCore`. This crate differs
// ONLY in how the session is built (the TRT execution provider + precision
// config) and which posture is logged.
//
// FAIL-CLOSED (the load-bearing safety property): the TRT EP is registered with
// `.error_on_failure()`. ort's default is to SILENTLY fall back to CPU when an EP
// fails to register (confirmed in ort rc.11 `apply_execution_providers`); that is
// exactly the silent-degradation hazard a safety path must reject. With
// `error_on_failure`, a TRT-unavailable runtime (e.g. CI's CPU-only ORT lib)
// makes `with_config` return `Err` — never a quiet CPU run.
//
// DETERMINISM HONESTY: GPU TensorRT is NOT bitwise-reproducible the way
// single-thread CPU is, so this backend does NOT read `InferenceThreads`
// (num_threads is a CPU concept). Its config anchor is `TrtPosture`. The safety
// posture is "fixed engine + fixed precision + decision-agreement bound
// (hardware-measured)", logged — not a bitwise-determinism claim.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use ort::ep::TensorRT;
use ort::session::Session;

use parko_core::backend::{
    BackendCapabilities, BackendDescriptor, BackendError, InferenceBackend, ModelHandle,
    TensorBatch, TensorStorage,
};
use parko_onnx::session_core::OrtRunCore;

/// TF32 control state. Ampere+ GPUs (the Orin) may use TF32 for fp32 matmuls,
/// silently dropping mantissa bits. ort's TensorRT EP exposes NO TF32 knob
/// (confirmed in ort rc.11 source — `with_fp16`/`with_int8` exist, no TF32), so
/// this backend CANNOT enforce TF32-off from Rust. This is an honest,
/// not-yet-resolved precision gap, surfaced rather than hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tf32Control {
    /// TF32 is NOT enforced off here. Resolution is Jetson-gated (e.g. the
    /// `NVIDIA_TF32_OVERRIDE=0` env override, measured against tolerance) or, if
    /// it cannot be controlled, the A2 (native nvinfer) escalation trigger.
    /// MUST NOT be read as "TF32 off / full precision guaranteed".
    UnenforcedPendingJetsonResolution,
}

impl Tf32Control {
    /// Honest one-line status for the init log. Deliberately does NOT say "off".
    #[must_use]
    pub fn status_str(self) -> &'static str {
        match self {
            Tf32Control::UnenforcedPendingJetsonResolution => "UNENFORCED (pending Jetson resolution; no TF32 knob in ort TRT EP)",
        }
    }
}

/// The TensorRT backend's execution posture — its config anchor and the audit
/// record logged at init. Replaces the CPU/OpenVINO `bitwise_reproducible`
/// claim, which does not hold on GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrtPosture {
    /// FP16 inference. FIXED false — full precision for the safety path.
    pub fp16: bool,
    /// INT8 inference. FIXED false — no silent quantization.
    pub int8: bool,
    /// TF32 control — see [`Tf32Control`]. NOT enforceable from ort's TRT EP.
    pub tf32: Tf32Control,
    /// Where the serialized TRT engine is cached (per model/shape/version/GPU).
    pub engine_cache_path: String,
    /// SHA of the built engine. `None` until an engine is actually built on
    /// hardware (Jetson-gated); it cannot be known on a GPU-less CI build.
    pub engine_sha: Option<String>,
}

impl TrtPosture {
    /// The fixed safety defaults for a given engine-cache path: full precision
    /// (no fp16/int8), TF32 unenforced-pending, no engine SHA yet.
    #[must_use]
    pub fn full_precision(engine_cache_path: impl Into<String>) -> Self {
        Self {
            fp16: false,
            int8: false,
            tf32: Tf32Control::UnenforcedPendingJetsonResolution,
            engine_cache_path: engine_cache_path.into(),
            engine_sha: None,
        }
    }

    /// True only if precision is *fully* guaranteed end to end. It is NOT, while
    /// TF32 is unenforceable from the EP — so this is honestly `false`. The init
    /// log surfaces this rather than implying full-precision determinism.
    #[must_use]
    pub fn full_precision_guaranteed(&self) -> bool {
        // Even with fp16/int8 off, full precision is NOT guaranteed while TF32 is
        // unenforceable from ort's TRT EP. Honestly `false` until TF32 is
        // resolved on the Jetson (env override measured vs tolerance, or A2).
        if self.fp16 || self.int8 {
            return false;
        }
        match self.tf32 {
            Tf32Control::UnenforcedPendingJetsonResolution => false,
        }
    }
}

/// Configuration for [`TrtBackend::with_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrtConfig {
    /// Engine-cache directory. See [`resolve_engine_cache_path`].
    pub engine_cache_path: String,
}

impl Default for TrtConfig {
    fn default() -> Self {
        Self { engine_cache_path: resolve_engine_cache_path(None) }
    }
}

/// Default engine-cache directory when none is configured. The fixed input
/// shapes Parko's sensor mappings emit mean one engine per model and clean cache
/// reuse, so a stable on-disk path is the intended setup.
pub const DEFAULT_ENGINE_CACHE_PATH: &str = "./parko_trt_engine_cache";

/// Resolve the engine-cache path: explicit wins, else `PARKO_TRT_ENGINE_CACHE`
/// env, else the default. Pure + GPU-free (unit-tested on CI).
#[must_use]
pub fn resolve_engine_cache_path(explicit: Option<&str>) -> String {
    if let Some(p) = explicit {
        return p.to_string();
    }
    std::env::var("PARKO_TRT_ENGINE_CACHE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_ENGINE_CACHE_PATH.to_string())
}

/// TensorRT inference backend. Construct with [`TrtBackend::with_config`] (or
/// [`TrtBackend::new`] for defaults). Real inference is Jetson-gated.
pub struct TrtBackend {
    core: OrtRunCore,
    posture: TrtPosture,
    /// SHA-256 of the serialized engine, captured by `warm_up` once an engine
    /// exists on disk. Interior-mutable so warm-up can run through a shared
    /// `&self` (backends are held behind `Arc` in the runtime node) and set it
    /// exactly once.
    warmed_engine_sha: OnceLock<String>,
}

impl TrtBackend {
    /// Construct with the default config (resolved engine-cache path, full
    /// precision). Jetson-gated at runtime: needs a TensorRT-enabled ORT lib.
    pub fn new(model_path: &str) -> Result<Self, BackendError> {
        Self::with_config(model_path, &TrtConfig::default())
    }

    /// Build a session that runs on the TensorRT EP ONLY (no CUDA/CPU EP entries
    /// — fail-closed is the posture), with full precision (fp16/int8 off) and
    /// engine caching enabled. Returns `Err` if the TRT EP cannot register
    /// against the dlopened ORT runtime (`error_on_failure`) — never a silent
    /// CPU run.
    pub fn with_config(model_path: &str, cfg: &TrtConfig) -> Result<Self, BackendError> {
        let posture = TrtPosture::full_precision(cfg.engine_cache_path.clone());

        // TRT EP only. fp16=false, int8=false (full precision); engine cache on.
        // `.error_on_failure()` makes a failed registration fatal (fail-closed).
        let trt_ep = TensorRT::default()
            .with_fp16(posture.fp16)
            .with_int8(posture.int8)
            .with_engine_cache(true)
            .with_engine_cache_path(&posture.engine_cache_path)
            .build()
            .error_on_failure();

        let session: Session = Session::builder()
            .map_err(|e| BackendError::InitializationError(format!("ort builder error: {e:?}")))?
            .with_execution_providers([trt_ep])
            .map_err(|e| BackendError::InitializationError(format!(
                "TensorRT EP registration failed — refusing to run (fail-closed; no CPU fallback). \
                 The dlopened ONNX Runtime lacks a usable TensorRT provider \
                 (expected on a CPU-only ORT build / CI): {e:?}"
            )))?
            .commit_from_file(model_path)
            .map_err(|e| BackendError::InitializationError(format!("ort session init error: {e:?}")))?;

        // Audit-relevant posture log. HONEST: full_precision_guaranteed is false
        // while TF32 is unenforceable, and GPU TRT is not bitwise-reproducible.
        tracing::info!(
            backend = "tensorrt",
            fp16 = posture.fp16,
            int8 = posture.int8,
            tf32 = %posture.tf32.status_str(),
            engine_cache_path = %posture.engine_cache_path,
            engine_sha = ?posture.engine_sha,
            full_precision_guaranteed = posture.full_precision_guaranteed(),
            "TrtBackend execution posture (TensorRT EP; not bitwise-reproducible — \
             fixed-engine + fixed-precision + hardware-measured decision-agreement posture)"
        );

        Ok(Self {
            core: OrtRunCore::new(session, "ort_trt"),
            posture,
            warmed_engine_sha: OnceLock::new(),
        })
    }

    /// The logged execution posture (audit / introspection).
    #[must_use]
    pub fn posture(&self) -> &TrtPosture {
        &self.posture
    }

    /// The SHA-256 of the cached engine captured by [`Self::warm_up_report`] (or
    /// the trait [`InferenceBackend::warm_up`]). `None` until warm-up has run and
    /// an engine exists on disk.
    #[must_use]
    pub fn engine_sha(&self) -> Option<&str> {
        self.warmed_engine_sha.get().map(String::as_str)
    }

    /// Force the per-model/shape TensorRT engine to build (or deserialize from the
    /// on-disk cache) NOW — at startup — so the multi-second first-build never lands
    /// on the first real command (PARK-021 #2). Runs ONE inference with a
    /// shape-correct zero input (output discarded; the engine build depends on the
    /// input SHAPE, not its values), then captures the cached engine's SHA-256 (see
    /// [`Self::engine_sha`]). Idempotent: against a warm cache it only deserializes.
    /// Measured cold build on a Jetson Orin (MNIST) is ~2.2 s — the budget this moves
    /// off the hot path; bigger models cost more.
    ///
    /// Takes `&self` (interior-mutable SHA capture) so it can run through the `Arc`
    /// the runtime node holds the backend behind. The Nominal `run` hot path is
    /// unchanged. The trait hook [`InferenceBackend::warm_up`] calls this and
    /// discards the report; use this method directly when the report is wanted.
    pub fn warm_up_report(&self, model: &ModelHandle) -> Result<WarmUpReport, BackendError> {
        if model.input_shapes.is_empty() {
            return Err(BackendError::InitializationError(
                "warm_up: model declares no inputs — cannot synthesize a warm-up batch".into(),
            ));
        }
        // Shape-correct zero input for every declared input (Owned → no lifetimes).
        let mut named_tensors = HashMap::new();
        for (name, shape) in &model.input_shapes {
            let total: usize = shape.iter().product();
            named_tensors.insert(name.clone(), TensorStorage::Owned(vec![0.0f32; total]));
        }
        let batch = TensorBatch { named_tensors, metadata: HashMap::new() };

        let t0 = Instant::now();
        // Build (cold) or deserialize (warm) the engine; the output is discarded.
        let _ = self.run(model, &batch)?;
        let warmed_ms = t0.elapsed().as_millis();

        // Capture the serialized engine's SHA-256 (the #2 hook: None until an engine
        // actually exists on hardware). Stored once via the OnceLock so the capture
        // survives through the shared `&self`; a second warm-up keeps the first SHA.
        let (engine_file, engine_sha256, engine_bytes) =
            sha256_cached_engine(&self.posture.engine_cache_path);
        if let Some(sha) = &engine_sha256 {
            let _ = self.warmed_engine_sha.set(sha.clone());
        }

        tracing::info!(
            backend = "tensorrt",
            warmed_ms = warmed_ms as u64,
            engine_file = ?engine_file,
            engine_sha = ?engine_sha256,
            engine_bytes = ?engine_bytes,
            "TrtBackend warm-up complete (engine built/deserialized at startup; engine_sha captured)"
        );

        Ok(WarmUpReport {
            warmed_ms,
            engine_cache_path: self.posture.engine_cache_path.clone(),
            engine_file,
            engine_sha256,
            engine_bytes,
        })
    }
}

/// Outcome of [`TrtBackend::warm_up_report`] — the engine was forced to build/deserialize
/// before any real command, and the cached engine fingerprinted for the audit log.
#[derive(Debug, Clone)]
pub struct WarmUpReport {
    /// Wall time the warm-up took (cold build vs warm deserialize). HOST-INDICATIVE
    /// — a startup-budget figure, NOT a WCET/FTTI claim.
    pub warmed_ms: u128,
    /// The engine-cache directory inspected.
    pub engine_cache_path: String,
    /// The serialized-engine file fingerprinted (`None` if the cache was empty —
    /// e.g. the EP did not persist an engine).
    pub engine_file: Option<String>,
    /// SHA-256 (hex) of that engine file — mirrored into `TrtPosture.engine_sha`.
    pub engine_sha256: Option<String>,
    /// Size of that engine file in bytes.
    pub engine_bytes: Option<u64>,
}

/// SHA-256 the serialized TRT engine in `dir`: prefer a `*.engine` file, else the
/// largest file. Returns `(filename, hex_sha256, size_bytes)`; all `None` if the
/// directory is unreadable or empty. Pure filesystem read — no GPU.
fn sha256_cached_engine(dir: &str) -> (Option<String>, Option<String>, Option<u64>) {
    use sha2::{Digest, Sha256};

    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return (None, None, None),
    };
    // Prefer a *.engine file; among candidates (or all files if none end in
    // .engine), choose the largest — the serialized engine dominates the cache.
    let mut best: Option<(std::path::PathBuf, u64, bool)> = None; // (path, size, is_engine)
    for entry in read_dir.flatten() {
        let Ok(md) = entry.metadata() else { continue };
        if !md.is_file() {
            continue;
        }
        let path = entry.path();
        let is_engine = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("engine"))
            .unwrap_or(false);
        let size = md.len();
        let better = match &best {
            None => true,
            // An .engine file outranks a non-.engine; within the same class, larger wins.
            Some((_, b_size, b_is_engine)) => match (is_engine, b_is_engine) {
                (true, false) => true,
                (false, true) => false,
                _ => size > *b_size,
            },
        };
        if better {
            best = Some((path, size, is_engine));
        }
    }

    let Some((path, size, _)) = best else {
        return (None, None, None);
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    match std::fs::read(&path) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            (name, Some(hex::encode(hasher.finalize())), Some(size))
        }
        // Engine exists but couldn't be read — report name+size, no SHA.
        Err(_) => (name, None, Some(size)),
    }
}

impl InferenceBackend for TrtBackend {
    fn load_model(&self, path: &str) -> Result<ModelHandle, BackendError> {
        self.core.load_model(path)
    }

    fn run(&self, model: &ModelHandle, inputs: &TensorBatch)
        -> Result<TensorBatch<'static>, BackendError>
    {
        self.core.run(model, inputs)
    }

    /// The startup lifecycle hook (PARK-021 #2): build/cache the TRT engine now so
    /// the cold build never lands on the first real command. Delegates to
    /// [`Self::warm_up_report`] and discards the detailed report (the report is
    /// logged inside; `engine_sha()` exposes the captured SHA). Fail-closed: a
    /// build failure propagates so the node refuses to advertise ready.
    fn warm_up(&self, model: &ModelHandle) -> Result<(), BackendError> {
        self.warm_up_report(model).map(|_| ())
    }

    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor::TensorRT
    }

    fn capabilities(&self) -> BackendCapabilities {
        // Jetson-gated: the real fp16/int8/batch capabilities are measured on
        // hardware (PARK-021). The CI-buildable skeleton reports the conservative
        // full-precision posture it actually configures.
        BackendCapabilities {
            supports_int8: false,
            supports_fp16: false,
            max_batch_size: None,
        }
    }
}

/// PARK-021 JETSON-GATED FOLLOW-UPS — status as validated on a Jetson Orin NX
/// (JP6.2, onnxruntime-gpu 1.23.0). These cannot run on CI (no GPU); each is
/// exercised by a self-skipping on-hardware probe in `tests/` (set
/// `PARKO_TRT_REQUIRE_EP=1` to make a skip a hard failure). Issues: #414 (closed),
/// #415 (remaining decisions).
///
/// 1. **Real `load_model`/`run` output** — DONE (#414). `tests/positive_probe.rs`
///    is green on the Orin: `with_config` Ok, descriptor TensorRT, MNIST runs.
///    The inference path is shared (`OrtRunCore`).
/// 2. **Engine build / cache + warm-up** — IMPLEMENTED + WIRED (#415, ADR-0010).
///    The [`InferenceBackend::warm_up`] trait hook (default no-op) is overridden here
///    via [`TrtBackend::warm_up_report`] to force the per-model/shape engine build at
///    startup and capture the engine's SHA-256 (see [`TrtBackend::engine_sha`]).
///    `parko_ros2_node` calls it after `load_model`, fail-closed. `engine_cache_probe`
///    measured ~2.2 s cold vs warm reuse on the Orin; cache key includes the GPU arch
///    (`…_sm87.engine`). `tests/warmup_probe.rs` validates it.
/// 3. **Precision validation / TF32** — MEASURED (#415). `tests/tf32_probe.rs`
///    (one-shot differential): `NVIDIA_TF32_OVERRIDE=0` is INERT for the ort TRT
///    EP, and #4's drift is fp32-epsilon scale (~3e-7, ~3000× below TF32 ε), i.e.
///    TF32 is not engaged for MNIST. The override is therefore NOT a usable fp32
///    guarantee — **A2 (native nvinfer FFI), which exposes `BuilderFlag::kTF32`,
///    remains the escalation** if guaranteed fp32 is required on a larger model.
/// 4. **Cross-backend equivalence** — MEASURED (#415). `tests/equivalence_probe.rs`
///    asserts TRT-vs-CPU decision agreement and reports drift (Orin/MNIST: max
///    2.98e-7, ~2.5 ULP). Still TODO: finalize the **decision-agreement** tolerance
///    on a production-representative model and fold into the #152 harness. Anchor on
///    `TrtPosture`, NOT `InferenceThreads` (a CPU concept).
/// 5. **Perf / latency** — PARTIAL (#415). `tests/engine_cache_probe.rs` reports
///    cold(build+run) ~3.8 s vs warm ~1.6 s on the Orin (host-indicative, NOT a
///    WCET/FTTI claim). Full throughput / warm-vs-cold sweep on a real model: TODO.
/// 6. **Runtime confirmation** — DONE (#414, closed). The JP6.2 ORT carries a
///    usable TensorRT EP, so `with_config` succeeds on hardware (and correctly
///    fail-closes on CI's CPU-only build).
pub mod park021_jetson_gated {}

#[cfg(test)]
mod tests {
    use super::*;

    // GPU-FREE — these RUN on CI (no ORT runtime, no Session construction).

    #[test]
    fn trt_posture_defaults_are_full_precision() {
        let p = TrtPosture::full_precision("/tmp/cache");
        assert!(!p.fp16, "fp16 must default off (full precision)");
        assert!(!p.int8, "int8 must default off (no silent quantization)");
        assert_eq!(p.tf32, Tf32Control::UnenforcedPendingJetsonResolution);
        assert_eq!(p.engine_sha, None, "no engine SHA until built on hardware");
    }

    #[test]
    fn full_precision_is_not_guaranteed_while_tf32_unenforced() {
        // Honesty guard: even with fp16/int8 off, precision is NOT fully
        // guaranteed because TF32 is unenforceable from the EP. The flag — and
        // therefore the init log — must say false.
        let p = TrtPosture::full_precision("/tmp/cache");
        assert!(!p.full_precision_guaranteed(),
            "must not claim full precision while TF32 is unenforced");
    }

    #[test]
    fn tf32_status_is_honest_not_off() {
        let s = Tf32Control::UnenforcedPendingJetsonResolution.status_str();
        assert!(s.contains("UNENFORCED"), "status must surface that TF32 is unenforced");
        assert!(!s.to_lowercase().contains("off"),
            "status must NOT read as 'TF32 off'");
    }

    #[test]
    fn engine_cache_path_explicit_wins() {
        assert_eq!(resolve_engine_cache_path(Some("/var/parko/trt")), "/var/parko/trt");
    }

    #[test]
    fn engine_cache_path_falls_back_to_default() {
        // With no explicit path and (assuming) no env, the default is used.
        // Not asserting under a set env to keep this hermetic.
        if std::env::var("PARKO_TRT_ENGINE_CACHE").is_err() {
            assert_eq!(resolve_engine_cache_path(None), DEFAULT_ENGINE_CACHE_PATH);
        }
    }

    #[test]
    fn config_default_uses_resolved_path() {
        let c = TrtConfig::default();
        assert!(!c.engine_cache_path.is_empty());
    }
}
