# Architecture Decision Log (ADL)

> Entries are immutable once written. Superseded decisions get a new entry
> referencing the old one by ADL number. Date format: YYYY-MM-DD.

---

## ADL-001 — Governor injection and authority model

**Date:** 2026-05-26
**Status:** Accepted
**Deciders:** Justin Looney

### Decision

`ControlLoop` in `parko-core` stores a governor as
`Option<Box<dyn SafetyGovernor>>`, injected via a builder method
`with_governor(impl SafetyGovernor + 'static)`. KirraGovernor holds final
authority over every command on the hot path. The authority model is:

- **Degraded:** KirraGovernor applies the MRC fallback `VehicleKinematicsContract`
  (ceiling: **5.0 m/s**). This is a velocity cap, not a hard zero veto. The
  vehicle is slowed to a safe reduced speed, not halted.
- **LockedOut:** KirraGovernor issues a hard stop — `EnforcementAction::Deny` —
  yielding **0.0 m/s**. No motion is permitted. The vehicle must remain stopped
  until a supervisor issues a manual reset. LockedOut and Degraded are **separate
  branches** that must never share a code path or contract instance.
- **Nominal:** KirraGovernor applies the nominal reference contract (ceiling:
  **35.0 m/s**) with a **tighter acceleration rate-limit** than the fallback
  contract. For proposed velocities between 5–35 m/s with no prior command
  (`previous = None`), the nominal rate constraint produces a more conservative
  output than the fallback's looser rate limit. The governor's output is bounded
  by both the speed cap and the single-tick rate budget.
- **Governor unreachable (timeout / partition):** Treat as Degraded. Apply the
  MRC cap locally. Log a `governor_unreachable` safety event. Do NOT treat as
  LockedOut — unreachability is a recoverable transient fault, not a structural
  safety failure.
- **RSS unsafe (sensor gap, safety distance violation):** Treat as Degraded.
  Apply the MRC cap. Log an RSS violation event. Do NOT treat as LockedOut — an
  RSS gap violation is recoverable once the sensor stream and clearance are
  restored.

The synchronous call path is: `planned_cmd → governor → final_cmd`. There is no
concurrent or asynchronous governor path in the control loop.

### Why

Safety policies are domain-specific; a trait object lets each deployment inject
KirraGovernor without forking `parko-core`. The `Option` preserves backward
compatibility. The MRC fallback model on Degraded matches fail-reduced (not
fail-stop) semantics — sudden full stops at speed are themselves hazardous;
slowing to a controlled 5.0 m/s allows the operator or safety driver to intervene
safely. LockedOut triggers a hard stop (0.0 m/s) because at this posture the
system has exceeded the threshold requiring mandatory human review before any
further motion is authorized. The nominal profile's tighter rate-of-change limit
prevents jerky acceleration that could destabilize the vehicle under normal
operation. The fallback-to-conservative-envelope rule ensures the loop never runs
unguarded even if the governor crate is temporarily unavailable.

### Alternatives Considered

1. **Compile-time generics** (`ControlLoop<G: SafetyGovernor>`): avoids heap
   allocation but makes default (no-governor) construction awkward. Rejected.
2. **Function pointer** (`fn(f64, PostureState) -> f64`): stateless; governors
   that require calibration data must close over state, which this can't express.
   Rejected.
3. **Best-effort async governor:** introduces non-determinism on the hot path.
   Rejected — synchronous path is required for determinism.

### Consequences

- The no-governor code path (built-in clamp) must remain correct and tested.
- Any crate injecting a governor owns the full safety guarantee for that loop.
- `SafetyGovernor` must be `Send + Sync`.
- Tests that assert LockedOut output == 0.0 are **correct**. Assertions of
  output ≤ 5.0 for LockedOut are insufficient — they conflate LockedOut with
  Degraded and must be replaced with the exact equality check. See PARK-003
  proptest (`governor_locked_out_always_returns_zero`, 10 000 cases).
- `KirraGovernor::evaluate()` must keep LockedOut and Degraded as separate
  match arms that never share a contract instance or code path.

### Note: PARK-003 bug history

PARK-003 (commit `47550ce`) wrote proptest assertions for the governor and found
that LockedOut and Degraded produced identical outputs. At that time ADL-001 was
incorrectly updated to accept this as correct behavior ("both postures share one
contract instance"). This was a documentation error, not a design insight.
Commits `9943aa9` and `e1ba1a2` corrected the governor and proptest respectively.
LockedOut is and always was a hard stop. The PARK-003 finding was a bug report.

---

## ADL-002 — Built-in degraded-mode clamp interaction with KirraGovernor

**Date:** 2026-05-26
**Status:** Accepted
**Deciders:** Justin Looney

### Decision

When a `SafetyGovernor` is injected, the built-in scalar clamp in
`ControlLoop::tick` is bypassed **entirely**. The governor receives the raw
proposed output and is solely responsible for producing a safe value. The
`builtin_clamp_ceiling(proposed, state) -> f64` helper is exposed as
`pub(crate)` so property tests can verify the governor never exceeds the
ceiling the built-in clamp would have applied.

If the governor becomes unreachable (see ADL-001), the built-in clamp is
re-activated as a conservative fallback, not as a co-enforcer — partial
suppression (both active simultaneously) is never the intended state.

### Why

Partial suppression creates ambiguity: which bound wins, does order matter, and
can a governor intentionally allow a wider envelope for a custom platform? Full
suppression makes the contract explicit. The `test_builtin_clamp_suppressed`
acceptance test enforces this invariant.

### Consequences

- `builtin_clamp_ceiling` must track production clamp logic exactly; any change
  to the clamp must update the helper in the same commit.
- Any crate injecting a governor must include property tests asserting its output
  is within the desired safety envelope for all posture states.

---

## ADL-003 — InferenceBackend trait: zero-copy hot-path contract

**Date:** 2026-05-26
**Status:** Accepted
**Deciders:** Justin Looney

### Decision

The `InferenceBackend` trait defines the hot-path method as:

```rust
fn run(&self, input: &[f32], output: &mut [f32]) -> Result<(), BackendError>;
```

All backend implementations must pre-allocate all scratch memory at `new()`. No
heap allocation is permitted on the `run` path. Shape mismatch returns
`BackendError::ShapeMismatch`; it never panics.

The multi-silicon backend architecture (`BackendDescriptor`, TensorRT, QNN, TIDL,
OpenVINO, AMD) is defined and specced but **not yet implemented** as of this ADL.
Only the CPU ONNX backend (`parko-onnx`) currently implements the trait; its MNIST
integration test must be verified before being treated as green. `MockBackend` is
the preferred backend for all parko-core unit tests.

### Why

Inference runs at 50–200 Hz on embedded targets. Dynamic allocation on every call
causes latency spikes and potential OOM in bounded-memory safety contexts. A
caller-provided output slice allows buffer reuse across ticks.

### Consequences

- Every backend must document required input/output slice lengths.
- `InferenceLoop` owns and reuses output buffers for its lifetime.
- When TensorRT and QNN backends are implemented (PARK-020–PARK-027), this ADL
  must be revisited if any SDK cannot satisfy the no-alloc constraint.

---

## ADL-004 — Deterministic tick grid: `RuntimeClock` / `MockClock` abstraction

**Date:** 2026-05-26
**Status:** Accepted
**Deciders:** Justin Looney

### Decision

`ControlLoop` and `InferenceLoop` in `parko-core` accept a `Clock` trait object
(`Arc<dyn Clock>`). Two implementations ship: `RuntimeClock` (wraps wall clock)
and `MockClock` (manually advanceable `AtomicU64`). All timing logic inside the
loop calls `self.clock.now_ms()`; no direct use of wall-clock APIs inside
timing-sensitive code.

The same `Clock` abstraction is used in `kirra-runtime-sdk` (`src/clock.rs`) for
`ScenarioRunner` and the telemetry watchdog. The two crates share the concept but
may not share the same type; use the parko-core definition for parko-core tests.

### Why

Tests that verify timing behaviour cannot use wall-clock time reliably in CI.
`MockClock::advance(ms)` lets tests advance time synchronously, making them fast,
deterministic, and independent of system load. This also eliminates
`std::thread::sleep` from any test in these crates.

### Consequences

- All duration constants are compared against `clock.now_ms()`, never against
  a wall-clock call directly.
- `ControlLoop::new()` accepts `Arc<dyn Clock>`; callers that don't care pass
  `Arc::new(RuntimeClock)`.
- The QNX deployment path (PARK-024) must verify that the clock abstraction
  compiles cleanly on QNX's POSIX subset before assuming compatibility.

---

## ADL-005 — Safety posture state machine: asymmetric transitions and hysteresis

**Date:** 2026-05-26
**Status:** Accepted
**Deciders:** Justin Looney

### Decision

`FleetPosture` (in `kirra-runtime-sdk`) has three variants: `Nominal`, `Degraded`,
`LockedOut`. Fault transitions are instantaneous (one bad event → Degraded;
configured consecutive violations → LockedOut). Recovery from Degraded requires
`AV_RECOVERY_STREAK_THRESHOLD = 5` consecutive clean reports within
`AV_RECOVERY_WINDOW_MS = 10,000` ms. Recovery from LockedOut is manual (requires
supervisor reset key; no automatic hysteresis).

`PostureState` in `parko-core` mirrors this three-variant structure for use in
governor and control-loop logic.

KirraGovernor authority maps onto this state machine (twice corrected 2026-05-26:
first correction removed the incorrect hard-veto description; second correction
established the definitive model: LockedOut is a hard stop at 0.0 m/s and must
never share a code path with Degraded):
- `Nominal`   → KirraGovernor applies nominal reference profile (35.0 m/s
                ceiling + strict rate-of-change limit).
- `Degraded`  → KirraGovernor applies MRC fallback profile (5.0 m/s ceiling).
                Velocity cap, not a hard zero veto.
- `LockedOut` → KirraGovernor issues a hard stop (**0.0 m/s**,
                `EnforcementAction::Deny`). No motion is permitted. Manual
                supervisor reset required to exit this state. LockedOut and
                Degraded are distinct postures with distinct enforcement and
                must never share a code path or contract instance.
- **Governor unreachable** → Degraded semantics. MRC cap applied locally.
  Not LockedOut — recoverable transient.
- **RSS unsafe** → Degraded semantics. MRC cap applied. Not LockedOut —
  recoverable once sensor stream and clearance are restored.

IEEE 2846 behavioral safety integration is planned but not yet implemented. When
implemented (PARK-015), RSS violations must reset the recovery streak to 0 on
every violation tick. IEC 61508 SIL 3 and ASTM F3269 safety case mappings
(PARK-039, PARK-040) will trace to this state machine when written.

### Why

Symmetric transitions cause posture flapping under noisy sensor streams.
Asymmetric hysteresis keeps the system degraded long enough for an operator to
diagnose the root cause. The Nominal/Degraded/LockedOut distinction maps to
ISO 26262 ASIL-D fail-safe decomposition.

### Consequences

- `should_route_command` is the single authoritative gate; posture must be read
  from the cache, never recomputed inline in a handler.
- Any path that transitions to LockedOut must emit a `LockoutReason` audit chain
  entry before changing state.
- Recovery streak resets on any new fault during the hysteresis window.

---

## ADL-006 — Clock abstraction naming (WallClock vs RuntimeClock)

**Date:** 2026-05-26
**Status:** Accepted
**Deciders:** Justin Looney

### Decision

Two types with "clock" in their name exist in parko-core and serve
different purposes. `WallClock` (clock.rs) implements the `Clock`
trait and returns wall-clock milliseconds via `SystemTime` — it is
the injectable clock used by `ControlLoop` for deterministic testing
via `MockClock`. `RuntimeClock` (runtime.rs) is a sleep-based
tick-rate driver — a different concept entirely. The name `WallClock`
was chosen specifically to avoid collision with the pre-existing
`RuntimeClock`.

Any future prompt or code that references `RuntimeClock` means the
sleep-based tick driver in runtime.rs. `WallClock` means the
injectable `Clock` trait implementation in clock.rs. These must never
be confused.

`MockClock` (clock.rs) is the test double: `Arc<AtomicU64>`,
`advance(ms)`, and `Clone`. The pattern is: test keeps one handle,
`ControlLoop` holds the other via `Arc::new(mock.clone())`. This
enables deterministic time control with zero `sleep()` calls.

`ControlLoop::tick()` returns `Result<Option<PostureSnapshot>, String>`:
- `Ok(None)` — tick interval not yet elapsed, no action taken
- `Ok(Some(snapshot))` — interval elapsed, inference ran, snapshot returned
- `Err(msg)` — error in the inference or governor path

### Why

`RuntimeClock` was already a public export from parko-core (`runtime.rs`) before
the Clock trait was introduced. Reusing the name would have created a type
collision and broken the existing public API. `WallClock` is descriptive and
unambiguous. The `Option`-wrapped return from `tick()` is the minimal API change
required to let callers distinguish "not time yet" from "fired" without introducing
a new error variant or changing the error path semantics.

### Consequences

- Callers of `ControlLoop::tick()` must unwrap two layers: `Result` then `Option`.
- `test_posture_divergence.rs` call sites updated: `.expect("...").expect("tick should fire")`.
- Any new control-loop integration test must use `MockClock` (not `RuntimeClock`) for
  timing control and must never call `std::thread::sleep` or `tokio::time::sleep`.
- Documentation and prompts must use the correct names: `WallClock` (Clock trait impl),
  `RuntimeClock` (tick-rate driver), `MockClock` (test double).

---

## ADL-007 — ORT session configuration: thread limit and optimization level

**Date:** 2026-05-26
**Status:** Accepted
**Deciders:** Justin Looney

### Decision

`OrtBackend::new()` must configure the ORT session with:
- `with_intra_threads(1)` — limits intra-op parallelism to one thread
- `with_optimization_level(GraphOptimizationLevel::Disable)` — skips graph
  optimization passes at session init time

The ORT shared library is not in the system library search path on any target
platform. `parko/.cargo/config.toml` sets `ORT_DYLIB_PATH` to the installed
location (`/root/.local/onnxruntime/lib/libonnxruntime.so`). Any new deployment
environment (Jetson, QNX) needs an equivalent config pointing to that platform's
ORT installation — there is no universal default path.

`OrtBackend::descriptor()` returns `BackendDescriptor::Cpu` via the default
impl added to `InferenceBackend` in PARK-008. No override is needed in
`parko-onnx`.

### Why

Without `with_intra_threads(1)`, the ORT session builder blocks indefinitely
during test runs: ORT detects the available core count and attempts to spawn a
full thread pool, which hangs in restricted CI environments. Disabling graph
optimization eliminates the initialization-time optimization passes that add
latency and are unnecessary for correctness in test scenarios. Both options are
reversible for production builds where throughput matters.

### Consequences

- Any backend that wraps an ORT session must apply these two builder options
  unless benchmarking explicitly requires otherwise.
- New deployment environments must add an equivalent `ORT_DYLIB_PATH` entry in
  their `.cargo/config.toml` before `cargo test -p parko-onnx` can succeed.
- Production builds that need ORT graph optimization must override
  `with_optimization_level` explicitly — the test-safe default is `Disable`.

---

## Crate and Struct Name Audit (2026-05-26)

> Read-only audit of the `parko/` workspace. Source: PARK-007.
> No files were modified. Commands run verbatim against the live tree.

### Workspace members (`parko/Cargo.toml`)

```
members = ["crates/parko-core", "crates/parko-onnx", "crates/parko-kirra"]
```

Three crates:

| Crate name    | Path                          | Role |
|---------------|-------------------------------|------|
| `parko-core`  | `crates/parko-core/`          | Core traits, ControlLoop, InferenceLoop, Clock types |
| `parko-onnx`  | `crates/parko-onnx/`          | CPU ONNX backend (InferenceBackend impl) |
| `parko-kirra` | `crates/parko-kirra/`         | KirraGovernor — SafetyGovernor impl for parko-core |

### Governor struct

The production governor is **`KirraGovernor`** (already Kirra-named; no rename needed):

- **Defined in:** `parko/crates/parko-kirra/src/lib.rs` — line 42 (`pub struct KirraGovernor`)
- **SafetyGovernor impl:** same file, line 96 (`impl SafetyGovernor for KirraGovernor`)
- **Constructors:** `KirraGovernor::new()`, `KirraGovernor::nominal()`, `KirraGovernor::mrc_fallback()`
- **Exported constant:** `MRC_VELOCITY_CEILING_MPS: f64 = 5.0` (pub const in `parko-kirra/src/lib.rs`)
- **No `AegisGovernor` anywhere in the workspace.**

### `SafetyGovernor` trait

- **Defined in:** `parko/crates/parko-core/src/safety.rs` line 43
  ```rust
  pub trait SafetyGovernor: Send + Sync { ... }
  ```
- **Re-exported from:** `parko-core/src/lib.rs` line 37
  ```rust
  pub use safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
  ```
- **Production impl:** `KirraGovernor` in `parko-kirra/src/lib.rs`
- **Test doubles (not production):**
  - `AllowAllGovernor`, `ClampToOneGovernor` — `parko-core/src/safety.rs`
  - `ClampToTwoGovernor`, `ZeroGovernor`, `RecordingGovernor` — `parko-core/src/scheduler.rs`

### Clock types (PARK-005 additions)

| Type | File | Purpose |
|------|------|--------|
| `Clock` (trait) | `parko-core/src/clock.rs:8` | `fn now_ms(&self) -> u64; Send + Sync` |
| `WallClock` | `parko-core/src/clock.rs:13` | Production impl — `SystemTime` / UNIX epoch |
| `MockClock` | `parko-core/src/clock.rs:30` | Test double — `Arc<AtomicU64>` + `advance(ms)`, `Clone` |
| `RuntimeClock` | `parko-core/src/runtime.rs:39` | Sleep-based tick-rate driver — **not** a `Clock` trait impl |

`WallClock` and `MockClock` are re-exported from `parko-core/src/lib.rs`:
```rust
pub use clock::{Clock, MockClock, WallClock};
```
`RuntimeClock` is also re-exported but from `runtime`:
```rust
pub use runtime::{RuntimeClock, RuntimeState, TickStatus};
```

### `ControlLoop::tick()` actual signature

```rust
// parko-core/src/control_loop.rs line 108
pub async fn tick(&mut self) -> Result<Option<PostureSnapshot>, String>
```

- `Ok(None)` — tick interval not yet elapsed
- `Ok(Some(snapshot))` — interval elapsed, inference ran
- `Err(msg)` — sensor stream exhausted or inference/governor error

### Files referencing `KirraGovernor` by name

The governor struct is already named `KirraGovernor`. For reference, every file
that would require editing if `KirraGovernor` were ever renamed:

| File | Nature of reference |
|------|---------------------|
| `parko-kirra/src/lib.rs` | Struct definition + all constructors + SafetyGovernor impl |
| `parko-kirra/tests/test_kirra_governor.rs` | Integration tests (instantiates governor) |
| `parko-core/tests/test_posture_divergence.rs` | Uses `KirraGovernor::new()` via `parko_kirra::KirraGovernor` |
| `parko-core/tests/posture_divergence_proptest.rs` | Uses `KirraGovernor` + `MRC_VELOCITY_CEILING_MPS` |
| `parko-core/src/scheduler.rs` | Doc comment reference only (line 39) |
| `parko-core/src/control_loop.rs` | Doc comment reference only (line 91) |

No rename is required. The struct, crate, and all call sites are already
using Kirra naming (`KirraGovernor`, `parko-kirra`).

---

## ADL-008 — AMD backend target: Vitis AI over ROCm

**Date:** 2026-05-27
**Status:** Accepted
**Deciders:** Justin Looney

### Decision

The AMD backend (PARK-030) will target Vitis AI on Xilinx/AMD FPGAs.
ROCm is deferred indefinitely unless a specific customer requires it.
Development target: AMD Kria K26 (~$200 edge AI platform).

### Why

ROCm targets data center discrete GPU inference — wrong deployment context
for Kirra. Kirra deploys at the edge (vehicles, robots, drones, industrial).
Vitis AI targets Xilinx/AMD FPGAs which are used in automotive safety systems
specifically because of deterministic latency. FPGA inference produces
nanosecond-predictable execution times — a genuine differentiator against
TensorRT (JIT variance) and QNN (thermal throttling variance).

### Consequences

- PARK-030 implements the Vitis AI backend when hardware arrives.
- ROCm deferred indefinitely unless a specific customer requires it.
- The feature flag `backend-amd` (added PARK-012) maps to Vitis AI, not ROCm.

---

## ADL-009 — MC/DC Coverage Baseline

**Date:** 2026-05-27
**Status:** Established
**Standard:** ISO 26262 Part 6 §8 (ASIL-D mandatory)

### Decision

MC/DC (Modified Condition/Decision Coverage) measurement added to CI via
`cargo-llvm-cov --mcdc`. Coverage runs on every push to main and every PR.
Reports uploaded to Codecov; HTML report generated locally via
`scripts/coverage-mcdc.sh`.

### Baseline

Baseline MC/DC coverage measurement pending first CI run on GitHub Actions.
Target for ASIL-D assessment: ≥ 90% MC/DC on safety-critical paths.

Safety-critical paths (priority coverage targets):
- `src/posture_engine.rs` — posture state machine and trigger handling
- `parko/parko-kirra/src/lib.rs` — KirraGovernor evaluate() authority model
- `src/audit_chain.rs` — hash-chained audit ledger
- `parko/parko-core/src/rss.rs` — RSS safe-distance calculations (IEEE 2846-2022)
- `parko/parko-core/src/control_loop.rs` — NaN/Inf guard and clock tick

### Rationale

ISO 26262 Part 6 Table 10 requires MC/DC at ASIL-D. TÜV SÜD will request
MC/DC evidence in the first assessment conversation. Rust + llvm-cov provides
source-level MC/DC reporting equivalent to LDRA/BullseyeCoverage for C.

### Consequences

- `scripts/coverage-mcdc.sh` — local measurement script (llvm-profdata + llvm-cov)
- `.github/workflows/ci.yml` — CI jobs: `test`, `coverage` (cargo-llvm-cov --mcdc), `static-analysis`
- `coverage-report/` and `*.profraw`/`*.profdata`/`lcov.info` added to `.gitignore`
- Codecov integration via `codecov/codecov-action@v4` (fail_ci_if_error: false)

### Update — 2026-05-27

- **Status:** Partial — branch coverage active, MC/DC pending
- **Current:** `--branch` coverage via `cargo-llvm-cov` on nightly
- **Blocked:** rustc nightly removed the `mcdc` value for `-Z coverage-options` (accepts only `block|branch|condition`); `cargo-llvm-cov` hasn't caught up
- **Tracked:** GitHub Issue #65
- **Target:** ≥ 90% MC/DC on safety-critical paths once the toolchain realigns
- **Safety-critical paths:** posture engine, `KirraGovernor::evaluate()`, audit chain, RSS safe-distance, NaN/Inf guard

---

## ADL-010 — QNX Cross-Compilation Findings (PARK-024)

**Date:** 2026-05-28
**Status:** In progress — spike open
**Target:** `x86_64-pc-nto-qnx800` via QNX SDP 8.0 at `/opt/qnx800/sdp2`
**Toolchain:** `nightly` + `-Zbuild-std=std,panic_abort` (tier-3 target, no precompiled stdlib)

### Toolchain confirmed working
- `qnxsdp-env.sh` sources cleanly; `QNX_HOST`, `QNX_TARGET`, `QNX_CONFIGURATION`, and `PATH` set as expected
- `qcc --version` returns `gcc 12.2.0`
- `scripts/test-qnx-vm.sh` clears the SDP guard and sources env

### POSIX-subset gaps encountered (running list)

**Gap #1 — `libc::TCP_KEEPALIVE` missing for `nto` target**

- Symptom: `socket2 v0.6.3` fails to compile —
  `error[E0432]: unresolved import 'libc::TCP_KEEPALIVE'` at `socket2/src/sys/unix.rs:309`
- Root cause: the `libc` crate does not export `TCP_KEEPALIVE` for `target_os = "nto"`. QNX's BSD-derived TCP stack does provide the option at the C header level, but the Rust bindings haven't been generated for it.
- Spike workaround: local `[patch.crates-io]` of `socket2` that selects `SO_KEEPALIVE` under `cfg(target_os = "nto")`. This unblocks compilation but is **not semantically correct** — `SO_KEEPALIVE` is socket-level on/off, not the TCP-level idle timer the original constant configures. Acceptable for the spike whose goal is "binary running"; **not acceptable for production**.
- Proper fix: PR to `rust-lang/libc` exposing `TCP_KEEPALIVE` for `nto-qnx` targets; revert the `socket2` patch once that lands.
- Tracked: GitHub Issue #66
