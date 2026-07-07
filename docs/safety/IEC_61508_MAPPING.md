# Kirra — IEC 61508 SIL 3 Preliminary Claim Mapping

Document ID: AEGIS-61508-001
Version: 1.0.0
Status: Draft (Preliminary — pre-assessment)
References: IEC 61508:2010 Parts 1–7, IEC 61508-3 Software Requirements
Date: 2026-05-23

---

## 1. Purpose

This document provides a preliminary mapping of Kirra safety functions to IEC 61508 requirements for a Safety Integrity Level 3 (SIL 3) claim using the Safety Element out of Context (SEooC) model defined in IEC 61508-2 Annex B.

This is a pre-assessment document intended to support:
1. Scoping discussions with a Functional Safety assessment body (Exida, TUV SUD, SGS)
2. Gap identification before committing to formal certification
3. Evidence that the Kirra development approach is aligned with IEC 61508

This document does not constitute a formal SIL claim. A formal claim requires an independent assessment body to verify compliance.

---

## 2. Item Definition and Scope

**E/E/PE Safety-related System:** Kirra Runtime Safety Kernel (`kirra-runtime-sdk` v1.5.0)

**Type of system:** Software SEooC — Safety Element out of Context

Under the SEooC model, Kirra is certified as a software component with assumed safety requirements (ASRs). Integrators perform in-context verification to confirm ASRs are satisfied in their specific deployment.

**System boundary:**
- In scope: All safety functions implemented in `kirra-runtime-sdk`, specifically the enforcement pipeline, posture derivation, and trust graph layer
- Out of scope: Hardware platform, operating system, upstream AI planner, physical actuators

**Claimed SIL:** SIL 3

**Justification for SIL 3:** Based on the risk assessment in AEGIS-HARA-001, the highest-ASIL hazards (H-001, H-002, H-003, H-006, H-009, H-012) are rated ASIL-D under ISO 26262. The IEC 61508 equivalent is SIL 3. A SIL 4 claim would require continuous mode operation with a PFH < 10^-8/h; the command-on-demand nature of Kirra enforcement places it in high-demand mode with PFH requirements, where SIL 3 (PFH < 10^-7/h) is the appropriate target.

---

## 3. Safety Functions

### SF-001: Motion Command Velocity Clamping [SIL 3]

**Description:** Kirra shall clamp the linear velocity of any motion command to the active kinematic contract's max_speed_mps before forwarding to the actuator.

**Implementation:** `validate_vehicle_command()` Priority 2 check in `src/gateway/kinematics_contract.rs`

**Safety goal:** SG-001

**IEC 61508-1 Clause 7.6.2.9:** Safety function specification complete — inputs (linear_velocity_mps, max_speed_mps from active contract), outputs (ClampLinear(max_speed_mps) or Allow), safe state (velocity clamped to contract limit), FTTI (per-command, < 1ms).

**Demand mode:** High demand (commands processed continuously during operation)

**Claimed PFH:** < 10^-7 / hour (SIL 3 target)

---

### SF-002: Lateral Acceleration Constraint [SIL 3]

**Description:** Kirra shall compute bicycle model lateral acceleration for every steering command and clamp steering angle when lateral acceleration would exceed max_lateral_accel_mps2.

**Implementation:** `validate_vehicle_command()` Priority 6 check — `lateral_accel = velocity^2 * tan(steering_deg.to_radians()) / wheelbase_m`

**Safety goal:** SG-002

**Safe state:** Steering angle clamped to the maximum angle that satisfies the lateral acceleration constraint at current velocity. Sign preserved (SG-002 invariant 3).

**FTTI:** Per-command, < 1ms

---

### SF-003: Posture-Gated Command Routing [SIL 3]

**Description:** Kirra shall deny all WriteState and SystemMutation commands when fleet posture is Degraded, and deny all commands when fleet posture is LockedOut or the posture cache is stale.

**Implementation:** `should_route_command()` in `src/posture_cache.rs`

**Safety goal:** SG-003, SG-005

**Safe state:**
- Degraded: Deny all non-ReadTelemetry commands
- LockedOut: Deny all commands
- Stale cache (age >= POSTURE_CACHE_TTL_MS = 5000ms): Deny all commands (fail-closed)

**FTTI:** Per-command for routing; 2000ms for posture update after sensor fault (AV_TELEMETRY_TIMEOUT_MS)

---

### SF-004: NaN/Inf Rejection [SIL 2]

**Description:** Kirra shall reject any motion command containing NaN or infinite values before any arithmetic evaluation.

**Implementation:** `validate_vehicle_command()` Priority 0 check — `!field.is_finite()` for all f64 fields

**Safety goal:** SG-004

**SIL justification:** SIL 2 (ASIL-C equivalent, H-005 rated S3/E2/C3 = C). Exposure E2 (low) reduces required SIL from D to C.

---

### SF-005: Unknown Command Denial [SIL 3]

**Description:** Kirra shall deny any action classified as OperationalCommand::Unknown in all fleet posture states, before posture evaluation.

**Implementation:** `should_route_command()` early return — `if command == OperationalCommand::Unknown { return false; }` in `src/posture_cache.rs`

**Safety goal:** SG-006

**Rationale for early return:** Unknown commands bypass the posture gate to prevent unknown-command-type exploitation in any posture state. This is Security Invariant #9.

---

### SF-006: Cross-Asset Trust Propagation [SIL 3]

**Description:** In a Multi-Asset Fabric deployment, when a leader asset transitions to LockedOut, Kirra shall propagate Degraded posture to all follower assets within one fabric recalculation cycle.

**Implementation:** `FabricRouter::propagate_cross_asset_trust()` in `src/fabric/router.rs` — 4 propagation rules (convoy, drone-controller, infrastructure, warehouse-robot)

**Safety goal:** SG-007

---

## 4. Architectural Constraints

IEC 61508-2 Table 2 and 3 define architectural constraints for SIL claims based on hardware fault tolerance (HFT) and safe failure fraction (SFF). For software-only SEooC, IEC 61508-3 architectural requirements apply.

### Software Architecture (IEC 61508-3 Clause 7.4)

| Requirement | Kirra Implementation | Compliance |
|-------------|---------------------|------------|
| Modular design | Functions decomposed into layers (Trust Graph, Posture Derivation, Enforcement) | Compliant |
| Structured programming | Rust ownership model enforces structured control flow; no goto, no unsafe on critical paths | Compliant |
| Defensive programming | Fail-closed semantics on all error paths; no unwrap() on safety-critical paths per AEGIS-CG-001 | Compliant |
| Error detection | All f64 inputs checked for finiteness before arithmetic; posture cache TTL enforced | Compliant |
| Static analysis | Rust compiler (borrow checker, lifetime analysis) provides strong static analysis guarantees | Compliant |
| Dynamic analysis / testing | 2,476 passing workspace unit/property/integration tests (as of 2026-07-07; live count via `cargo test --workspace`); proptest generates counterexample-guided test cases | Partial — MC/DC coverage not yet measured |

### Avoidance of Systematic Failures (IEC 61508-3 Clause 7.4.4)

| Technique | Kirra Implementation |
|-----------|---------------------|
| Defensive programming (B.2.2) | Fail-closed on every error path; LockedOut as default unknown posture |
| Modular approach (B.2.4) | Three-layer architecture; each layer independently testable |
| Structured programming (B.2.5) | Rust enforced; no unsafe on safety-critical paths |
| Use of trusted/proven code (B.2.6) | Core dependencies: axum 0.8, tokio 1.x, ed25519-dalek 2.x, rusqlite 0.31 (established, widely used) |
| Static analysis tools (B.3.6) | Rust compiler + clippy; AEGIS-CG-001 prohibits unsafe on critical paths |
| Dynamic analysis (B.4.7) | proptest property-based testing; ScenarioRunner temporal harness |
| Independence of testing (B.5.4) | Test suite developed against specifications in AEGIS-RTM-001; property tests are specification-independent |

---

## 5. Software Development Process (IEC 61508-3 Part 6)

| Phase | IEC 61508-3 Requirement | Kirra Status |
|-------|------------------------|--------------|
| Software requirements specification | Safety goals (AEGIS-SG-001) + technical requirements (AEGIS-RTM-001) | Complete (Draft) |
| Software architecture design | Three-layer safety architecture (AEGIS-SA-001) | Complete (Draft) |
| Software unit design and implementation | Rust source, AEGIS-CG-001 coding guidelines | Complete |
| Software unit testing | 306 unit and property-based tests | Partial — MC/DC coverage not measured |
| Software integration testing | actuator_middleware_integration.rs; scenario_runner tests | In progress |
| Software validation | End-to-end test against safety goals | Pending |
| Traceability | AEGIS-RTM-001 (48 technical requirements traced to implementation and tests) | Complete (Draft) |
| Configuration management | Git with signed commits, branch protection | Partial — formal CM process not yet established |

---

## 6. Assumed Safety Requirements (SEooC)

For the SEooC model, Kirra declares the following Assumed Safety Requirements (ASRs) that integrators must verify in-context:

| ASR ID | Assumption | Integrator Verification Required |
|--------|------------|----------------------------------|
| ASR-001 | The upstream AI planner sends commands in the ProposedVehicleCommand JSON schema via HTTP POST to the Kirra enforcement endpoint | Integrator documents the interface contract between planner and Kirra |
| ASR-002 | KIRRA_ADMIN_TOKEN and KIRRA_SUPERVISOR_RESET_KEY are provisioned as non-empty environment variables before startup | Integrator documents key provisioning procedure |
| ASR-003 | The kinematic contract parameters (max_speed_mps, max_lateral_accel_mps2, wheelbase_m) are correctly configured for the specific platform | Integrator provides vehicle/robot specification and confirms parameters |
| ASR-004 | The deployment platform provides a monotonic clock with resolution sufficient for AV_WATCHDOG_SWEEP_MS (100ms) operation | Integrator confirms platform clock specification |
| ASR-005 | The SQLite database is on durable storage; filesystem failure is covered by the integrator's platform FMEA | Integrator documents storage reliability |
| ASR-006 | Network latency between the AI planner and Kirra is < 50ms under normal operation | Integrator measures and documents network latency |
| ASR-007 | The Kirra process is supervised by a process monitor (systemd, QNX resource manager, or equivalent) that restarts it on crash | Integrator documents process supervision configuration |

---

## 7. Gap Analysis: Path to SIL 3 Certificate

| Gap ID | Description | IEC 61508 Clause | Priority | Effort |
|--------|-------------|-----------------|----------|--------|
| 61508-GAP-001 | MC/DC structural coverage not measured for safety-critical functions | IEC 61508-3 Table A.5 (SIL 3 recommended) | High | 2–4 weeks (establish coverage tooling) |
| 61508-GAP-002 | Tool qualification not complete — standard rustc used, not Ferrocene | IEC 61508-3 Clause 7.4.4.3 | High | 4–8 weeks (Ferrocene evaluation + qualification) |
| 61508-GAP-003 | Formal change management process not established | IEC 61508-1 Clause 6.2 | High | Phase 2 (AEGIS-ROAD-001) |
| 61508-GAP-004 | Independent software verification not yet performed | IEC 61508-3 Clause 7.9 | High | Phase 4 (pre-assessment) |
| 61508-GAP-005 | Software validation plan not yet documented | IEC 61508-3 Clause 7.7 | Medium | Phase 2 |
| 61508-GAP-006 | FMEA for posture engine failure modes not yet documented | IEC 61508-2 Clause 7.4.3 | Medium | Phase 2 |
| 61508-GAP-007 | Real-time platform qualification (QNX or Linux PREEMPT_RT) not yet completed | IEC 61508-2 Annex C | Medium | Phase 3 (AEGIS-ROAD-001) |

---

## 8. Next Steps

1. **Scoping call with Exida** — Obtain formal assessment scope and cost estimate for SIL 3 SEooC
2. **Establish MC/DC coverage measurement** — cargo-tarpaulin or llvm-cov with branch coverage reporting for safety-critical modules
3. **Ferrocene compiler evaluation** — Contact Ferrous Systems for Ferrocene qualification kit and pricing
4. **Document validation plan** — Define acceptance criteria for software validation phase

---

## 9. Document Control

| Field | Value |
|-------|-------|
| Prepared by | Kirra Engineering |
| Review status | Pre-assessment (not yet reviewed by assessment body) |
| Next review | 2026-11-23 |
| Supersedes | None |
