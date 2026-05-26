# Backlog

> 40 tasks derived from the roadmap. Pull into `active.md` when starting.
> Move to `done.md` on merge. Max 3 tasks in `active.md` at once.
>
> Labels: `backend-qnn` `backend-tidl` `backend-openvino` `backend-rocm`
>         `behavioral-safety` `control-loop` `aegis-integration`
>         `docs` `packaging` `simulation`

---

## Increment 1 — Deterministic Runtime Core

---

### PARK-001 `control-loop`
**Attach `SafetyGovernor` to `ControlLoop`**

Implement `ControlLoop::with_governor(impl SafetyGovernor + 'static)` in
`parko-core`. When a governor is present the built-in scalar clamp must be
suppressed so the two enforcement paths cannot conflict. Add
`test_builtin_clamp_suppressed` to confirm the built-in path is bypassed when a
governor is attached.

#### Claude Code Prompt
```
In the parko-core crate (parko/parko-core/src/control_loop.rs), implement a
with_governor builder method on ControlLoop.

Requirements:
- Signature: pub fn with_governor(mut self, g: impl SafetyGovernor + 'static) -> Self
- Store the governor as Option<Box<dyn SafetyGovernor>> on the ControlLoop struct
- When a governor is present, suppress the built-in scalar clamp entirely so both
  paths cannot run on the same tick
- When no governor is present (default), the built-in clamp runs as before
- Add a test named test_builtin_clamp_suppressed that attaches a mock governor,
  sends a value the built-in clamp would reduce, and asserts the built-in clamp
  did NOT run (governor output is returned instead)
- All existing parko-core tests must continue to pass
- No unsafe code
```

---

### PARK-002 `control-loop`
**Add test-only posture state setter**

Add `set_state_for_test(state: PostureState)` to `parko-core` behind
`#[cfg(test)]`. This unblocks posture-divergence and recovery tests without
exposing a mutation path in production binaries. Confirm the method is absent
from the release binary using `nm`.

#### Claude Code Prompt
```
In parko/parko-core/src/, add a test-only state mutation method to ControlLoop
(or whichever struct owns PostureState).

Requirements:
- Annotate with #[cfg(test)] so it is completely absent from release builds
- Signature: pub fn set_state_for_test(&mut self, state: PostureState)
- The method sets the internal posture state directly with no transition
  validation (it is a test seam, not a production API)
- Add a compile-time assertion test that calls nm on the release binary and
  confirms the symbol does not appear (or write a doc comment explaining the
  manual verification step)
- All existing parko-core tests must continue to pass
```

---

### PARK-003 `control-loop`
**Posture-divergence property test**

Write a `proptest` suite in `parko-core` asserting that for all valid
`(proposed_output: f32, posture_state: PostureState)` inputs the
`KirraKernelGovernor`'s result is at least as conservative as the built-in clamp.
This is the core correctness invariant for the governor-as-SafetyGovernor path.

#### Claude Code Prompt
```
In parko/parko-core/tests/posture_divergence.rs (create if it does not exist),
write a proptest suite for the governor/clamp divergence invariant.

Requirements:
- Use proptest to generate (proposed_output: f32, posture_state: PostureState)
  pairs, filtering out NaN and Inf
- For each pair, run both the KirraKernelGovernor (from kirra-runtime-sdk, used
  via parko-aegis) and the parko-core built-in clamp
- Assert: governor_output <= builtin_clamp_output for all Nominal and Degraded
  states; governor_output == 0.0 (or Halt) for LockedOut
- Run at least 10_000 cases (set proptest config accordingly)
- All three PostureState variants must be covered
- proptest is already a dev-dependency in the workspace; do not add new
  non-dev dependencies
```

---

### PARK-004 `control-loop`
**NaN/Inf rejection at tick boundary**

Add an input guard at the top of `ControlLoop::tick` that rejects any `NaN` or
`Inf` float with `EnforcementAction::Halt` before the value reaches the governor
or built-in clamp. This prevents undefined behavior from propagating through the
safety stack.

#### Claude Code Prompt
```
In parko/parko-core/src/control_loop.rs, add an input validation guard at the
entry of ControlLoop::tick (or the equivalent tick method).

Requirements:
- Before any governor or clamp logic, check if the input contains NaN or Inf
- If any NaN or Inf is detected, return EnforcementAction::Halt immediately
  without calling the governor
- Add a proptest that generates adversarial f32 values (including NaN, Inf,
  -Inf, subnormals) and asserts all NaN/Inf inputs produce Halt
- Add a unit test confirming a normal f32 still reaches the governor unchanged
- No change to existing governor or clamp logic
```

---

### PARK-005 `control-loop`
**`VirtualClock` integration in `ControlLoop`**

Wire the `VirtualClock` / `SystemClock` abstraction (already in
`kirra-runtime-sdk`) into `parko-core`'s `ControlLoop` so tick timing is
injectable in tests. This enables deterministic temporal tests without sleep.

#### Claude Code Prompt
```
In parko/parko-core/src/control_loop.rs, wire the Clock trait (VirtualClock /
SystemClock pattern already used in kirra-runtime-sdk/src/clock.rs) into
ControlLoop.

Requirements:
- Add a Clock type parameter or trait object to ControlLoop (prefer trait object
  Box<dyn Clock> to avoid infecting all call sites with a type parameter)
- Default to SystemClock when constructed with ControlLoop::new()
- Expose ControlLoop::with_clock(impl Clock + 'static) -> Self builder
- Add a test that uses VirtualClock::advance to simulate elapsed ticks without
  sleeping
- Do not duplicate the Clock trait; import or re-export from kirra-runtime-sdk
  if the dependency is already present in the workspace
```

---

### PARK-006 `control-loop`
**`parko-core` v0.1.0 release tag**

Verify all tests pass, set version to `0.1.0` in `parko/parko-core/Cargo.toml`,
update the workspace `Cargo.toml` if needed, and tag `parko-core-v0.1.0`. This
is the first shippable artifact of the runtime core increment.

---

## Increment 2 — Hardware Abstraction Layer

---

### PARK-007 `control-loop`
**Define `BackendDescriptor` enum**

Add `BackendDescriptor { Cpu, QualcommQnn, TiTidl, AmdRocm, IntelOpenVino }` to
`parko-core` as the canonical hardware-target discriminant used by all backend
crates. Derive `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`.

#### Claude Code Prompt
```
In parko/parko-core/src/lib.rs (or a new parko/parko-core/src/backend.rs),
define the BackendDescriptor enum.

Requirements:
- pub enum BackendDescriptor { Cpu, QualcommQnn, TiTidl, AmdRocm, IntelOpenVino }
- Derive Debug, Clone, PartialEq, Eq, Hash
- Re-export from parko_core root so downstream crates use parko_core::BackendDescriptor
- Add a unit test that round-trips each variant through Debug formatting
- No new dependencies required
```

---

### PARK-008 `backend-qnn`
**QNN stub backend**

Implement `QnnStubBackend` in `parko-core` (or a new `parko-qnn` crate) that
returns deterministic zeroed outputs, gated behind `features = ["backend-qnn"]`.
The stub must pass CI on ubuntu-latest without Qualcomm hardware.

#### Claude Code Prompt
```
In parko/parko-core/src/backends/qnn_stub.rs (create the backends/ module if
needed), implement QnnStubBackend.

Requirements:
- Implement the InferenceBackend trait from parko-core
- Return deterministic outputs: fill output slice with 0.0f32
- BackendDescriptor::QualcommQnn must be returned from backend_descriptor()
- Gate the entire module behind #[cfg(feature = "backend-qnn")]
- Add "backend-qnn" as an optional feature in parko/parko-core/Cargo.toml
- Add a test: create QnnStubBackend, run inference with a dummy input, assert
  output is all zeros and no panic occurs
- CI must pass without Qualcomm SDK installed
```

---

### PARK-009 `backend-tidl`
**TIDL stub backend**

Implement `TidlStubBackend` with configurable simulated DSP latency (default 2 ms
via `std::thread::sleep` in the stub). Gate behind `features = ["backend-tidl"]`.

#### Claude Code Prompt
```
In parko/parko-core/src/backends/tidl_stub.rs, implement TidlStubBackend.

Requirements:
- Implement InferenceBackend trait
- Accept a latency_ms: u64 parameter (default 2) that the stub sleeps for to
  simulate DSP processing time
- Return deterministic outputs: fill output slice with 0.0f32
- BackendDescriptor::TiTidl returned from backend_descriptor()
- Gate behind #[cfg(feature = "backend-tidl")]
- Add "backend-tidl" as an optional feature in Cargo.toml
- Add a test: create stub with latency_ms=0 (for test speed), assert output is
  zeros and elapsed time is under 10 ms
```

---

### PARK-010 `backend-openvino`
**OpenVINO stub backend**

Implement `OpenVinoStubBackend` gated behind `features = ["backend-openvino"]`.
This stub exists so CI can run the backend-openvino feature path without an
Intel platform or full OpenVINO SDK install.

#### Claude Code Prompt
```
In parko/parko-core/src/backends/openvino_stub.rs, implement OpenVinoStubBackend.

Requirements:
- Implement InferenceBackend trait
- Return deterministic zeros without calling any real OpenVINO API
- BackendDescriptor::IntelOpenVino returned from backend_descriptor()
- Gate behind #[cfg(feature = "backend-openvino")]
- Add "backend-openvino" as an optional feature in Cargo.toml
- Test: create stub, run inference, assert output zeros, no panic
- The real OpenVinoBackend (PARK-014) will be a separate struct in the same
  module; this stub must not conflict with it
```

---

### PARK-011 `backend-rocm`
**ROCm stub backend**

Implement `RocmStubBackend` returning zeroed outputs, gated behind
`features = ["backend-rocm"]`. CI must pass without AMD GPU hardware.

#### Claude Code Prompt
```
In parko/parko-core/src/backends/rocm_stub.rs, implement RocmStubBackend.

Requirements:
- Implement InferenceBackend trait
- Return deterministic zeros
- BackendDescriptor::AmdRocm returned from backend_descriptor()
- Gate behind #[cfg(feature = "backend-rocm")]
- Add "backend-rocm" as optional feature in Cargo.toml
- Test: run inference, assert zeros, no panic
```

---

### PARK-012 `control-loop`
**Backend latency watchdog**

Add a watchdog inside `InferenceLoop` that measures wall-clock time per
`InferenceBackend::run` call. If the call exceeds `deadline_ms`, emit a
`LatencyViolation` event and hold the last safe output rather than blocking.
Connect this to the posture engine: sustained violations → `Degraded`.

#### Claude Code Prompt
```
In parko/parko-core/src/inference_loop.rs (or control_loop.rs), add a per-tick
latency watchdog.

Requirements:
- Add deadline_ms: Option<u64> field to InferenceLoop (or ControlLoop)
- On each tick, record start time (use SystemClock / VirtualClock), call
  backend.run(), record end time
- If elapsed > deadline_ms, emit LatencyViolation { backend: BackendDescriptor,
  elapsed_ms: u64, deadline_ms: u64 } event
- On LatencyViolation, return the last safe output (cached from previous tick)
  without updating the output
- After N=3 consecutive violations, set posture to Degraded (make N configurable)
- Add a test using TidlStubBackend with a short deadline to trigger the watchdog
  and confirm posture transitions to Degraded
```

---

### PARK-013 `backend-openvino`
**CI matrix: all four stub backends**

Add a GitHub Actions matrix job that builds and tests all four backend stubs
(`backend-qnn`, `backend-tidl`, `backend-openvino`, `backend-rocm`) on
ubuntu-latest in the same workflow run.

---

### PARK-014 `backend-openvino`
**Real `OpenVinoBackend`**

Implement `OpenVinoBackend` using the `openvino-rs` crate with model loading,
input shape validation, and output slice writing. Gate behind
`features = ["backend-openvino"]`.

#### Claude Code Prompt
```
In parko/parko-core/src/backends/openvino_stub.rs (or a new openvino.rs),
implement the real OpenVinoBackend alongside the stub.

Requirements:
- Use the openvino crate (add as optional dependency under features = ["backend-openvino"])
- OpenVinoBackend::new(model_path: &str) -> Result<Self, BackendError>
- Implement InferenceBackend::run(&self, input: &[f32], output: &mut [f32])
  -> Result<(), BackendError>
- Validate input.len() == expected_input_size before calling inference; return
  BackendError::ShapeMismatch if wrong
- Copy output tensor into the output slice; return BackendError::ShapeMismatch
  if output buffer is wrong size
- Add an integration test that loads a trivial identity model (provide as a
  test fixture) and confirms output matches input
- Gate entire OpenVinoBackend behind #[cfg(feature = "backend-openvino")]
```

---

## Increment 3 — Behavioral Safety (RSS-Equivalent)

---

### PARK-015 `behavioral-safety`
**`RssSafeDistance::longitudinal`**

Implement the IEEE 2846 longitudinal safe-distance formula in
`parko-core::rss`. The function computes the minimum following distance given
ego velocity, lead velocity, reaction time, maximum acceleration, and maximum
braking.

#### Claude Code Prompt
```
Create parko/parko-core/src/rss.rs and implement the RSS longitudinal safe
distance model.

Requirements:
- pub fn longitudinal_safe_distance(ego_vel: f64, lead_vel: f64,
    reaction_time: f64, accel_max: f64, brake_max: f64, brake_min: f64) -> f64
- Implement per IEEE 2846-2022 equation (response distance + braking distance
  difference): d_min = ego_vel * reaction_time + 0.5 * accel_max *
  reaction_time^2 + (ego_vel + accel_max * reaction_time)^2 / (2 * brake_min)
  - lead_vel^2 / (2 * brake_max)
- Return 0.0 if the result is negative (ego is already slower than lead)
- Unit tests must cover: equal speeds, ego faster, ego slower, zero speed,
  very high speed (verify no overflow)
- Reference: IEEE 2846-2022 Section 5.1
```

---

### PARK-016 `behavioral-safety`
**`RssSafeDistance::lateral`**

Implement the IEEE 2846 lateral safe-distance formula. Inputs: lateral velocities
of ego and object, maximum lateral acceleration, friction coefficient.

#### Claude Code Prompt
```
In parko/parko-core/src/rss.rs, add the lateral safe distance calculation.

Requirements:
- pub fn lateral_safe_distance(ego_lat_vel: f64, obj_lat_vel: f64,
    lat_accel_max: f64, reaction_time: f64) -> f64
- Implement the lateral model per IEEE 2846-2022 Section 5.2
- Return 0.0 if the computed margin is negative
- Unit tests: vehicles moving apart (margin 0), converging at high speed,
  stationary case
```

---

### PARK-017 `behavioral-safety`
**`RssState` and posture integration**

Add `RssState { safe: bool, longitudinal_margin: f64, lateral_margin: f64 }` to
the posture evaluation pipeline in `kirra-runtime-sdk`. An RSS violation
immediately transitions fleet posture to `Degraded`; recovery follows the
existing 5-tick hysteresis.

#### Claude Code Prompt
```
In kirra-runtime-sdk/src/posture_engine.rs (and posture_engine_v2.rs if needed),
integrate RssState into the posture evaluation pipeline.

Requirements:
- Add RssState { safe: bool, longitudinal_margin: f64, lateral_margin: f64 }
  (import from parko-core::rss or define locally)
- Add evaluate_rss_state(state: &RssState) -> PostureRecalcTrigger that returns
  a trigger when state.safe == false
- Wire evaluate_rss_state into start_posture_engine_worker: when called, send a
  PostureRecalcTrigger::RssViolation trigger
- In derive_fleet_posture / recalculate_and_broadcast, an active RssViolation
  trigger produces FleetPosture::Degraded
- Recovery uses the existing AV hysteresis: 5 consecutive safe RSS reports
  within 10s window required to leave Degraded
- Add an integration test: inject RssState { safe: false }, assert posture
  transitions to Degraded; inject 5 consecutive RssState { safe: true }, assert
  posture returns to Nominal
```

---

### PARK-018 `aegis-integration`
**Wire RSS into `KirraKernelGovernor`**

Integrate `RssSafeDistance` as a pre-actuator gate in `kirra-runtime-sdk`'s
`KirraKernelGovernor`. An RSS violation clamps the commanded velocity to zero
before any kinematics envelope check.

#### Claude Code Prompt
```
In kirra-runtime-sdk/src/kirra_core.rs, add RSS safe-distance gating to
KirraKernelGovernor::enforce (or the equivalent enforcement method).

Requirements:
- Accept current RssState as a parameter (or field) of the governor
- If rss_state.safe == false, clamp the velocity command to 0.0 before any
  other enforcement step
- The RSS clamp runs BEFORE the kinematics envelope clamp (first line of
  enforcement)
- Add a unit test: set rss_state.safe = false, send any positive velocity,
  assert output velocity is 0.0
- Add a unit test: set rss_state.safe = true, send a legal velocity, assert the
  kinematics envelope applies normally
- Do not change the KirraKernelGovernor constructor signature; provide the
  RssState via an update_rss_state(&mut self, state: RssState) method
```

---

### PARK-019 `behavioral-safety`
**RSS property test**

Write a `proptest` suite asserting no RSS-violating command reaches the actuator
for any physically valid `(ego_vel, lead_vel, gap)` triple with the governor
in-line.

#### Claude Code Prompt
```
In parko/parko-core/tests/rss_property.rs (create if needed), write a proptest
suite for the RSS safe-distance invariant through the governor.

Requirements:
- Generate (ego_vel: f64, lead_vel: f64, gap: f64, commanded_vel: f64) tuples
  using proptest; filter to physically valid ranges (all non-negative, gap > 0)
- Compute longitudinal_safe_distance for each tuple
- If gap < safe_distance, assert the governor outputs velocity == 0.0 (RSS
  violation was caught)
- If gap >= safe_distance, assert the governor outputs the commanded velocity
  (possibly clamped by kinematics, but not zeroed by RSS)
- Run at least 10_000 cases
- Test must pass with no hardware; use parko-aegis KirraKernelGovernor as the
  SafetyGovernor implementation
```

---

### PARK-020 `aegis-integration`
**`RssViolationEvent` in audit chain**

Add `RssViolationEvent` to the `kirra-runtime-sdk` audit chain. The event must
include ego state, object state, computed margins, and a timestamp, and must be
part of the hash-chained ledger so it is tamper-evident.

#### Claude Code Prompt
```
In kirra-runtime-sdk/src/audit_chain.rs, add RssViolationEvent to the audit
chain.

Requirements:
- Define: pub struct RssViolationEvent { pub ego_vel: f64, pub lead_vel: f64,
    pub gap: f64, pub longitudinal_margin: f64, pub lateral_margin: f64,
    pub timestamp_ms: u64 }
- Add AuditEntry::RssViolation(RssViolationEvent) variant to whatever enum
  represents audit chain entries
- Implement serialization (JSON or CBOR; match existing chain format)
- Wire into AuditChainLinker: expose append_rss_violation(&mut self, event:
  RssViolationEvent) -> Result<(), AuditError>
- The entry must be included in the SHA-256 chain hash (same as existing entries)
- Add a test: append an RssViolationEvent, call verify_chain(), assert no error
```

---

### PARK-021 `simulation`
**10 000 adversarial trajectory simulation**

Run 10 000 adversarial trajectory scenarios via `kirra-runtime-sdk`'s
`ScenarioRunner` with the RSS governor in-line. Assert zero unsafe commands
exit the stack.

#### Claude Code Prompt
```
In kirra-runtime-sdk/tests/rss_simulation.rs (create if needed), write a
simulation test that exercises the full RSS + governor + posture stack.

Requirements:
- Use ScenarioRunner from kirra-runtime-sdk::scenario_runner
- Use VirtualClock to advance time without sleeping
- Generate 10_000 scenarios: each scenario is a sequence of ego/lead velocities
  and commanded velocities that include both safe and unsafe gaps
- For each scenario, run the full stack: RssState → posture engine → governor →
  output
- Assert: no commanded velocity that violates RSS ever appears in the output
  sequence
- Assert: posture correctly degrades and recovers across scenarios
- Test must complete in under 60 seconds on CI
```

---

## Increment 4 — Silicon Matrix Expansion

---

### PARK-022 `backend-qnn`
**Real `QnnBackend`**

Implement `QnnBackend` using Qualcomm AI Engine Direct SDK C bindings. Zero-copy
input/output via `&[f32]` slices; quantization happens internally if required by
the model.

#### Claude Code Prompt
```
In parko/parko-core/src/backends/qnn.rs, implement the real QnnBackend.

Requirements:
- Use the Qualcomm AI Engine Direct SDK via unsafe C FFI bindings
  (Qnn_Interface_t, QnnBackend_Config_t, QnnContext_Config_t)
- QnnBackend::new(model_path: &str, device: QnnDevice) -> Result<Self, BackendError>
- Implement InferenceBackend::run(&self, input: &[f32], output: &mut [f32])
- If the model requires int8 quantization, perform the conversion internally
  using QDQ parameters from the model context
- BackendDescriptor::QualcommQnn from backend_descriptor()
- Gate the entire implementation behind #[cfg(feature = "backend-qnn")]
- Write a hardware integration test (marked #[ignore] on CI) that runs a
  MobileNetV2 model and compares top-1 class with the ORT CPU reference
```

---

### PARK-023 `backend-tidl`
**Real `TidlBackend`**

Implement `TidlBackend` via TI TIDL runtime C FFI, cross-compiled to
`aarch64-unknown-linux-gnu`. Target platform: TDA4VM.

#### Claude Code Prompt
```
In parko/parko-core/src/backends/tidl.rs, implement the real TidlBackend.

Requirements:
- Use TI TIDL runtime C FFI (tivxTIDLNode, TIDL_IOBufDesc_t)
- TidlBackend::new(model_path: &str) -> Result<Self, BackendError>
- Implement InferenceBackend::run via TIDL execute call
- Cross-compile target: aarch64-unknown-linux-gnu; use cross or cargo-cross
- Gate behind #[cfg(feature = "backend-tidl")]
- Write a hardware integration test marked #[ignore] comparing output to ORT
  CPU reference within tolerance 1e-3
```

---

### PARK-024 `backend-rocm`
**Real `RocmBackend`**

Implement `RocmBackend` via MIGraphX Rust bindings. Target: AMD RX 6000 series
or MI100.

#### Claude Code Prompt
```
In parko/parko-core/src/backends/rocm.rs, implement RocmBackend using MIGraphX.

Requirements:
- Use migraphx crate or raw HIP C FFI
- RocmBackend::new(model_path: &str) -> Result<Self, BackendError>
- Implement InferenceBackend::run with GPU memory allocation happening once at
  init, not per inference
- BackendDescriptor::AmdRocm from backend_descriptor()
- Gate behind #[cfg(feature = "backend-rocm")]
- Hardware integration test marked #[ignore]
```

---

### PARK-025 `control-loop`
**`BackendSelector`: runtime backend selection**

Implement `BackendSelector` that picks the active backend from a
`BackendDescriptor` at runtime, falling back to CPU ORT if the requested backend
is unavailable.

#### Claude Code Prompt
```
In parko/parko-core/src/backend_selector.rs, implement BackendSelector.

Requirements:
- pub struct BackendSelector { descriptor: BackendDescriptor, backend:
    Box<dyn InferenceBackend> }
- BackendSelector::new(descriptor: BackendDescriptor) -> Result<Self, BackendError>
  - QualcommQnn → try to create QnnBackend, fall back to QnnStubBackend if feature
    not enabled or hardware unavailable
  - TiTidl → TidlBackend / TidlStubBackend
  - IntelOpenVino → OpenVinoBackend / OpenVinoStubBackend
  - AmdRocm → RocmBackend / RocmStubBackend
  - Cpu → ORT backend always
- Log a warning (using tracing::warn!) when falling back to a stub
- Test: BackendSelector::new(BackendDescriptor::QualcommQnn) on CI (no hardware)
  falls back to stub and returns Ok
```

---

### PARK-026 `simulation`
**Cross-backend determinism validation**

Run the same scenario on ORT + QnnStub + TidlStub and assert outputs are
bit-identical within tolerance. This validates that backend substitution does not
silently change safety-critical outputs.

#### Claude Code Prompt
```
In parko/parko-core/tests/cross_backend_determinism.rs, write a determinism
validation test.

Requirements:
- Define a fixed input tensor (e.g. 128 f32 values seeded from a constant)
- Run inference on ORT CPU backend, QnnStubBackend, and TidlStubBackend
- Assert all three outputs are within 1e-5 of each other element-wise
  (stubs return zeros so this validates the stub contract, not real backend
  correctness; update tolerance when real backends are available)
- The test must run on CI without hardware
- Add a note comment: "Replace stub comparisons with real backend outputs when
  hardware CI runners are available (PARK-022, PARK-023)"
```

---

## Increment 5 — Safety OS Packaging

---

### PARK-027 `packaging`
**Unified `kirra_safety_runtime` binary**

Create a new `kirra_safety_runtime` binary that starts `kirra-runtime-sdk`'s
posture engine and `parko-core`'s inference loop in the same process, configured
by environment variables.

#### Claude Code Prompt
```
Create kirra-runtime-sdk/src/bin/kirra_safety_runtime.rs.

Requirements:
- Read KIRRA_ADMIN_TOKEN, KIRRA_DB_PATH, KIRRA_VERIFIER_ADDR from env (fail if
  missing, matching existing kirra_verifier_service behavior)
- Read KIRRA_BACKEND env var (values: "ort", "qnn", "tidl", "openvino", "rocm";
  default "ort") and create the appropriate BackendSelector
- Start the axum HTTP service (reuse existing route setup from
  kirra_verifier_service.rs)
- Start the InferenceLoop with the selected backend at the tick rate specified
  by KIRRA_TICK_RATE_HZ (default 100)
- Wire the InferenceLoop posture output into the kirra-runtime-sdk posture engine
  via PostureEngineSender
- Serve GET /health returning 200 when both the posture engine and inference loop
  are running
- All existing kirra_verifier_service tests must still pass
```

---

### PARK-028 `packaging`
**systemd unit with watchdog**

Write `scripts/kirra-safety-runtime.service` for the unified binary with
`WatchdogSec`, `MemoryMax`, and `CPUQuota`. The service must restart
automatically on watchdog timeout.

---

### PARK-029 `packaging`
**Backend-aware installer**

Extend `install.sh` with `--backend <ort|qnn|tidl|openvino|rocm>`. Non-interactive
mode (`--yes`) must work fully unattended. The correct feature-gated binary must
be downloaded and the systemd unit configured.

---

### PARK-030 `packaging`
**Dashboard inference panels**

Add tick rate, backend P99 latency, RSS margin, and posture history sparkline
panels to the React dashboard. Panels must render live data against a running
`kirra_safety_runtime`.

#### Claude Code Prompt
```
In dashboard/src/, add four new panels to the existing React dashboard.

Requirements:
- InferenceTick panel: connects to a new GET /inference/status endpoint (add
  this endpoint to kirra_safety_runtime returning { tick_rate_hz: f64,
  backend: String, p99_latency_ms: f64 }); renders current tick rate and P99
- RssMargin panel: connects to GET /fleet/rss/status (add endpoint returning
  { longitudinal_margin: f64, lateral_margin: f64, safe: bool }); renders
  margin bars, red when safe==false
- PostureSparkline panel: reads last 60 posture events from the existing SSE
  stream or REST history endpoint; renders a 60-point sparkline
- BackendLatency panel: renders a histogram of the last 100 backend latency
  samples from the /inference/status endpoint
- All panels must handle the service being unreachable gracefully (show "—"
  rather than crashing)
- Use the existing dashboard component patterns (no new UI libraries)
```

---

### PARK-031 `packaging`
**`v1.2.0` release pipeline**

Update the GitHub Actions release pipeline to build all backend variants
(`ort`, `qnn-stub`, `tidl-stub`, `openvino-stub`, `rocm-stub`) for x86_64,
aarch64, and armv7. All variants must be attached to the GitHub Release with
SHA256 sums.

---

## Increment 6 — Certification-Ready Runtime

---

### PARK-032 `docs`
**Complete RTM (`KIRRA-RTM-001`)**

Expand the Requirements Traceability Matrix to trace every safety requirement
to its source line, test ID, and coverage report entry. No new code; documentation
and traceability work only.

---

### PARK-033 `docs`
**MC/DC coverage report**

Generate MC/DC coverage for `posture_cache.rs`, `posture_engine_v2.rs`,
`kirra_core.rs`, and `rss.rs` using `cargo-llvm-cov`. Report must be committed
to `docs/coverage/` with a CI job that fails if coverage drops below 100%.

#### Claude Code Prompt
```
Add a GitHub Actions job to .github/workflows/coverage.yml (create if needed)
that generates MC/DC coverage for kirra-runtime-sdk.

Requirements:
- Use cargo-llvm-cov: cargo llvm-cov --mcdc --html
- Target files: posture_cache.rs, posture_engine_v2.rs, kirra_core.rs
  (rss.rs once PARK-015/016 are merged)
- Fail the CI job if any target file has MC/DC < 100%
- Upload the HTML report as a GitHub Actions artifact named "mcdc-coverage"
- Run on every push to main and on every PR
- Use the existing CI toolchain setup; do not pin to a different Rust version
```

---

### PARK-034 `docs`
**FMEA (`KIRRA-FMEA-001`)**

Write a Failure Mode and Effects Analysis covering posture engine stale cache,
governor bypass, attestation replay, nonce exhaustion, and RSS model numerical
overflow. Each failure mode must include detection method and mitigation.

---

### PARK-035 `docs`
**DFA (`KIRRA-DFA-001`)**

Write a Dependent Failure Analysis for the HA active/passive pair sharing SQLite
on NFS. Identify all single points of failure and propose independent protection
measures per ISO 26262 Part 9.

---

### PARK-036 `aegis-integration`
**Offline `kirra_audit_verify` binary**

Implement a `kirra_audit_verify` binary that reads the audit chain from SQLite,
verifies Ed25519 signatures, and prints a tamper-evidence report without running
the service.

#### Claude Code Prompt
```
Create kirra-runtime-sdk/src/bin/kirra_audit_verify.rs.

Requirements:
- Accept --db <path> CLI argument (use clap or std::env::args)
- Open the SQLite database at the given path using VerifierStore (read-only)
- Read all rows from the audit_log_chain table in order
- For each row, verify: (a) the SHA-256 chain hash matches the previous row's
  hash; (b) the Ed25519 signature (if present) is valid against the public key
  stored in trusted_federation_controllers
- Print a line per entry: "OK" or "TAMPERED: <reason>"
- Exit with code 0 if all entries are OK, code 1 if any entry fails
- Add a test: create a chain with 5 valid entries, run verify, assert exit 0;
  corrupt one byte in the middle entry, run again, assert exit 1 and the
  tampered row is identified
- The binary must run without KIRRA_ADMIN_TOKEN (read-only, no service required)
```

---

### PARK-037 `docs`
**SOTIF analysis (`KIRRA-SOTIF-001`)**

Write a SOTIF (ISO 21448) analysis covering intended function boundaries,
triggering conditions, and evaluation scenarios for the inference loop and RSS
governor integration.

---

### PARK-038 `simulation`
**Hardware-in-the-loop (HIL) test harness**

Design and document a HIL test harness that connects `kirra_safety_runtime` to a
simulated vehicle plant model (CARLA or a custom kinematics integrator). The
harness must be scriptable for nightly regression runs.

#### Claude Code Prompt
```
In kirra-runtime-sdk/tests/hil/ (create directory), create the HIL test harness
scaffold.

Requirements:
- hil_runner.rs: a test binary that connects to a running kirra_safety_runtime
  instance via HTTP and the existing CARLA client (kirra_carla_client)
- Sends vehicle state updates at 100 Hz via the existing /attestation/verify and
  /fleet/posture endpoints
- Reads posture and governor output via SSE /system/posture/stream
- Logs all inputs, outputs, and posture transitions to a CSV file for post-run
  analysis
- Detects and fails on: (a) any RSS violation that produces a non-zero velocity
  output; (b) posture staying Nominal when RSS is violated; (c) service crash
- README.md in tests/hil/ explaining how to run against CARLA and against the
  built-in kinematics simulator
```

---

### PARK-039 `packaging`
**Helm chart: inference backend values**

Update `charts/kirra-verifier/values.yaml` to support `inferenceBackend`,
`tickRateHz`, `modelPath`, and `rssReactionTimeS` as configurable values. The
chart must deploy the correct backend binary variant.

---

### PARK-040 `docs`
**Architecture overview document**

Write `docs/architecture.md` with a system-level block diagram (Mermaid),
data-flow description, security boundary enumeration, and a mapping from each
component to its ASIL decomposition claim.

#### Claude Code Prompt
```
Create docs/architecture.md for the kirra-runtime-sdk + parko ecosystem.

Requirements:
- System block diagram using Mermaid flowchart syntax showing: parko-core
  InferenceLoop → BackendSelector → [ORT/QNN/TIDL/OpenVINO/ROCm] → output →
  KirraKernelGovernor → RssGate → posture engine → actuator
- Data flow section: describe each arrow in the diagram (what data crosses it,
  who owns the buffer, what validation happens)
- Security boundary section: identify trust boundaries (what is inside the safety
  envelope, what is outside); map to kirra-runtime-sdk security invariants
  (KIRRA_ADMIN_TOKEN, constant_time_compare, etc.)
- ASIL decomposition table: for each component, state the ASIL claim
  (e.g. KirraKernelGovernor: ASIL-D, InferenceLoop: ASIL-B, backend: QM)
- Do not invent requirements; derive everything from existing CLAUDE.md,
  docs/safety/, and the kirra-runtime-sdk source
```
