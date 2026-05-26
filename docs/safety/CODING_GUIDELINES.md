# Aegis Safety Kernel — Rust Safety Coding Guidelines

Document ID: AEGIS-CG-001
Version: 1.0.0
Status: Draft
Classification: ISO 26262 Part 6 / Ferrocene Language Specification
Date: 2026-05-23

---

## 1. Scope and Applicability

### 1.1 Purpose

These guidelines define the Rust coding standard for safety-critical code paths in the `aegis-runtime-sdk` crate. They are derived from MISRA C:2012 principles adapted for the Rust programming language and aligned with the Ferrocene Language Specification (FLS), which defines the safe subset of Rust suitable for use in safety-critical systems under IEC 61508 and ISO 26262.

The guidelines are binding for all code in the `aegis-runtime-sdk` crate that implements or supports a safety goal in AEGIS-SG-001. They are advisory for non-safety-critical code (e.g., CARLA integration, dashboard tooling).

### 1.2 Safety-Critical Paths

The following modules and files are classified as safety-critical and are subject to all rules in this document:

| Module / File | Safety Goals | Classification |
|---------------|-------------|----------------|
| `src/verifier.rs` | SG-003, SG-007, SG-008 | ASIL D |
| `src/posture_cache.rs` | SG-005, SG-006 | ASIL D |
| `src/posture_engine.rs` | SG-003, SG-005, SG-007 | ASIL D |
| `src/posture_engine_v2.rs` | SG-005 | ASIL D |
| `src/gateway/kinematics_contract.rs` | SG-001, SG-002, SG-004 | ASIL D |
| `src/gateway/policy_layer.rs` | SG-006, SG-008 | ASIL D |
| `src/gateway/policy.rs` | SG-006 | ASIL D |
| `src/telemetry_watchdog.rs` | SG-003 | ASIL D |
| `src/recovery_hysteresis.rs` | SG-013 | ASIL B |
| `src/standby_monitor.rs` | SG-009 | ASIL B |
| `src/audit_chain.rs` | SG-010, SG-012 | ASIL B |
| `src/startup_sentinel.rs` | SG-008 | ASIL D |
| `src/security.rs` | SG-015 | ASIL B |
| `src/fabric/router.rs` | SG-007 | ASIL D |
| `src/adapters/canopen.rs` | SG-011 | ASIL C |
| `src/adapters/dnp3.rs` | SG-012 | ASIL B |
| `src/federation_reconciliation.rs` | SG-014 | ASIL B |

Non-safety-critical files (e.g., `src/bin/aegis_carla_client.rs`, `src/metrics.rs`, `examples/`) are subject to guidelines at the discretion of the code reviewer and are not held to ASIL compliance.

### 1.3 Relationship to Standards

These guidelines implement:
- MISRA C:2012 mandatory rules, adapted to Rust semantics
- ISO 26262-6:2018 §8.4 (Coding guidelines for safety-related embedded software)
- Ferrocene Language Specification (FLS) safe subset
- ISO/IEC 17961:2013 (C secure coding rules, as applicable to equivalent Rust constructs)

---

## 2. Memory Safety Rules

### Rule MEM-001: No Unsafe Blocks in Safety-Critical Paths (Mandatory)

`unsafe` blocks shall not be present in any safety-critical file listed in Section 1.2 without documented justification and a minimum of two independent code reviews. Each `unsafe` block must be accompanied by a `// SAFETY:` comment that precisely explains why the block is sound.

Rationale: The Ferrocene Language Specification defines a safe subset of Rust that is free of undefined behavior. Safety-critical code shall operate within this subset. Unsafe code introduces C-like undefined behavior risks that are incompatible with ASIL-D requirements.

Exception: FFI boundaries in `src/ffi.rs` are permitted to use `unsafe` with documented justification but are not in the safety-critical path (no direct actuator control).

### Rule MEM-002: No unwrap() or expect() on Critical Paths (Mandatory)

`.unwrap()` and `.expect()` shall not be called on `Option` or `Result` values in any safety-critical function. All fallible operations shall be handled with explicit pattern matching or the `?` operator with appropriate error propagation.

Rationale: `.unwrap()` and `.expect()` panic on `None` or `Err`, causing process termination. On safety-critical paths, process termination without proper safe-state entry is a violation of SG-008.

Permitted alternatives:
- `match` with explicit `None` / `Err` handling
- `if let` with explicit fallback
- `unwrap_or`, `unwrap_or_else`, `unwrap_or_default` where semantically appropriate
- `?` operator with typed error propagation

Exception: `.expect()` is permitted in test code and in `startup_sentinel.rs` where the intent is to abort on invariant violation (which is the correct safe-state behavior).

### Rule MEM-003: No Box::leak() or mem::forget() on Safety-Critical Paths (Mandatory)

`Box::leak()`, `std::mem::forget()`, and `ManuallyDrop` shall not be used in safety-critical code without documented justification. These constructs intentionally prevent drop execution and can cause resource exhaustion.

### Rule MEM-004: Vec and String Allocation in Enforcement Hot Path (Advisory)

`Vec`, `String`, and other heap-allocating types shall be avoided in the hot path of `validate_vehicle_command()` and `should_route_command()`. Pre-allocated structures shall be preferred to prevent allocation failures from affecting enforcement latency.

---

## 3. Arithmetic Safety Rules

### Rule ARITH-001: f64 Finiteness Check Before Arithmetic (Mandatory)

All `f64` values received from external sources (HTTP request bodies, protocol adapter inputs, federation reports) shall be checked with `f64::is_finite()` before any arithmetic operation is performed. This check shall occur at the earliest possible point in the processing pipeline.

In `validate_vehicle_command()`, this check is implemented at Priority 0 and shall remain the first check in the function body. This implements SG-004 (TR-004).

```rust
// Correct:
if !value.is_finite() {
    return Err(ValidationError::NonFiniteValue { field: "linear_velocity_mps" });
}

// Prohibited on safety-critical paths:
let result = value * coefficient;  // arithmetic before finiteness check
```

### Rule ARITH-002: Saturating Arithmetic for Timestamps and Counters (Mandatory)

Timestamp arithmetic (e.g., `now_ms - generated_at_ms`) and counter operations (e.g., streak increments) shall use saturating or checked arithmetic to prevent integer overflow/underflow. Standard Rust `-`, `+`, `*` on integer types wrap in release builds (in Rust, overflow is defined as wrapping unless using debug assertions).

```rust
// Correct:
let age_ms = now_ms.saturating_sub(generated_at_ms);

// Prohibited:
let age_ms = now_ms - generated_at_ms;  // panics in debug, wraps in release
```

### Rule ARITH-003: Division Operations Must Guard Against Zero Divisor (Mandatory)

Any division operation where the divisor is derived from a variable or function parameter shall include an explicit zero check before the division. This applies especially to the bicycle model computation in `validate_vehicle_command()` where `turn_radius_m` is computed from `angular_velocity_radps`.

```rust
// Correct:
if angular_velocity_radps.abs() < f64::EPSILON {
    // Straight-line motion; skip lateral accel check
} else {
    let turn_radius = linear_velocity_mps / angular_velocity_radps;
    // ...
}

// Prohibited:
let turn_radius = linear_velocity_mps / angular_velocity_radps;  // possible divide by zero
```

### Rule ARITH-004: No Lossy Numeric Casts on Safety-Critical Values (Mandatory)

Casts that may silently truncate or lose precision (e.g., `f64 as f32`, `u64 as u32`, `i64 as u64` when the value may be negative) shall not be used on safety-critical numeric values. Use `TryFrom`/`TryInto` or range-checked conversions.

---

## 4. Concurrency Rules

### Rule CONC-001: Lock Ordering Must Be Documented and Enforced (Mandatory)

Any code that acquires multiple locks simultaneously shall document the lock acquisition order and shall always acquire locks in the documented order to prevent deadlock. In the Aegis codebase, the established lock ordering is:

1. `AppState.store` (`Arc<Mutex<VerifierStore>>`) — innermost
2. `SharedPostureCache` (`Arc<RwLock<...>>`) — outer
3. `AppState.nodes` (DashMap shard locks) — acquired implicitly

No code shall acquire `store` lock while holding the `SharedPostureCache` write lock.

### Rule CONC-002: No std::env::set_var in Multithreaded Context (Mandatory)

`std::env::set_var()` shall never be called in a multithreaded context. This is a security invariant (CLAUDE.md invariant 13) and a correctness invariant: `set_var` is not thread-safe and can cause data races with concurrent `var()` calls on POSIX systems.

Environment variables shall be read once at startup and stored in the application configuration struct. Dynamic modification of environment variables is prohibited.

### Rule CONC-003: RwLock Read Guard Must Check Staleness Before Use (Mandatory)

When acquiring a read guard on `SharedPostureCache`, the caller shall immediately check the staleness condition (`now_ms - generated_at_ms >= POSTURE_CACHE_TTL_MS`) before using the cached posture for any enforcement decision. Callers shall not use the raw posture value without the staleness check.

Use `resolve_posture_with_reason(cache, now_ms)` rather than directly reading the cache to ensure staleness is always checked.

```rust
// Correct:
let posture_result = resolve_posture_with_reason(&cache, now_ms).await;

// Prohibited:
let guard = cache.read().await;
let posture = guard.as_ref().map(|c| c.posture.clone());  // no staleness check
```

### Rule CONC-004: Atomic Operations on mode_active Must Use Correct Ordering (Mandatory)

All operations on `AppState.mode_active` (`Arc<AtomicBool>`) in the HA promotion path shall use `Ordering::SeqCst` for both the `compare_exchange` success and failure orderings. Relaxed or Acquire/Release orderings are insufficient for the cross-thread visibility guarantees required by the promotion logic.

### Rule CONC-005: DashMap Iteration Must Not Hold Shard Lock Across Await Points (Mandatory)

DashMap shard guards shall not be held across `.await` points. If asynchronous operations are required while iterating a DashMap, the iteration result shall be collected into an owned `Vec` before the await.

```rust
// Correct:
let node_ids: Vec<String> = app.nodes.iter().map(|e| e.key().clone()).collect();
for node_id in node_ids {
    some_async_operation(&node_id).await;
}

// Prohibited:
for entry in app.nodes.iter() {
    some_async_operation(entry.key()).await;  // holds DashMap shard lock across await
}
```

---

## 5. Error Handling Rules

### Rule ERR-001: No Panic on Safety-Critical Paths (Mandatory)

Code in safety-critical functions shall not call `panic!()`, `todo!()`, `unimplemented!()`, `unreachable!()`, or `assert!()` (in non-test code) unless the intent is to enter an explicit safe state (process abort). Use `Result` with typed error variants to propagate failures.

Exception: `unreachable!()` may be used only when an exhaustiveness proof exists that the branch is genuinely unreachable, with a `// SAFETY:` comment explaining the proof.

### Rule ERR-002: Explicit Result Handling — No Silently Discarded Errors (Mandatory)

The result of every fallible operation (`Result<T, E>`) in a safety-critical function shall be explicitly handled. The `#[must_use]` attribute is relied upon by the compiler; additionally, code reviewers shall verify that no `.ok()` or `let _ =` calls are used to silently discard errors on safety-critical paths.

```rust
// Correct:
store.save_node(&node).map_err(|e| {
    tracing::error!(node_id = %id, error = %e, "Failed to persist node");
    ServiceError::PersistenceFailed(e)
})?;

// Prohibited:
store.save_node(&node).ok();  // error silently discarded
let _ = store.save_node(&node);  // error silently discarded
```

### Rule ERR-003: Audit Write Errors Are Logged but Not Fatal (Conditional)

Audit chain write failures (`AuditChainLinker::append()` returning `Err`) shall be logged at `ERROR` level with full context (operation type, node ID, timestamp). For non-broadcast operations, audit write failure shall not block the enforcement decision. For DNP3 broadcast commands, audit write failure is fatal and shall block the control output (TR-012a).

This asymmetry is intentional: the enforcement action (blocking an unsafe command) is more safety-critical than the audit record. Blocking enforcement due to a disk error would be a fail-open behavior. However, for broadcast industrial commands where auditability is the primary safety concern (SG-012), the audit write failure is treated as fatal.

### Rule ERR-004: Error Types Must Be Informative (Advisory)

Error types returned from safety-critical functions shall carry sufficient context for diagnosis: the failing operation, the input values involved, and the expected constraint. Generic `()` errors and string-only errors are prohibited in safety-critical code.

---

## 6. Determinism Rules

### Rule DET-001: No Clock Calls Inside derive_fleet_posture or validate_vehicle_command (Mandatory)

The functions `derive_fleet_posture()`, `recalculate_and_broadcast()`, `validate_vehicle_command()`, and `should_route_command()` shall not call `std::time::SystemTime::now()`, `std::time::Instant::now()`, or any equivalent system clock function internally.

Time-dependent behavior in these functions shall be achieved by accepting a `now_ms: u64` parameter from the caller. This enables deterministic testing with VirtualClock injection and is required for the ScenarioRunner test harness to function correctly.

```rust
// Correct:
fn resolve_posture_with_reason(cache: &CachedFleetPosture, now_ms: u64) -> PostureResult {
    let age_ms = now_ms.saturating_sub(cache.generated_at_ms);
    // ...
}

// Prohibited:
fn resolve_posture_with_reason(cache: &CachedFleetPosture) -> PostureResult {
    let now_ms = SystemTime::now()...; // hidden clock call; not testable
    // ...
}
```

### Rule DET-002: No RNG on Safety-Critical Paths (Mandatory)

Cryptographically secure random number generation (e.g., `rand::thread_rng()`, `OsRng`) and pseudo-random generation shall not be used in safety-critical enforcement functions. Randomness is permitted only in:
- Attestation challenge generation (nonce production in challenge-response protocol)
- Federation nonce generation

Even in these permitted uses, the RNG shall be seeded from `OsRng` and shall not be used in functions that implement the three enforcement layers.

### Rule DET-003: Posture Calculation Must Be Idempotent (Mandatory)

`derive_fleet_posture()` shall produce the same result when called multiple times with the same `AppState.nodes` and `AppState.dependency_graph` contents. The function shall have no side effects on the state it reads. Side effects (cache updates, generation increment, SSE broadcast) belong in `recalculate_and_broadcast()`, not in `derive_fleet_posture()`.

---

## 7. Security Invariants

The following 13 security invariants, as defined in `CLAUDE.md`, are binding for all code in the `aegis-runtime-sdk` crate. Any pull request that violates these invariants shall be rejected without exception, regardless of other justifications.

### INV-01: require_admin_token Must Never Be Bypassed (Mandatory, ASIL B)

`require_admin_token` must never be commented out, bypassed, or removed from any mutation route. It reads `AEGIS_ADMIN_TOKEN` from the environment; if absent or empty, it returns HTTP 503 (fail-closed), never fail-open. This implements SG-015 (TR-015, TR-015a).

### INV-02: constant_time_compare for All Token Comparisons (Mandatory, ASIL B)

`constant_time_compare` (from `src/security.rs`) must be used for all security-critical byte sequence comparisons. The standard `==` operator shall never be used on tokens, secrets, or HMAC digests. This prevents timing side-channel attacks that could allow token extraction.

### INV-03: verify_attestation Must Use Real HMAC Proof (Mandatory, ASIL D)

`verify_attestation` must never use a mock or hardcoded `NodeTrustState::Trusted` assignment. The HMAC-SHA256 proof must be computed from the challenge nonce and the node's registered attestation key, then compared using `constant_time_compare`.

### INV-04: Gray/Black DAG Traversal Must Remain Intact (Mandatory, ASIL D)

`FleetNodePosture` and the gray/black two-set DAG algorithm in `AppState::recursive_calculate` must never be replaced with a mock, simplified traversal, or hardcoded result. The real traversal must remain intact. This is the primary safety mechanism for detecting dependency-based trust failures.

### INV-05: Challenge Nonces Must Remain Volatile (Mandatory, ASIL D)

`pending_challenges: DashMap<String, ChallengeEntry>` must never be removed. Nonces are volatile, in-memory only, and expire after `CHALLENGE_TTL_MS = 30000` ms. Persisting nonces to SQLite would allow replay attacks after process restart.

### INV-06: AEGIS_ADMIN_TOKEN From Environment Only (Mandatory, ASIL B)

`AEGIS_ADMIN_TOKEN` must come from `std::env::var("AEGIS_ADMIN_TOKEN")` only. No hardcoded fallback values, default strings, or configuration-file overrides for this variable are permitted. Absent or empty resolves to HTTP 503.

### INV-07: AEGIS_SUPERVISOR_RESET_KEY From Environment Only (Mandatory, ASIL B)

`AEGIS_SUPERVISOR_RESET_KEY` must come from the environment variable only, with no hardcoded fallbacks. The value must be present, non-empty, and at most 64 bytes. Any value failing these constraints results in startup abort via `startup_sentinel`.

### INV-08: Hard Boundary Clamp Before Rate-of-Change Limiter (Mandatory, ASIL D)

The velocity envelope cap (Priority 2 in `validate_vehicle_command`) must always be applied before the rate-of-change limiter in `AegisKernelGovernor`. The hard clamp to `max_speed_mps` takes absolute priority. No ordering inversion is permitted. This implements SG-001 and addresses H-015.

### INV-09: OperationalCommand::Unknown Denied Before Posture Check (Mandatory, ASIL D)

The early return `if command == OperationalCommand::Unknown { return false; }` in `should_route_command` must never be removed, conditioned on posture state, or moved after the posture evaluation. This implements SG-006 and addresses H-009.

### INV-10: DDS Actuator Topics Must Use Volatile Durability (Mandatory, ASIL C)

DDS actuator topics created in `src/dds_bridge.rs` must use `DurabilityPolicy::Volatile`. `DurabilityPolicy::TransientLocal` is prohibited for actuator topics. This implements SG-016 and addresses H-017.

### INV-11: Handlers Use State<Arc<ServiceState>> (Mandatory)

All axum route handlers must use `State<Arc<ServiceState>>` as the state extractor. `State<Arc<AppState>>` is incorrect; `ServiceState` wraps both `Arc<AppState>` and `SharedPostureCache` and is the only correct state type for handlers that need both.

### INV-12: SQLite Writes: Disk Before Memory (Mandatory, ASIL D)

In `persist_and_insert_node` and all equivalent persistence-then-memory operations, the SQLite write (`save_node`) must occur before the in-memory insert (`nodes.insert`). Reversing this order would allow in-memory state to diverge from persisted state on crash, creating an inconsistent trust state on restart.

### INV-13: No std::env::set_var in Multithreaded Context (Mandatory)

`std::env::set_var()` is not thread-safe on POSIX systems and shall never be called after the tokio runtime starts. All environment variable reads shall occur at startup via `std::env::var()` and be stored in the application state. See Rule CONC-002.

---

## 8. Code Review Requirements

### 8.1 Review Classification

All changes to files listed in Section 1.2 require a minimum of two independent reviewers, both of whom must have read these coding guidelines.

For changes to ASIL D files, at least one reviewer must be a designated Safety Reviewer with knowledge of the relevant safety goal(s) affected by the change.

### 8.2 Invariant Impact Assessment

Every pull request modifying a safety-critical file shall include an invariant impact assessment in the PR description, addressing each of the 13 security invariants in Section 7 that could be affected by the change. The assessment shall explicitly state whether the change introduces a risk of invariant violation and how the change has been verified to be invariant-preserving.

Template:
```
## Invariant Impact Assessment
- INV-01 (require_admin_token): [Not affected | Affected — justification]
- INV-02 (constant_time_compare): [Not affected | Affected — justification]
...
- INV-13 (set_var): [Not affected | Affected — justification]
```

### 8.3 Protected Code Regions

The following function bodies are Protected Code Regions. Any modification requires explicit Safety Goal impact assessment and approval from the Safety Reviewer:

| Protected Region | Safety Goals | Invariants |
|-----------------|-------------|------------|
| `src/posture_cache.rs:should_route_command` | SG-005, SG-006 | INV-09 |
| `src/gateway/kinematics_contract.rs:validate_vehicle_command` | SG-001, SG-002, SG-004 | INV-08 |
| `src/verifier.rs:AppState::recursive_calculate` | SG-003, SG-007 | INV-04 |
| `src/bin/aegis_verifier_service.rs:require_admin_token` | SG-015 | INV-01, INV-02, INV-06 |
| `src/recovery_hysteresis.rs:evaluate_recovery_report` | SG-013 | — |
| `src/posture_engine_v2.rs:resolve_posture_with_reason` | SG-005 | — |
| `src/standby_monitor.rs:spawn_promotion_monitor` | SG-009 | — |
| `src/audit_chain.rs:AuditChainLinker::append` | SG-010, SG-012 | — |
| `src/dds_bridge.rs` (topic creation) | SG-016 | INV-10 |

### 8.4 Test Requirement for Safety-Critical Changes

Every change to a Protected Code Region must be accompanied by at least one new or modified test that exercises the specific invariant or behavior being changed. PRs that modify safety-critical behavior without test changes shall be rejected.

---

## 9. Tool Qualification Notes

### 9.1 Rust Compiler (rustc)

The Rust compiler used for safety-critical builds must be:
- A stable release channel version (no nightly-only features in safety-critical code)
- The specific version pinned in `rust-toolchain.toml` at the repository root
- Qualified under ISO 26262-8:2018 §11 (Software tool qualification) for ASIL D use

The Ferrocene compiler (by Ferrous Systems) is the recommended qualified Rust compiler for ISO 26262 ASIL D applications. The standard rustc compiler may be used for development but must be replaced with a qualified compiler before certification submission.

### 9.2 cargo (Build Tool)

`cargo` is used for dependency management, build orchestration, and test execution. For certification:
- Dependency versions shall be pinned in `Cargo.lock` and audited with `cargo-audit`
- Third-party crates used in safety-critical paths shall be assessed for qualification or replaced with qualified alternatives
- `cargo test` shall produce reproducible results; test randomization (proptest seeds) shall be recorded

### 9.3 proptest (Property-Based Testing)

`proptest` v1 is used for property-based testing of the kinematics validation functions. For certification purposes:
- All failing proptest cases shall be persisted in `proptest-regressions/` and shall remain in version control
- The proptest test count and seed shall be sufficient to achieve the required structural coverage metric (MC/DC for ASIL D)
- Proptest alone does not satisfy the MC/DC requirement; it supplements but does not replace unit tests with explicit oracle assertions

### 9.4 rusqlite (SQLite Binding)

`rusqlite` v0.31 with the `bundled` feature includes a compiled SQLite amalgamation. For certification:
- The bundled SQLite version shall be recorded and assessed against known CVEs
- SQLite itself is not safety-qualified; its use is acceptable for persistence of non-real-time safety state (audit log, node registry) but shall not be on the direct enforcement path
- WAL mode journaling provides crash consistency but not ACID guarantees equivalent to a qualified database

### 9.5 Static Analysis

The following static analysis tools shall be run as part of the CI pipeline for safety-critical changes:
- `cargo clippy --deny warnings` with the `clippy::nursery` and `clippy::pedantic` lint groups enabled for safety-critical files
- `cargo +nightly miri` for detection of undefined behavior in unsafe code blocks (where present)
- A commercial SAST tool (e.g., CodeSonar, Polyspace) shall be integrated before ASIL D certification submission

---

## 10. Document Control

| Field | Value |
|-------|-------|
| Prepared by | Aegis Engineering |
| Review status | Pending TUV pre-assessment |
| Next review | 2026-11-23 |
| Supersedes | None |
