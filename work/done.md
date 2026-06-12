# Completed Work

Completed tasks will be appended here weekly.

---

## HA-001 — Durable HA epoch fence (split-brain prevention)
Completed: 2026-05-30
Commit: 56e1fa7
Labels: safety, ha, fix
Notes: Replaces the in-memory `mode_active` AtomicBool CAS in
`perform_promotion` (which falsely claimed to "ensure only one standby
promotes even under split-brain" — a per-process atomic cannot
coordinate across instances). Adds a singleton `ha_state` row
(epoch, active_instance_id, updated_at_ms) and a `try_claim_epoch`
conditional UPDATE that gives a real distributed compare-and-set —
SQLite serializes write transactions, so two racers reading the same
observed epoch can never both see `rows_affected == 1`. AppState
gains `held_epoch: Arc<AtomicU64>`; the gateway mutation gate compares
held vs current DB epoch on every state-mutating request and
self-demotes (`mode_active.store(false)`) + returns 503 on mismatch.
Heartbeat-aware startup arbitration: a configured-Active node defers
to a live holder rather than steal. Heartbeat writer now performs a
per-tick proactive epoch check and self-demotes if fenced. Five real
concurrent-promotion tests added (temp-file SQLite, NOT :memory:).
Three tautological tests removed (re-tested std AtomicBool /
assert!(true)). Safety invariant: under partition, clock skew, or a
restarted old primary, at-most-one-effective-writer holds. Filed
follow-ups: #79 (epoch-in-write-transaction for top-tier writes,
closes gate TOCTOU), #80 (clock-skew failover detection — liveness
only), #81 (full leader election for zero spurious failovers).

---

## AUDIT-002 — Wire hash-v2 migration anchor into verifier service startup
Completed: 2026-05-30
Commit: 55e3e9e
Labels: safety, audit, fix
Notes: `ensure_hash_v2_migration_anchor` was defined and tested but
had ZERO production callers — the v1/v2 boundary anchor was never
written. Wired into `kirra_verifier_service::main` after
`set_signing_key` (so the HASH_V2_MIGRATION entry is signed) and
before `axum::serve`. Active-only (passive standby is read-only;
logs a skip line). Failure is logged loudly, never silently
swallowed. Both info/error log lines retained as observable proof
the wiring is live until the build_app extraction (#72) provides
an automated guard. Promotion-path call deliberately not added in
this commit — flagged as #78.

---

## AUDIT-001 — Hash-v2 versioned audit chain (bind event_type + sequence)
Completed: 2026-05-30
Commit: 6f25437
Labels: safety, audit, fix
Notes: v1 hash bound `(previous_hash, event_json, created_at_ms)`
only — relabeling an entry's `event_type` or shuffling the audit
table order survived the hash-only integrity check. v2 hashing
(`canonical_signing_payload_v2`, `compute_record_hash_v2`) is
length-prefixed + domain-separated ("KIRRA-AUDIT-V2") and binds
`event_type` + `sequence` into the hash. Non-destructive migration:
`hash_version INTEGER NOT NULL DEFAULT 1` and nullable `sequence`
columns added via idempotent ALTER; both verifiers dispatch by
`hash_version`. `ensure_hash_v2_migration_anchor` writes a one-time
HASH_V2_MIGRATION boundary marker. Also fixed: `append_audit_event_tx`
no longer treats `QueryReturnedNoRows` (legitimate genesis) as a read
error that silently forks the chain. Two test suites added:
`audit_chain_bypass_tests` and `audit_hash_v2_tests`.

---

## PARK-024 — QNX deployment spike (findings only)
Completed: 2026-05-28
Commit: ae2fe8d
Labels: qnx, spike
Notes: Spike complete; binary-running goal deferred to upstream
contribution work (PARK-024b). Cross-compilation toolchain confirmed
working — qcc 12.2.0, env from /opt/qnx800/sdp2/qnxsdp-env.sh, build-std
on nightly compiles core/std/alloc successfully for x86_64-pc-nto-qnx800.
Identified two architectural gaps between QNX SDP 8.0 and Linux/macOS:
(1) TCP per-socket keepalive (TCP_KEEPIDLE / TCP_KEEPALIVE) not present in
QNX headers; (2) Unix-socket peer credentials (SO_PEERCRED / LOCAL_PEEREID)
not present in QNX headers. Neither is a libc-bindings gap — the constants
are absent from QNX C headers. Fix requires upstream PRs to socket2 and
tokio adding cfg(target_os = "nto") code paths. Spike workaround: local
socket2 fork patched to use SO_KEEPALIVE (semantically wrong, spike-only),
plus `[patch.crates-io]` entry in Cargo.toml. ADL-010 documents the full
findings; issue #66 re-scoped to track the socket2 + tokio upstream work.

---

## PARK-004 — NaN/Inf input guard at tick() entry
Completed: 2026-05-27
Commit: b4bcb2d
Labels: control-loop, safety
Notes: NaN/Inf guard added at `InferenceLoop::tick()` boundary. Non-finite
input (NaN/Inf) returns `Ok(PostureSnapshot { active_command: stopped(0.0),
active_state_degraded: true })` before any governor or clamp logic runs.
Proptest in scheduler.rs:587 confirms zero non-finite values reach governor
across the full f32 NaN/Inf/subnormal space.

---

## PARK-007 — Crate and struct name audit
Completed: 2026-05-27
Commit: 5ed412e
Labels: audit, documentation
Notes: parko-core, parko-onnx, parko-kirra confirmed as workspace members.
KirraGovernor at parko-kirra/src/lib.rs:43; SafetyGovernor trait imported
from parko_core::safety. WallClock vs RuntimeClock distinction recorded in
decisions.md as ADL-006 — WallClock (clock.rs) is the injectable Clock impl
swapped for MockClock; RuntimeClock (runtime.rs) is the sleep-based tick
driver. No rename or refactor required.

---

## PARK-008 — Finalize InferenceBackend trait zero-copy boundary
Completed: 2026-05-27
Commit: 973763e
Labels: backend-architecture
Notes: InferenceBackend trait at backend.rs:113 with load_model/run signature
returning Result<_, BackendError>. BackendDescriptor enum (6 variants: Cpu,
TensorRT, Qnn, Tidl, OpenVino, Amd) at backend.rs:90. BackendError enum
(ShapeMismatch, Io, Unsupported, …) at backend.rs:10 with Display impl.
BackendCapabilities struct (Default) at backend.rs:43; trait provides
default `capabilities()` returning `BackendCapabilities::default()` so each
backend overrides only what differs. OrtBackend inherits Cpu descriptor
from default impl.

---

## aarch64 cross-compilation environment
Completed: 2026-05-27
Commit: 70e7c77
Notes: QEMU 8.2.2 installed. aarch64-unknown-linux-gnu Rust target added.
kirra_verifier_service confirmed ELF 64-bit ARM aarch64. .cargo/config.toml
wired with aarch64-linux-gnu-gcc linker. QNX targets blocked until SDP
installed — test-qnx-vm.sh ready with guard. ENVIRONMENT.md documents
all four target triples.

---

## PARK-019 — 10,000-scenario adversarial trajectory simulation
Completed: 2026-05-26
Commit: 9172191
Labels: behavioral-safety, simulation
Notes: 3 integration tests in tests/rss_simulation.rs.
test_rss_adversarial_10k_scenarios: 10,000 scenarios × 10 ticks,
StdRng(0xDEAD_BEEF_CAFE), completes in 1.2s. Zero violations escaped.
test_rss_posture_lifecycle_violation_to_recovery: streak below threshold
stays Degraded; threshold tick → Nominal.
test_locked_out_hard_stop_dominates_rss_gate: untrusted DAG → LockedOut;
RSS-safe state does not override hard stop — confirms PARK-003 fix holds
under integration conditions. Design note: Nominal kinematics contract
may return velocity > commanded to prevent abrupt stops — correct behavior,
scoped assertions accordingly. parko-kirra added to dev-dependencies.
kirra-runtime-sdk: 340 tests (321 unit + 19 integration).

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

**Completed:** 2026-05-26 | **Commit:** `e1ba1a2` (with governor fix in `9943aa9`) | **Branch:** `claude/claude-md-reference-AtTWy`

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

## PARK-018 — RssViolationEvent in kirra-runtime-sdk audit chain
Completed: 2026-05-27
Commit: db97f39
Labels: behavioral-safety
Notes: AuditChainLinker is a unit struct — append_rss_violation follows
static pattern (tx: &Transaction, no &mut self). AuditEntry enum and
AuditError created fresh (none pre-existed). Chain hash input is
event_json_string consistent with all other entries — RSS event JSON
flows through compute_record_hash unchanged. Test A: 5-entry mixed chain
walks clean. Test B: one flipped bit in event_json fails hash check.
kirra-runtime-sdk: 337 tests (321 unit + 16 integration).

---

## PARK-017 — RSS property test
Completed: 2026-05-27
Commit: 4a8b773
Labels: behavioral-safety, kirra-governor
Notes: parko/crates/parko-core/tests/rss_governor_proptest.rs — 3 blocks
× 10,000 cases = 30,000 total. Block 1 Nominal: exact MRC contract when
RSS unsafe, kinematic envelope when safe. Block 2 Degraded: exact MRC
regardless of RSS — verifies single apply_mrc_profile() code path under
adversarial inputs. Block 3 LockedOut: always 0.0 — confirms hard stop
dominates RSS gate. No bare 5.0 anywhere. rss_safe derived from
longitudinal_safe_distance() per IEEE 2846-2022 §5.1.
parko-core: 61 tests (54 unit + 7 proptests / 70,000 total cases).

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

---

## KEY-001 — Unified key registry (#329, ADR-0008)
Completed: 2026-06-12 (Phase A merged; Phase A.1 in PR #338)
Commits: #337 (Phase A), feat/audit-key-registry-329 (Phase A.1)
Labels: epic, domain:boundary, domain:fleet, security
Source: code-review register #319 (gap 1) — four key stores in three encodings,
no unified registry; fleet ingest verified against a caller-supplied key.

Phase A (#337, merged):
- src/key_registry.rs: additive `KeyRegistry` over `VerifierStore`.
  `resolve_ed25519_pubkey(principal_id, role) -> Result<Option<[u8;32]>>` +
  `verify_for(...)` unify lookup across node attestation / federation controller /
  operator / fleet-grant stores, normalizing PEM/b64 → raw 32 bytes at the read
  boundary. Fail-closed throughout (unknown principal / revoked operator / malformed
  or wrong-length key → None; Err reserved for SQL failure).
- Fleet adoption: `accept_report_from_registry` / `ingest_clearance_grant_from_registry`
  resolve the verifying key from a stored registration, not a caller-supplied string.
- No schema change, no encoding migration, no breaking API change; talisman byte-identical.

Phase A.1 (PR #338 — the read-only audit-key residual):
- VerifierStore::audit_verifying_key() exposes the public half of the in-memory
  audit signer; KeyRole::AuditSigning resolves the chain's verifying key keyed by its
  verifying_key_id fingerprint (the audit chain's key_id). Read-only, fail-closed.

Named residuals still deferred (ADR-0008 "conditions that reopen"):
- Audit-key ROTATION + persisted key HISTORY (touches store schema + chain trust model).
- On-disk ENCODING migration to one canonical format (normalization stays at read boundary).
