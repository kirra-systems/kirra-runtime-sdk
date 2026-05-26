# Roadmap

> Lean-agile increments. Each increment is small, independently testable, and
> ships a concrete artifact. No increment blocks the next from delivering value.

---

## Increment 1 — Deterministic Runtime Core

**Goal:** A clock-driven, posture-aware inference loop that compiles, passes all
tests, and can be consumed as a library by any downstream crate.

### Tasks

**1.1 — Attach `SafetyGovernor` to `ControlLoop`**
Implement a `with_governor(impl SafetyGovernor)` builder on `ControlLoop` in
`parko-core`. When a governor is attached the built-in scalar clamp is suppressed
so the two enforcement paths cannot conflict. Done when `cargo test -p parko-core`
passes and a new `test_builtin_clamp_suppressed` test confirms the built-in path
is bypassed.

**1.2 — Add test-only posture state setter**
Add `set_state_for_test(state: PostureState)` to `parko-core` behind
`#[cfg(test)]`. This unblocks posture-divergence tests without exposing a
mutation path in production builds. Done when the method is absent from
`cargo build --release` (verified with `nm`) and present in `cargo test`.

**1.3 — Posture-divergence property test**
Write a `proptest` suite in `parko-core` asserting that for all valid
`(proposed_output, posture_state)` inputs the governor's result is at least as
conservative as the built-in clamp. Done when ≥ 10 000 proptest cases pass for
all three posture states.

**1.4 — Harden tick boundary: NaN/Inf rejection**
Add an input guard at the top of `ControlLoop::tick` that rejects any `NaN` or
`Inf` float with `EnforcementAction::Halt` before the value reaches the governor.
Done when a property test generates adversarial floats and confirms zero reach
the governor.

**1.5 — Tag and publish `parko-core` v0.1.0**
Verify all `parko-core` tests pass, update `Cargo.toml` to version `0.1.0`, and
tag the release. Done when `cargo publish --dry-run -p parko-core` exits cleanly
and the tag exists in the repo.

---

## Increment 2 — Hardware Abstraction Layer

**Goal:** A zero-copy `InferenceBackend` trait with one real backend (ONNX Runtime)
and stub backends for Qualcomm QNN, TI TIDL, and Intel OpenVINO that pass CI
without hardware.

### Tasks

**2.1 — Harden `parko-onnx` ORT backend**
Refactor `parko-onnx` for session reuse, correct error mapping, and stable output
slice lifetimes. Done when the ORT backend sustains 1 kHz tick rate in a
benchmark test without heap allocation on the hot path.

**2.2 — Define `BackendDescriptor` enum**
Add `BackendDescriptor { Cpu, QualcommQnn, TiTidl, AmdRocm, IntelOpenVino }` to
`parko-core` as the canonical hardware-target discriminant. Done when all existing
crates compile against the new type and the enum is re-exported from the workspace
root.

**2.3 — QNN stub backend**
Implement `QnnStubBackend` in `parko-core` (or a new `parko-qnn` crate) returning
deterministic fixed outputs, gated behind `features = ["backend-qnn"]`. Done when
`cargo test --features backend-qnn` passes on CI without Qualcomm hardware.

**2.4 — TIDL stub backend**
Implement `TidlStubBackend` with configurable simulated DSP latency (default 2 ms)
gated behind `features = ["backend-tidl"]`. Done when the stub passes CI and the
latency simulation is observable in the benchmark output.

**2.5 — OpenVINO stub backend**
Implement `OpenVinoStubBackend` wrapping the `openvino` crate's no-op path, gated
behind `features = ["backend-openvino"]`. Done when `cargo test --features
backend-openvino` passes on ubuntu-latest CI.

**2.6 — Backend latency watchdog**
Add a watchdog inside `InferenceLoop`: if `InferenceBackend::run` exceeds a
configurable `deadline_ms`, emit a `LatencyViolation` event and hold the last
safe output. Done when a test that deliberately exceeds the deadline confirms the
posture transitions to `Degraded`.

---

## Increment 3 — Behavioral Safety (RSS-Equivalent)

**Goal:** A pure-Rust RSS-class behavioral safety layer integrated with
`parko-core` and `kirra-runtime-sdk`. Replaces ad-hoc kinematics with a formal
safe-distance model.

### Tasks

**3.1 — Implement `RssSafeDistance` model**
Build `parko-core::rss::RssSafeDistance` with `longitudinal` and `lateral`
methods per IEEE 2846. Done when unit tests cover minimum following distance,
zero-speed edge cases, and the model matches reference values from the IEEE 2846
example annex.

**3.2 — Wire RSS into `KirraKernelGovernor`**
Integrate `RssSafeDistance` as a pre-actuator gate in `kirra-runtime-sdk`'s
`KirraKernelGovernor`: an RSS violation clamps velocity to zero immediately. Done
when an integration test confirms no RSS-violating `cmd_vel` exits the governor
in any posture state.

**3.3 — RSS property test**
Write a `proptest` suite asserting no RSS-violating command reaches the actuator
for any `(ego_vel, lead_vel, gap)` triple in the valid physical range. Done when
≥ 10 000 cases pass across all posture states.

**3.4 — `RssViolationEvent` in audit chain**
Add `RssViolationEvent { ego_state, object_state, longitudinal_margin,
lateral_margin, timestamp_ms }` to the `kirra-runtime-sdk` audit chain. Done when
the event appears in the hash-chained ledger and `kirra_audit_verify` can decode
it offline.

**3.5 — RSS posture integration**
Update the posture engine: an `RssViolationEvent` immediately transitions fleet
posture to `Degraded`; recovery follows the existing 5-tick hysteresis. Done when
a scenario test confirms the posture transition and the correct recovery streak.

---

## Increment 4 — Silicon Matrix Expansion

**Goal:** Real (non-stub) inference on at least two hardware targets (QNN and
OpenVINO). TIDL and ROCm delivered as feature-gated stubs promoted to real
backends when hardware is available.

### Tasks

**4.1 — `QnnBackend` (real)**
Implement `QnnBackend` using Qualcomm AI Engine Direct SDK C bindings behind
`features = ["backend-qnn"]`. Done when inference runs on a QCS6490 or SA8295
dev board and output tensors match the ORT CPU reference within tolerance.

**4.2 — `OpenVinoBackend` (real)**
Implement `OpenVinoBackend` using `openvino-rs` with model loading and output
shape validation. Done when inference runs on an Intel platform and matches ORT
reference output within tolerance.

**4.3 — `TidlBackend` (real)**
Implement `TidlBackend` via TI TIDL runtime C FFI, cross-compiled to
`aarch64-unknown-linux-gnu`. Done when inference runs on a TDA4VM and output
matches ORT reference.

**4.4 — `RocmBackend` (real)**
Implement `RocmBackend` via MIGraphX Rust bindings or ROCm HIP C FFI, gated
behind `features = ["backend-rocm"]`. Done when inference runs on RX 6000 series
and matches ORT reference.

**4.5 — CI matrix across all backends**
Add a GitHub Actions matrix job that builds and tests all four stub backends on
ubuntu-latest. Done when all four feature flags pass CI in the same workflow run.

---

## Increment 5 — Safety OS Packaging

**Goal:** A single installable tarball containing the full safety runtime
(posture engine + inference loop + chosen backend) as a systemd-managed service
with dashboard, installer, and Helm chart.

### Tasks

**5.1 — Unified `kirra_safety_runtime` binary**
Merge `kirra-runtime-sdk` posture engine with `parko-core` inference loop into a
single binary. Done when `kirra_safety_runtime --backend ort` starts, serves
`/health`, and processes inference ticks.

**5.2 — systemd unit with watchdog**
Write `scripts/kirra-safety-runtime.service` with `WatchdogSec=5`,
`MemoryMax=512M`, `CPUQuota=80%`. Done when `systemd-analyze verify` reports no
errors and the service restarts automatically on watchdog timeout.

**5.3 — Backend-aware installer**
Extend `install.sh` with a `--backend` flag. Done when non-interactive install
with `--backend qnn` downloads the correct feature-gated binary and configures
the systemd unit without prompts.

**5.4 — Dashboard inference panels**
Add inference tick rate, backend P99 latency, RSS margin, and posture history
sparkline to the React dashboard. Done when the panels render live data against
a running `kirra_safety_runtime`.

**5.5 — `v1.2.0` release**
Tag and cut a GitHub Release with x86_64, aarch64, armv7 tarballs for each
backend variant. Done when the release pipeline completes green and SHA256 sums
are attached.

---

## Increment 6 — Certification-Ready Runtime

**Goal:** Sufficient documentation, traceability, and process evidence to begin a
formal ASIL-D / SIL 3 pre-assessment.

### Tasks

**6.1 — Complete RTM**
Expand `KIRRA-RTM-001` to trace every safety requirement to source line, test ID,
and coverage report entry. Done when a TÜV reviewer can follow every requirement
to a passing test without ambiguity.

**6.2 — MC/DC coverage report**
Generate MC/DC coverage for `posture_cache.rs`, `posture_engine_v2.rs`,
`kirra_core.rs`, and `rss.rs` using `cargo-llvm-cov`. Done when all four modules
report 100% MC/DC and the report is committed to `docs/coverage/`.

**6.3 — FMEA**
Write `KIRRA-FMEA-001` covering posture engine stale cache, governor bypass,
attestation replay, nonce exhaustion, and RSS model numerical overflow. Done when
every failure mode has a detection method and a mitigation.

**6.4 — DFA**
Write `KIRRA-DFA-001` analyzing common-cause failures in the HA active/passive
pair sharing SQLite on NFS. Done when the document identifies all single points
of failure and proposes independent protection measures.

**6.5 — Offline audit verifier binary**
Implement `kirra_audit_verify`: reads the audit chain from SQLite, verifies
Ed25519 signatures, and prints a tamper-evidence report without running the
service. Done when the binary correctly detects a single-byte corruption injected
into the chain.
