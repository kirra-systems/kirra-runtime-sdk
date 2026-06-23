# Backlog

> ~40 tasks derived from the roadmap. Every coding task includes a self-contained
> Claude Code Prompt. Framing is corrected: all hardware backends beyond CPU ONNX
> are new work; IEEE 2846 / IEC 61508 / ASTM F3269 integrations are new work, not
> refinements; parko-core has ~30–40 tests (not 333); the MNIST integration test
> must be verified, not assumed green; the governor crate name must be searched
> before any rename task is written.

---

## PARK-001 `control-loop` `feat`

**Attach `SafetyGovernor` to `ControlLoop`**

Add `with_governor(impl SafetyGovernor + 'static)` builder to `ControlLoop` in
`parko-core`. The governor is stored as `Option<Box<dyn SafetyGovernor>>`. When
present, the built-in scalar clamp is suppressed entirely — both enforcement paths
must not fire on the same tick. This is the foundation for KirraGovernor's
integration with the parko-core inference loop.

### Claude Code Prompt
```
You are working in the parko-core crate. Before writing any code, search the
workspace for the actual crate and struct names:

  find parko/ -name "*.toml" | xargs grep -l "\[package\]"
  grep -r "SafetyGovernor\|Governor\|AegisGovernor\|KirraGovernor" parko/ --include="*.rs" -l

If the governor struct is named AegisGovernor or similar, rename it to KirraGovernor
in the same commit. Use Kirra naming in all new comments and docs.

Task: Add `with_governor` builder to ControlLoop in parko-core/src/control_loop.rs.

Requirements:
1. Add field: governor: Option<Box<dyn SafetyGovernor>> to ControlLoop struct.
2. Add method: pub fn with_governor(mut self, g: impl SafetyGovernor + 'static) -> Self
3. In tick(): if self.governor.is_some() { delegate to governor, skip built-in clamp }
              else { run built-in clamp as before }
4. SafetyGovernor trait (if not yet defined) in parko-core/src/governor.rs:
     pub trait SafetyGovernor: Send + Sync {
         fn enforce(&self, proposed: f64, posture: PostureState) -> f64;
     }
5. Write test test_builtin_clamp_suppressed:
   - ZeroGovernor always returns 0.0.
   - Inject via with_governor.
   - Call tick with value above built-in clamp threshold.
   - Assert result == 0.0.
6. Write test test_no_governor_uses_builtin_clamp:
   - No governor injected.
   - Call tick with value above clamp threshold.
   - Assert result == clamped value.
7. Run `cargo test -p parko-core` — confirm exit 0.
   Do NOT assume any specific test count.
   Do NOT assume the MNIST integration test is passing.
8. No unsafe code.
```

---

## PARK-002 `control-loop` `feat`

**Add test-only posture state setter**

Add `set_state_for_test(state: PostureState)` to `ControlLoop` in `parko-core`
behind `#[cfg(test)]`. This is a pure test seam — it mutates internal posture state
directly without transition validation, unblocking posture-divergence property tests.
The method must be absent from release builds (verified with `nm`) and present only
when running tests.

### Claude Code Prompt
```
In parko-core/src/control_loop.rs, add a cfg(test) method to ControlLoop.

Before writing code, search for the actual governor struct name:
  grep -r "AegisGovernor\|KirraGovernor\|Governor" parko/ --include="*.rs" -l
Use Kirra naming in all new comments.

Requirements:
1. Add this block to the ControlLoop impl:
     #[cfg(test)]
     pub fn set_state_for_test(&mut self, state: PostureState) {
         self.posture_state = state;
     }
2. Do not modify any production code paths.
3. Write a test that:
   - Creates a ControlLoop.
   - Calls set_state_for_test(PostureState::Degraded).
   - Calls tick and asserts output consistent with Degraded behaviour.
4. After release build, run:
     nm target/release/<binary> | grep set_state_for_test
   Confirm output is empty. Locate binary name from workspace Cargo.toml.
5. cargo test -p parko-core exits 0.
   Do NOT assume any specific test count.
6. No unsafe code.
```

---

## PARK-003 `control-loop` `test`

**Write posture divergence property test**

Proptest suite: for all valid `(proposed_output: f64, posture_state: PostureState)`
pairs, the KirraGovernor output is at least as conservative as the built-in clamp
ceiling. This is the core correctness invariant for the governor integration. Depends
on PARK-002 for posture state injection; requires ≥ 10,000 cases per PostureState
variant.

### Claude Code Prompt
```
PARK-002 must be complete before this task.
You are working in the parko-core crate.

Before writing code, verify the actual governor crate name:
  find parko/ -name "*.toml" | xargs grep -l "\[package\]"
  grep -r "impl.*SafetyGovernor\|KirraGovernor\|AegisGovernor" parko/ --include="*.rs"

Task: Write proptest suite asserting governor output <= builtin clamp ceiling.

Requirements:
1. Add proptest = "1" to parko-core dev-dependencies if not present.
2. Create parko-core/tests/posture_divergence.rs.
3. Expose in parko-core/src/control_loop.rs:
     pub(crate) fn builtin_clamp_ceiling(proposed: f64, state: PostureState) -> f64
   Must return exact ceiling the built-in clamp applies — not a copy.
4. Three proptest! blocks (one per PostureState variant):
   - Nominal/Degraded: prop_assert!(gov_out <= ceiling)
   - LockedOut: prop_assert!(gov_out == 0.0)
5. cases = 10_000 per block.
6. Add governor crate as dev-dependency in parko-core/Cargo.toml.
   Verify crate name from workspace Cargo.toml before editing.
7. cargo test -p parko-core -- --test-threads=1 exits 0.
   Do NOT assume any specific test count.
   Do NOT assume MNIST integration test is passing.
8. No unsafe code.
```

---

## PARK-004 `control-loop` `safety`

**NaN/Inf input guard at tick boundary**

Input guard at the top of `ControlLoop::tick`: any NaN or Inf input returns
`EnforcementAction::Halt` before reaching governor or clamp. Prevents undefined
floating-point behavior from propagating through the safety stack. Must be verified
by a proptest generating adversarial floats (NaN, Inf, -Inf, subnormals).

### Claude Code Prompt
```
PREREQUISITE: PARK-001 (with_governor builder) must be complete.
Verify: grep -n "governor\|with_governor" parko/parko-core/src/control_loop.rs
If absent, stop: "PARK-001 not complete."

You are working in the parko-core crate.
Use Kirra naming in all new comments and docs.

AUTHORITY MODEL (canonical, commits 9943aa9/e1ba1a2/21c3a35):
  LockedOut → 0.0 (hard stop, no exceptions)
  Degraded  → min(proposed, MRC_VELOCITY_CEILING_MPS)
  Nominal   → nominal profile
  The NaN/Inf guard fires BEFORE the governor. Its safe return value
  is the return type's safe floor — NOT the MRC ceiling.

STEP 0: VERIFY TYPES BEFORE WRITING CODE

  grep -n "fn tick" parko/parko-core/src/control_loop.rs
  grep -r "EnforcementAction\|enum.*Action" parko/parko-core/src/ --include="*.rs"
  grep -n "fn tick\|proposed\|input\|cmd" parko/parko-core/src/control_loop.rs | head -20

Determine safe return value:
- EnforcementAction::Halt if that variant exists
- 0.0 if tick() returns f64
- zeroed struct if tick() returns a command struct
- Err(...) if tick() returns Result

Add comment: // Priority 0: NaN/Inf rejection — guard fires before governor
             // Safe return is NOT the MRC ceiling — ADL-001

REQUIREMENTS:
1. Guard at top of tick(), before any governor or clamp logic:
     if proposed.is_nan() || proposed.is_infinite() {
         return <safe return value>;
     }
   If struct input: check each numeric field.
   If slice input: check all elements in a loop.
2. Do not change governor logic.
3. Do not change clamp logic.
4. No unsafe code.

TESTS:

TEST 1 — Adversarial proptest:
  proptest! {
      fn test_nan_inf_inputs_guard_fires_no_panic(
          v in prop_oneof![
              Just(f64::NAN), Just(f64::INFINITY),
              Just(f64::NEG_INFINITY), proptest::num::f64::SUBNORMAL,
          ]
      ) {
          let result = ControlLoop::new().tick(<cmd from v>);
          prop_assert_eq!(result, <safe return value>);
          // Note: subnormals are finite — assert no panic only
      }
  }

TEST 2 — Valid input reaches governor (RecordingGovernor):
  struct RecordingGovernor { last: Arc<Mutex<Option<f64>>> }
  impl SafetyGovernor for RecordingGovernor {
      fn enforce(&self, proposed: <type>, _: PostureState) -> <type> {
          *self.last.lock().unwrap() = Some(<value>);
          proposed
      }
  }
  - Inject RecordingGovernor, call tick(5.0), assert recorded == 5.0.

Verify: cargo test -p parko-core — exit 0. Current count: 33.
Commit: feat(parko-core): add NaN/Inf input guard at top of tick() — Priority 0
```

---

## PARK-005 `control-loop` `feat`

**RuntimeClock / MockClock abstraction in ControlLoop**

Wire the `Clock` trait into `ControlLoop` so all timing logic calls
`self.clock.now_ms()` instead of wall-clock APIs. `MockClock` is used in tests
and `RuntimeClock` wraps wall-clock as the default. Eliminates `sleep` dependencies
from all timing tests in parko-core.

### Claude Code Prompt
```
PREREQUISITE: PARK-001 must be complete.
Verify: grep -n "with_governor" parko/parko-core/src/control_loop.rs

Use Kirra naming in all new comments and docs.

STEP 0: VERIFY WHAT ALREADY EXISTS

  grep -r "trait Clock\|RuntimeClock\|MockClock" parko/parko-core/src/ --include="*.rs"
  grep -n "SystemTime\|Instant::now\|now_ms\|elapsed" parko/parko-core/src/control_loop.rs
  grep -n "tick\|interval\|hz\|period" parko/parko-core/src/control_loop.rs

Only create clock.rs if Clock trait does not already exist.

DEFINE IN parko-core/src/clock.rs (if absent):

  pub trait Clock: Send + Sync { fn now_ms(&self) -> u64; }
  pub struct RuntimeClock;
  impl Clock for RuntimeClock { /* SystemTime::now() as_millis() as u64 */ }
  pub struct MockClock { current_ms: Arc<AtomicU64> }
  impl MockClock {
      pub fn new(start_ms: u64) -> Self { ... }
      pub fn advance(&self, ms: u64) {
          self.current_ms.fetch_add(ms, Ordering::SeqCst); // fetch_add not store
      }
  }
  impl Clock for MockClock { fn now_ms(&self) -> u64 { load(SeqCst) } }

REQUIREMENTS:
1. Add field: clock: Arc<dyn Clock> to ControlLoop.
2. Add builder: pub fn with_clock(mut self, c: Arc<dyn Clock>) -> Self.
3. Default: clock: Arc::new(RuntimeClock) in ControlLoop::new().
4. Replace direct time reads (from STEP 0) with self.clock.now_ms().
5. No unsafe code.

TESTS:

TEST 1 — test_mock_clock_tick_count (zero sleep):
  - If tick interval not configurable, add #[cfg(test)] with_tick_interval_ms.
  - Set interval 50ms. Advance 200ms total.
  - Assert exactly 4 ticks fired. Zero sleep() calls.

TEST 2 — test_runtime_clock_default_smoke:
  - ControlLoop::new() with no with_clock().
  - Call tick() once. Assert no panic, result > 0.

Verify: cargo test -p parko-core — exit 0.
Commit: feat(parko-core): wire Clock trait into ControlLoop with MockClock support
```

---

## PARK-006 `chore`

**parko-core v0.1.0 release tag**

Set version to `0.1.0` in `parko-core/Cargo.toml`. Verify `cargo publish --dry-run
-p parko-core` exits cleanly. Tag `parko-core-v0.1.0` in the repo. No code changes
— version bump and tagging only.

---

## PARK-007 `backend-architecture` `docs`

**Verify crate and struct names in parko/ workspace**

Search the parko/ workspace for all crate names, struct names, and governor
implementations before any rename or refactor. If the governor struct is still
`AegisGovernor` or a similar legacy name, record the rename target (`KirraGovernor`)
and any import paths that will be affected. Document findings in `decisions.md`
before any renaming task is started.

### Claude Code Prompt
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

## PARK-008 `backend-architecture` `feat`

**Finalize InferenceBackend trait zero-copy boundary**

Finalize the `InferenceBackend` trait with a zero-copy hot-path signature:
`run(&self, input: &[f32], output: &mut [f32]) -> Result<(), BackendError>`. All
scratch memory must be pre-allocated at `new()`; no heap allocation on the `run`
path. Shape mismatch must return `BackendError::ShapeMismatch`, never panic.

### Claude Code Prompt
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

## PARK-009 `backend-architecture` `feat`

**Validate parko-onnx CPU backend against InferenceBackend trait**

The parko-onnx crate contains a CPU-based ONNX Runtime backend and a MNIST-style
integration test. Wire it against the finalized `InferenceBackend` trait from
PARK-008 and verify the MNIST integration test is actually green by running it —
do not assume it passes without verification. The CPU baseline must be solid before
multi-silicon work begins.

### Claude Code Prompt
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

## PARK-010 `backend-architecture` `feat`

**Add MockBackend for parko-core unit tests**

Add a `MockBackend` to `parko-core` that accepts configurable output values for
deterministic testing. Eliminates the ORT dependency from the parko-core test binary.
`MockBackend` is the preferred backend for all parko-core unit and property tests;
it must not require any external crate.

### Claude Code Prompt
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

## PARK-011 `backend-architecture` `feat`

**Define backend capability reporting**

Add a `capabilities()` method to `InferenceBackend` and a `BackendCapabilities`
struct describing supported features (quantization, int8, fp16, max batch size).
Each backend reports its descriptor and capabilities at construction time. Enables
`BackendSelector` runtime decisions and logging in later increments.

### Claude Code Prompt
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

## PARK-012 `backend-architecture` `chore`

**Feature-gated stub backends for CI**

Define feature-gated zero-output stub backends for TensorRT, QNN, TIDL, OpenVINO,
and AMD in `parko-core`. Each stub is gated behind `features = ["backend-<name>"]`
and returns zeros deterministically. CI builds and tests all stubs without hardware.
These are stubs only — real implementations are PARK-020 through PARK-030.

### Claude Code Prompt
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

## PARK-013 `behavioral-safety` `safety`

**Longitudinal RSS safe-distance — first implementation**

First implementation of the IEEE 2846-2022 §5.1 longitudinal safe-distance formula
in `parko-core::rss`. No prior behavioral-safety code exists in the repository. The
formula uses ego and lead vehicle kinematics (velocities, reaction time, braking
limits) to compute the minimum safe following distance.

### Claude Code Prompt
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

## PARK-014 `behavioral-safety` `safety`

**Lateral RSS safe-distance — first implementation**

First implementation of the IEEE 2846-2022 §5.2 lateral safe-distance formula.
Computes minimum lateral separation required given lateral velocities and maximum
lateral acceleration of both actors. No prior behavioral-safety code exists.

### Claude Code Prompt
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

## PARK-015 `behavioral-safety` `kirra-governor` `safety`

**Wire RssState into kirra-runtime-sdk posture engine**

Define `RssState { safe, longitudinal_margin, lateral_margin }` and wire it into
the `kirra-runtime-sdk` posture engine. An RSS violation triggers `Degraded` posture
using the existing 5-tick / 10 s recovery hysteresis. An RSS violation resets the
recovery streak to 0.

### Claude Code Prompt
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

## PARK-016 `behavioral-safety` `kirra-governor` `safety`

**RSS pre-actuator gate in KirraGovernor**

Add an RSS pre-actuator gate to the KirraGovernor crate. When `rss_state.safe ==
false`, clamp velocity to 0.0 before any kinematic envelope check. KirraGovernor
already hard-vetoes on Degraded/LockedOut per the authority model; this adds RSS
as an additional input to Nominal-mode decisions. Verify the actual governor crate
name before editing.

### Claude Code Prompt
```
PREREQUISITES: PARK-013 and PARK-007 must be complete.
Governor fix commits 9943aa9/e1ba1a2 must be in place.
Verify:
  grep -n "longitudinal_safe_distance" parko/parko-core/src/rss.rs
  find parko/ -name "Cargo.toml" | xargs grep "^name ="
  grep -r "impl.*SafetyGovernor\|pub struct.*Governor" parko/ --include="*.rs" -n
  cargo test -p <governor-crate-name>  # must be green before starting

AUTHORITY MODEL (canonical, commits 9943aa9/e1ba1a2/21c3a35):
  LockedOut → 0.0 (hard stop)
  Degraded  → min(proposed, MRC_VELOCITY_CEILING_MPS)
  RSS unsafe → Degraded semantics (MRC cap), NOT LockedOut

STEP 0: FIND EXISTING MRC METHOD

  grep -n "mrc\|MRC\|fallback\|Degraded\|LockedOut" parko/<governor>/src/*.rs

Identify whether apply_mrc_profile (or equivalent) exists.
If not, extract it from the Degraded branch of enforce() before adding the RSS gate.
This ensures RSS unsafe and Degraded share ONE code path.

STEP 1: VERIFY MRC_VELOCITY_CEILING_MPS CONSTANT

  grep -rn "MRC_VELOCITY_CEILING_MPS" parko/ --include="*.rs"

Must be defined with doc comment stating "NOT applied to LockedOut".

REQUIREMENTS:
1. Add field: rss_state: RssState (from parko_core::rss)
   Default: RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX }
2. Add: pub fn update_rss_state(&mut self, state: RssState)
3. In enforce() BEFORE kinematic checks:
     if !self.rss_state.safe {
         // RSS unsafe → Degraded semantics (MRC cap), NOT hard stop
         // Per ADL-001: RSS violation is recoverable, NOT LockedOut
         return self.apply_mrc_profile(proposed);
     }
4. Do not change constructor signature.
5. No unsafe code.

TESTS (use MRC_VELOCITY_CEILING_MPS constant, never hardcode 5.0):
A: safe=false, vel=MRC+5.0 → assert output == vel.min(MRC_VELOCITY_CEILING_MPS)
B: safe=true,  vel=3.0     → assert output == 3.0 (normal kinematics)
C: safe=false, vel=MRC-1.0 → assert output == vel  (MRC is cap, not fixed)
D: safe=false vs Degraded  → assert both return same value (shared code path)

Verify: cargo test -p <governor-crate-name> — exit 0.
Commit: feat(governor): add RSS pre-actuator gate — MRC fallback on unsafe state
```

---

## PARK-017 `behavioral-safety` `test`

**RSS property test**

Proptest: for all valid `(ego_vel, lead_vel, gap, commanded_vel)` in physically
plausible ranges (all ≥ 0, gap > 0, vel < 150 m/s), no RSS-violating command exits
the governor for any posture state. 10,000 cases covering Nominal, Degraded, and
LockedOut.

### Claude Code Prompt
```
PREREQUISITES: PARK-013 and PARK-016 must be complete.
Governor tests must be green before starting:
  cargo test -p <governor-crate-name>

AUTHORITY MODEL (canonical, commits 9943aa9/e1ba1a2/21c3a35):
  LockedOut → 0.0 (hard stop)
  Degraded  → min(proposed, MRC_VELOCITY_CEILING_MPS)
  RSS unsafe → Degraded semantics (MRC cap)
Use MRC_VELOCITY_CEILING_MPS constant everywhere. Never hardcode 5.0.

FILE: parko-core/tests/rss_property.rs

THREE proptest! BLOCKS — cases = 10_000 each.
Input strategy (never arbitrary f64):
  ego_vel in 0.0f64..150.0, lead_vel in 0.0f64..150.0,
  gap in 0.001f64..500.0,   commanded in 0.0f64..150.0

For each block:
  safe_dist = longitudinal_safe_distance(ego_vel, lead_vel, 0.5, 3.0, 6.0, 8.0)
  rss_safe = gap >= safe_dist
  let expected = commanded.min(MRC_VELOCITY_CEILING_MPS);

BLOCK 1 — Nominal:
  if !rss_safe: prop_assert_eq!(out, expected)    // MRC cap — exact contract
  if  rss_safe: prop_assert!(out <= commanded)

BLOCK 2 — Degraded:
  prop_assert_eq!(out, expected)  // MRC applies regardless of RSS

BLOCK 3 — LockedOut:
  prop_assert_eq!(out, 0.0,       // hard stop — NOT MRC cap
      "LockedOut must return 0.0, got {} for input {}", out, commanded);

No unsafe code.

Verify: cargo test -p parko-core -- rss_property — exit 0.
Any failure is a real contract violation — report, do not suppress.
Commit: test: RSS property tests — exact MRC contract, 3 posture variants × 10,000 cases
```

---

## PARK-018 `behavioral-safety` `safety`

**RssViolationEvent in kirra-runtime-sdk audit chain**

`RssViolationEvent { ego_vel, lead_vel, gap, longitudinal_margin, lateral_margin,
timestamp_ms }` appended to the SHA-256 hash-chained audit ledger. A single-byte
corruption of any entry must cause `verify_chain()` to fail. All ~333 existing
kirra-runtime-sdk tests must remain green.

### Claude Code Prompt
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

## PARK-019 `behavioral-safety` `simulation` `test`

**10,000-scenario adversarial trajectory simulation**

`ScenarioRunner` + `MockClock` simulation: 10,000 scenarios mixing safe and unsafe
RSS gaps through the full posture engine + governor stack. Assert zero unsafe commands
exit. Must complete in < 60 s on CI. All ~333 kirra-runtime-sdk tests must remain
green.

### Claude Code Prompt
```
PREREQUISITES: PARK-013, PARK-015, PARK-016, PARK-007 must be complete.
Governor fix commits 9943aa9/e1ba1a2 must be in place.
Verify:
  grep -n "longitudinal_safe_distance" parko/parko-core/src/rss.rs
  grep -n "RssViolation\|RssState" kirra-runtime-sdk/src/posture_engine*.rs
  grep -n "update_rss_state" parko/ -r --include="*.rs"
  grep -r "MockClock\|VirtualClock\|struct.*Clock" kirra-runtime-sdk/src/ --include="*.rs" -n
  grep -r "pub struct.*Governor\|impl SafetyGovernor" parko/ --include="*.rs" -n
  cargo test -p <governor-crate-name>  # must be green

Record actual clock type name and governor struct name.

AUTHORITY MODEL (canonical, commits 9943aa9/e1ba1a2/21c3a35):
  LockedOut → 0.0 (hard stop)
  Degraded  → min(proposed, MRC_VELOCITY_CEILING_MPS)
  RSS unsafe → Degraded semantics (MRC cap), NOT LockedOut

FILE: kirra-runtime-sdk/tests/rss_simulation.rs

REQUIREMENTS:
1. Use ScenarioRunner from kirra_verifier::scenario_runner.
2. Use actual clock type from prerequisite check. No sleep().
3. 10,000 scenarios × 10 ticks. Deterministic seed.
   Include scenarios that reach LockedOut posture as well as Degraded.
4. Per tick:
   a. Compute RssState from longitudinal_safe_distance.
   b. Feed into posture engine via PostureRecalcTrigger::RssViolation.
   c. Feed into governor via update_rss_state (actual struct name).
   d. Call governor.enforce(commanded_vel, current_posture).
   e. Record output velocity and current_posture.
5. Assertions (use MRC_VELOCITY_CEILING_MPS, never hardcode 5.0):

   If current_posture == LockedOut:
     assert_eq!(output_velocity, 0.0,
         "LockedOut: hard stop required, got {}", output_velocity);

   If gap < safe_distance AND posture == Degraded:
     assert!(output_velocity <= MRC_VELOCITY_CEILING_MPS,
         "RSS Degraded: output {} exceeded MRC ceiling", output_velocity);

   If gap >= safe_distance AND posture == Nominal:
     assert!(output_velocity <= commanded_vel);

6. Posture lifecycle:
   - RSS violation → posture == Degraded.
   - 5 consecutive safe ticks within 10s → posture == Nominal.
7. Must complete < 60s on CI.
8. No unsafe code.

Verify:
  cargo test -p kirra-runtime-sdk -- rss_simulation
  cargo test -p kirra-runtime-sdk  # ~333 existing must stay green
Commit: test(kirra-runtime-sdk): 10,000-scenario RSS adversarial simulation
```

---

## PARK-020 `backend-tensorrt` `feat`

**TensorRT API spike (TIME-SENSITIVE — Jetson arriving)**

Set up TensorRT FFI bindings (`tensorrt` crate or `trt-sys`) and verify a trivial
model loads and runs on the Jetson hardware. Document the build toolchain and any
driver/SDK version requirements in `decisions.md`. Gate everything behind
`features = ["backend-tensorrt"]`.

### Claude Code Prompt
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

## PARK-021 `backend-tensorrt` `feat`

**Implement TensorRTBackend struct**

Full `TensorRTBackend` implementation: `new(engine_path)` deserializes a `.trt`
plan and pre-allocates CUDA input/output buffers at init. `run()` performs H→D copy,
execute, D→H copy with no per-inference allocation. Implements `InferenceBackend`.

### Claude Code Prompt
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

## PARK-022 `backend-tensorrt` `feat`

**Integrate TensorRT into BackendSelector**

`BackendSelector::new(BackendDescriptor::TensorRT)` creates a `TensorRTBackend`
when the feature is enabled; falls back to `TensorRTStubBackend` with
`tracing::warn!` otherwise. Enables `KIRRA_BACKEND=tensorrt` env-var runtime
selection in the Kirra safety runtime binary.

### Claude Code Prompt
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

## PARK-023 `backend-tensorrt` `test`

**CPU vs TensorRT output comparison**

Same fixed input through the CPU ONNX backend and the TensorRT backend; outputs must
be within 1e-3 element-wise. Hardware test `#[ignore]`'d in CI; comment documents
that the test requires Jetson. Validates the TensorRT implementation against the CPU
baseline.

### Claude Code Prompt
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

## PARK-024 `qnx` `feat`

**QNX deployment spike (TIME-SENSITIVE — 30-day license)**

Bring up the `kirra_verifier_service` binary on QNX. Identify and document any
POSIX subset gaps (signal handling, threading model, filesystem paths, dynamic
linking). Target: service starts and `/health` returns 200 on QNX. Record all
findings in `decisions.md` before the license expires.

### Claude Code Prompt
```
Target: cross-compile kirra-runtime-sdk for QNX and bring up kirra_verifier_service.

QNX SDP 8.0 install: /opt/qnx800/sdp2 — source /opt/qnx800/sdp2/qnxsdp-env.sh before building.

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

## PARK-024b `qnx` `upstream`

**QNX 8.0 upstream contributions — gate nto socket code on target_env**

PARK-024 spike (see ADL-010) confirmed the cross-compilation toolchain
is operational but landed on a precise libc / tokio / socket2 gap for
QNX 8.0 specifically. `libc 0.2.186` defines `TCP_KEEPALIVE` and
`LOCAL_PEEREID` under `cfg_if! { if #[cfg(any(target_env = "nto70",
target_env = "nto71"))] }` (QNX 7.0/7.1 only). There is **no nto80
arm**, and QNX 8.0 system headers do not define these constants —
they were present in QNX 7.x but absent in 8.0. Tokio PR #6421
(tokio v1.37+, kirra is on 1.52.3) added the nto code path but
imports `libc::LOCAL_PEEREID` for any `target_os = "nto"`, so QNX 8.0
builds fail at the libc-import step.

Upstream PR work needed:

- **`tokio-rs/tokio` (#8178 / new PR)** — gate `impl_netbsd` mod and
  the `LOCAL_PEEREID` / `SO_PEERCRED` imports on
  `cfg(any(target_env = "nto70", target_env = "nto71"))`, not the
  broader `cfg(target_os = "nto")`. On QNX 8.0, return
  `Unsupported` or an equivalent fail-soft for the peer-cred lookup.

- **`rust-lang/socket2` (#657 / new PR)** — same pattern for
  `TCP_KEEPALIVE` / `KEEPALIVE_TIME` in `src/sys/unix.rs:295-320`.

- **Optional `rust-lang/libc` PR** — only if QNX 8.0 headers DO
  expose these constants under a different name (needs further
  investigation of `/opt/qnx800/sdp2/target/qnx/usr/include/`). If
  yes, add the nto80 arm with the actual values; if no, the
  tokio/socket2 PRs are the entire fix.

Done when:
- Both upstream PRs merged and a published release of each is in use
- The local socket2-qnx `[patch.crates-io]` fork (laptop-only,
  uncommitted) can be removed
- `cargo build --target x86_64-pc-nto-qnx800 --bin kirra_verifier_service`
  succeeds against stock upstream crates
- Binary boots in a QNX 8.0 VM and `/health` returns 200

Tracking: GitHub issue #67, ADL-010 update (2026-05-29).

### Claude Code Prompt
```
Target: open upstream PRs against tokio-rs/tokio and rust-lang/socket2
that gate nto-specific socket code on cfg(target_env = "nto70") /
"nto71" instead of cfg(target_os = "nto").

Requirements:
1. Read tokio/src/net/unix/ucred.rs and confirm the impl_netbsd mod
   (which imports libc::LOCAL_PEEREID) is the only blocker for
   target_os = "nto" with target_env = "nto80". If so, change the
   gate to only fire for nto70/nto71.
2. Read socket2/src/sys/unix.rs and confirm the TCP_KEEPALIVE import
   has the same structure. Apply the same gate refinement.
3. For QNX 8.0, choose either (a) return io::Error::Unsupported for
   the affected code paths, or (b) implement using a QNX 8.0-native
   API if one exists. Document the choice.
4. Add a CI matrix entry for x86_64-pc-nto-qnx800 (build-only is
   acceptable for tier-3) so the gate refinement is enforced going
   forward.
5. Reference PARK-024b and ADL-010 in the PR description.
```

---

## PARK-025 `qnx` `backend-qnn` `docs`

**QNN + QNX compatibility analysis**

Document the Qualcomm AI Engine Direct SDK version requirements on QNX, FFI linking
differences from Linux, and memory model constraints relevant to the no-alloc backend
contract. Record findings in `decisions.md`. This analysis gates the QNN backend
implementation (PARK-027) and must be completed before QNN work starts.

### Claude Code Prompt
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

## PARK-026 `qnx` `backend-architecture` `docs`

**Define QNX-safe backend selection rules**

Document and enforce QNX-safe backend selection: no dynamic allocation in the
backend hot-path, restricted POSIX API surface, and single-process model constraints.
Add QNX as a recognized target in `BackendSelector` with appropriate restrictions.
Blocked until PARK-024 confirms which POSIX features are available.

### Claude Code Prompt
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

## PARK-027 `backend-qnn` `feat`

**QNN backend MVP — first implementation**

First real implementation of the QNN backend via Qualcomm AI Engine Direct SDK C
FFI. No prior QNN backend code exists in this repository. Depends on PARK-025
(compatibility analysis). Hardware test `#[ignore]`'d in CI; stub from PARK-012
used for CI validation.

### Claude Code Prompt
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

## PARK-028 `backend-tidl` `feat`

**TIDL backend MVP — first implementation**

First real implementation of the TIDL backend via TI TIDL runtime C FFI,
cross-compiled to `aarch64-unknown-linux-gnu`. No prior TIDL backend code exists.
Target hardware: TDA4VM. Hardware test `#[ignore]`'d; CI uses the stub from PARK-012.

### Claude Code Prompt
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

## PARK-029 `backend-openvino` `feat`

**OpenVINO backend MVP — first implementation**

First real implementation of the OpenVINO backend using `openvino-rs`. Unlike other
hardware backends, testable in CI using the OpenVINO CPU plugin. Integration test
uses an identity model fixture; output must match input within 1e-6. First
implementation; no prior OpenVINO backend code exists.

### Claude Code Prompt
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

## PARK-030 `backend-amd` `feat` `docs`

**AMD backend MVP — decide Vitis AI vs ROCm, then implement**

Decide between AMD Vitis AI (Xilinx FPGA path) and AMD ROCm (GPU path) based on
available hardware and customer requirements. Record the decision in `decisions.md`.
Implement the chosen path as an MVP; hardware test `#[ignore]`'d in CI. First
implementation; no prior AMD backend code exists.

### Claude Code Prompt
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

## PARK-031 `packaging` `chore`

**Normalize Kirra naming across Docker/Helm**

Remove remaining Aegis references from Docker image names, Helm chart values,
environment variable names, service unit files, and install scripts. All deployment
artifacts must use Kirra naming consistently. A `grep -r aegis` scan (case-insensitive)
should return only intentional or historical references after this task.

### Claude Code Prompt
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

## PARK-032 `packaging` `feat`

**Add Parko runtime into Kirra Docker image**

Extend the Kirra Docker image to include parko-core, the InferenceLoop, and
`BackendSelector`. One image contains parko runtime + kirra-runtime-sdk +
KirraGovernor + dashboard. Configured by `KIRRA_BACKEND` env var; both `/health`
and the inference loop must respond in the combined image.

### Claude Code Prompt
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

## PARK-033 `packaging` `chore`

**Backend-aware installer**

Update `install.sh` to accept `--backend <cpu|tensorrt|qnn|tidl|openvino|amd>`.
Downloads the correct binary variant for the host architecture, configures the
systemd unit with the right `KIRRA_BACKEND` value, and completes without prompts
when `--yes` is passed.

---

## PARK-034 `packaging` `chore`

**systemd unit with watchdog**

Create `scripts/kirra-safety-runtime.service` with `WatchdogSec=5`,
`MemoryMax=512M`, `CPUQuota=80%`. The unit must restart automatically on watchdog
timeout or OOM kill. Verify with `systemd-analyze verify` before marking Done.

---

## PARK-035 `packaging` `qnx` `chore`

**QNX packaging stub**

Define the `kirra-qnx.tar.gz` artifact structure and a placeholder Makefile for QNX
deployment. This task is blocked until PARK-024 (QNX deployment spike) produces a
working binary. Create the stub so the release pipeline has a slot for the QNX
artifact when QNX work lands.

---

## PARK-036 `ros2` `robot` `chore`

**Bring up ROS2 Jazzy on Ubuntu 24.04**

Install and configure ROS2 Jazzy on Ubuntu 24.04 for the reference robot workspace.
Verify basic pub/sub with `ros2 topic echo`. Create the colcon workspace with the
`kirra_safety` package. BLOCKED: requires Hiwonder hardware delivery or an
alternative simulation environment.

---

## PARK-037 `ros2` `robot` `kirra-governor` `feat`

**Integrate Parko + KirraGovernor with ROS2 cmd_vel topics**

Wire the Parko control loop and KirraGovernor into ROS2 cmd_vel topics:
`cmd_vel` → governor → `cmd_vel_safe` (gated output; `/filtered_cmd_vel` was
never implemented — retired per #171). The governor's hard-veto on
Degraded/LockedOut must be observable on the gated output topic. Depends on PARK-036.
KirraGovernor authority model: hard-veto on Degraded/LockedOut, clamp on Nominal,
conservative fallback if unreachable.

### Claude Code Prompt
```
PREREQUISITES: PARK-001, PARK-002, PARK-007 must be complete.
Governor fix commits 9943aa9/e1ba1a2 must be in place.
Hiwonder robot must be available. ROS2 Jazzy must be installed.
Verify:
  grep -n "with_governor" parko/parko-core/src/control_loop.rs
  grep -r "pub struct.*Governor\|impl SafetyGovernor" parko/ --include="*.rs" -n
  grep -rn "MRC_VELOCITY_CEILING_MPS" parko/ --include="*.rs"
  cargo test -p <governor-crate-name>  # must be green before any ROS2 code
  ros2 --version
  find ros2_ws/ -name "cmd_vel_interceptor.py"

If governor tests are NOT green: STOP. Fix the governor first.
Record: actual governor struct name, MRC_VELOCITY_CEILING_MPS value, file path.

AUTHORITY MODEL (canonical, commits 9943aa9/e1ba1a2/21c3a35):
  LockedOut → 0.0 (hard stop) — NO motion permitted
  Degraded  → min(proposed, MRC_VELOCITY_CEILING_MPS)
  Nominal   → nominal profile
  The ROS2 layer must NOT implement its own velocity cap.
  Delegate entirely to governor.enforce(). Governor is single source of truth.

FILE: <actual path from prerequisite check>

REQUIREMENTS:
1. Subscribe to /cmd_vel (geometry_msgs/Twist).
2. Per message:
   a. Query FleetPosture from kirra-runtime-sdk (KIRRA_VERIFIER_ADDR).
   b. Map to PostureState: Nominal/Degraded/LockedOut.
   c. Call <actual governor struct>.enforce(commanded_vel, posture).
   d. Publish result directly to /cmd_vel_safe. No post-processing.
      Comment: "Output is governor output — governor is single source of truth"
3. Unreachable: drop to Degraded, call enforce(vel, Degraded), log
   "governor_unreachable — applying local Degraded posture".
   Do NOT implement a separate Python cap.
4. Kirra naming. No new Aegis references.

TESTS (use MRC_VELOCITY_CEILING_MPS constant, never hardcode 5.0):
A: LockedOut, cmd=MRC+5.0 → assert filtered == 0.0 (hard stop)
B: Degraded,  cmd=MRC+5.0 → assert filtered == cmd.min(MRC_VELOCITY_CEILING_MPS)
C: Nominal,   cmd=2.0     → assert filtered == 2.0 (within tolerance)
D: Degraded,  cmd=MRC-1.0 → assert filtered == cmd (MRC is cap not fixed)

Test A confirms LockedOut ≠ Degraded at the ROS2 boundary.

Verify:
  python3 -m pytest ros2_ws/src/kirra_safety/tests/ -v  # before hardware
  colcon build --packages-select kirra_safety
  ros2 launch kirra_safety kirra_safety.launch.py        # on hardware
Commit: feat(ros2): wire KirraGovernor into cmd_vel — delegates to governor, ADL-001
```

---

## PARK-038 `ros2` `robot` `simulation` `feat`

**Build full reference robot stack**

Full integration: Parko + KirraGovernor + ROS2 Jazzy + kirra_safety interlock +
CARLA simulation as hardware alternative. Depends on PARK-037 and Hiwonder hardware
availability. BLOCKED until PARK-037 is complete and physical hardware (Hiwonder
robot) is available or CARLA simulation is substituted.

---

## PARK-039 `safety-case` `docs`

**Map IEC 61508 SIL 3 requirements — first implementation**

IEC 61508 SIL 3 has been identified as a target standard but no mapping document
exists. Identify existing Kirra safety functions that can claim SIL 3 compliance;
identify gaps; document required mitigations or additional measures. Every SIL 3
safety function claim must have an implementation entry or explicit gap note.

---

## PARK-040 `safety-case` `docs`

**Map ASTM F3269-21 bounded-operation envelope — first implementation**

ASTM F3269 has been identified as a target standard but no mapping exists. Define
the Nominal, Degraded, and BLLOS (Beyond Line of Sight) operational envelopes per
§6; trace each to the posture engine states and KirraGovernor limits in the codebase.
Do not claim any ASTM F3269 compliance until this mapping is complete and reviewed.

---

## Certification Readiness Track

These tasks are required before engaging TÜV SÜD for ISO 26262 ASIL-D
assessment. All can be completed without hardware. Prioritize in order
listed.

---

## CERT-001 `safety-case` `docs` `certification`

**MC/DC Code Coverage in CI**

MC/DC (Modified Condition/Decision Coverage) is mandatory at ASIL-D
per ISO 26262 Part 6. It requires demonstrating that each individual
condition in a decision independently affects the outcome. TÜV will
ask for MC/DC coverage reports in the first assessment conversation.
Currently Kirra has no MC/DC measurement — only statement and branch
coverage from proptest and unit tests.

#### Claude Code Prompt
```
You are working in the kirra-runtime-sdk repository (root at ~/kirra-runtime-sdk).

Task: Set up MC/DC code coverage measurement using llvm-cov and add it to CI.

STEP 0: Verify the Rust toolchain supports MC/DC:
  rustup show
  rustc --version
  llvm-profdata --version 2>/dev/null || echo "llvm-profdata not found"

STEP 1: Add a coverage script at scripts/coverage-mcdc.sh:

  #!/bin/bash
  set -e
  echo "Running MC/DC coverage measurement..."

  # Clean previous coverage data
  find . -name "*.profraw" -delete
  find . -name "*.profdata" -delete

  # Build and run tests with coverage instrumentation
  RUSTFLAGS="-C instrument-coverage" \
  LLVM_PROFILE_FILE="kirra-%p-%m.profraw" \
    cargo test --workspace 2>&1

  # Merge profile data
  llvm-profdata merge -sparse *.profraw -o coverage.profdata

  # Generate MC/DC report
  llvm-cov report \
    --use-color \
    --ignore-filename-regex='/.cargo/registry' \
    --ignore-filename-regex='/rustup/toolchains' \
    --instr-profile=coverage.profdata \
    $(cargo test --workspace --no-run --message-format=json 2>/dev/null \
      | jq -r 'select(.profile.test == true) | .filenames[]' \
      | grep -v dSYM \
      | sed 's/^/--object /') \
    2>/dev/null

  # Generate HTML report
  llvm-cov show \
    --use-color \
    --ignore-filename-regex='/.cargo/registry' \
    --ignore-filename-regex='/rustup/toolchains' \
    --instr-profile=coverage.profdata \
    --format=html \
    --output-dir=coverage-report \
    $(cargo test --workspace --no-run --message-format=json 2>/dev/null \
      | jq -r 'select(.profile.test == true) | .filenames[]' \
      | grep -v dSYM \
      | sed 's/^/--object /') \
    2>/dev/null

  echo "Coverage report generated at coverage-report/index.html"

Make the script executable:
  chmod +x scripts/coverage-mcdc.sh

STEP 2: Run the script and capture output:
  ./scripts/coverage-mcdc.sh 2>&1 | head -60

STEP 3: Add coverage to .github/workflows/ci.yml (or create it if absent):

  coverage:
    name: MC/DC Coverage
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: llvm-tools-preview
      - name: Install llvm-cov
        run: cargo install cargo-llvm-cov
      - name: Run MC/DC coverage
        run: |
          cargo llvm-cov --workspace --mcdc \
            --ignore-filename-regex='/.cargo/registry' \
            --lcov --output-path lcov.info
      - name: Upload coverage
        uses: codecov/codecov-action@v4
        with:
          files: lcov.info
          fail_ci_if_error: false

STEP 4: Add coverage-report/ to .gitignore if not already present.

STEP 5: Run cargo test to confirm baseline still passes:
  cargo test --workspace 2>&1 | tail -5

STEP 6: Document the baseline MC/DC coverage percentage in
/work/decisions.md under a new entry:

  ### ADL-009 — MC/DC Coverage Baseline
  Date: [today]
  Status: Established
  Baseline MC/DC coverage: [percentage from report]
  Target for ASIL-D assessment: ≥ 90% MC/DC on safety-critical paths
  Safety-critical paths: posture engine, KirraGovernor enforce/evaluate,
  audit chain, RSS safe-distance calculations, NaN/Inf guard.

Commit message:
  feat(ci): add MC/DC code coverage measurement — ISO 26262 ASIL-D prerequisite

Do NOT modify any Rust source files.
Do NOT change any test logic.
Only add the script, CI config, .gitignore entry, and decisions.md entry.
```

---

## CERT-002 `safety-case` `certification`

**Static Analysis in CI (cargo clippy + cargo audit)**

ISO 26262 Part 6 requires documented static analysis as part of the
software safety lifecycle. TÜV expects to see static analysis running
automatically on every change, with results documented. Currently
Kirra runs clippy manually; it is not enforced in CI with automotive-
grade lint rules, and cargo audit (dependency vulnerability scan) is
not running at all.

#### Claude Code Prompt
```
You are working in the kirra-runtime-sdk repository (root at ~/kirra-runtime-sdk).

Task: Add automotive-grade static analysis to CI and document results.

STEP 0: Run baseline static analysis and capture results:
  cargo clippy --workspace -- -D warnings 2>&1 | tee /tmp/clippy_baseline.txt
  cargo audit 2>&1 | tee /tmp/audit_baseline.txt
  echo "Clippy issues: $(grep "^error" /tmp/clippy_baseline.txt | wc -l)"
  echo "Audit advisories: $(grep "^error" /tmp/audit_baseline.txt | wc -l)"

Report what you find before changing anything.

STEP 1: Add static analysis job to .github/workflows/ci.yml:

  static-analysis:
    name: Static Analysis (ISO 26262)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - name: Install cargo-audit
        run: cargo install cargo-audit
      - name: Clippy (automotive-grade)
        run: |
          cargo clippy --workspace -- \
            -D warnings \
            -W clippy::all \
            -W clippy::pedantic \
            -W clippy::nursery \
            -A clippy::module_name_repetitions \
            -A clippy::must_use_candidate \
            2>&1 | tee clippy-report.txt
      - name: Dependency audit
        run: cargo audit 2>&1 | tee audit-report.txt
      - name: Upload reports
        uses: actions/upload-artifact@v4
        if: always()
        with:
          name: static-analysis-reports
          path: |
            clippy-report.txt
            audit-report.txt

STEP 2: If clippy baseline from STEP 0 has errors, fix them before
adding the CI job. Document each fix with a comment explaining why
the change was made for safety reasons.

STEP 3: Add a scripts/static-analysis.sh for running analysis locally:

  #!/bin/bash
  set -e
  echo "=== Clippy (automotive-grade) ==="
  cargo clippy --workspace -- -D warnings -W clippy::all -W clippy::pedantic
  echo "=== Dependency audit ==="
  cargo audit
  echo "Static analysis passed."

STEP 4: Document in /work/decisions.md:

  ### ADL-010 — Static Analysis Standard
  Date: [today]
  Status: Active
  Tool: cargo clippy + cargo audit
  Lint level: -D warnings -W clippy::all -W clippy::pedantic
  Frequency: Every PR via CI
  Baseline: [number of existing warnings fixed in this task]
  Rationale: ISO 26262 Part 6 §8 requires documented static analysis
  for ASIL-D software. Rust's compiler + clippy collectively address
  most MISRA C rule categories. cargo audit prevents known-vulnerable
  dependencies from entering the codebase.

Commit message:
  feat(ci): add automotive-grade static analysis — ISO 26262 Part 6 prerequisite

Do NOT modify any safety-critical logic.
Fix only lint warnings that are style/correctness issues.
If any clippy suggestion changes safety logic behavior, skip it and
add a #[allow(clippy::...)] with a comment explaining why.
```

---

## CERT-003 `safety-case` `docs` `certification`

**Complete Requirements Traceability Matrix**

ISO 26262 requires every safety requirement to trace to a specific
test that verifies it. The current RTM in docs/safety/ is incomplete —
not every safety goal has a corresponding named test. TÜV will verify
this traceability in the first document review.

#### Claude Code Prompt
```
You are working in the kirra-runtime-sdk repository (root at ~/kirra-runtime-sdk).

Task: Audit the Requirements Traceability Matrix and identify gaps.

STEP 0: Read the current safety goals and RTM:
  cat docs/safety/SAFETY_GOALS.md
  cat docs/safety/REQUIREMENTS_TRACEABILITY.md

STEP 1: Find all safety goal IDs referenced in the codebase:
  grep -rn "KIRRA-SG-\|KIRRA-SR-\|KIRRA-HARA-" \
    src/ tests/ parko/ --include="*.rs" | sort

STEP 2: Find all safety goal IDs in the safety documents:
  grep -rn "KIRRA-SG-\|KIRRA-SR-" \
    docs/safety/ --include="*.md" | sort

STEP 3: Compare the two lists. Identify:
  a. Safety goals that have NO corresponding test reference
  b. Tests that reference safety goals not in the RTM
  c. Safety goals with only one test (single point of failure in verification)

STEP 4: For each gap found in STEP 3a, add a placeholder test stub
in the appropriate test file with a TODO comment:

  #[test]
  #[ignore = "TODO(CERT-003): implement test for KIRRA-SG-XXX"]
  fn test_safety_goal_kirra_sg_xxx() {
      // Safety Goal: KIRRA-SG-XXX — [description from safety goals doc]
      // Requirement: [requirement from RTM]
      // This test must verify: [what needs to be tested]
      // Currently unimplemented — tracked in CERT-003
      todo!("implement KIRRA-SG-XXX verification")
  }

STEP 5: Update docs/safety/REQUIREMENTS_TRACEABILITY.md to add a
"Test Coverage" column showing which test(s) verify each requirement.

STEP 6: Generate a gap report at docs/safety/RTM_GAP_REPORT.md:
  # RTM Gap Report
  Generated: [date]
  Total safety goals: N
  Goals with test coverage: N
  Goals without test coverage: N (list them)
  Coverage percentage: N%

STEP 7: Document in /work/decisions.md:
  ### ADL-011 — RTM Coverage Baseline
  [baseline coverage percentage and gap count]

Commit message:
  docs(safety): audit RTM coverage, add test stubs for gaps — CERT-003

Do NOT implement the test logic — only add #[ignore] stubs with TODO.
Do NOT modify safety goal definitions.
```

---

## CERT-004 `safety-case` `simulation` `certification`

**Fault Injection Test Suite**

ISO 26262 ASIL-D requires demonstrating that the system enters a safe
state under fault conditions. Currently Kirra has adversarial input
testing (proptest) but no structured fault injection — deliberately
triggering specific failure modes and verifying the documented safe
state response. TÜV will ask for fault injection evidence.

#### Claude Code Prompt
```
You are working in the kirra-runtime-sdk repository (root at ~/kirra-runtime-sdk).

Task: Create a fault injection test suite that verifies safe-state
responses for each documented failure mode.

AUTHORITY MODEL (canonical):
  LockedOut → hard stop (0.0) — requires human intervention
  Degraded  → MRC cap (MRC_VELOCITY_CEILING_MPS)
  Governor unreachable → Degraded semantics + governor_unreachable log

STEP 0: Read the existing safe state documentation:
  cat docs/safety/SAFETY_GOALS.md | grep -A3 "safe state\|SafeState\|fail"
  grep -rn "governor_unreachable\|SafeState\|LockedOut\|Degraded" \
    src/ --include="*.rs" | head -20

STEP 1: Create tests/fault_injection.rs with the following structure.
For each fault, verify the system enters the correct safe state.

  // CERT-004: Fault Injection Test Suite
  // Verifies safe-state responses per ISO 26262 ASIL-D requirements
  // Each test maps to a documented failure mode in SAFETY_GOALS.md

  // FAULT 1: NaN/Inf at model output boundary
  // Expected: tick() returns safe floor (Halt or 0.0), does not panic
  #[tokio::test]
  async fn fault_nan_inf_at_model_output_boundary() { ... }

  // FAULT 2: Governor unreachable (timeout)
  // Expected: posture drops to Degraded, MRC cap applied, event logged
  #[tokio::test]
  async fn fault_governor_unreachable() { ... }

  // FAULT 3: Sensor telemetry timeout
  // Expected: posture transitions to Degraded after watchdog fires
  #[tokio::test]
  async fn fault_sensor_telemetry_timeout() { ... }

  // FAULT 4: DAG cycle detected (dependency graph corruption)
  // Expected: LockedOut posture, hard stop
  #[tokio::test]
  async fn fault_dag_cycle_detected() { ... }

  // FAULT 5: RSS violation (gap < safe distance)
  // Expected: Degraded posture, MRC cap applied
  #[tokio::test]
  async fn fault_rss_violation() { ... }

  // FAULT 6: Multiple simultaneous faults
  // Expected: most severe posture wins (LockedOut > Degraded > Nominal)
  #[tokio::test]
  async fn fault_multiple_simultaneous() { ... }

  // FAULT 7: Admin token absent on mutation route
  // Expected: 503 returned, no state change
  #[tokio::test]
  async fn fault_admin_token_absent() { ... }

  // FAULT 8: Recovery from Degraded (streak threshold met)
  // Expected: posture returns to Nominal after N consecutive clean ticks
  #[tokio::test]
  async fn fault_recovery_from_degraded() { ... }

For each test:
- Read the actual safety goal ID from SAFETY_GOALS.md and use it
- Follow the existing test patterns in tests/rss_posture_tests.rs
- Use ScenarioRunner for posture tests
- Use actual tokio::test for async tests
- Assert the specific safe state, not just "no panic"

STEP 2: Run the test suite:
  cargo test --test fault_injection 2>&1

STEP 3: Add fault injection to CI in .github/workflows/ci.yml:
  - name: Fault injection tests
    run: cargo test --test fault_injection

STEP 4: Document in /work/decisions.md:
  ### ADL-012 — Fault Injection Coverage
  [list of fault modes tested and their safe state responses]

Commit message:
  test(safety): add fault injection suite — ISO 26262 ASIL-D safe state verification

Do NOT add #[ignore] to fault injection tests.
All fault injection tests must pass before committing.
If a test reveals an actual safety bug, stop and report before proceeding.
```

---

## CERT-005 `safety-case` `docs` `certification`

**Rust Safety Coding Standard (MISRA-equivalent)**

ISO 26262 Part 6 requires documented coding guidelines enforced
throughout the safety-critical codebase. MISRA C/C++ is the standard
for C/C++. For Rust, an equivalent must be defined and documented.
TÜV will ask for the coding standard and evidence of its enforcement.

#### Claude Code Prompt
```
You are working in the kirra-runtime-sdk repository (root at ~/kirra-runtime-sdk).

Task: Create a Rust Safety Coding Standard document for Kirra and
audit the codebase for compliance.

STEP 0: Read the existing coding guidelines if present:
  cat docs/safety/CODING_GUIDELINES.md 2>/dev/null || echo "Not found"

STEP 1: Create docs/safety/RUST_SAFETY_CODING_STANDARD.md with the
following content. This is the authoritative coding standard for all
safety-critical Rust code in the Kirra codebase.

  # Kirra Rust Safety Coding Standard
  Version: 1.0
  Applicable to: All code in src/, parko/parko-core/src/,
                 parko/parko-kirra/src/
  Standard: Kirra-RSS-001 (Rust Safety Standard)
  Rationale: ISO 26262 Part 6 requires documented coding guidelines
             for ASIL-D software. This standard is the Rust equivalent
             of MISRA C, adapted for Rust's ownership model.

  ## RSR-001: No unsafe in safety-critical paths
  Rationale: unsafe bypasses Rust's memory safety guarantees.
  Rule: unsafe blocks are forbidden in src/posture_engine*.rs,
        src/verifier.rs, src/audit_chain.rs, parko/parko-kirra/src/,
        parko/parko-core/src/control_loop.rs, parko/parko-core/src/rss.rs
  Verification: grep -rn "unsafe" <path> must return zero results.

  ## RSR-002: No unwrap() in safety-critical paths
  Rationale: unwrap() panics on None/Err, violating fail-closed semantics.
  Rule: unwrap() forbidden in safety-critical paths listed above.
        Use ?, match, or unwrap_or_else with a safe default.
  Verification: grep -rn "\.unwrap()" <path> must return zero results
                in safety-critical files.

  ## RSR-003: No unbounded recursion
  Rationale: Unbounded recursion causes stack overflow, undefined behavior.
  Rule: All recursive functions must have a documented depth bound
        enforced by a counter parameter or const limit.
        (Kirra's DAG traversal uses MAX_DEPENDENCY_DEPTH = 10)
  Verification: Code review + static analysis.

  ## RSR-004: All error paths explicitly handled
  Rationale: Silently ignored errors violate ASIL-D fault detection.
  Rule: Result and Option types must be explicitly handled.
        let _ = ... is forbidden for safety-critical return values.
  Verification: cargo clippy -W clippy::must_use_candidate

  ## RSR-005: No dynamic allocation in hot safety path
  Rationale: Heap allocation can fail or introduce non-deterministic latency.
  Rule: The evaluate()/tick() hot path must not allocate.
        Pre-allocate buffers at initialization.
  Verification: Profiling + code review.

  ## RSR-006: Constants for all safety thresholds
  Rationale: Magic numbers in safety logic are a maintenance hazard.
  Rule: All safety thresholds must be named constants with doc comments.
        (e.g. MRC_VELOCITY_CEILING_MPS, MAX_DEPENDENCY_DEPTH,
        CHALLENGE_TTL_MS, AV_RECOVERY_STREAK_THRESHOLD)
  Verification: grep -rn "[0-9]\+\.[0-9]" src/ for bare float literals
                in safety logic.

  ## RSR-007: Deterministic behavior under all inputs
  Rationale: Non-determinism in safety logic is unacceptable at ASIL-D.
  Rule: Safety-critical functions must produce identical output for
        identical input. No randomness, no time-dependent behavior
        in enforce()/evaluate()/tick() hot paths.
  Verification: Proptest property-based tests (PARK-003, PARK-017).

STEP 2: Audit the codebase for RSR-001 through RSR-006 compliance:
  echo "=== RSR-001: unsafe in safety paths ==="
  grep -rn "unsafe" src/posture_engine*.rs src/verifier.rs \
    src/audit_chain.rs parko/parko-kirra/src/ \
    parko/parko-core/src/control_loop.rs \
    parko/parko-core/src/rss.rs 2>/dev/null

  echo "=== RSR-002: unwrap() in safety paths ==="
  grep -rn "\.unwrap()" src/posture_engine*.rs src/verifier.rs \
    parko/parko-kirra/src/ parko/parko-core/src/control_loop.rs \
    parko/parko-core/src/rss.rs 2>/dev/null

  echo "=== RSR-006: bare float literals in safety logic ==="
  grep -rn "[0-9]\+\.[0-9]" src/posture_engine*.rs \
    parko/parko-kirra/src/ parko/parko-core/src/rss.rs 2>/dev/null

Report all findings before making any changes.

STEP 3: For any RSR-002 violations (unwrap() in safety paths), replace
with explicit error handling. Document each change in a commit message.

STEP 4: Document audit results in /work/decisions.md:

  ### ADL-013 — Rust Safety Coding Standard Compliance Baseline
  Date: [today]
  Standard: RUST_SAFETY_CODING_STANDARD.md v1.0
  RSR-001 (no unsafe): PASS / N violations found
  RSR-002 (no unwrap): PASS / N violations fixed
  RSR-003 (no unbounded recursion): PASS (MAX_DEPENDENCY_DEPTH enforced)
  RSR-004 (error handling): [result]
  RSR-005 (no hot-path alloc): PASS (pre-allocated in constructors)
  RSR-006 (named constants): PASS / N bare literals found
  RSR-007 (deterministic): PASS (verified by proptests PARK-003, PARK-017)

STEP 5: Verify cargo test still passes after any fixes:
  cargo test --workspace 2>&1 | tail -5

Commit message:
  docs(safety): add Rust Safety Coding Standard, audit compliance — CERT-005

Do NOT change safety logic behavior when fixing RSR-002 violations.
If an unwrap() in a safety path cannot be safely replaced without
changing behavior, add a comment explaining why it is safe and file
a follow-up issue.
```

---

## CERT-006 `kirra-governor` `safety-case` `certification`

> **Status: derived / implemented — pending safety-engineer review (NOT closed).**
> - v1–v3: software lockstep comparator with posture-aware, speed-gated,
>   two-axis divergence escalation + audit sink (`comparator.rs`).
> - **Diversity (2026-06-01):** the identical-redundancy instantiation is
>   replaced by structural/algorithmic diversity (Approach A) —
>   `DiverseKirraGovernor` (`parko/crates/parko-kirra/src/diverse.rs`) is a
>   second governor enforcing the same properties via different computation;
>   the comparator is now generic over the shadow (default = diverse). This
>   catches implementation-level systematic faults; it does NOT catch
>   spec-level faults shared by both (honest limit). Full N-version
>   (Approach B) is the stronger-but-later step.
> - Diversity argument: `docs/safety/COMPARATOR_DIVERSITY.md`
>   (KIRRA-CERT006-DIVERSITY-001, DRAFT). Needs the same human review as #136
>   before it can be cited as validated coverage.

**Primary + Shadow Governor Comparator**

NVIDIA DRIVE AGX uses hardware lockstep — two cores run the same
computation and outputs are compared. Kirra implements the software
equivalent: two independent KirraGovernor instances receive the same
inputs, their outputs are compared, and divergence triggers LockedOut.
This is the architectural answer to hardware redundancy for the safety
governance layer and is required for the ASIL-D decomposition argument.

#### Claude Code Prompt
```
You are working in the parko-kirra crate.

PREREQUISITE: PARK-016 (RSS gate in KirraGovernor) must be complete.
Verify: grep -n "update_rss_state\|MRC_VELOCITY_CEILING_MPS" \
  parko/parko-kirra/src/lib.rs

AUTHORITY MODEL (canonical, commits 9943aa9/e1ba1a2/21c3a35):
  LockedOut  → 0.0 (hard stop)
  Degraded   → min(proposed, MRC_VELOCITY_CEILING_MPS)
  Divergence between primary and shadow → LockedOut

Task: Implement GovernorComparator — software lockstep for KirraGovernor.

STEP 0: Read the current KirraGovernor evaluate() signature:
  grep -n "fn evaluate\|pub fn\|ControlCommand\|EnforcementAction" \
    parko/parko-kirra/src/lib.rs | head -20

STEP 1: Create parko/parko-kirra/src/comparator.rs:

  use crate::{KirraGovernor, MRC_VELOCITY_CEILING_MPS};
  use parko_core::PostureState;

  /// Tolerance for floating-point comparison between primary and shadow.
  /// Set to 1e-9 — effectively exact equality for f64 safety computations.
  const COMPARATOR_TOLERANCE: f64 = 1e-9;

  /// Software lockstep safety comparator.
  ///
  /// Runs two independent KirraGovernor instances with identical inputs.
  /// If their outputs diverge beyond COMPARATOR_TOLERANCE, returns
  /// EnforcementAction::Halt (LockedOut semantics).
  ///
  /// This is the software equivalent of hardware lockstep dual-core
  /// execution used in NVIDIA DRIVE AGX and NXP S32 safety MCUs.
  /// Per CERT-006 — ISO 26262 ASIL-D decomposition argument.
  pub struct GovernorComparator {
      primary: KirraGovernor,
      shadow: KirraGovernor,
  }

  impl GovernorComparator {
      pub fn new(primary: KirraGovernor, shadow: KirraGovernor) -> Self {
          Self { primary, shadow }
      }

      /// Evaluate a command through both governors.
      /// Returns the primary output if both agree within tolerance.
      /// Returns Halt (LockedOut) if outputs diverge.
      pub fn evaluate(
          &self,
          cmd: &<ControlCommand type from STEP 0>,
          posture: PostureState,
      ) -> <EnforcementAction type from STEP 0> {
          let primary_out = self.primary.evaluate(cmd, posture);
          let shadow_out  = self.shadow.evaluate(cmd, posture);

          let primary_vel = <extract velocity from primary_out>;
          let shadow_vel  = <extract velocity from shadow_out>;

          if (primary_vel - shadow_vel).abs() > COMPARATOR_TOLERANCE {
              tracing::error!(
                  primary = primary_vel,
                  shadow = shadow_vel,
                  delta = (primary_vel - shadow_vel).abs(),
                  "GovernorComparator: primary/shadow divergence — LockedOut"
              );
              <EnforcementAction::Halt or zero equivalent>
          } else {
              primary_out
          }
      }

      /// Update RSS state on both governors.
      pub fn update_rss_state(&mut self, state: parko_core::rss::RssState) {
          self.primary.update_rss_state(state.clone());
          self.shadow.update_rss_state(state);
      }
  }

STEP 2: Export from parko-kirra/src/lib.rs:
  pub mod comparator;
  pub use comparator::GovernorComparator;

STEP 3: Write tests in comparator.rs:
  Test A: Identical inputs → primary returned
  Test B: Injected divergence → Halt returned
  Test C: update_rss_state propagates to both governors
  Test D: LockedOut posture → both return 0.0 → no divergence

STEP 4: Run tests:
  cargo test -p parko-kirra 2>&1 | tail -10

Commit message:
  feat(parko-kirra): add GovernorComparator software lockstep — CERT-006

No unsafe code.
```

---

## CERT-007 `safety-case` `docs` `certification`

**Safe State Definition Document**

ISO 26262 requires an explicit safe state definition — the state the
system enters when a fault is detected. Kirra has this behavior
implicitly (LockedOut → hard stop, Degraded → MRC cap) but it is not
documented as a standalone safe state specification. TÜV requires
this document before the first assessment conversation.

#### Claude Code Prompt
```
You are working in the kirra-runtime-sdk repository (root at ~/kirra-runtime-sdk).

Task: Create the Safe State Specification document.
This is documentation only. Do NOT modify any source files.

Create docs/safety/SAFE_STATE_SPECIFICATION.md:

  # Kirra Safe State Specification
  Document ID: KIRRA-SSS-001
  Version: 1.0
  Status: Active
  Standard: ISO 26262 ASIL-D

  ## Overview
  A safe state is a system state in which no unreasonable risk exists.
  When Kirra detects a fault condition, it transitions to the appropriate
  safe state based on fault severity. This document specifies each safe
  state, its trigger conditions, its behavior, and its recovery path.

  ## Safe States

  ### SS-001: Normal Operation (PostureState::Nominal)
  Behavior: Full kinematic envelope, 35.0 m/s ceiling, stricter accel
            rate-limit applied by KirraGovernor nominal profile.
  Entry: All nodes trusted, no RSS violation, governor reachable.
  Exit: Any fault trigger below.

  ### SS-002: Minimum Risk Condition (PostureState::Degraded)
  Behavior: MRC_VELOCITY_CEILING_MPS (5.0 m/s) cap applied by
            KirraGovernor MRC fallback profile. System continues
            operating in reduced-capability mode.
  Entry (any of):
    - Sensor telemetry timeout (AV_TELEMETRY_TIMEOUT_MS exceeded)
    - RSS violation (gap < longitudinal_safe_distance)
    - Governor unreachable (timeout or network partition)
    - Node trust state Untrusted with non-critical dependency impact
  Recovery: AV_RECOVERY_STREAK_THRESHOLD (5) consecutive clean ticks
            within AV_RECOVERY_WINDOW_MS (10,000ms) → Nominal
  Implements: ISO 26262 safe state for recoverable faults

  ### SS-003: Lockout / Hard Stop (PostureState::LockedOut)
  Behavior: 0.0 m/s — hard stop. No commands forwarded to actuators.
            Human intervention required to clear.
  Entry (any of):
    - DAG cycle detected in dependency graph
    - Multiple critical nodes Untrusted (DAG propagation)
    - GovernorComparator divergence detected (CERT-006)
    - MAX_DEPENDENCY_DEPTH exceeded in DAG traversal
  Recovery: Requires explicit human-initiated reset via
            KIRRA_SUPERVISOR_RESET_KEY endpoint.
            Automatic recovery from LockedOut is NOT permitted.
  Implements: ISO 26262 safe state for non-recoverable faults

  ## Fault to Safe State Mapping

  | Fault Mode | Safe State | Recovery |
  |------------|-----------|---------|
  | NaN/Inf model output | SS-002 Degraded (safe floor) | Automatic |
  | Sensor telemetry timeout | SS-002 Degraded | Automatic (streak) |
  | RSS violation | SS-002 Degraded | Automatic (streak) |
  | Governor unreachable | SS-002 Degraded | Automatic |
  | DAG cycle detected | SS-003 LockedOut | Human reset required |
  | DAG depth exceeded | SS-003 LockedOut | Human reset required |
  | GovernorComparator divergence | SS-003 LockedOut | Human reset required |
  | Multiple simultaneous faults | Most severe wins | Per worst fault |

  ## Safe State Transition Invariants
  1. LockedOut can only be cleared by human reset — never automatic
  2. Degraded recovery requires N consecutive clean ticks — not immediate
  3. If governor is unreachable, local MRC floor applies — not pass-through
  4. NaN/Inf model output → safe floor before governor runs
  5. DAG LockedOut propagates upward — never downgraded by RSS recovery
  6. LockedOut dominates Degraded — if both conditions present, LockedOut wins

  ## Implementation References
  - PostureState enum: src/posture_engine.rs
  - KirraGovernor authority model: parko/parko-kirra/src/lib.rs
  - ADL-001: /work/decisions.md
  - Safety goals: docs/safety/SAFETY_GOALS.md

Commit message:
  docs(safety): add Safe State Specification — KIRRA-SSS-001 — CERT-007
```
