//! Backend selection — PARK-022.
//!
//! `BackendSelector` resolves a [`BackendDescriptor`] to a concrete
//! [`InferenceBackend`], via three ordered rules (see [`BackendSelector::new`]):
//! a **registered factory** → a **compiled-in stub** → otherwise an **error**.
//!
//! # Topology note (why a registry, not direct construction)
//!
//! The original PARK-022 design had `parko-core` construct real backends
//! directly. That cannot work under the current crate topology: the real
//! backends live in crates *above* `parko-core` in the dependency DAG
//! (`parko-onnx` for `Cpu`/`Cuda`/the TensorRT execution provider,
//! `parko-tensorrt`, a future `parko-qnn`, …), and they depend on `parko-core`.
//! `parko-core` therefore cannot name or construct them without a dependency
//! cycle. The **factory registry** inverts that: higher crates *register* a
//! constructor function at startup, and `parko-core` calls it by descriptor —
//! no upward dependency, no cycle.
//!
//! ## Registration contract
//!
//! Each crate that provides a real backend registers a [`BackendFactory`] for
//! the descriptor(s) it implements, once, early in process startup (e.g. from
//! the integrator's `main`, or a crate init hook):
//!
//! ```ignore
//! // in parko-onnx (or the integrator's startup):
//! use parko_core::{register_backend_factory, BackendDescriptor, InferenceBackend, BackendError};
//!
//! fn make_cpu() -> Result<Box<dyn InferenceBackend>, BackendError> {
//!     Ok(Box::new(parko_onnx::OrtBackend::new_cpu()?))
//! }
//!
//! register_backend_factory(BackendDescriptor::Cpu, make_cpu);
//! register_backend_factory(BackendDescriptor::Cuda, make_cuda);
//! ```
//!
//! Re-registration is **last-wins**: a later [`register_backend_factory`] for
//! the same descriptor replaces the earlier one (see the test of the same
//! name). The registry is process-global and `Sync`.
//!
//! # Note on the fallback warning
//!
//! The PARK-022 spec calls for a `tracing::warn!` when a non-inference stub is
//! selected. `parko-core` deliberately carries **no `tracing` dependency** (its
//! only deps are `serde`, `thiserror`, `tokio`), and adding one for a single
//! log line is not warranted — so the stub-fallback warning is emitted to
//! `stderr` via `eprintln!` (std-only). The requirement (a *visible* warning
//! naming the descriptor and that it is a non-inference stub) is met without a
//! new dependency.
//
// SAFETY: parko backend selection | REQ: PARK-022 | TEST: backend_selector::tests::{unconstructible_descriptor_is_err_not_panic, register_backend_factory_is_last_wins, env_unknown_value_is_fail_closed_err, from_env_value_unset_is_default_mock}

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::backend::{BackendDescriptor, BackendError, InferenceBackend};
use crate::backends::mock::MockBackend;

/// Environment variable read by [`BackendSelector::from_env`].
pub const KIRRA_BACKEND_ENV: &str = "KIRRA_BACKEND";

/// A constructor for a real [`InferenceBackend`], registered by a higher crate.
///
/// A plain `fn` pointer (not a boxed closure) keeps the registry `Copy`/`Sync`
/// and capture-free: registration carries no state, only the constructor.
pub type BackendFactory = fn() -> Result<Box<dyn InferenceBackend>, BackendError>;

/// Process-global descriptor → factory registry.
///
/// Lazily initialized, guarded by an `RwLock`. The lock is only ever held
/// around the map insert/lookup — never across a factory call — so a panicking
/// factory cannot poison it.
static FACTORY_REGISTRY: OnceLock<RwLock<HashMap<BackendDescriptor, BackendFactory>>> =
    OnceLock::new();

fn registry() -> &'static RwLock<HashMap<BackendDescriptor, BackendFactory>> {
    FACTORY_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register a constructor for `descriptor`. **Last-wins**: a later call for the
/// same descriptor replaces the earlier one. Called by higher crates
/// (`parko-onnx`, `parko-tensorrt`, …) at startup — see the module docs.
pub fn register_backend_factory(descriptor: BackendDescriptor, f: BackendFactory) {
    registry()
        .write()
        .expect("backend factory registry lock poisoned")
        .insert(descriptor, f);
}

/// Look up a registered factory for `descriptor`, if any. `fn` pointers are
/// `Copy`, so this clones the pointer out and drops the read lock immediately.
fn registered_factory(descriptor: &BackendDescriptor) -> Option<BackendFactory> {
    registry()
        .read()
        .expect("backend factory registry lock poisoned")
        .get(descriptor)
        .copied()
}

/// Build the compiled-in **stub** for `descriptor`, if one is compiled.
///
/// Mirrors the `cfg` structure of `backends/mod.rs` exactly: each arm exists
/// only when its `backend-*` feature is enabled, otherwise the descriptor falls
/// through to `None`. Stubs are zero-output, non-inference placeholders for CI /
/// hardware-absent builds.
fn compiled_stub(descriptor: &BackendDescriptor) -> Option<Box<dyn InferenceBackend>> {
    match descriptor {
        #[cfg(feature = "backend-tensorrt")]
        BackendDescriptor::TensorRT => Some(Box::new(crate::backends::TensorRTStubBackend)),
        #[cfg(feature = "backend-qnn")]
        BackendDescriptor::QualcommQnn => Some(Box::new(crate::backends::QnnStubBackend)),
        #[cfg(feature = "backend-tidl")]
        BackendDescriptor::TiTidl => Some(Box::new(crate::backends::TidlStubBackend)),
        #[cfg(feature = "backend-openvino")]
        BackendDescriptor::IntelOpenVino => Some(Box::new(crate::backends::OpenVinoStubBackend)),
        #[cfg(feature = "backend-amd")]
        BackendDescriptor::AmdVitis => Some(Box::new(crate::backends::AmdStubBackend)),
        _ => None,
    }
}

/// Map a `KIRRA_BACKEND` string to a [`BackendDescriptor`], case-insensitively.
///
/// Accepted values: `cpu | cuda | tensorrt | qnn | tidl | openvino | vitis`.
/// An unrecognized value is an **error** (fail-closed) — a typo must never
/// silently select a different backend.
pub fn descriptor_from_env_str(value: &str) -> Result<BackendDescriptor, BackendError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "cpu" => Ok(BackendDescriptor::Cpu),
        "cuda" => Ok(BackendDescriptor::Cuda),
        "tensorrt" => Ok(BackendDescriptor::TensorRT),
        "qnn" => Ok(BackendDescriptor::QualcommQnn),
        "tidl" => Ok(BackendDescriptor::TiTidl),
        "openvino" => Ok(BackendDescriptor::IntelOpenVino),
        "vitis" => Ok(BackendDescriptor::AmdVitis),
        other => Err(BackendError::InitializationError(format!(
            "unknown {KIRRA_BACKEND_ENV} value {other:?}; \
             expected one of cpu|cuda|tensorrt|qnn|tidl|openvino|vitis"
        ))),
    }
}

/// The deployment platform a backend would run on.
///
/// An explicit *parameter* (not hidden behind `cfg`) so every QNX-safe selection
/// rule (PARK-026; `parko/QNX_BACKEND_SELECTION.md`) is testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPlatform {
    Linux,
    Qnx,
    Other,
}

/// The platform this binary was compiled for.
///
/// QNX Neutrino is `target_os = "nto"`. This `cfg` is the *only* compile-target
/// dependence; the policy ([`backend_permitted`]) takes the platform as an
/// argument, so the rules themselves are host-testable.
#[must_use]
pub fn current_platform() -> TargetPlatform {
    #[cfg(target_os = "nto")]
    {
        TargetPlatform::Qnx
    }
    #[cfg(target_os = "linux")]
    {
        TargetPlatform::Linux
    }
    #[cfg(not(any(target_os = "nto", target_os = "linux")))]
    {
        TargetPlatform::Other
    }
}

// SAFETY: parko backend selection | REQ: PARK-026 | TEST: backend_selector::tests::{off_qnx_permits_every_backend, qnx_forbids_cuda_and_tensorrt, qnx_pending_backends_denied_naming_36, platform_gate_precedes_factory}
/// Apply the QNX-safe backend selection rules (PARK-026) for `platform`.
///
/// - **`Linux` / `Other` → always `Ok`.** Off-QNX behavior is unchanged — the held
///   line: existing users see zero difference; the QNX rules engage only on QNX.
/// - **`Qnx` → the `parko/QNX_BACKEND_SELECTION.md` §3 allowlist.** `Cuda` and
///   `TensorRT` are **FORBIDDEN** (no CUDA/TensorRT runtime on QNX Neutrino); every
///   other descriptor is **PENDING-#36** and **denied by default** until the #36
///   spike confirms the target's POSIX facilities. Both return `Err` naming the
///   rule doc; the pending case also names #36. Deny-by-default, never
///   allow-until-denied — and the `_` arm denies any future (`#[non_exhaustive]`)
///   descriptor too.
pub fn backend_permitted(
    platform: TargetPlatform,
    d: BackendDescriptor,
) -> Result<(), BackendError> {
    const DOC: &str = "parko/QNX_BACKEND_SELECTION.md";
    match platform {
        TargetPlatform::Linux | TargetPlatform::Other => Ok(()),
        TargetPlatform::Qnx => match d {
            // FORBIDDEN — no CUDA/TensorRT runtime on standard QNX Neutrino.
            BackendDescriptor::Cuda | BackendDescriptor::TensorRT => {
                Err(BackendError::InitializationError(format!(
                    "backend {d:?} is FORBIDDEN on QNX (no CUDA/TensorRT runtime on \
                     QNX Neutrino); see {DOC} §3"
                )))
            }
            // PENDING-#36 — deny-by-default until the QNX deployment spike (#36)
            // confirms the required POSIX facilities. `_` also denies any future
            // descriptor — conservative by design.
            _ => Err(BackendError::InitializationError(format!(
                "backend {d:?} is not yet permitted on QNX: POSIX availability \
                 unconfirmed (deny-by-default until PARK-024 / #36 confirms); see {DOC} §3"
            ))),
        },
    }
}

/// Owns the selected inference backend.
///
/// Derefs to `dyn InferenceBackend`, so a `BackendSelector` is usable directly
/// as the backend (`selector.load_model(..)`, `selector.run(..)`); [`backend`]
/// and [`into_inner`] are explicit accessors.
///
/// [`backend`]: BackendSelector::backend
/// [`into_inner`]: BackendSelector::into_inner
pub struct BackendSelector(Box<dyn InferenceBackend>);

impl BackendSelector {
    /// Resolve `descriptor` to a backend, in priority order:
    ///
    /// 1. a **registered factory** for `descriptor` → use it (the real-backend
    ///    path; a factory returning `Err` propagates that error);
    /// 2. else a **compiled-in stub** for `descriptor` (its `backend-*` feature
    ///    is enabled) → use the stub, after emitting a stderr warning that this
    ///    is a non-inference placeholder;
    /// 3. else **`Err`** — a descriptor with neither a factory nor a compiled
    ///    stub is unconstructible. This is always an error, **never a panic**
    ///    and **never a silent substitution** of a different backend.
    pub fn new(descriptor: BackendDescriptor) -> Result<Self, BackendError> {
        Self::new_for_platform(current_platform(), descriptor)
    }

    /// [`new`] with the platform as an explicit argument, so the QNX gate
    /// (PARK-026) is host-testable. The platform rule is consulted **first** —
    /// before factory or stub resolution — so a registered factory cannot bypass
    /// it (`parko/QNX_BACKEND_SELECTION.md` §4).
    ///
    /// [`new`]: BackendSelector::new
    fn new_for_platform(
        platform: TargetPlatform,
        descriptor: BackendDescriptor,
    ) -> Result<Self, BackendError> {
        // PARK-026 platform gate, BEFORE any factory/stub resolution.
        backend_permitted(platform, descriptor.clone())?;

        // 1. Registered real backend wins.
        if let Some(factory) = registered_factory(&descriptor) {
            return factory().map(BackendSelector);
        }

        // 2. Compiled-in stub fallback (feature-gated). Emit a visible warning —
        //    see the module docs for why this is `eprintln!`, not `tracing`.
        if let Some(stub) = compiled_stub(&descriptor) {
            eprintln!(
                "[parko-core] WARNING: BackendSelector is using the {descriptor:?} \
                 NON-INFERENCE STUB (no factory registered, no real backend available). \
                 This backend produces ZERO output and must not drive production inference. \
                 Register a real backend via register_backend_factory({descriptor:?}, ...)."
            );
            return Ok(BackendSelector(stub));
        }

        // 3. Unconstructible → fail-closed error.
        Err(BackendError::InitializationError(format!(
            "no backend available for {descriptor:?}: no factory registered and no \
             compiled-in stub (enable the matching backend-* feature, or register a factory)"
        )))
    }

    /// Select a backend from the `KIRRA_BACKEND` environment variable.
    ///
    /// Unset → the deterministic default (see [`from_env_value`]); a recognized
    /// value → [`new`] for the mapped descriptor; an unrecognized value → `Err`
    /// (fail-closed).
    ///
    /// [`from_env_value`]: BackendSelector::from_env_value
    /// [`new`]: BackendSelector::new
    pub fn from_env() -> Result<Self, BackendError> {
        // Read once; the pure routing lives in `from_env_value` so it is testable
        // without mutating process env (parko/kirra invariant: no set_var in tests).
        let raw = std::env::var(KIRRA_BACKEND_ENV).ok();
        Self::from_env_value(raw.as_deref())
    }

    /// The env-routing logic, separated from the env read for testability.
    ///
    /// - `None` (or whitespace-only) → the **default**: a deterministic
    ///   [`MockBackend`]. Absent an explicit operator choice, the safety runtime
    ///   selects a predictable zero-output backend rather than auto-engaging real
    ///   hardware inference — engaging a real accelerator must be a deliberate,
    ///   configured act. (`Cpu`-if-registered was the alternative; `Mock` is
    ///   chosen because it assumes no hardware and is bitwise-deterministic.)
    /// - `Some(name)` → [`descriptor_from_env_str`] then [`new`]; an unknown
    ///   `name` is an `Err` (fail-closed — never a silent different backend).
    ///
    /// [`new`]: BackendSelector::new
    pub fn from_env_value(value: Option<&str>) -> Result<Self, BackendError> {
        match value {
            None => Ok(Self::default_mock()),
            Some(s) if s.trim().is_empty() => Ok(Self::default_mock()),
            Some(s) => Self::new(descriptor_from_env_str(s)?),
        }
    }

    /// The unconfigured default: a deterministic zero-output `MockBackend`.
    ///
    /// Intentionally bypasses the PARK-026 platform gate: the Mock uses no
    /// hardware and no restricted POSIX surface, so it is safe on any target and
    /// is the correct fail-safe fallback on QNX when no real backend is permitted.
    fn default_mock() -> Self {
        BackendSelector(Box::new(MockBackend::new(
            HashMap::new(),
            BackendDescriptor::Cpu,
        )))
    }

    /// Borrow the selected backend.
    pub fn backend(&self) -> &dyn InferenceBackend {
        self.0.as_ref()
    }

    /// Consume the selector, yielding the owned backend.
    pub fn into_inner(self) -> Box<dyn InferenceBackend> {
        self.0
    }
}

impl std::ops::Deref for BackendSelector {
    type Target = dyn InferenceBackend;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

// Manual Debug: `Box<dyn InferenceBackend>` is not `Debug`, so derive can't apply.
// Reporting the descriptor is the useful identity here.
impl std::fmt::Debug for BackendSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BackendSelector")
            .field(&self.0.descriptor())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendDescriptor;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Every descriptor, for the platform-policy sweeps.
    fn all_descriptors() -> [BackendDescriptor; 7] {
        [
            BackendDescriptor::Cpu,
            BackendDescriptor::Cuda,
            BackendDescriptor::TensorRT,
            BackendDescriptor::QualcommQnn,
            BackendDescriptor::TiTidl,
            BackendDescriptor::IntelOpenVino,
            BackendDescriptor::AmdVitis,
        ]
    }

    // The factory registry is process-global and persists across the whole test
    // binary, which runs tests in parallel. To stay race-free, each registry-
    // mutating / constructing test OWNS a DISTINCT descriptor that no other test
    // registers or constructs:
    //   - `Cuda`          → unconstructible_descriptor_is_err_not_panic (never registered; no stub)
    //   - `TensorRT`      → registry_first_then_stub_then_err (feature lane only; has a stub)
    //   - `QualcommQnn`   → register_backend_factory_is_last_wins
    //   - `TiTidl`        → factory_err_propagates
    //   - `IntelOpenVino` → from_env_value_recognized_routes_to_new
    // `Cpu` is only ever constructed directly (default_mock), never registered.
    // Pure-parsing tests (descriptor_from_env_str) touch no global state.
    // (A registered factory always wins over any compiled stub, so the stub
    // descriptors above resolve deterministically in BOTH feature lanes.)

    fn ok_mock_cpu() -> Result<Box<dyn InferenceBackend>, BackendError> {
        Ok(Box::new(MockBackend::new(HashMap::new(), BackendDescriptor::Cpu)))
    }

    #[test]
    fn unconstructible_descriptor_is_err_not_panic() {
        // `Cuda` has no compiled stub in any feature configuration and no test
        // registers a Cuda factory → always an Err, never a panic.
        let err = BackendSelector::new(BackendDescriptor::Cuda).unwrap_err();
        assert!(
            matches!(err, BackendError::InitializationError(_)),
            "unconstructible descriptor must be InitializationError, got {err:?}"
        );
    }

    #[cfg(feature = "backend-tensorrt")]
    #[test]
    fn registry_first_then_stub_then_err() {
        // Feature on, no factory yet → the compiled TensorRT stub is returned.
        let stub = BackendSelector::new(BackendDescriptor::TensorRT).unwrap();
        assert_eq!(stub.descriptor(), BackendDescriptor::TensorRT);
        assert_eq!(
            stub.load_model("m").unwrap().model_id,
            "tensorrt-stub",
            "with no factory + feature on, the non-inference stub must be used"
        );

        // Register a real factory → it must now WIN over the stub.
        register_backend_factory(BackendDescriptor::TensorRT, ok_mock_cpu);
        let real = BackendSelector::new(BackendDescriptor::TensorRT).unwrap();
        assert_eq!(
            real.descriptor(),
            BackendDescriptor::Cpu,
            "a registered factory must win over the compiled stub"
        );
        assert_eq!(real.load_model("m").unwrap().model_id, "mock::m");
    }

    #[test]
    fn register_backend_factory_is_last_wins() {
        fn first() -> Result<Box<dyn InferenceBackend>, BackendError> {
            Ok(Box::new(MockBackend::new(HashMap::new(), BackendDescriptor::Cpu)))
        }
        fn second() -> Result<Box<dyn InferenceBackend>, BackendError> {
            // distinguishable from `first` via a non-default output map
            Ok(Box::new(MockBackend::new(
                HashMap::from([("sentinel".to_string(), vec![1.0_f32])]),
                BackendDescriptor::Cpu,
            )))
        }
        // Owns `QualcommQnn`. A registered factory wins over the stub (if any),
        // so this is deterministic regardless of the backend-qnn feature.
        register_backend_factory(BackendDescriptor::QualcommQnn, first);
        register_backend_factory(BackendDescriptor::QualcommQnn, second);
        let sel = BackendSelector::new(BackendDescriptor::QualcommQnn).unwrap();
        // `second` declared one output shape; `first` declared none → proves last-wins.
        assert_eq!(
            sel.load_model("m").unwrap().output_shapes.len(),
            1,
            "last registration must win"
        );
    }

    #[test]
    fn factory_err_propagates() {
        fn boom() -> Result<Box<dyn InferenceBackend>, BackendError> {
            Err(BackendError::InitializationError("boom".into()))
        }
        // Owns `TiTidl` (no other test registers or constructs it).
        register_backend_factory(BackendDescriptor::TiTidl, boom);
        let err = BackendSelector::new(BackendDescriptor::TiTidl).unwrap_err();
        assert!(
            matches!(&err, BackendError::InitializationError(m) if m.contains("boom")),
            "a factory's Err must propagate unchanged, got {err:?}"
        );
    }

    #[test]
    fn descriptor_from_env_str_maps_and_is_case_insensitive() {
        assert_eq!(descriptor_from_env_str("cpu").unwrap(), BackendDescriptor::Cpu);
        assert_eq!(descriptor_from_env_str("cuda").unwrap(), BackendDescriptor::Cuda);
        assert_eq!(descriptor_from_env_str("tensorrt").unwrap(), BackendDescriptor::TensorRT);
        assert_eq!(descriptor_from_env_str("TENSORRT").unwrap(), BackendDescriptor::TensorRT);
        assert_eq!(descriptor_from_env_str("  TensorRT  ").unwrap(), BackendDescriptor::TensorRT);
        assert_eq!(descriptor_from_env_str("qnn").unwrap(), BackendDescriptor::QualcommQnn);
        assert_eq!(descriptor_from_env_str("tidl").unwrap(), BackendDescriptor::TiTidl);
        assert_eq!(descriptor_from_env_str("openvino").unwrap(), BackendDescriptor::IntelOpenVino);
        assert_eq!(descriptor_from_env_str("vitis").unwrap(), BackendDescriptor::AmdVitis);
    }

    #[test]
    fn env_unknown_value_is_fail_closed_err() {
        // A typo'd backend name must FAIL, never silently select something else.
        assert!(descriptor_from_env_str("garbage").is_err());
        assert!(BackendSelector::from_env_value(Some("garbage")).is_err());
        assert!(BackendSelector::from_env_value(Some("tensor-rt")).is_err());
    }

    #[test]
    fn from_env_value_unset_is_default_mock() {
        // Unset and whitespace-only both resolve to the deterministic Mock default.
        for v in [None, Some(""), Some("   ")] {
            let sel = BackendSelector::from_env_value(v).unwrap();
            assert_eq!(sel.descriptor(), BackendDescriptor::Cpu);
            assert_eq!(
                sel.load_model("m").unwrap().model_id,
                "mock::m",
                "default must be the Mock backend"
            );
        }
    }

    #[test]
    fn from_env_value_recognized_routes_to_new() {
        // Owns `IntelOpenVino`: register a factory, then prove the env path
        // string→descriptor→new→factory works end to end.
        register_backend_factory(BackendDescriptor::IntelOpenVino, ok_mock_cpu);
        let sel = BackendSelector::from_env_value(Some("OpenVino")).unwrap();
        assert_eq!(sel.load_model("m").unwrap().model_id, "mock::m");
    }

    // ---- PARK-026 — QNX-safe backend selection rules -----------------------

    #[test]
    fn off_qnx_permits_every_backend() {
        // The held line: off-QNX is unchanged — every descriptor is permitted.
        for p in [TargetPlatform::Linux, TargetPlatform::Other] {
            for d in all_descriptors() {
                assert!(
                    backend_permitted(p, d.clone()).is_ok(),
                    "{p:?} must permit {d:?} (zero behavior change off-QNX)"
                );
            }
        }
    }

    #[test]
    fn qnx_forbids_cuda_and_tensorrt() {
        for d in [BackendDescriptor::Cuda, BackendDescriptor::TensorRT] {
            let msg = backend_permitted(TargetPlatform::Qnx, d.clone())
                .unwrap_err()
                .to_string();
            assert!(
                msg.contains("FORBIDDEN") && msg.contains("QNX_BACKEND_SELECTION.md"),
                "{d:?} on QNX must be FORBIDDEN and name the rule doc, got: {msg}"
            );
        }
    }

    #[test]
    fn qnx_pending_backends_denied_naming_36() {
        // PENDING rows are denied by default and name #36 + the rule doc.
        for d in [
            BackendDescriptor::Cpu,
            BackendDescriptor::QualcommQnn,
            BackendDescriptor::TiTidl,
            BackendDescriptor::IntelOpenVino,
            BackendDescriptor::AmdVitis,
        ] {
            let msg = backend_permitted(TargetPlatform::Qnx, d.clone())
                .unwrap_err()
                .to_string();
            assert!(
                msg.contains("#36") && msg.contains("QNX_BACKEND_SELECTION.md"),
                "PENDING backend {d:?} on QNX must be denied naming #36 + the doc, got: {msg}"
            );
        }
    }

    #[test]
    fn qnx_is_currently_deny_all() {
        // Conservative starting posture: NO row is CONFIRMED yet, so QNX denies
        // every descriptor (FORBIDDEN or PENDING-#36). Encodes deny-by-default.
        for d in all_descriptors() {
            assert!(
                backend_permitted(TargetPlatform::Qnx, d.clone()).is_err(),
                "{d:?} must be denied on QNX (deny-by-default until #36)"
            );
        }
    }

    // Owns `AmdVitis`. A fn-pointer factory flags whether it ran.
    static ORDERING_FACTORY_CALLED: AtomicBool = AtomicBool::new(false);
    fn ordering_flagging_factory() -> Result<Box<dyn InferenceBackend>, BackendError> {
        ORDERING_FACTORY_CALLED.store(true, Ordering::SeqCst);
        Ok(Box::new(MockBackend::new(HashMap::new(), BackendDescriptor::Cpu)))
    }

    #[test]
    fn platform_gate_precedes_factory() {
        register_backend_factory(BackendDescriptor::AmdVitis, ordering_flagging_factory);

        // On QNX the gate denies (AmdVitis is PENDING) BEFORE the factory runs —
        // a registered factory must NOT bypass the platform rule.
        ORDERING_FACTORY_CALLED.store(false, Ordering::SeqCst);
        let err = BackendSelector::new_for_platform(TargetPlatform::Qnx, BackendDescriptor::AmdVitis)
            .unwrap_err();
        assert!(
            !ORDERING_FACTORY_CALLED.load(Ordering::SeqCst),
            "platform gate must deny before the registered factory is consulted"
        );
        assert!(matches!(err, BackendError::InitializationError(_)));

        // Off-QNX the same descriptor + factory resolves normally (held line).
        ORDERING_FACTORY_CALLED.store(false, Ordering::SeqCst);
        let sel =
            BackendSelector::new_for_platform(TargetPlatform::Linux, BackendDescriptor::AmdVitis)
                .unwrap();
        assert!(
            ORDERING_FACTORY_CALLED.load(Ordering::SeqCst),
            "off-QNX the registered factory must run (zero behavior change)"
        );
        assert_eq!(sel.descriptor(), BackendDescriptor::Cpu);
    }
}
