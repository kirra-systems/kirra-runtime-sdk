# Kirra Safety Kernel — ASIL-D Certification Roadmap

Document ID: AEGIS-ROAD-001
Version: 1.0.0
Status: Draft
Classification: ISO 26262 Part 2
Date: 2026-05-23

---

## 1. Overview

This document defines the roadmap for achieving ISO 26262 ASIL-D certification for the Kirra Runtime Safety Kernel (`kirra-runtime-sdk` v1.5.0 and subsequent versions). The roadmap is organized into five phases, each with defined entry criteria, deliverables, exit criteria, and estimated duration.

The target certification scope is:
- Item: Kirra Runtime Safety Kernel (software element)
- ASIL: ASIL D (highest integrity level, per AEGIS-HARA-001)
- Standard: ISO 26262:2018 (second edition), Parts 1-9
- Supplementary: IEC 62443 (industrial cybersecurity), ROS 2 Safety WG recommendations

---

## 2. Phase Overview

| Phase | Name | Status | Estimated Duration | Dependencies |
|-------|------|--------|-------------------|--------------|
| Phase 1 | Foundation | Complete | Completed 2026-05-23 | None |
| Phase 2 | Process Compliance | In Progress | 3-6 months | Phase 1 complete |
| Phase 3 | QNX Platform Qualification | In Progress | Concurrent with Phase 2 | Phase 1 complete |
| Phase 4 | Pre-Assessment | Not started | 6-12 months post Phase 3 | Phases 2, 3 complete |
| Phase 5 | Formal Certification | Not started | Follows assessment | Phase 4 complete |

---

## 3. Phase 1 — Foundation

**Status:** Complete
**Completion date:** 2026-05-23

### 3.1 Objectives

Establish the safety case foundation documents that define the item, identify hazards, specify safety goals, describe the safety architecture, trace requirements to implementation, and define the coding standard.

### 3.2 Deliverables

| Deliverable | Document ID | Status |
|-------------|-------------|--------|
| Hazard Analysis and Risk Assessment | AEGIS-HARA-001 | Complete |
| Safety Goals (SG-001 to SG-016) | AEGIS-SG-001 | Complete |
| Safety Architecture (three-layer) | AEGIS-SA-001 | Complete |
| Requirements Traceability Matrix | AEGIS-RTM-001 | Complete |
| Rust Safety Coding Guidelines | AEGIS-CG-001 | Complete |
| Safety Case Index and GSN argument | AEGIS-SC-000 | Complete |
| Certification Roadmap | AEGIS-ROAD-001 | Complete |

### 3.3 Key Milestones

- HARA with 17 hazards and ASIL assignments: complete
- 16 safety goals with FTTI and verification methods: complete
- Three-layer safety architecture documented with mechanism IDs M-001 to M-013: complete
- 48 technical requirements traced to implementation and tests: complete
- 13 security invariants codified in coding guidelines: complete
- Workspace test baseline: established (2,476 passing as of 2026-07-07 — derive the live figure from `cargo test --workspace`, do not carry counts forward by hand)

### 3.4 Exit Criteria

All Phase 1 documents exist, are internally consistent, and have been reviewed by at least one engineer with ISO 26262 familiarity. The safety case argument in AEGIS-SC-000 identifies all undeveloped claims.

Phase 1 exit criteria: Met.

---

## 4. Phase 2 — Process Compliance

**Status:** In Progress
**Estimated duration:** 3-6 months from 2026-05-23
**Target completion:** 2026-08-23 to 2026-11-23

### 4.1 Objectives

Establish and document the software development process to ISO 26262 Part 8 requirements for tool qualification, configuration management, change management, and verification planning. Achieve structural coverage metrics (MC/DC) for all ASIL D safety goals.

### 4.2 Deliverables

#### 4.2.1 Tool Qualification

| Tool | Current Version | Qualification Status | Target |
|------|----------------|---------------------|--------|
| rustc (Ferrocene) | Pending adoption | Not started | Q3 2026 |
| cargo | Latest stable | Under assessment | Q3 2026 |
| proptest | 1.x | Under assessment | Q3 2026 |
| rusqlite | 0.31 | Under assessment | Q3 2026 |
| SAST tool (CodeSonar / Polyspace) | TBD | Not selected | Q3 2026 |

Tool qualification shall be performed per ISO 26262-8:2018 §11. Tools classified as TCL3 (Tool Confidence Level 3, highest impact on safety output) require full qualification. Tools classified as TCL1-2 require documented assessment only.

Classification:
- `rustc` / Ferrocene: TCL3 (code generation directly impacts safety output)
- `cargo`: TCL2 (build orchestration; not a code generator)
- `proptest`: TCL2 (test execution; coverage contribution at TCL2)
- `rusqlite`: TCL1 (library, not a build or test tool)

#### 4.2.2 Configuration Management

- Establish a formal configuration item (CI) list for all safety-critical source files
- Define version identification scheme for safety-critical releases (semantic version + safety case revision)
- Establish baseline management: each ASIL D release tagged in git with a signed tag referencing the safety case document set
- Define change management process: all changes to Protected Code Regions (AEGIS-CG-001 Section 8.3) require safety impact assessment before merge

#### 4.2.3 Structural Coverage

MC/DC (Modified Condition/Decision Coverage) is required for ASIL D per ISO 26262-6:2018 §9.4.3.

Scope: All safety-critical functions listed in AEGIS-CG-001 Section 1.2.

Actions:
- Integrate `cargo-llvm-cov` or `grcov` into the CI pipeline for line and branch coverage
- Evaluate MC/DC tooling (commercial or tarpaulin-based) for the Rust toolchain
- Achieve 100% MC/DC coverage on `validate_vehicle_command`, `should_route_command`, `resolve_posture_with_reason`, and `evaluate_recovery_report`
- Document coverage gaps and add tests to close them

Target coverage:
- Statement coverage: 100% on safety-critical functions
- Branch coverage: 100% on safety-critical functions
- MC/DC: 100% on ASIL D functions (SG-001, SG-002, SG-003, SG-005, SG-006, SG-007, SG-008)

#### 4.2.4 Verification Plan

Produce AEGIS-VP-001: Software Verification Plan covering:
- Unit test strategy and completion criteria
- Integration test strategy (including ScenarioRunner temporal integration tests)
- Review strategy (inspection, walkthrough) for each safety-critical module
- Static analysis strategy (clippy, miri, SAST)
- Regression test requirements for each TR

#### 4.2.5 Open Issue Resolution

Address open issues from AEGIS-SC-000 Section 4:
- ISSUE-001: EtherNet/IP adapter HARA addendum
- ISSUE-002: ROS2 interlock node safety requirements tracing
- ISSUE-003: MC/DC coverage measurement establishment
- ISSUE-005: Process documentation

### 4.3 Key Milestones

| Milestone | Target Date |
|-----------|-------------|
| Ferrocene compiler adoption decision | 2026-07-01 |
| SAST tool selection and integration | 2026-07-15 |
| MC/DC measurement infrastructure deployed | 2026-08-01 |
| 100% statement and branch coverage on ASIL D functions | 2026-09-01 |
| MC/DC gap analysis complete | 2026-09-15 |
| MC/DC 100% on all ASIL D safety goals | 2026-10-15 |
| Configuration management plan approved | 2026-08-15 |
| Software Verification Plan (AEGIS-VP-001) issued | 2026-09-01 |
| EtherNet/IP and ROS2 HARA addenda issued | 2026-09-15 |

### 4.4 Exit Criteria

- All tools qualified or assessed to their TCL level
- 100% MC/DC coverage on all ASIL D functions, with coverage report in the safety case
- Configuration management plan approved and in use
- Software Verification Plan (AEGIS-VP-001) issued and reviewed
- All open issues from AEGIS-SC-000 resolved or formally deferred with risk assessment
- Coding guideline compliance measured via static analysis with zero violations in safety-critical files

### 4.5 Dependencies

- Phase 1 complete (met)
- Engineering resource availability for coverage gap remediation
- Ferrocene commercial license acquisition (if selected as TCL3 compiler)
- SAST tool procurement

---

## 5. Phase 3 — QNX Platform Qualification

**Status:** In Progress (concurrent with Phase 2)
**Estimated duration:** Concurrent with Phase 2; target completion aligned with Phase 2 exit
**Target completion:** 2026-11-23

### 5.1 Objectives

Qualify the target execution platform to a level commensurate with ASIL D requirements for the Kirra safety kernel deployment. The primary target platform is QNX Neutrino RTOS for automotive-grade deployments.

### 5.2 Background

ISO 26262-6:2018 §5.4.3 requires that the operating environment (OS, RTOS, hardware) be either:
- Qualified as a Safety Element out of Context (SEooC) to the required ASIL, or
- Assessed as a pre-existing element with sufficient evidence of fitness for purpose

QNX Neutrino RTOS 7.1 is commercially available with TUV Rheinland ASIL D certification under ISO 26262. Kirra deployed on QNX can reference this existing certification.

Linux-based deployments require a separate assessment. A Linux real-time kernel with PREEMPT_RT patch and appropriate hardening may achieve ASIL B; ASIL D on Linux requires additional mitigations (partitioning, hypervisor).

### 5.3 Deliverables

| Deliverable | Description | Target |
|-------------|-------------|--------|
| Platform Assessment Report | Assessment of QNX 7.1 for Kirra deployment | Q3 2026 |
| Hardware Qualification Plan | Assessment of target ECU hardware | Q3 2026 |
| Timing Analysis | WCET analysis for enforce path on target hardware | Q4 2026 |
| Memory Footprint Analysis | Stack and heap usage for safety-critical tasks | Q4 2026 |

### 5.4 QNX-Specific Integration Requirements

- Kirra posture engine worker shall be a dedicated QNX pulse-based thread at a fixed RT priority above the command handler threads
- The telemetry watchdog shall use QNX timer_create with CLOCK_MONOTONIC to guarantee sweep interval accuracy
- Stack allocation for safety-critical threads shall be pre-allocated and guarded with stack canaries
- The DDS bridge shall use the QNX DDS (OMG DDS for Embedded, or a QNX-certified DDS implementation)

### 5.5 Worst-Case Execution Time (WCET) Requirements

| Function | Required WCET | Measurement Method |
|----------|---------------|-------------------|
| `validate_vehicle_command` | <= 1 ms | Static analysis + measurement on target hardware |
| `should_route_command` | <= 0.5 ms | Static analysis + measurement |
| `resolve_posture_with_reason` | <= 0.5 ms | Static analysis + measurement |
| Posture engine recalculation (50 nodes) | <= 10 ms | Measurement on target hardware |
| Telemetry watchdog sweep | <= 5 ms | Measurement on target hardware |

### 5.6 Key Milestones

| Milestone | Target Date |
|-----------|-------------|
| Target hardware selected | 2026-07-01 |
| QNX platform assessment initiated | 2026-07-15 |
| Kirra ported to QNX and building cleanly | 2026-08-15 |
| WCET measurement infrastructure deployed | 2026-09-01 |
| WCET measurements complete and within bounds | 2026-10-15 |
| Platform Assessment Report issued | 2026-11-01 |

### 5.7 Exit Criteria

- QNX platform assessment report issued and reviewed
- WCET measurements on target hardware within required bounds for all safety-critical functions
- Memory footprint within target ECU constraints
- All timing violations (if any) addressed with architectural mitigations

### 5.8 Dependencies

- Phase 1 complete (met)
- Target hardware procurement
- QNX license and SDK
- Ferrocene compiler availability for QNX target (or rustc cross-compilation assessment for QNX)

---

## 6. Phase 4 — Pre-Assessment

**Status:** Not started
**Estimated duration:** 6-12 months post Phase 3 completion
**Target start:** Following Phase 3 exit criteria satisfied (est. 2026-12-01)
**Target completion:** 2027-06-01 to 2027-12-01

### 6.1 Objectives

Engage a TUV-accredited third-party assessor (TUV Rheinland, TUV SUD, or equivalent) for an ISO 26262 pre-assessment. The pre-assessment identifies gaps between the current safety case and the requirements for a formal certification audit. All gaps identified in the pre-assessment are remediated before Phase 5.

### 6.2 Pre-Assessment Scope

The pre-assessment shall cover:
- HARA review: completeness of hazard identification, correctness of ASIL assignments
- Safety goal review: precision, testability, FTTI correctness
- Safety architecture review: independence of layers, fail-closed properties, completeness
- RTM review: bidirectional traceability completeness, test coverage evidence
- Coding guidelines review: alignment with MISRA C adaptation, tool qualification evidence
- Process compliance review: configuration management, change management, verification plan
- Test evidence review: MC/DC coverage reports, test execution records, defect tracking

### 6.3 Deliverables

| Deliverable | Description | Owner |
|-------------|-------------|-------|
| Pre-Assessment Brief | Summary of safety case for assessor briefing | Safety Lead |
| Assessment Package | Complete safety case document set | Safety Lead |
| Gap Analysis Report | Assessor-identified gaps and observations | Assessor |
| Gap Closure Plan | Remediation plan for each identified gap | Safety Lead |
| Gap Closure Evidence | Evidence of remediation for each gap | Engineering |
| Pre-Assessment Completion Report | Assessor confirmation of gap closure | Assessor |

### 6.4 Key Milestones

| Milestone | Target Date |
|-----------|-------------|
| Assessor engagement (RFQ, selection, contract) | 2026-12-01 |
| Pre-assessment brief delivered to assessor | 2027-01-15 |
| On-site pre-assessment review | 2027-02-15 |
| Gap Analysis Report received | 2027-03-15 |
| Gap Closure Plan approved | 2027-04-01 |
| Gap closure remediation complete | 2027-06-01 |
| Pre-Assessment Completion Report received | 2027-07-01 |

### 6.5 Exit Criteria

- Pre-Assessment Completion Report received from accredited assessor with no open blocking findings
- All critical and major gaps closed with evidence
- Minor gaps accepted with documented risk assessment and timeline for resolution before Phase 5

### 6.6 Dependencies

- Phases 2 and 3 complete with all exit criteria met
- Assessor selected and contract signed
- Assessment package complete and internally reviewed

---

## 7. Phase 5 — Formal Certification

**Status:** Not started
**Estimated duration:** Follows Phase 4 pre-assessment completion
**Target start:** Following Phase 4 exit criteria satisfied (est. 2027-07-01)
**Target completion:** 2027-12-01 to 2028-06-01

### 7.1 Objectives

Achieve ISO 26262 ASIL D certification for the Kirra Runtime Safety Kernel from a recognized accreditation body. The certification covers the software item as defined in AEGIS-HARA-001 deployed on the qualified QNX platform.

### 7.2 Certification Process

1. **Formal Assessment Initiation:** Formal assessment agreement with TUV-accredited body; definition of assessment scope, methods, and sampling strategy.
2. **Document Review Phase:** Assessor reviews complete safety case document set, process evidence, and tool qualification evidence.
3. **Source Code Review:** Assessor reviews safety-critical source code for compliance with AEGIS-CG-001 and safety goal implementation.
4. **Test Witness:** Assessor witnesses execution of the complete test suite (306+ tests) on target hardware; reviews MC/DC coverage reports.
5. **Process Audit:** Assessor audits software development process artifacts: configuration management records, change requests, review records, defect tracking.
6. **Assessment Report:** Assessor issues assessment report with findings.
7. **Certificate Issuance:** Following resolution of assessment findings, certificate of conformance to ISO 26262 ASIL D is issued for kirra-runtime-sdk.

### 7.3 Certification Maintenance

After initial certification, the following activities maintain certification validity:
- Safety impact assessment for all changes to safety-critical modules (per AEGIS-CG-001 Section 8)
- Annual review of HARA to confirm no new hazards from operational experience
- Regression test suite execution on each release
- Periodic assessment renewal per assessor agreement (typically every 3 years or on major version change)

### 7.4 Key Milestones

| Milestone | Target Date |
|-----------|-------------|
| Formal assessment agreement signed | 2027-07-15 |
| Document review phase complete | 2027-09-01 |
| Source code review complete | 2027-10-01 |
| Test witness session complete | 2027-10-15 |
| Process audit complete | 2027-11-01 |
| Assessment report received | 2027-11-15 |
| Findings resolved | 2027-12-15 |
| Certificate of conformance issued | 2028-01-15 |

### 7.5 Exit Criteria

- Certificate of conformance to ISO 26262 ASIL D issued by accredited body for `kirra-runtime-sdk` as deployed on qualified QNX platform
- No open blocking or major findings from assessment
- Certification maintenance plan established and approved

### 7.6 Dependencies

- Phase 4 complete with pre-assessment completion report
- Formal assessment contract signed
- All documentation in final (non-Draft) status

---

## 8. Phase Dependencies Summary

```
Phase 1 (Foundation)
    |  Complete: 2026-05-23
    |
    +---> Phase 2 (Process Compliance) -------+
    |     Duration: 3-6 months                 |
    |     Target: 2026-08 to 2026-11           |
    |                                          |
    +---> Phase 3 (QNX Platform) ------------>+
          Duration: concurrent with Phase 2   |
          Target: 2026-11-23                  |
                                              |
                          Phase 4 (Pre-Assessment)
                          Duration: 6-12 months post Phase 3
                          Target: 2026-12 to 2027-12
                                              |
                                              v
                          Phase 5 (Formal Certification)
                          Duration: follows Phase 4
                          Target: 2027-07 to 2028-06
```

---

## 9. Risk Register

| Risk ID | Description | Likelihood | Impact | Mitigation |
|---------|-------------|------------|--------|------------|
| R-001 | Ferrocene compiler not available for QNX target in required timeframe | Medium | High | Evaluate rustc cross-compilation assessment as backup; engage Ferrous Systems early |
| R-002 | MC/DC coverage gaps require significant new test development | Medium | Medium | Begin coverage measurement early in Phase 2; allocate dedicated test engineering capacity |
| R-003 | TUV assessor identifies HARA gaps requiring additional hazards and safety goals | Low | High | Engage assessor for informal review of HARA before formal Phase 4 |
| R-004 | WCET analysis reveals enforcement functions exceeding bounds on target hardware | Low | High | Establish WCET measurement on target hardware early in Phase 3; optimize hot paths if needed |
| R-005 | EtherNet/IP adapter introduces new ASIL D hazards not in current HARA | Medium | Medium | Complete EtherNet/IP HARA addendum in Phase 2 before Phase 4 engagement |
| R-006 | Process compliance gaps (configuration management) delay Phase 4 | Medium | Medium | Initiate configuration management tooling in first month of Phase 2 |

---

## 10. Document Control

| Field | Value |
|-------|-------|
| Prepared by | Kirra Engineering |
| Review status | Pending TUV pre-assessment |
| Next review | 2026-11-23 |
| Supersedes | None |
