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
