# IEC 61508 SIL 3 Requirements Mapping

Document ID: KIRRA-SIL3-001
Version: 1.0
Status: Draft
Standard: IEC 61508:2010 (Functional Safety of E/E/ES Systems)
Date: 2026-05-29

## 1. Overview

This document maps Kirra's architecture and safety mechanisms to
IEC 61508 Safety Integrity Level 3 (SIL 3) requirements.
IEC 61508 is the foundational functional safety standard for
electrical, electronic, and programmable electronic safety-related
systems (E/E/PES). SIL 3 requires a probability of dangerous failure
per hour (PFH) between 10⁻⁸ and 10⁻⁷.

Kirra targets SIL 3 compliance for the runtime safety governance
layer. This mapping covers:
- Part 3: Software requirements
- Part 4: Definitions and abbreviations
- Relevant clauses from Part 7: Overview of techniques and measures

This is a **self-assessment**. Independent third-party assessment by
a notified body has not yet been performed.

Relationship to the existing preliminary mapping in
`docs/safety/IEC_61508_MAPPING.md` (AEGIS-61508-001 v1.0.0): that
earlier document established the initial claim shape under the prior
Aegis branding; this document (KIRRA-SIL3-001) is the up-to-date
working mapping aligned with the current Kirra architecture and the
CERT-track artifacts. The earlier document remains in the safety
case index for historical traceability.

## 2. SIL 3 Software Requirements Mapping

### 2.1 Software Safety Lifecycle (IEC 61508-3 §7.1)

| IEC 61508-3 Requirement | SIL 3 Requirement | Kirra Implementation | Status |
|------------------------|------------------|---------------------|--------|
| Software safety requirements specification | Mandatory | `docs/safety/SAFETY_GOALS.md` — 16 safety goals with ASIL / SIL level, rationale, and acceptance criteria | ✅ Implemented |
| Software architecture design | Mandatory | `docs/safety/SAFETY_ARCHITECTURE.md` — module decomposition, trust boundaries, data flows | ✅ Implemented |
| Software design and development | Mandatory | Rust + cargo workspace, deterministic build | ✅ Implemented |
| Software module testing | Mandatory | 340+ unit tests, 70,000+ proptest cases | ✅ Implemented |
| Software integration testing | Mandatory | Integration test suite, fault injection tests (`tests/fault_injection.rs`) | ✅ Implemented |
| Software validation | Mandatory | RTM with traceability to safety goals | Partial — 50 % coverage |
| Software modification | Mandatory | Git version control, PR workflow | ✅ Implemented |
| Software verification | Mandatory | `cargo clippy`, `cargo audit`, static analysis CI | ✅ Implemented |

### 2.2 Software Design Requirements (IEC 61508-3 §7.4)

| Requirement | SIL 3 Recommendation | Kirra Implementation | Status |
|-------------|---------------------|---------------------|--------|
| Defensive programming | Highly recommended | Rust ownership model — no null pointers, no buffer overflows, no use-after-free | ✅ By language |
| Modular approach | Highly recommended | Workspace crates: `kirra-runtime-sdk`, `parko-core`, `parko-kirra`, `parko-onnx` | ✅ Implemented |
| Design and coding standards | Highly recommended | `docs/safety/CODING_GUIDELINES.md` (RSR-001 through RSR-007) | ✅ Implemented |
| Structured programming | Mandatory | Rust enforces structured control flow | ✅ By language |
| Use of certified tools | Recommended | Rust compiler (memory-safe), cargo, clippy | ✅ Implemented |
| Semi-formal methods | Recommended | Property-based testing (proptest) — 70,000+ cases | ✅ Implemented |
| Dynamic analysis and testing | Highly recommended | Fault injection test suite (CERT-004) | ✅ Implemented |

### 2.3 Software Verification Requirements (IEC 61508-3 §7.9)

| Requirement | SIL 3 Requirement | Kirra Implementation | Status |
|-------------|------------------|---------------------|--------|
| Static analysis | Highly recommended | `cargo clippy` + `cargo audit` in CI (CERT-002) | ✅ Implemented |
| Dynamic analysis | Highly recommended | Fault injection, adversarial simulation (PARK-019) | ✅ Implemented |
| Data flow analysis | Recommended | Rust borrow checker (compile-time) | ✅ By language |
| Control flow analysis | Recommended | Rust ownership + clippy | ✅ By language |
| Code coverage | Highly recommended | Branch coverage CI (MC/DC pending — issue #65) | Partial |
| Traceability | Mandatory | RTM (`docs/safety/REQUIREMENTS_TRACEABILITY.md`) | Partial — 50 % |
| Failure mode analysis | Recommended | `docs/safety/SAFE_STATE_SPECIFICATION.md`, `docs/safety/RTM_GAP_REPORT.md` | ✅ Implemented |

### 2.4 Functional Safety Mechanisms (IEC 61508-3 §7.4.2)

These are the core runtime mechanisms Kirra implements that directly
address IEC 61508 SIL 3 requirements:

#### FM-001: Fail-Closed Safety State Machine
IEC 61508 reference: §7.4.2.3 — Detection of faults
Kirra implementation:
- `PostureState` / `FleetPosture` enum: Nominal / Degraded / LockedOut
- Transitions defined and documented in `SAFE_STATE_SPECIFICATION.md` (KIRRA-SSS-001)
- Fail-closed: unknown state → Degraded; stale cache → LockedOut (never assumes Nominal)
- Hard stop on LockedOut — 0.0 m/s, no exceptions
Source: `src/posture_cache.rs`, `src/verifier.rs`

#### FM-002: Redundant Safety Enforcement (Software Lockstep)
IEC 61508 reference: §7.4.5 — Redundancy; §7.4.4 — diverse (N-version) software
Kirra implementation:
- `GovernorComparator` runs a primary `KirraGovernor` against a **diverse**
  shadow `DiverseKirraGovernor` (CERT-006 diversity). The two enforce the same
  safety properties via structurally different computation, so a systematic
  *implementation* fault is unlikely to manifest identically in both. The
  comparator is generic over the shadow; a second `KirraGovernor` still yields
  the legacy identical-redundancy pairing (random-fault detection only).
- Divergence beyond `COMPARATOR_TOLERANCE = 1e-9` → posture-aware, speed-gated
  escalation to `EnforcementAction::Deny` (LockedOut semantics)
- Software equivalent of hardware dual-core lockstep (NVIDIA DRIVE AGX / NXP S32),
  now with implementation diversity layered on top
- **Honest limit:** diversity is at the implementation layer only; primary and
  shadow share the specification and config, so spec-level faults are NOT
  covered. See `docs/safety/COMPARATOR_DIVERSITY.md` (DRAFT — pending review).
Source: `parko/crates/parko-kirra/src/comparator.rs`,
`parko/crates/parko-kirra/src/diverse.rs` (CERT-006)

#### FM-003: Watchdog and Telemetry Timeout Detection
IEC 61508 reference: §7.4.2.5 — Watchdog
Kirra implementation:
- `AV_TELEMETRY_TIMEOUT_MS = 2_000` watchdog on per-node sensor telemetry
- `AV_WATCHDOG_SWEEP_MS = 100` sweep interval — bounded detection latency 2.1 s
- Timeout → node marked `Untrusted`, posture recalculation triggered
- Cannot be disabled at runtime
Source: `src/verifier.rs`

#### FM-004: Tamper-Evident Audit Chain
IEC 61508 reference: §7.9.2 — Traceability
Kirra implementation:
- SHA-256 hash-chained audit log (`prev_hash` field on every entry)
- Optional Ed25519 signed entries (when `KIRRA_LOG_SIGNING_KEY` set)
- `prev_hash` mismatch detected on startup and via `/system/audit/verify` endpoint
Source: `src/audit_chain.rs`

#### FM-005: Startup Safety Sentinel
IEC 61508 reference: §7.4.2.1 — Checks at startup
Kirra implementation:
- `startup_sentinel` verifies all invariants before TCP listener binds
- Fails closed (process abort) if any invariant missing
- Invariants: admin token, supervisor reset key, posture engine, SQLite WAL mode, DDS Volatile durability
Source: `src/bin/kirra_verifier_service.rs`

#### FM-006: Constant-Time Security Comparisons
IEC 61508 reference: §7.4.2 — Prevention of dangerous failures
Kirra implementation:
- `constant_time_compare()` for all token comparisons
- Prevents timing side-channel attacks on authentication
- Forbidden to compare security-critical byte sequences with `==` (CERT-005 RSR)
Source: `src/security.rs`

#### FM-007: Dependency Graph Safety Propagation
IEC 61508 reference: §7.4.6 — Common cause failures
Kirra implementation:
- Gray / black DAG traversal for cycle detection
- `LockedOut` propagates upward — never downgraded by RSS recovery
- `MAX_DEPENDENCY_DEPTH = 10` prevents unbounded recursion
Source: `src/verifier.rs`

### 2.5 Random Hardware Failure Requirements

IEC 61508 SIL 3 requires a PFH of 10⁻⁸ to 10⁻⁷ for the overall safety
function. Kirra is a software safety layer; the random-hardware-failure
budget is allocated to the underlying platform.

For **QNX** deployments: QNX OS for Safety 8.x is pre-certified to
IEC 61508 SIL 3. The hardware platform (e.g., NXP S32, NVIDIA AGX)
contributes additional hardware-level diagnostic coverage. QNX
toolchain support is tracked in PARK-024 / PARK-024b (issue #67);
binary boot is currently blocked on upstream Rust ecosystem gaps
(`libc` `nto80` constants).

For **Linux** deployments: Hardware-level SIL 3 requires additional
platform certification not provided by standard Linux. Use of a
SIL 3 RTOS or hardware-diagnosed substrate is required for a full
SIL 3 claim.

## 3. SIL 3 vs ASIL-D Comparison

| Property | ASIL-D (ISO 26262) | SIL 3 (IEC 61508) | Kirra Status |
|----------|-------------------|-------------------|-------------|
| Target domain | Automotive | Industrial / General | Both targeted |
| Probability of failure | PMHF < 10⁻⁸ /hr | PFH 10⁻⁸ to 10⁻⁷ /hr | Architectural target |
| Code coverage | MC/DC mandatory | Branch / decision highly recommended | Branch CI active; MC/DC pending (#65) |
| Formal methods | Recommended | Recommended | Property-based testing |
| Tool qualification | Required | Required | Rust compiler + clippy (qualification doc not yet produced) |
| Independence of V&V | Required | Required | Pending third-party assessment |
| Safety manual | Required | Required | This document + `SAFETY_GOALS.md` + `SAFE_STATE_SPECIFICATION.md` |

Kirra's safety mechanisms satisfy both ASIL-D and SIL 3 architectural
requirements. The primary difference is the certification process —
ASIL-D requires automotive-sector TÜV SÜD assessment, while SIL 3
can be assessed by any IEC 61508 notified body.

## 4. Gaps and Open Items

The following items are required for SIL 3 compliance but not yet
complete:

| Gap | Priority | Tracking |
|-----|----------|---------|
| RTM coverage < 100 % (currently 50 % goal-level, 20 % test-level) | High | CERT-003, CERT-004, `RTM_GAP_REPORT.md` |
| MC/DC coverage not yet measured | High | Issue #65 |
| Independent V&V not performed | High | Pending TÜV / notified-body engagement |
| Tool qualification documentation (Rust toolchain) | Medium | Not started |
| Random-hardware-failure analysis (FMEDA) | Medium | Platform-dependent — QNX path waits on PARK-024b |
| QNX SIL 3 integration testing | Medium | PARK-024 (#36), PARK-024b (#67) |

## 5. Implementation References

- Safety goals: `docs/safety/SAFETY_GOALS.md`
- Safe state specification: `docs/safety/SAFE_STATE_SPECIFICATION.md`
- RTM: `docs/safety/REQUIREMENTS_TRACEABILITY.md`
- Coding standard: `docs/safety/CODING_GUIDELINES.md`
- RTM gap report: `docs/safety/RTM_GAP_REPORT.md`
- Predecessor mapping: `docs/safety/IEC_61508_MAPPING.md` (AEGIS-61508-001)
- GovernorComparator: `parko/crates/parko-kirra/src/comparator.rs`
- Posture engine: `src/posture_cache.rs`, `src/verifier.rs`
- Audit chain: `src/audit_chain.rs`
- Security: `src/security.rs`
- Startup sentinel: `src/bin/kirra_verifier_service.rs`
