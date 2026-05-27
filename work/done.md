# Completed Work

Completed tasks will be appended here weekly.

---

## PARK-001 — Attach `SafetyGovernor` to `ControlLoop`

**Completed:** 2026-05-26 | **Commit:** `10f8c88` | **Branch:** `claude/claude-md-reference-AtTWy`

- `with_governor(impl SafetyGovernor + 'static) -> Self` added to both `InferenceLoop` and `ControlLoop`; governor boxed internally.
- Built-in scalar clamp suppressed when governor is present (ADL-002).
- `test_builtin_clamp_suppressed` and `test_no_governor_uses_builtin_clamp` added.
- Stale Aegis references fixed in runtime.rs and scheduler.rs doc comments.
- 31 tests pass (28 unit + 3 integration). No unsafe code.

---

## PARK-002 — Add test-only posture state setter

**Completed:** 2026-05-26 | **Commit:** `c6bcb0a` | **Branch:** `claude/claude-md-reference-AtTWy`

- `set_state_for_test` gated with `#[cfg(any(test, feature = "test-helpers"))]`.
- `test-helpers` Cargo feature added; absent from release builds (nm confirmed).
- `[[test]] required-features = ["test-helpers"]` for test_posture_divergence target.
- Inline unit test `set_state_for_test_overrides_initial_warmup_state` added.
- 29 unit tests pass; 4 integration tests pass with `--features test-helpers`.

---

## PARK-003 — Write posture divergence property test

**Completed:** 2026-05-26 | **Commit:** TBD | **Branch:** `claude/claude-md-reference-AtTWy`

- Proptest suite in `tests/posture_divergence_proptest.rs`: 4 properties × 10,000 cases each.
- Properties verified: nominal ceiling ≤ 35.0, degraded ceiling ≤ 5.0, locked-out = fallback (5.0), locked-out ≡ degraded.
- Discovered: LockedOut uses MRC fallback profile (same as Degraded), not a hard-veto; nominal profile has stricter rate-of-change limits than fallback.
- proptest = "1" added to dev-dependencies; `*.proptest-regressions` added to .gitignore.
- All 29 unit + 4 proptest tests pass (`cargo test -p parko-core`). No unsafe code.

---

## PARK-014 — Lateral RSS safe-distance — first implementation
Completed: 2026-05-27
Commit: 111e7d0
Labels: behavioral-safety

Notes: lateral_stop_distance() closure avoids duplicating the three-step
calculation for ego and object. test_lateral_negative_velocity_matches_positive
verifies the .abs() contract — negating both velocities must produce identical
margin. parko-core: 54 unit tests + 4 proptests.

---

## PARK-013 — Longitudinal RSS safe-distance — first implementation
Completed: 2026-05-27
Commit: a40948e
Labels: behavioral-safety

Notes: IEEE 2846-2022 §5.1 formula implemented in parko-core/src/rss.rs.
RssState struct added. Expected values computed as exact rational fractions
(487/48, 142/3) to eliminate floating-point rounding ambiguity.
parko-core: 49 unit tests + 4 proptests.

---

## PARK-012 — Feature-gated stub backends for CI
Completed: 2026-05-27
Commit: f4d1803
Labels: backend-architecture

Notes: 5 stub files in backends/ — file-level #![cfg(feature="...")] gates
entire file cleanly. Each stub implements InferenceBackend returning empty
TensorBatch and BackendCapabilities::default(). Feature flags added:
backend-tensorrt, backend-qnn, backend-tidl, backend-openvino, backend-amd.
Test counts: baseline 44 unit + 4 proptests; each stub adds 2; all five → 54.

---

## PARK-011 — Define backend capability reporting
Completed: 2026-05-27
Commit: 0a50a0d
Labels: backend-architecture

Notes: BackendCapabilities derives Default — all 5 existing backends
inherit capabilities() from trait default (net 9 fewer lines).
descriptor_vendor() exhaustively matches all 6 BackendDescriptor variants
(no wildcard — non_exhaustive doesn't require it within the defining crate).
capabilities_precision() bridges to RuntimeTelemetry.backend_precision via
PrecisionMode (INT8/FP16/FP32) without new struct fields.
parko-core: 44 unit + 4 proptests. parko-onnx: 3 integration tests.

---

## PARK-010 — MockBackend for parko-core unit tests
Completed: 2026-05-27
Commit: 58c197b
Labels: backend-architecture

What landed:
- parko-core/src/backends/mock.rs: MockBackend implements InferenceBackend
- parko-core/src/backends/mod.rs: new backends/ submodule
- lib.rs: pub mod backends + pub use backends::mock::MockBackend

Notes: output_data stored as HashMap<String, Vec<f32>> — run() produces
fresh TensorBatch<'static> via TensorStorage::Owned on each call, avoiding
Clone requirement on TensorBatch. call_count uses AtomicU64 for Send+Sync
without &mut self. No cfg gate — fully public; downstream test crates use
parko_core::MockBackend directly.

7 new unit tests: run output, repeatability, call count, descriptor,
load_model shape, capabilities, Send+Sync compile-time assertion.

Test count after PARK-010: 43 parko-core unit tests (was 34 after PARK-005).

---

## PARK-009 — Validate parko-onnx CPU backend; fix hanging MNIST test
Completed: 2026-05-26
Commit: dff915c
Labels: parko-onnx, hal

What landed:
- parko/.cargo/config.toml: sets ORT_DYLIB_PATH to the installed shared library
  location so cargo test -p parko-onnx works without manual env var exports
- OrtBackend::new(): adds with_intra_threads(1) and
  with_optimization_level(GraphOptimizationLevel::Disable) to prevent the ORT
  session builder from blocking indefinitely during initialization
- tests/test_onnx_backend.rs: adds test_ort_backend_descriptor_is_cpu —
  verifies OrtBackend::descriptor() returns BackendDescriptor::Cpu

Root cause of hang: libonnxruntime.so at /root/.local/onnxruntime/lib/ was not
on the standard library search path. ORT_DYLIB_PATH in .cargo/config.toml
resolves this for all cargo subcommands in the parko workspace.

Key naming (ADL-007):
- .cargo/config.toml is per-workspace; new deployment targets (Jetson, QNX)
  need their own equivalent entry for that platform's ORT installation
- OrtBackend::descriptor() inherits the default impl from InferenceBackend
  (added PARK-008) — no override needed

Test count after PARK-009: 2 integration tests pass (cargo test -p parko-onnx)
Both tests complete in < 1s (previously: hung > 60s)

---

## PARK-016 — RSS pre-actuator gate in KirraGovernor
Completed: 2026-05-27
Commit: 470027b
Labels: kirra-governor, behavioral-safety
Notes: Governor method is evaluate() not enforce(). Command type is
&ControlCommand → EnforcementAction. apply_mrc_profile() extracted
from inline Degraded branch — single code path for Degraded and RSS
unsafe. Three-tier priority in evaluate(): LockedOut hard stop → RSS
gate (Degraded semantics) → kinematic envelope checks. All three
constructors (new, nominal, mrc_fallback) initialize rss_state to
safe=true, margins=f64::MAX. MRC_VELOCITY_CEILING_MPS is single source
of truth — no bare 5.0 in source. Tests A-E all pass.
parko-kirra: 10 unit + 3 integration tests pass.

---

## PARK-015 — Wire RssState into posture engine

**Completed:** 2026-05-27 | **Commit:** `31b8979` | **Branch:** `claude/claude-md-reference-AtTWy`

- `parko-core` added to root Cargo.toml; `RssState` derives `Debug + Clone`.
- `AppState`: `rss_active_violation: Arc<AtomicBool>` + `rss_recovery_streak: Arc<Mutex<RssRecoveryStreak>>`.
- `PostureRecalcTrigger::RssViolation(RssState)` added; `Display` updated.
- `apply_rss_state()`: violation activates flag and resets streak; safe ticks advance streak; recovery confirmed at `AV_RECOVERY_STREAK_THRESHOLD` (5) within `AV_RECOVERY_WINDOW_MS` (10 s).
- Posture engine worker processes `RssViolation` before calling `recalculate_and_broadcast`.
- `recalculate_and_broadcast`: active violation escalates `Nominal` → `Degraded`; `LockedOut` from DAG is never downgraded.
- `ScenarioEvent::RssReport(RssState)` added to `ScenarioRunner`.
- `tests/rss_posture_tests.rs`: `test_rss_violation_degrades_nominal_posture`, `test_rss_recovery_requires_full_streak`.
- 319 unit + 16 integration tests pass (335 total). No unsafe code.

---

## PARK-005 — RuntimeClock / MockClock abstraction in ControlLoop
Completed: 2026-05-26
Commit: a50363d
Labels: control-loop

What landed:
- clock.rs: Clock trait, WallClock (production), MockClock (test double)
- ControlLoop<B>: clock field (Arc<dyn Clock>), tick_interval_ms, last_tick_ms
- with_clock(Arc<dyn Clock>) builder
- #[cfg(test)] with_tick_interval_ms(u64) builder
- tick() return type: Result<Option<PostureSnapshot>, String>
- 2 new tests: test_mock_clock_tick_count, test_runtime_clock_default_smoke

Key naming decision (ADL-006):
- WallClock = injectable Clock trait impl (clock.rs)
- RuntimeClock = sleep-based tick driver (runtime.rs) — unchanged
- MockClock = test double with Arc<AtomicU64> and advance(ms)

Test count after PARK-005: 34 (parko-core)
