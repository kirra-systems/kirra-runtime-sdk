# Active

> Max 3 tasks in flight at once. Pull from `backlog.md`. Move to `done.md` on merge.

---

## PARK-001 `control-loop` — Attach governor to ControlLoop

Implement `ControlLoop::with_governor(impl SafetyGovernor + 'static)` in
`parko-core`. When a governor is attached the built-in scalar clamp must be
suppressed so the two enforcement paths cannot conflict — the governor is solely
responsible for enforcement on that tick. Done when `test_builtin_clamp_suppressed`
passes and all existing `parko-core` tests continue to pass.

### Acceptance Criteria
- `ControlLoop::with_governor` compiles and chains correctly as a builder
- Built-in clamp is bypassed when a governor is present (confirmed by test)
- `ControlLoop::new()` (no governor) still applies the built-in clamp as before
- No `unsafe` code introduced
- `cargo test -p parko-core` fully green

### Claude Code Prompt
```
In the parko-core crate (parko/parko-core/src/control_loop.rs), implement a
with_governor builder method on ControlLoop.

Requirements:
- Signature: pub fn with_governor(mut self, g: impl SafetyGovernor + 'static) -> Self
- Store the governor as Option<Box<dyn SafetyGovernor>> on the ControlLoop struct
- When a governor is present, suppress the built-in scalar clamp entirely — both
  enforcement paths must NOT run on the same tick
- When no governor is present (default), the built-in clamp runs as before
- Add a test named test_builtin_clamp_suppressed that:
    1. Creates a mock governor that returns EnforcementAction::Allow(proposed)
       unchanged
    2. Sends a value the built-in clamp WOULD reduce (above the clamp threshold)
    3. Asserts the output equals the governor's output, not the clamped value
- Add a test named test_no_governor_uses_builtin_clamp that confirms the clamp
  still runs without a governor
- All existing parko-core tests must continue to pass
- No unsafe code
- Files: parko/parko-core/src/control_loop.rs, parko/parko-core/src/lib.rs
```

---

## PARK-002 `control-loop` — Add test-only posture state setter

Add `set_state_for_test(state: PostureState)` to `parko-core` behind
`#[cfg(test)]`. This unblocks posture-divergence and recovery-hysteresis tests
without exposing a mutation path in production binaries. Done when the method
is confirmed absent from `cargo build --release` output and present in `cargo test`.

### Acceptance Criteria
- Method is callable inside `#[cfg(test)]` modules and integration test files
- Method sets posture state directly with no transition validation (test seam only)
- `cargo build --release` produces no `set_state_for_test` symbol (verify with `nm`)
- All existing `parko-core` tests unaffected

### Claude Code Prompt
```
In parko/parko-core/src/ (whichever file owns PostureState or ControlLoop's
internal state), add a test-only state mutation method.

Requirements:
- Annotate with #[cfg(test)]
- Signature: pub fn set_state_for_test(&mut self, state: PostureState)
- The method sets the internal posture state directly, bypassing all transition
  logic — it is a test seam, not a production API
- Add a doc comment: "Test-only. Not compiled into release builds. Use only in
  #[cfg(test)] blocks or integration tests."
- Add a test that calls set_state_for_test(PostureState::Degraded), then calls
  tick(), and asserts the governor sees PostureState::Degraded
- Add a doc-level note explaining that release binary verification (nm) should
  be done manually or in CI
- All existing parko-core tests must pass
- Files: parko/parko-core/src/control_loop.rs or parko/parko-core/src/posture.rs
```

---

## PARK-003 `control-loop` — Posture-divergence property test

Write a `proptest` suite asserting that for all valid `(proposed_output: f32,
posture_state: PostureState)` inputs the `KirraKernelGovernor`'s enforcement
result is at least as conservative as the `parko-core` built-in clamp. This is
the core correctness invariant proving the governor integration is safe. Done
when ≥ 10 000 cases pass for all three posture states (`Nominal`, `Degraded`,
`LockedOut`).

### Acceptance Criteria
- ≥ 10 000 proptest cases generated per posture state
- Assertion: `governor_output <= builtin_clamp_output` for `Nominal` and `Degraded`
- Assertion: `governor_output == 0.0` (or `Halt`) for `LockedOut`
- NaN and Inf explicitly filtered from the input strategy
- Test file: `parko/parko-core/tests/posture_divergence.rs`
- `proptest` already a dev-dependency in workspace; no new non-dev deps

### Claude Code Prompt
```
Create parko/parko-core/tests/posture_divergence.rs and implement a proptest
suite for the governor/clamp divergence invariant.

Context:
- parko-core has a ControlLoop with an optional SafetyGovernor
- kirra-runtime-sdk's KirraKernelGovernor implements SafetyGovernor (via
  parko-aegis adapter)
- The invariant: governor output must always be <= built-in clamp output
  (governor is at least as conservative)

Requirements:
- Use proptest::prelude::* to generate (proposed_output: f32, state: PostureState)
  pairs; filter out NaN and Inf using prop_filter
- For each pair:
    1. Run proposed_output through the built-in clamp (call ControlLoop without
       governor)
    2. Run proposed_output through KirraKernelGovernor (call ControlLoop with
       governor, using set_state_for_test to set the posture)
    3. Assert: for Nominal and Degraded, governor_result <= builtin_clamp_result
    4. Assert: for LockedOut, governor_result == 0.0 or EnforcementAction::Halt
- Configure proptest with cases = 10_000
- All three PostureState variants must be explicitly tested (use three separate
  proptest! blocks or parameterize)
- proptest is already a dev-dependency; do not add new dependencies
- The test must pass with cargo test -p parko-core
- Files: parko/parko-core/tests/posture_divergence.rs
         parko/parko-core/Cargo.toml (add [[test]] entry if needed)
```
