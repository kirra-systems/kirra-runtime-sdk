# Architecture Decision Log (ADL)

> Entries are immutable once written. Superseded decisions get a new entry
> referencing the old one by ADL number. Date format: YYYY-MM-DD.

---

## ADL-001 — Governor injection model

**Date:** 2026-05-26
**Status:** Accepted

### Decision
`SafetyGovernor` is injected into `ControlLoop` via a `with_governor` builder
method and stored as `Option<Box<dyn SafetyGovernor>>`. The governor is optional;
calling `ControlLoop::new()` without it activates the built-in scalar clamp by
default.

### Why
A required constructor argument would break all existing `ControlLoop` call sites
and make the no-governor use case (pure inference without safety enforcement, e.g.
simulation replay) impossible. A global or thread-local singleton creates hidden
coupling, makes parallel test execution order-dependent, and prevents running
multiple loops with different governors in the same process — a requirement for
multi-asset fabric deployments.

### Alternatives Considered
1. **Required constructor argument** — rejected: breaks existing API, forces
   every caller to supply a governor even for non-safety workloads.
2. **Global/thread-local governor** — rejected: hidden coupling, not testable in
   parallel, violates the principle that all safety state must be explicit and
   auditable.
3. **Tower middleware layer** — deferred: correct for the HTTP gateway
   (`KirraPolicyLayer` already does this) but the inference loop is not
   HTTP-driven; Tower adds latency overhead inappropriate for a 1 kHz tick path.

### Consequences
The `Option` check adds one branch per tick. At 1 kHz this is < 1 ns and
negligible. The built-in scalar clamp must be explicitly suppressed when a
governor is present to prevent two enforcement paths from conflicting on the
same tick. All safety contracts documented in `docs/backend_contract.md`.

---

## ADL-002 — Built-in degraded-mode logic interaction

**Date:** 2026-05-26
**Status:** Accepted

### Decision
When a `SafetyGovernor` is attached, the `ControlLoop` built-in scalar clamp is
fully and unconditionally suppressed. The governor alone is responsible for all
enforcement on that tick. The built-in clamp runs only when no governor is
present.

### Why
Running both enforcement paths simultaneously produces non-deterministic behavior:
the built-in clamp might allow a value the governor would deny, or the governor
might pass a value the clamp would reduce, and the "winner" depends on execution
order. This ambiguity cannot be certified under ISO 26262 because the safety
contract of the combined output is undefined. Suppression eliminates the
ambiguity entirely — one path, one contract.

### Alternatives Considered
1. **Run both and take the more conservative result** — rejected: requires
   knowledge of output domain ordering semantics (scalar, vector, struct) that
   the generic `ControlLoop` does not have; adds hidden semantic coupling between
   two independently-maintained enforcement implementations.
2. **Warn in debug builds when paths disagree** — deferred: useful for developer
   diagnostics but not a production invariant; can be added as a
   `#[cfg(debug_assertions)]` assertion without changing this decision.
3. **Compose governor + clamp in series** — rejected: reintroduces the conflict
   in a different form; whichever runs second can undo the first's enforcement.

### Consequences
Any caller that previously relied on the built-in clamp as a backstop must ensure
their `SafetyGovernor` implementation covers all cases the clamp handled. This is
a documented pre-condition of the `SafetyGovernor` trait. Verified by the
posture-divergence proptest (PARK-003).

---

## ADL-003 — Backend trait zero-copy boundary

**Date:** 2026-05-26
**Status:** Accepted

### Decision
`InferenceBackend::run` accepts `input: &[f32]` and writes results to
`output: &mut [f32]` — caller-allocated, pre-sized slices. No heap allocation
is permitted on the hot path. Backends that require a different memory format
(quantized int8 for QNN/TIDL) perform the conversion internally using scratch
memory allocated once at session creation.

### Why
Safety-critical runtimes targeting embedded silicon (Qualcomm QNN on SA8295, TI
TIDL on TDA4VM) operate under deterministic memory budgets. Heap allocation on
the inference hot path causes non-deterministic latency spikes that invalidate
worst-case execution time (WCET) analysis — a hard requirement for ASIL-D timing
decomposition. The zero-copy contract makes this constraint explicit at the type
system level and compiler-enforced at every call site.

### Alternatives Considered
1. **`Vec<f32>` return type** — rejected: heap allocation on every inference
   tick; WCET analysis impossible; incompatible with ASIL-D timing claims.
2. **`ndarray` / `nalgebra` tensors** — rejected: adds large transitive
   dependencies, version coupling across backends, and row/column-major layout
   ambiguity that is a latent source of silent numerical errors.
3. **Backend-specific associated types** — deferred: would allow backends to
   expose their native tensor types but breaks generic composition in
   `InferenceLoop`; revisit when ≥ 3 real (non-stub) backends are in production
   and the type-erased overhead is measured to be significant.

### Consequences
`InferenceLoop` pre-allocates input and output buffers for the lifetime of the
loop and passes slices to each tick. Buffer sizes are fixed at backend
initialization and must match the model's input/output shapes; a shape mismatch
is a `BackendError::ShapeMismatch` (not a panic). Documented in
`docs/backend_contract.md`.

---

## ADL-004 — Deterministic tick grid design

**Date:** 2026-05-26
**Status:** Accepted

### Decision
`ControlLoop` advances on a fixed-period tick grid driven by a `Clock` trait
abstraction (`SystemClock` in production, `VirtualClock` in tests). The loop does
not self-pace on inference completion time. If `InferenceBackend::run` exceeds its
configured deadline, the loop emits a `LatencyViolation` event, holds the last
safe output, and continues on the next tick — it never blocks.

### Why
Deterministic timing is a hard requirement for ASIL-D certification under ISO
26262 Part 6 (Section 9, temporal analysis). A self-pacing loop has unbounded
jitter that cannot be statically analyzed for WCET. The `VirtualClock` pattern
is already proven in `kirra-runtime-sdk`'s `ScenarioRunner` and
`telemetry_watchdog`; reusing the same abstraction avoids a second clock
implementation and keeps the test idiom consistent across both crates.

### Alternatives Considered
1. **Self-pacing loop (fire when inference finishes)** — rejected: jitter is
   unbounded; WCET analysis impossible; incompatible with ISO 26262 temporal
   analysis; SSE posture stream would have variable update rate.
2. **OS real-time scheduling (SCHED_FIFO)** — complementary, not alternative:
   OS-level RT scheduling reduces jitter but does not eliminate it; the
   `LatencyViolation` + hold-last-output fallback is still required for overruns.
3. **Hardware timer interrupt-driven loop** — deferred: correct for bare-metal
   RTOS targets; `VirtualClock` abstraction allows this to be wired in later
   without changing `ControlLoop` internals, since the interrupt handler would
   simply call `clock.advance(tick_period_us)`.

### Consequences
`Clock` must be injected at construction time or defaulted to `SystemClock`. All
time-dependent tests use `VirtualClock::advance` — no `sleep` calls in the test
suite. The maximum sustainable tick rate is bounded by `InferenceBackend` P99
latency; backend selection must be validated against the target tick rate during
integration testing on each hardware platform.

---

## ADL-005 — Safety posture state machine

**Date:** 2026-05-26
**Status:** Accepted

### Decision
The safety posture state machine has exactly three states — `Nominal`, `Degraded`,
`LockedOut` — with asymmetric transitions: degradation is immediate on a single
fault event; recovery from `Degraded` requires a configurable streak of
consecutive healthy ticks (default: 5) within a configurable time window
(default: 10 s). `LockedOut` requires an explicit operator action via
`KIRRA_ADMIN_TOKEN`. This matches the `AV_RECOVERY_STREAK_THRESHOLD` and
`AV_RECOVERY_WINDOW_MS` constants already enforced in
`kirra-runtime-sdk`'s `recovery_hysteresis` module.

### Why
Symmetric state machines (one bad event degrades, one good event recovers) produce
posture flapping under real-world noisy sensor conditions: a single noisy reading
alternately allows and denies commands, and the safety layer provides no useful
protection. Asymmetric hysteresis ensures the system stays in the conservative
state long enough to confirm genuine recovery rather than noise. The fail-closed
philosophy — the cost of a false positive (staying degraded too long) is far lower
than a false negative (recovering prematurely into a degraded-but-acting-nominal
state) — is a core design axiom documented in `KIRRA-HARA-001`.

### Alternatives Considered
1. **Symmetric transitions** — rejected: leads to posture flapping; cannot be
   certified under ISO 26262 as it violates the fail-closed requirement and
   produces non-deterministic command routing.
2. **Exponential back-off recovery window** — considered: more nuanced than a
   fixed streak but harder to analyze and certify; the fixed-streak model has a
   deterministic worst-case recovery time (streak × tick_period) that can be
   directly documented in the HARA and verified by the scenario test suite.
3. **Continuous confidence score (no discrete states)** — rejected: continuous
   scores require a calibrated threshold that varies across hardware platforms and
   sensor types; discrete states produce a binary command-routing decision that
   is auditable and certifiable.

### Consequences
Any new subsystem feeding into posture evaluation (RSS violations, backend latency
watchdog, telemetry silence detector) must declare its recovery threshold
explicitly in the FMEA (`KIRRA-FMEA-001`) and add a scenario test covering the
full fault-to-recovery cycle. High-frequency deployments (> 100 Hz tick rate) may
need a larger `AV_RECOVERY_STREAK_THRESHOLD` to cover the same wall-clock window;
this is a deployment parameter, not a code change.
