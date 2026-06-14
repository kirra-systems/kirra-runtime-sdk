# Architecture Decision Log (ADL)

> Entries are immutable once written. Superseded decisions get a new entry
> referencing the old one by ADL number. Date format: YYYY-MM-DD.

---

## ADL-001 — Governor injection and authority model

**Date:** 2026-05-26
**Status:** Accepted
**Deciders:** Kirra Systems, LLC

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
**Deciders:** Kirra Systems, LLC

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
**Deciders:** Kirra Systems, LLC

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
**Deciders:** Kirra Systems, LLC

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
**Deciders:** Kirra Systems, LLC

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
**Deciders:** Kirra Systems, LLC

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
**Deciders:** Kirra Systems, LLC

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
**Deciders:** Kirra Systems, LLC

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

## ADL-010 — QNX POSIX gap analysis (PARK-024 spike findings)

**Date:** 2026-05-28
**Status:** Spike complete — architectural gaps identified
**Target:** `x86_64-pc-nto-qnx800` (also valid for `aarch64-unknown-nto-qnx800`)
**Toolchain:** `nightly` + `-Zbuild-std=std,panic_abort` (tier-3, no precompiled stdlib)
**SDP:** QNX SDP 8.0 at `/opt/qnx800/sdp2`

### Cross-compilation toolchain confirmed working

- `qcc --version` → gcc 12.2.0
- Env sourced cleanly from `/opt/qnx800/sdp2/qnxsdp-env.sh` (`QNX_HOST`, `QNX_TARGET`, `QNX_CONFIGURATION`, `PATH`)
- `scripts/test-qnx-vm.sh` clears the SDP guard and sources env without error
- `cargo +nightly build -Zbuild-std=std,panic_abort --target x86_64-pc-nto-qnx800` builds stdlib successfully

### Architectural gaps identified

Two architectural gaps between QNX SDP 8.0 and Linux/macOS — **not** Rust-bindings problems. The constants are absent from the QNX C headers themselves.

**1. TCP keepalive per-socket options — not present in QNX**

- Linux uses `TCP_KEEPIDLE`; macOS uses `TCP_KEEPALIVE`. **QNX has neither.**
- Only hit in `/opt/qnx800/sdp2/target/qnx/usr/include/` is `curl.h`'s unrelated `CURLOPT_TCP_KEEPALIVE` (libcurl option enum, not the TCP socket option).
- Likely available only as system-wide tuning via `sysctl` or unsupported entirely.
- Compile symptom: `socket2 v0.6.3` fails at `src/sys/unix.rs:309` with `error[E0432]: unresolved import 'libc::TCP_KEEPALIVE'`.

**2. Unix-socket peer credentials — not present in QNX via these mechanisms**

- Linux uses `SO_PEERCRED`; macOS/BSD uses `LOCAL_PEEREID`. **QNX exposes neither in its headers.**
- May be obtainable via `getpeereid()` or `SCM_CREDS` (not investigated in this spike).
- Compile symptom: `tokio v1.52.3` fails at `src/net/unix/ucred.rs:137` with `error[E0432]: unresolved import 'libc::LOCAL_PEEREID'`.

### Impact

A `libc` PR adding bindings is **not** the fix — there is nothing in QNX to bind to. The proper fix is upstream changes in the **consuming crates**:

- `rust-lang/socket2` — add `cfg(target_os = "nto")` arm that disables or stubs the TCP-keepalive-timer code path
- `tokio-rs/tokio` — add `cfg(target_os = "nto")` arm in `net/unix/ucred.rs` that returns an `Unsupported` error or compiles out the peer-cred lookup

The local `[patch.crates-io] socket2 = { path = "../socket2-qnx" }` in `kirra-runtime-sdk/Cargo.toml` remains as a placeholder spike fork; it should be replaced by the upstream-merged behavior.

### Next steps

- File upstream issues / PRs against `rust-lang/socket2` and `tokio-rs/tokio` to add QNX-aware code paths
- Track follow-up work as PARK-024b (upstream contribution)
- "Binary running on QNX" remains the eventual goal but is multi-day upstream-PR work, not in scope for this spike
- Issue #66 has been re-scoped from "libc PR" to "upstream socket2/tokio contributions"

### ADL-010 Update — 2026-05-29: libc QNX 8.0 gap identified

Root cause refined: `libc 0.2.186` defines `TCP_KEEPALIVE` and
`LOCAL_PEEREID` for QNX 7.0 / 7.1 (`target_env = "nto70"` / `"nto71"`)
inside a `cfg_if!` block at `src/unix/nto/mod.rs:907`, but has no
`nto80` arm. QNX 8.0 system headers do not contain these constants
at all — they were present in QNX 7.x but absent in 8.0 (confirmed
by `grep -r 'TCP_KEEPALIVE\|LOCAL_PEEREID' /opt/qnx800/sdp2/target/qnx/usr/include/`
on the development laptop earlier in this session).

Tokio PR #6421 (merged May 2024, tokio v1.37+) is **necessary but
not sufficient**. Kirra uses tokio 1.52.3 which includes the PR — the
`#[cfg(any(target_os = "netbsd", target_os = "nto"))]` gate around
`impl_netbsd` in `ucred.rs` is in place — but compilation still fails
because libc has no `nto80` definitions for the symbols tokio imports.
Same story for socket2 0.6.3 with `TCP_KEEPALIVE`.

Two possible upstream fix paths:

1. **libc PR**: add explicit `target_env = "nto80"` arm to the
   `cfg_if!` block at `src/unix/nto/mod.rs:907`. Only valid if QNX 8.0
   headers actually define the constants — current evidence (header
   grep) is they **do not**.

2. **tokio + socket2 PRs**: gate nto socket code on
   `target_env = "nto70"` / `"nto71"` only, **not** the broader
   `target_os = "nto"`. QNX 8.0 builds then compile cleanly by
   omitting the unavailable socket options. Lost functionality
   (per-socket TCP keepalive idle timer, Unix-socket peer credentials)
   would need either a runtime degrade-and-warn path or a separate
   QNX 8.0 implementation using whatever the SDP 8.0 API actually
   provides for the same goals.

**Path 2 is the more likely correct fix given the header evidence.**
Tracked in PARK-024b / GitHub issue #67.

### CERT-002 baseline (2026-05-29)

- **Total initial errors:** 21 (`cargo clippy --workspace -- -D warnings`)
- **Safety fix:** `not_unsafe_ptr_arg_deref` in `src/ffi.rs::kirra_reset_state` → commit `e0b583d` (CERT-005 RSR-001)
- **Style auto-fix:** 13 lints (`new_without_default`, `redundant_pattern_matching`, `manual_is_multiple_of`, `explicit_auto_deref`, `doc_overindented_list_items`, `len_without_is_empty`, `needless_range_loop`) → commit `349cab1`
- **Remaining:** 6 judgment-call lints — tracked, not auto-fixed:
  - `too_many_arguments` ×3 (fns with 8/9/10 args)
  - `result_unit_err` ×2 (`Result<_, ()>` instead of typed error)
  - `needless_range_loop` ×1 (one indexed-loop pattern that auto-fix couldn't rewrite)
- **`cargo audit`:** 0 vulnerabilities across 268 dependencies (clean)
- **CI static-analysis job:** pending CERT-002 CI addition
- **`needless_range_loop` in `security.rs`:** intentionally suppressed via `#[allow]` — `write_volatile` required to prevent dead-code elimination of secret-zeroing loop. Auto-fix would introduce memory-residue side channel. Documented exception per CERT-005 RSR-007.

---

## ADL-011 — RTM Coverage Baseline (CERT-003)

**Date:** 2026-05-29

| Metric | Value |
|---|---|
| Total safety goals | 16 |
| Tests named in RTM | 40 |
| Tests found in code | 5 |
| Goals with any test coverage | 5 |
| Goals with zero coverage | 11 |
| Goal-level coverage | **31.25%** |
| Test-level coverage | **12.5%** |
| Gaps identified | 11 (zero-coverage SGs) + 5 (single-coverage SGs) |
| Code references to safety goal IDs | 0 (no back-traceability from code to safety goals) |

**Gap report:** `docs/safety/RTM_GAP_REPORT.md`
**Stub file:** `tests/cert_003_rtm_gap_stubs.rs` — 11 `#[ignore]`'d `todo!()` stubs, one per zero-coverage goal
**RTM annotations:** `docs/safety/REQUIREMENTS_TRACEABILITY.md` — each named test now carries `✓` (exists) or `✗` (missing) status marker inline

The RTM's self-reported "All 16 safety goals are covered" was aspirational and never reconciled with the codebase. Closing CERT-003 requires implementing the 11 stub bodies and the 7 single-coverage gaps (TR-001b, TR-002a, TR-002b, TR-004a, TR-004b, TR-005a, TR-011a, TR-011b) before any ASIL-D pre-assessment can be defended.

---

## ADL-012 — PARK-025: QNN + QNX compatibility analysis

**Date:** 2026-06-11
**Status:** Analysis complete — #39 (QNN backend MVP) remains BLOCKED; #38 QNN row stays PENDING
**Deciders:** Kirra Systems, LLC
**Source:** Owner-supplied vendor research (live Qualcomm / QNX sources, June 2026). Findings are filed with their caveats intact; **on-target verification rides the eventual hardware day** (no claim here is bench-confirmed in this repo).
**Cross-refs:** #37 (this analysis), #39 (QNN backend MVP), #38 / PARK-026 (`parko/QNX_BACKEND_SELECTION.md`, the QNN row), #36 / PARK-024 (QNX spike), #276 (vendor-engagement track), ADL-003 (no-alloc hot-path contract), ADL-010 (QNX POSIX gaps).

### 1. Distribution gate (the headline)

QNN / QAIRT **QNX binaries are NOT in the public SDK** — the public SDK ships
Android / Windows / Linux-aarch64 artifacts only. QNX artifacts ship through the
**Snapdragon Ride / automotive BSP channel under commercial agreement**, not via
public download.

- **Production precedent exists:** SA8295 + QNX 7.1 + SDK 2.29, with the **CPU
  backend confirmed working in the field**; the **GPU backend has known OpenCL
  issues** on that stack.
- **Consequence for #39 (record both options — the choice is an owner / business
  call, related to the #276 vendor-engagement track):**
  1. **Vendor engagement** — obtain the QNX QAIRT artifacts via a Qualcomm
     automotive / Ride agreement, then implement against them; or
  2. **Phased FFI-first** — scope #39's first phase to the **API-level FFI
     bindings compiled against the public headers**, with the **QNX link step
     deferred** until the artifacts are in hand.
- Either way, **#39 cannot start from public artifacts.**

### 2. Versioning + the ORT non-path

- The SDK was **rebranded QNN → QAIRT** (~2.32; current ~2.34+).
- **Version selection on QNX is BSP-coupled:** the Ride BSP bundles its matched
  QAIRT. **Pin QAIRT per-target**, not free-floating.
- The **ONNX Runtime QNN execution provider is tested on Android / Windows only.**
  parko's `ort`-based backend pattern (parko-onnx) **does NOT extend to QNX QNN.**
  → **Confirmed design input for #39:** the correct plan is **direct C FFI via
  `QnnInterface_t`**, not an ORT EP.

### 3. FFI / linking differences from Linux

- Inference uses a **per-accelerator backend `.so` set**: `libQnnCpu` / `libQnnGpu`
  / `libQnnHtp`, plus per-arch **HtpV6x / V7x Stub** libs and their **hexagon Skel**
  counterparts, plus `libQnnSystem`.
- **HTP rides a Stub/Skel FastRPC split to the cDSP** → the **QNX BSP must provide
  the FastRPC transport driver** (a BSP precondition, not something parko can supply).
- **`dlopen`-style loading of the backend `.so` is the SDK's own model.** Mapped
  against `parko/QNX_BACKEND_SELECTION.md` **R-2** (the sanctioned loader path):
  **COMPATIBLE-IF-CONFINED** — one documented `dlopen` of a **pinned path at init**,
  **never on the hot path**. Arbitrary / relative / per-run `dlopen` remains forbidden.

### 4. Memory model vs the no-alloc contract (ADL-003)

- **Steady-state inference can be allocation-stable** via buffers **pre-registered
  at load** (QnnMem-style registration) — this satisfies the inference-time no-alloc
  requirement.
- **Context / graph creation allocates internally.**
- **Contract mapping:** *no-alloc at inference time, registration at load time,
  QNN-internal allocation a **NAMED RESIDUAL*** requiring **vendor confirmation**
  for any safety-partition use.
- **Hard limit: < 4 GB model per process (HTP).** The vendor workaround is
  **multi-process**, which **conflicts with the single-process constraint (R-3)** in
  `parko/QNX_BACKEND_SELECTION.md`. **Under our rules a > 4 GB model is effectively
  FORBIDDEN — it is not multi-processed around.**

### 5. Pipeline fact

- **Context-binary generation runs offline on x64 hosts (no device).** → KIRRA's
  model-prep can be **CI-side**; **only execution needs silicon.**

### 6. Verdict

- **#39 remains BLOCKED** — silicon **and** the distribution gate (§1).
- **#38's QNN row stays PENDING-#36**, now citing this analysis.
- **The analysis half of #37 is DONE.** This entry is the closing evidence; **#37
  is closed**, with on-target verification deferred to the hardware day (the
  per-target QAIRT pin, the FastRPC driver presence, and the memory-residual vendor
  confirmation all ride #36 / a #276 engagement).


## ADL-013 — SG6 decel convention: gravity-deviation + M-of-N confirmation window (#321)

**Date:** 2026-06-12
**Status:** Decided — implemented in this PR. Thresholds are **VALIDATION-PENDING** (bench/SOTIF characterization rides the eventual hardware day); the *convention* and the *window mechanism* are decided.
**Deciders:** Kirra Systems, LLC
**Source:** Code-review register #319 (finding H2+A1). The prior raw-norm proxy (`‖a‖`, gravity-inclusive) had a floor bug: at rest `‖a‖ ≈ 9.80665`, so the courier threshold of 8.0 was *below gravity* — a static, level courier latched on gravity alone — and a single-tick jolt permanently latched (human clearance required). Both are fixed here.
**Cross-refs:** #321 (this fix), #319 (register), #311 (the raw-norm proxy this replaces), #312/#316 (the per-class contract family), `docs/CONTRACT_PROFILES.md` (the normative `*.impact_spike` rows + the convention row), `parko/crates/parko-core/src/impact.rs` (`ImpactCfg`), `parko/crates/parko-ros2/src/clearance_gate.rs` (`decel_deviation` / `SpikeDebouncer`).

### 1. Convention — gravity-DEVIATION, not raw norm

The IMU decel proxy is now the **absolute deviation of the accelerometer-vector
magnitude from standard gravity**:

> `decel_deviation = | ‖a‖ − G |`,  `G = 9.80665 m/s²` (ISO 80000-3 / CGPM standard gravity).

At rest `‖a‖ ≈ G` ⇒ deviation ≈ 0, so **no threshold below gravity is needed and no
class false-latches at rest** — the H2 floor bug is structurally removed. A
collision-grade deceleration raises `‖a‖` ⇒ a large deviation; free-fall lowers `‖a‖`
⇒ also a large deviation (a robot going off a curb edge is an anomaly worth latching —
acceptable secondary trigger).

**Named residual (orientation-corrected projection).** Because `‖a‖` combines the
collision impulse with gravity *vectorially*, the deviation still under-represents a
purely horizontal impulse (`√(c²+G²) − G < c`). The better convention subtracts the
gravity vector using the orientation quaternion (`‖a − R·g‖`), but that requires a
**reliable** orientation (`ImuSample::orientation` is `Option` and may be absent —
and a fabricated identity quaternion would assert a false attitude, exactly the hazard
`imu_shim` guards). So orientation-corrected projection is a **named future
improvement**, gated on a trustworthy quaternion; the deviation convention is the
floor-bug fix that ships now, with thresholds set conservatively to absorb the
residual.

### 2. Confirmation window — M consecutive of the last N (a debounce, not a vote)

`ImpactCfg` gains `confirmation_m: u8` and `confirmation_n: u8`. A decel detection is
**confirmed** only when the last `N` observations contain a run of **≥ M CONSECUTIVE**
above-threshold ticks. This is a *debounce against a single-tick jolt* (pothole, curb
strike), NOT an M-of-N vote: `T,F,T` does **not** confirm (no consecutive run) — a
sustained deceleration is the signal, two scattered jolts are not. (`SpikeDebouncer`
holds a `VecDeque<bool>` bounded at capacity `N`; `drain_confirmed` consumes the
confirmation by resetting the window only when it fires.)

**Defaults `M=1, N=1` ⇒ zero regression:** a single tick above threshold confirms
immediately — bit-identical to the prior single-tick behavior. Every existing
`is_impact` / latch test passes unchanged; that default IS the backward-compat proof.

### 3. Revised per-class thresholds (deviation units; M-of-N) — all VALIDATION-PENDING

| class | threshold (`|‖a‖−G|`, m/s²) | M / N | basis |
|---|---|---|---|
| Courier (sidewalk) | **2.5** | 2 / 3 | walking-pace (~1.5–3 m/s) collision is a small but distinct deviation; above ordinary curb/bump jolts, which M=2/N=3 debounces; a courier strikes curbs often. (replaces the old **8.0 raw-norm**, which was *below gravity*.) |
| Delivery-AV (road pod) | **8.0** | 2 / 3 | ~11 m/s road-pod collision decelerates harder than a sidewalk hit, less than a full crash; mid deviation, debounced against road bumps. |
| Robotaxi | **22.0** | 1 / 1 | full-speed collision-grade decel (~30 m/s² raw-norm ≈ 20–22 deviation once combined with gravity). **M=1/N=1: a highway crash is unambiguous in one tick and the FTTI is tight — no latency budget to wait for confirmation.** |
| `ImpactCfg::default()` | 30.0 (unchanged) | 1 / 1 | the conservative fallback when no class is selected; threshold left at 30.0 so the convention change introduces zero regression in the default path. |

**The latency-vs-false-latch tradeoff (the A1 decision):** lower-speed classes
(courier / delivery-AV) accept a 2-of-3 confirmation delay (≤ ~150 ms at 20 Hz) to
reject the frequent benign jolts of pedestrian/road-pod operation; the robotaxi class
refuses that delay (M=1) because at highway speed the collision is unambiguous and the
fault-tolerant time interval does not permit it. M/N is therefore a **per-class**
parameter, not a global constant.

### 4. Config-load validation

`ImpactCfg::validate()` enforces `spike_threshold_mps2 > 1.0` (a deviation threshold ≤
1 would trip on noise) ∧ `confirmation_m ≥ 1` ∧ `confirmation_n ≥ confirmation_m`. Every
built-in `impact_cfg_for_class` profile is asserted valid; a future bad built-in trips
in tests.

### 5. Named residuals (not fixed here)
- **Orientation-corrected projection** (§1) — gated on a reliable quaternion.
- **Threshold + M/N certification** — all numbers are VALIDATION-PENDING placeholders;
  bench characterization of low-speed collision decel signatures replaces them.
- **Per-class wiring into the node binary** — selecting `impact_cfg_for_class(class)`
  at runtime is the #312 remainder; until then the node uses the default (30.0, 1/1).

---

## ADL-014: QNX 8.0 cross-compile — first light (PARK-024 / #36)

**Org migration (2026-06-14):** repo moved to github.com/kirra-systems/kirra-runtime-sdk; all internal references updated.

**Date:** 2026-06-12
**Status:** COMPILE CONFIRMED — deploy pending QNX target

### Toolchain findings
- QNX SDP 2.0.4 (`/opt/qnx800/sdp2`) does NOT ship rustc/cargo
- Upstream stable rustup has no prebuilt std for `x86_64-pc-nto-qnx800`
- Working build path: `cargo +nightly build -Z build-std=std,panic_abort
  --release --target x86_64-pc-nto-qnx800 -p kirra-governor-service`
- Compiler: GCC 12.2.0 (`gcc_ntox86_64_cxx`), rustc 1.98.0-nightly (2026-06-11)

### Binary confirmed genuine QNX NTO ELF
- `readelf -l` output: `[Requesting program interpreter: /usr/lib/ldqnx-64.so.2]`
- QNX dynamic linker confirmed in SDP:
  `/opt/qnx800/sdp2/target/qnx/x86_64/usr/lib/ldqnx-64.so.2`
  `/opt/qnx800/sdp2/target/qnx/aarch64le/usr/lib/ldqnx-64.so.2`
- Note: `file` reports SYSV (QNX uses SYSV ELF format); the interpreter path
  is the authoritative QNX marker, not the OS label in `file` output

### Both targets confirmed available in SDP 2.0.4
- `gcc_ntox86_64_cxx` → x86_64 QNX (laptop/VM/x86 target)
- `gcc_ntoaarch64le_cxx` → AArch64 (Jetson Orin NX — the cert target)
- AArch64 cross-compile uses identical process: swap target triple to
  `aarch64-unknown-nto-qnx800` and variant to `gcc_ntoaarch64le_cxx`

### #274-verify finding
QNX SDP 2.0.4 does NOT ship Ferrocene or an edition-2024-equivalent Rust
toolchain. The nightly `-Z build-std` path produces a working NTO binary but
is not cert-quality. Ferrocene ≥1.85-equiv remains the required toolchain for
#274/#278 (iceoryx2 safety-partition adoption). The edition-2024 gate in
ADR-0006 Constraints stands — this finding confirms it is a real gap, not a
theoretical one.

### Remaining for full #36 closure
1. Deploy binary to a running QNX instance (VM or Jetson hardware)
2. Confirm `/health` returns 200 on QNX
3. Document POSIX findings (signal handling, threading, filesystem, dynamic
   linking) → flip PENDING rows in parko/QNX_BACKEND_SELECTION.md to
   CONFIRMED or FORBIDDEN with target evidence
Cross-ref: #274-verify, #278, ADR-0006, parko/QNX_BACKEND_SELECTION.md

