# Pending Claude Code Prompts

> Auto-generated from /work/backlog.md. Excludes completed tasks (PARK-001–003)
> and tasks with no Claude Code Prompt block (PARK-006, 033–036, 038–040).
>
> **Stale authority model note:** PARK-016 and PARK-037 both describe
> LockedOut as a hard-veto to 0.0. Per PARK-003 findings (commit 47550ce),
> KirraGovernor maps LockedOut to the MRC fallback profile (5.0 m/s), same
> as Degraded. Update those prompts before starting work.

---

## PARK-004 — NaN/Inf input guard at tick boundary

```
In parko-core/src/control_loop.rs, add an input guard at the top of tick().

Requirements:
1. Before any governor or clamp logic:
     if proposed_output.is_nan() || proposed_output.is_infinite() {
         return EnforcementAction::Halt;
     }
   (adjust to match actual input type — may be a slice; check all elements)
2. Add proptest generating adversarial f64 (NaN, Inf, -Inf, subnormals):
   assert all NaN/Inf inputs → Halt, no panic.
3. Add unit test: a valid f64 still reaches the governor unchanged.
4. Do not change governor or clamp logic.
5. cargo test -p parko-core exits 0. Do NOT assume specific test count.
```

---

## PARK-005 — RuntimeClock / MockClock abstraction in ControlLoop

```
In parko-core/src/control_loop.rs, wire the Clock trait into ControlLoop.

Requirements:
1. Clock trait (in parko-core/src/clock.rs if not already defined):
     pub trait Clock: Send + Sync { fn now_ms(&self) -> u64; }
   pub struct RuntimeClock;
   pub struct MockClock { current_ms: Arc<AtomicU64> }
   impl MockClock { pub fn advance(&self, ms: u64) }
2. Add field: clock: Arc<dyn Clock> to ControlLoop.
3. Add builder: pub fn with_clock(mut self, c: Arc<dyn Clock>) -> Self.
4. Default: Arc::new(RuntimeClock) in ControlLoop::new().
5. Replace all direct time reads with self.clock.now_ms().
6. Test: use MockClock, advance manually, assert tick fired correct count
   without any sleep.
7. cargo test -p parko-core exits 0. Do NOT assume specific test count.
```

---

## PARK-007 — Verify crate and struct names in parko/ workspace

```
You are doing a read-only audit of the parko/ workspace. Do NOT rename anything.

Run these commands and report the findings:
  find parko/ -name "Cargo.toml" -exec grep -l "name" {} \; | xargs grep "^name ="
  grep -r "struct.*Governor\|AegisGovernor\|KirraGovernor\|SafetyGovernor" \
       parko/ --include="*.rs" -n
  grep -r "pub trait SafetyGovernor\|impl SafetyGovernor" parko/ --include="*.rs" -n
  cat parko/Cargo.toml   # workspace members list

Write a summary to decisions.md under a new section "Crate and Struct Name Audit
(DATE)" with:
- Actual crate names in parko/ workspace
- Actual governor struct name(s)
- Whether SafetyGovernor trait is defined and where
- List of files that will need updating when renamed to KirraGovernor

Do NOT modify any Rust source files.
```

---

## PARK-008 — Finalize InferenceBackend trait zero-copy boundary

```
In parko-core/src/backend.rs (create if absent), finalize InferenceBackend.

Requirements:
1. pub trait InferenceBackend: Send + Sync {
       fn run(&self, input: &[f32], output: &mut [f32]) -> Result<(), BackendError>;
       fn descriptor(&self) -> BackendDescriptor;
   }
2. #[non_exhaustive] pub enum BackendDescriptor {
       Cpu, TensorRT, QualcommQnn, TiTidl, IntelOpenVino, AmdVitis
   }
3. pub enum BackendError {
       ShapeMismatch { expected: usize, got: usize },
       Io(String),
       Unsupported,
   }
4. Re-export from parko_core lib.rs.
5. Unit test: round-trip each BackendDescriptor variant through format!("{:?}", v).
6. No new dependencies.
7. cargo test -p parko-core exits 0. Do NOT assume specific test count.
```

---

## PARK-009 — Validate parko-onnx CPU backend against InferenceBackend trait

```
In parko-onnx/src/lib.rs, implement InferenceBackend for the existing ORT backend.

Requirements:
1. Implement parko_core::InferenceBackend for the existing OrtBackend struct.
   First, find the actual struct name: grep -r "pub struct" parko/crates/parko-onnx/src/
2. run(&self, input: &[f32], output: &mut [f32]):
   - Validate lengths; return BackendError::ShapeMismatch on mismatch.
   - Run ORT session; copy result into output slice.
   - No Vec<f32> allocation on the hot path (pre-allocate scratch at new()).
3. descriptor() returns BackendDescriptor::Cpu.
4. Run cargo test -p parko-onnx and check whether the MNIST integration test passes.
   If it fails, fix it or document the failure — do NOT assume it is passing.
5. Add parko-core as dependency in parko-onnx/Cargo.toml if not present.
```

---

## PARK-010 — Add MockBackend for parko-core unit tests

```
Create parko-core/src/backends/mock.rs.

Requirements:
1. pub struct MockBackend { output: Vec<f32>, descriptor: BackendDescriptor }
2. impl MockBackend {
       pub fn new(output: Vec<f32>) -> Self
       pub fn new_with_descriptor(output: Vec<f32>, d: BackendDescriptor) -> Self
   }
3. impl InferenceBackend for MockBackend:
   - run: copy self.output into output slice; ShapeMismatch if lengths differ.
   - descriptor: return self.descriptor.
4. Re-export: pub use backends::mock::MockBackend in parko-core lib.rs.
5. Test: MockBackend::new(vec![1.0, 2.0]), run with 2-element output, assert values.
6. Confirm parko-core tests compile without any ORT link.
7. cargo test -p parko-core exits 0. Do NOT assume specific test count.
```

---

## PARK-011 — Define backend capability reporting

```
Extend parko-core/src/backend.rs.

Requirements:
1. pub struct BackendCapabilities {
       pub supports_int8: bool,
       pub supports_fp16: bool,
       pub max_batch_size: Option<usize>,
   }
2. Add to InferenceBackend trait:
     fn capabilities(&self) -> BackendCapabilities;
3. MockBackend::capabilities() returns all false, None.
4. OrtBackend::capabilities() returns appropriate values for CPU ONNX Runtime.
5. Unit test: capability struct for MockBackend matches expected defaults.
6. cargo test -p parko-core exits 0.
```

---

## PARK-012 — Feature-gated stub backends for CI

```
Create stub backends in parko-core/src/backends/:
  tensorrt_stub.rs, qnn_stub.rs, tidl_stub.rs, openvino_stub.rs, amd_stub.rs

For each stub (example for TensorRT; repeat for others):
1. #[cfg(feature = "backend-tensorrt")]
   pub struct TensorRTStubBackend;
   impl InferenceBackend for TensorRTStubBackend {
       fn run(&self, _input: &[f32], output: &mut [f32]) -> Result<(), BackendError> {
           output.iter_mut().for_each(|v| *v = 0.0);
           Ok(())
       }
       fn descriptor(&self) -> BackendDescriptor { BackendDescriptor::TensorRT }
       fn capabilities(&self) -> BackendCapabilities { BackendCapabilities::default() }
   }
2. Add optional features to parko-core/Cargo.toml:
     [features]
     backend-tensorrt = []
     backend-qnn = []
     backend-tidl = []
     backend-openvino = []
     backend-amd = []
3. Test each: cargo test -p parko-core --features backend-<name>
   Assert all output elements == 0.0; assert descriptor matches.
4. No hardware, no external dependencies.
```

---

## PARK-013 — Longitudinal RSS safe-distance — first implementation

```
Create parko-core/src/rss.rs. This is a first implementation; no prior RSS code exists.

Requirements:
1. pub fn longitudinal_safe_distance(
       ego_vel: f64, lead_vel: f64,
       reaction_time: f64, accel_max: f64,
       brake_min: f64, brake_max: f64,
   ) -> f64
   Formula (IEEE 2846-2022 §5.1):
     d_response = ego_vel * reaction_time + 0.5 * accel_max * reaction_time.powi(2)
     v_after = ego_vel + accel_max * reaction_time
     d_brake_ego = v_after.powi(2) / (2.0 * brake_min)
     d_brake_lead = lead_vel.powi(2) / (2.0 * brake_max)
     d_min = (d_response + d_brake_ego - d_brake_lead).max(0.0)
2. Unit tests: equal speeds, ego faster, ego slower, both zero, high speed (no NaN).
3. Add pub mod rss; to parko-core/src/lib.rs.
4. cargo test -p parko-core exits 0.
```

---

## PARK-014 — Lateral RSS safe-distance — first implementation

```
In parko-core/src/rss.rs (created in PARK-013), add lateral safe distance.

Requirements:
1. pub fn lateral_safe_distance(
       ego_lat_vel: f64, obj_lat_vel: f64,
       lat_accel_max: f64, reaction_time: f64,
   ) -> f64
   Compute reaction and braking distances for both actors; return max(0.0, margin).
2. Unit tests: converging fast (large margin), diverging (margin 0), both stationary.
3. cargo test -p parko-core exits 0.
```

---

## PARK-015 — Wire RssState into kirra-runtime-sdk posture engine

```
In kirra-runtime-sdk/src/posture_engine.rs and posture_engine_v2.rs,
integrate RssState. The kirra-runtime-sdk has ~333 tests; all must remain green.

Requirements:
1. pub struct RssState { pub safe: bool, pub longitudinal_margin: f64,
       pub lateral_margin: f64 }
2. Add PostureRecalcTrigger::RssViolation to the trigger enum.
3. In start_posture_engine_worker: handle RssViolation → recalculate_and_broadcast.
4. In derive_fleet_posture: if any active RssViolation → FleetPosture::Degraded.
5. Recovery: AV_RECOVERY_STREAK_THRESHOLD=5, AV_RECOVERY_WINDOW_MS=10_000.
   An RssViolation resets the streak to 0.
6. Integration test using ScenarioRunner:
   - inject RssState { safe: false } → assert Degraded
   - inject 5x RssState { safe: true } within 10s → assert Nominal
7. cargo test -p kirra-runtime-sdk exits 0. ~333 existing tests must remain green.
```

---

## PARK-016 — RSS pre-actuator gate in KirraGovernor

> **Note:** Step 3 says `return 0.0` for unsafe RSS state. Per ADL-001 (updated
> 2026-05-26), the governor's LockedOut/Degraded behavior is a velocity cap (5.0
> m/s), not a hard zero. Clarify the intended RSS-unsafe behavior before implementing.

```
Before writing code, find the actual Kirra governor crate name and struct name:
  find parko/ -name "Cargo.toml" | xargs grep "^name ="
  grep -r "impl.*SafetyGovernor\|pub struct.*Governor" parko/ --include="*.rs" -n

In the Kirra governor crate (verify the crate name first), add RSS gating.

Requirements:
1. Add field: rss_state: RssState (import from parko_core::rss) to governor struct.
   Default: RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX }
2. Add method: pub fn update_rss_state(&mut self, state: RssState)
3. In enforce() — BEFORE kinematic envelope checks:
   if !self.rss_state.safe { return 0.0; }
4. Unit test A: rss_state.safe=false, input vel=5.0 → assert output==0.0.
5. Unit test B: rss_state.safe=true, input vel=5.0 → normal kinematics apply.
6. Do not change governor constructor signature.
7. cargo test -p <governor-crate-name> exits 0.
```

---

## PARK-017 — RSS property test

```
Create parko-core/tests/rss_property.rs or add to the Kirra governor test suite.

Requirements:
1. proptest generates (ego_vel: f64, lead_vel: f64, gap: f64, commanded_vel: f64)
   filtered: all >= 0.0, gap > 0.0, vels < 150.0.
2. For each tuple:
   safe_dist = longitudinal_safe_distance(ego_vel, lead_vel, 0.5, 3.0, 6.0, 8.0)
   rss_safe = gap >= safe_dist
   Set RssState { safe: rss_safe } on the governor.
   out = governor.enforce(commanded_vel, PostureState::Nominal)
   if !rss_safe: assert out == 0.0
   if rss_safe: assert out <= commanded_vel
3. cases = 10_000. Cover all three PostureState variants.
4. No unsafe code.
```

---

## PARK-018 — RssViolationEvent in kirra-runtime-sdk audit chain

```
In kirra-runtime-sdk/src/audit_chain.rs, add RssViolationEvent.

Requirements:
1. pub struct RssViolationEvent { pub ego_vel: f64, pub lead_vel: f64,
       pub gap: f64, pub longitudinal_margin: f64, pub lateral_margin: f64,
       pub timestamp_ms: u64 }
2. Add AuditEntry::RssViolation(RssViolationEvent) variant.
3. pub fn append_rss_violation(&mut self, e: RssViolationEvent) -> Result<(), AuditError>
   Include event bytes in the SHA-256 chain hash.
4. Test A: 5-entry chain including one RssViolation; verify_chain() returns Ok.
5. Test B: corrupt one byte of the RssViolation entry; verify_chain() returns Err.
6. cargo test -p kirra-runtime-sdk exits 0. ~333 existing tests must remain green.
```

---

## PARK-019 — 10,000-scenario adversarial trajectory simulation

```
Create kirra-runtime-sdk/tests/rss_simulation.rs.

Requirements:
1. Use ScenarioRunner from kirra_runtime_sdk::scenario_runner.
2. Use MockClock (or VirtualClock — check actual name in src/clock.rs); no sleep.
3. Generate 10_000 scenarios: each is 10 ticks with varying kinematic state.
   Some gaps below safe distance; some above.
4. For each tick:
   - Compute RssState from parko_core::rss::longitudinal_safe_distance.
   - Feed into posture engine via PostureEngineSender.
   - Feed into KirraGovernor (verify struct name before importing).
   - Record output velocity.
5. Assert: every tick where gap < safe_distance → output velocity == 0.0.
6. Assert: posture degrades on violation; recovers after 5 clean ticks.
7. Test must complete in < 60 s on CI.
8. cargo test -p kirra-runtime-sdk exits 0. ~333 existing tests must remain green.
```

---

## PARK-020 — TensorRT API spike

```
Create parko-core/src/backends/tensorrt_spike.rs. Gate: #[cfg(feature = "backend-tensorrt")].

Requirements:
1. Find the best available TensorRT Rust binding (check crates.io for tensorrt,
   trt-sys, or similar). Document choice in decisions.md.
2. Implement a minimal struct TensorRTBackend with new(engine_path: &str).
   Load a .trt serialized engine file.
3. Add a hardware test (mark #[ignore] for CI):
     #[ignore] #[test]
     fn test_tensorrt_trivial_model() { /* load model, run inference, check no segfault */ }
4. The stub (PARK-012 TensorRTStubBackend) must still compile when this file is active.
5. Add feature = ["backend-tensorrt"] to parko-core/Cargo.toml.
6. Document: which TRT SDK version was tested, Jetson toolchain commands used.
```

---

## PARK-021 — Implement TensorRTBackend struct

```
In parko-core/src/backends/tensorrt.rs (extend from PARK-020 spike).

Requirements:
1. pub struct TensorRTBackend {
       engine: /* TRT engine handle */,
       input_buf: /* CUDA device buffer, pre-allocated */,
       output_buf: /* CUDA device buffer, pre-allocated */,
       input_size: usize,
       output_size: usize,
   }
2. TensorRTBackend::new(engine_path: &str, input_size: usize, output_size: usize)
   Loads .trt plan; allocates CUDA buffers. No per-inference alloc after this.
3. impl InferenceBackend:
   - run: H2D copy input; execute; D2H copy output. Return ShapeMismatch on bad lengths.
   - descriptor: BackendDescriptor::TensorRT
4. Hardware test #[ignore]: load real model, run inference, output is not all zeros.
5. Stub test (runs in CI): TensorRTStubBackend from PARK-012 still outputs zeros.
6. Gate: #[cfg(feature = "backend-tensorrt")].
```

---

## PARK-022 — Integrate TensorRT into BackendSelector

```
Create parko-core/src/backend_selector.rs.

Requirements:
1. pub struct BackendSelector(Box<dyn InferenceBackend>);
2. BackendSelector::new(d: BackendDescriptor, model_path: Option<&str>)
   -> Result<Self, BackendError>:
   TensorRT → TensorRTBackend if feature enabled + model_path provided,
              else TensorRTStubBackend with tracing::warn!
   Cpu → OrtBackend (always available)
   Others → respective stubs with tracing::warn!
3. impl InferenceBackend for BackendSelector (delegates to inner).
4. Test: BackendSelector::new(TensorRT, None) on CI (no GPU) → Ok;
   descriptor() == TensorRT (stub returns correct descriptor).
5. pub use backend_selector::BackendSelector in lib.rs.
```

---

## PARK-023 — CPU vs TensorRT output comparison

```
Create parko-core/tests/tensorrt_cpu_comparison.rs.

Requirements:
1. const FIXED_INPUT: [f32; N] = /* deterministic values */;
2. Load same model on OrtBackend and TensorRTBackend.
3. Run FIXED_INPUT through both; assert element-wise diff < 1e-3.
4. Mark the whole test #[ignore]:
   #[ignore] // requires Jetson with TensorRT runtime
   #[test]
   fn test_cpu_vs_tensorrt() { ... }
5. Comment: "Update tolerance if model quantization changes (see PARK-021)."
```

---

## PARK-024 — QNX deployment spike

```
Target: cross-compile kirra-runtime-sdk for QNX and bring up kirra_verifier_service.

Requirements:
1. Identify the correct Rust target triple for QNX (e.g., x86_64-pc-nto-qnx710).
2. Add cross-compilation configuration to .cargo/config.toml.
3. Identify and fix any POSIX subset issues (signal, threads, sockets, filesystem).
   Document each issue and fix in decisions.md.
4. Build kirra_verifier_service for QNX:
     cargo build --target x86_64-pc-nto-qnx710 --bin kirra_verifier_service
5. Run on QNX device/VM; confirm /health returns 200 with KIRRA_ADMIN_TOKEN set.
6. Document in decisions.md: QNX version, SDK version, any feature flags disabled.
Note: Time-sensitive — 30-day license window. Prioritize getting a binary running
over feature completeness.
```

---

## PARK-025 — QNN + QNX compatibility analysis

```
Research and document QNN + QNX compatibility. Write findings to decisions.md.

Tasks:
1. Review Qualcomm AI Engine Direct SDK release notes for QNX support.
   Document supported QNX versions and SDK version requirements.
2. Identify FFI linking differences: shared library names, rpath differences,
   init/teardown order between QNX and Linux.
3. Document memory model constraints:
   - Can QNN SDK allocate device memory on QNX without dynamic alloc in run()?
   - What is the POSIX memory API subset available on QNX?
4. Identify any features of the InferenceBackend zero-copy contract (ADL-003)
   that conflict with QNX + QNN requirements.
5. Write a "QNN + QNX Compatibility" section to decisions.md with:
   - Go/no-go recommendation for PARK-027 on QNX
   - Required SDK versions
   - List of known constraints
```

---

## PARK-026 — Define QNX-safe backend selection rules

```
After PARK-024 is complete, add QNX-safe rules to BackendSelector.

Requirements:
1. Add compile-time gate:
     #[cfg(target_os = "nto")]  // QNX Neutrino
   to any code path that uses features unavailable on QNX (as found in PARK-024).
2. In BackendSelector::new on QNX targets:
   - If TensorRT requested: warn + fall back to CPU (TensorRT not available on QNX).
   - If QNN requested and feature enabled: proceed (per PARK-025 analysis).
   - Document the QNX-specific fallback table in decisions.md.
3. Add a doc comment to BackendSelector explaining QNX constraints.
4. Verify: cargo build --target x86_64-pc-nto-qnx710 -p parko-core succeeds.
```

---

## PARK-027 — QNN backend MVP — first implementation

```
Create parko-core/src/backends/qnn.rs. This is a first implementation.
Gate: #[cfg(feature = "backend-qnn")].

IMPORTANT: First complete PARK-025 (QNN + QNX compatibility analysis).
Verify SDK version requirements before writing any FFI bindings.

Requirements:
1. Use Qualcomm QNN SDK C FFI: Qnn_Interface_t, QnnBackend_Config_t, QnnTensor_t.
2. pub struct QnnBackend { /* context, graph, tensor handles; no per-run alloc */ }
3. QnnBackend::new(model_path: &str) -> Result<Self, BackendError>
4. run: populate input tensor; if int8 model, quantize using scale/offset from metadata;
   execute; dequantize output to &mut [f32].
5. descriptor() returns BackendDescriptor::QualcommQnn.
6. Hardware test #[ignore]: compare top-1 class with CPU reference within tolerance.
7. CI test: QnnStubBackend from PARK-012 still compiles and outputs zeros.
```

---

## PARK-028 — TIDL backend MVP — first implementation

```
Create parko-core/src/backends/tidl.rs. This is a first implementation.
Gate: #[cfg(feature = "backend-tidl")].

Requirements:
1. Use TI TIDL C FFI (tivxTIDLNode, TIDL_IOBufDesc_t).
2. Cross-compile target: aarch64-unknown-linux-gnu.
3. TidlBackend::new(model_path: &str) -> Result<Self, BackendError>.
4. run: copy &[f32] to TIDL input buffer; execute; copy to &mut [f32].
5. descriptor() returns BackendDescriptor::TiTidl.
6. Hardware test #[ignore]: compare output within 1e-3 of CPU reference.
7. Add parko-core/build.rs for TIDL C FFI linking if needed.
```

---

## PARK-029 — OpenVINO backend MVP — first implementation

```
Create parko-core/src/backends/openvino.rs. This is a first implementation.
Gate: #[cfg(feature = "backend-openvino")].

Requirements:
1. Add openvino = { version = "0.7", optional = true } activated by feature.
2. pub struct OpenVinoBackend { core, compiled, input_size, output_size }
3. OpenVinoBackend::new(model_xml: &str, model_bin: &str) -> Result<Self, BackendError>
4. run: validate lengths; set tensor; infer; copy to output slice.
5. Integration test (NOT #[ignore]; runs on CI via CPU plugin):
   Use tiny identity model in tests/fixtures/identity.xml + identity.bin.
   Assert output == input within 1e-6.
6. Stub (PARK-012) still usable when feature is off.
```

---

## PARK-030 — AMD backend MVP — decide Vitis AI vs ROCm, then implement

```
Before writing code, record the AMD backend decision in decisions.md:
- Vitis AI: requires Xilinx FPGA, uses xrt crate or Vitis AI C API.
- ROCm: requires AMD GPU, uses migraphx crate or HIP C FFI.
- State which was chosen and why (hardware availability, customer pull).

Then implement the chosen backend:
Gate: #[cfg(feature = "backend-amd")].

Requirements:
1. pub struct AmdBackend { /* pre-allocated buffers */ }
2. AmdBackend::new(model_path: &str) -> Result<Self, BackendError>.
3. run: no per-inference alloc; copy to output slice.
4. descriptor() returns BackendDescriptor::AmdVitis.
5. Hardware test #[ignore].
6. Stub (PARK-012) still compiles when feature is off.
```

---

## PARK-031 — Normalize Kirra naming across Docker/Helm

```
Search for all remaining Aegis references in deployment artifacts:
  grep -ri "aegis" docker-compose.yml Dockerfile helm/ charts/ scripts/ install.sh

For each reference, either:
- Rename to Kirra equivalent (e.g. aegis-verifier → kirra-verifier)
- Or add a comment explaining why the legacy name is intentionally preserved

After renaming:
- Verify docker-compose.yml builds: docker compose build
- Verify helm chart lints: helm lint helm/kirra/
- Verify install.sh --help runs without error
- Run: grep -ri "aegis" docker-compose.yml Dockerfile helm/ charts/ scripts/ install.sh
  and confirm only intentional references remain
```

---

## PARK-032 — Add Parko runtime into Kirra Docker image

```
Update the Kirra Dockerfile to include parko-core.

Requirements:
1. Add parko workspace to the Dockerfile build stage:
   COPY parko/ parko/
   RUN cargo build --release -p parko-core -p parko-onnx (+ any governor crate)
2. The combined image must start kirra_safety_runtime (or equivalent combined binary)
   and expose /health + /inference/status.
3. KIRRA_BACKEND env var selects backend (default: cpu).
4. Test: docker build -t kirra:test . && docker run --rm -e KIRRA_ADMIN_TOKEN=test
         kirra:test /health → 200
5. Update docker-compose.yml to use the combined image.
6. Image must be < 2 GB uncompressed (document any deviation).
```

---

## PARK-037 — Integrate Parko + KirraGovernor with ROS2 cmd_vel topics

> **Note:** Step 3 says LockedOut/Degraded → output velocity == 0.0 (hard veto).
> Per ADL-001 (updated 2026-05-26), KirraGovernor maps both to the MRC fallback
> profile (5.0 m/s cap), not a hard zero. Clarify intended behavior before implementing.

```
In ros2_ws/src/kirra_safety/kirra_safety/cmd_vel_interceptor.py (or equivalent),
wire the Parko ControlLoop + KirraGovernor into the ROS2 pipeline.

Before writing code, verify the actual governor struct name:
  grep -r "AegisGovernor\|KirraGovernor\|Governor" parko/ --include="*.rs" -n

Requirements:
1. Subscribe to /cmd_vel (geometry_msgs/Twist).
2. For each message:
   a. Query current FleetPosture from kirra-runtime-sdk.
   b. Call KirraGovernor.enforce(commanded_vel, posture).
   c. Publish result to /cmd_vel_safe.
3. If posture is Degraded or LockedOut: output velocity == 0.0 (hard veto).
4. If posture is Nominal: apply kinematic clamp; publish clamped value.
5. Test: inject Degraded posture via set_state_for_test; assert /cmd_vel_safe == 0.
6. Use Kirra naming in all comments and docs.
```
