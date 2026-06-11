# Kirra Safety Kernel — Safety Case Index

Document ID: AEGIS-SC-000
Version: 1.0.0
Status: Draft
Classification: ISO 26262 Part 2 / GSN (Goal Structuring Notation)
Date: 2026-05-23

---

## 1. Document Registry

| Doc ID | Title | Version | Status | File | Last Updated |
|--------|-------|---------|--------|------|--------------|
| AEGIS-SC-000 | Safety Case Index | 1.0.0 | Draft | docs/safety/SAFETY_CASE_INDEX.md | 2026-05-23 |
| AEGIS-HARA-001 | Hazard Analysis and Risk Assessment | 1.0.0 | Draft | docs/safety/HARA.md | 2026-05-23 |
| AEGIS-SG-001 | Safety Goals | 1.0.0 | Draft | docs/safety/SAFETY_GOALS.md | 2026-05-23 |
| AEGIS-SA-001 | Safety Architecture | 1.0.0 | Draft | docs/safety/SAFETY_ARCHITECTURE.md | 2026-05-23 |
| AEGIS-RTM-001 | Requirements Traceability Matrix | 1.0.0 | Draft | docs/safety/REQUIREMENTS_TRACEABILITY.md | 2026-05-23 |
| AEGIS-CG-001 | Rust Safety Coding Guidelines | 1.0.0 | Draft | docs/safety/CODING_GUIDELINES.md | 2026-05-23 |
| AEGIS-ROAD-001 | ASIL-D Certification Roadmap | 1.0.0 | Draft | docs/safety/ROADMAP_TO_ASIL_D.md | 2026-05-23 |
| AEGIS-STD-001 | Safety Standards Matrix | 1.0.0 | Draft | docs/safety/STANDARDS_MATRIX.md | 2026-05-23 |
| AEGIS-F3269-001 | ASTM F3269 Run Time Assurance Mapping | 1.0.0 | Draft | docs/safety/ASTM_F3269_MAPPING.md | 2026-05-23 |
| KIRRA-RTA-001 | ASTM F3269-21 Bounded Operation Mapping (current) | 1.0 | Draft | docs/safety/ASTM_F3269_RTA_MAPPING.md | 2026-05-29 |
| AEGIS-61508-001 | IEC 61508 SIL 3 Preliminary Claim Mapping | 1.0.0 | Draft | docs/safety/IEC_61508_MAPPING.md | 2026-05-23 |
| KIRRA-SIL3-001 | IEC 61508 SIL 3 Requirements Mapping (current) | 1.0 | Draft | docs/safety/IEC_61508_SIL3_MAPPING.md | 2026-05-29 |
| KIRRA-REV-001 | External Security/Safety Review Wrap-Up | 1.0 | Final | docs/safety/REVIEW_WRAPUP_2026-05-30.md | 2026-05-30 |
| KIRRA-CERT006-DIVERSITY-001 | Governor Comparator Diversity Argument (CERT-006; structural/algorithmic diverse shadow + honest limits) | 0.1 | Draft — pending review | docs/safety/COMPARATOR_DIVERSITY.md | 2026-06-01 |
| KIRRA-OCCY-SG-001 | Occy Safety Goals (HARA + STPA derivation for Occy planner item; complements AEGIS-SG-001) | 0.1 | Draft | docs/safety/OCCY_SAFETY_GOALS.md | 2026-05-30 |
| KIRRA-OCCY-ODD-001 | Occy ODD + SOTIF triggering-condition catalog (ISO 21448) | 0.1 | Draft | docs/safety/OCCY_SOTIF.md | 2026-05-31 |
| KIRRA-OCCY-SPEED-001 | Occy speed-envelope analysis (SSD / breaking-point / derate) | 0.1 | Draft | docs/safety/SPEED_ENVELOPE.md | 2026-05-31 |
| KIRRA-OCCY-ADR-001 | ADR-0001: Occy ODD speed cap = 50 mph / 80 km/h | 1.0 | Accepted | docs/adr/0001-occy-odd-speed-cap.md | 2026-05-31 |
| KIRRA-OCCY-DFA-001 | Occy ASIL decomposition + Dependent Failure Analysis | 0.1 | Draft | docs/safety/OCCY_DFA.md | 2026-05-31 |
| KIRRA-OCCY-IDC-001 | Occy focused Independent Detection Channel (IDC) design | 0.1 | Draft | docs/safety/OCCY_INDEPENDENT_DETECTOR.md | 2026-05-31 |
| KIRRA-OCCY-ADR-002 | ADR-0002: Condition-dependent speed cap + sub-ODD partition | 1.0 | Accepted | docs/adr/0002-condition-dependent-cap-subodds.md | 2026-05-31 |
| KIRRA-OCCY-ARCH-001 | Occy two-tier architecture (base Governor SEooC + optional D1 add-on) | 0.1 | Draft | docs/safety/OCCY_ARCHITECTURE_TIERS.md | 2026-05-31 |
| KIRRA-OCCY-ADR-003 | ADR-0003: Two-tier KIRRA architecture — base + optional D1 | 1.0 | Accepted | docs/adr/0003-two-tier-base-and-d1-addon.md | 2026-05-31 |
| KIRRA-OCCY-ADR-004 | ADR-0004: Independent Safety Channel — D1–D3 settlement | 1.0 | Superseded by ADR-0003 | docs/adr/0004-independent-safety-channel.md | 2026-05-31 |
| KIRRA-OCCY-INTEG-001 | Occy Governor integrity evidence plan (S3 — WCET / MC/DC / traceability / FFI / Ferrocene / safety manual) | 0.1 | Draft | docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md | 2026-05-31 |
| KIRRA-OCCY-FAULT-001 | Occy Governor fault model + degraded-mode availability (S7) | 0.1 | Draft | docs/safety/OCCY_FAULT_MODEL.md | 2026-05-31 |
| KIRRA-OCCY-TRACE-001 | Occy Safety Traceability Convention (tag format + CI gate spec) | 0.1 | Draft | docs/safety/TRACEABILITY.md | 2026-05-31 |
| KIRRA-OCCY-TRACE-MATRIX-001 | Occy Safety Traceability Matrix (auto-generated from `// SAFETY:` tags) | 0.1 | Auto-generated | docs/safety/TRACEABILITY_MATRIX.md | 2026-05-31 |
| KIRRA-OCCY-MANUAL-001 | KIRRA Governor Safety Manual (SEooC consolidated deliverable for integrators/assessors) | 0.1 | Draft | docs/safety/GOVERNOR_SAFETY_MANUAL.md | 2026-05-31 |
| KIRRA-OCCY-FFI-001 | Occy Freedom From Interference (FFI) evidence — spatial / temporal / communication isolation | 0.1 | Draft | docs/safety/OCCY_FFI_EVIDENCE.md | 2026-05-31 |
| KIRRA-OCCY-MCDC-001 | Occy MC/DC coverage evidence (pair-completing tests + branch-coverage fallback measurement) | 0.1 | Draft | docs/safety/OCCY_MCDC_EVIDENCE.md | 2026-05-31 |
| KIRRA-OCCY-OPTIONB-001 | Occy #131 Option-B per-trajectory wiring on Autoware (two-rate check; SG2 live; ROS 2 adapter) | 0.1 | Draft | docs/safety/OCCY_131_OPTIONB_DESIGN.md | 2026-05-31 |
| KIRRA-OCCY-SG2-MARGIN-001 | Occy SG2 lateral margin derivation (S8 #120 Item A; PRIMARY 0.40 m + 0.75 m fallback; G2 AoU #123) | 0.1 | Draft | docs/safety/OCCY_SG2_MARGIN.md | 2026-05-31 |
| KIRRA-OCCY-SPEED-VAL-001 | Occy speed-cap validation matrix (S8 #120 Item C; PROVEN/OK-ANALYTICAL/AoU-GAP dispositions for each ADR-0001 assumption; cap unchanged at 50 mph) | 0.1 | Draft | docs/safety/OCCY_SPEED_CAP_VALIDATION.md | 2026-05-31 |
| KIRRA-OCCY-IDC-RANGES-001 | Occy D1 IDC detection-range specification (S8 #120 Item B; per-sensor spec table + SSD-derate cap-impact + vendor-RFP requirements; closes Item C AoU rows 1+4 in the D1 tier) | 0.1 | Draft | docs/safety/OCCY_IDC_DETECTION_RANGES.md | 2026-05-31 |
| KIRRA-OCCY-QUANT-001 | Occy quantitative HW safety metrics (S8 #120 Item D; SPFM/LFM/PMHF target-vs-claimed across 5 sub-elements; single-supply PMHF 17.7 FIT FAIL, dual-supply 8.7 FIT PASS; deployment requirement: ASIL-D-class redundant supply) | 0.1 | Draft | docs/safety/OCCY_QUANTITATIVE_METRICS.md | 2026-05-31 |
| KIRRA-TR5469-001 | ISO/IEC TR 5469 AI-functional-safety alignment (Kirra as the usage-class-2 non-AI safety function over AI-controlled equipment; align/cite — NOT a certification target, TR = guidance) | 0.1 | Draft | docs/safety/ISO_IEC_TR_5469_MAPPING.md | 2026-06-03 |
| KIRRA-OCCY-AOU-001 | Assumptions of Use register (SEooC AoU / SRAC; cross-cutting deployment-precondition assumptions). Entries: AOU-PERCEPTION-FRAME-001 (object velocity absolute map/world-frame; OPEN pre-enable gate for KIRRA_PERCEPTION_DERATE_ENABLED); AOU-MSG-TOOLCHAIN-001 (full-message-set codegen — **SUPERSEDED** 2026-06-05 option C, discharged for TOPO-1 via the curated interface / KIRRA-OCCY-MSGSYNC-001); AOU-MSG-TOOLCHAIN-002 (co-resident-with-full-Autoware r2r codegen residual; OPEN); AOU-PERCEPTION-RANGE-001 + AOU-PERCEPTION-CLASS-001 + AOU-VEHICLE-FRICTION-001 (#126 Perception Input Contract — detection range / worst-case class / road friction; AoU-GAP/OK-ANALYTICAL); AOU-ACTUATION-LATENCY-001 (#127 actuation safe-stop ≤ 499 ms + loss-of-verdict MRC; OK-PROVEN Governor + AoU-GAP residual); AOU-HW-POWER-001 (DR-1) + AOU-HW-COMMBUS-001 (DR-2) (#127 hardware PMHF/LFM deployment gates); AOU-LOCALIZATION-001 (#123 G2 localization ≤ 0.10 m 95th-pct lateral error, else 0.75 m fallback; AoU-GAP base, runtime gate PR #264); AOU-CLEARANCE-AUTH-001 (#103 SG6 clearance grants issued only after operator authentication — parko enforces structure not identity; AoU by design, structural loop live PR #267) | 0.1 | Draft | docs/safety/ASSUMPTIONS_OF_USE.md | 2026-06-11 |
| KIRRA-OCCY-DEPLOY-001 | Pacifica AV-pilot deployment architecture (roadmap record, NOT a build commitment; Pacifica Hybrid + Dataspeed DBW + BELIV Autoware bridge + Jetson Orin; Kirra at the command gateway on the existing kirra-ros2-adapter; solved-vs-build split; layered safety; surfaced AoUs incl. live radar precondition + proposed vehicle-interface-timing AoU; sim→bench→vehicle validation ladder) | 0.1 | Draft | docs/safety/PACIFICA_PILOT_ARCHITECTURE.md | 2026-06-04 |
| KIRRA-OCCY-PMON-004 | Perception-derate validation gate, SIM tier (operationalizes PMON-003 §D4; mechanism-first split — sub-gate 1 synthetic-publisher mechanism+freshness, sub-gate 2 AWSIM frame deferred; scenarios (a)-(e) with confirmed step-table cap values; earns KIRRA_PERCEPTION_DERATE_ENABLED in sim only). §8 Execution Record (2026-06-04, ROS 2 Jazzy): sub-gate-1 Layer-2 decode boundary PASS; full node tick + sub-gate 2 pending; AOU-PERCEPTION-FRAME-001 OPEN, flag OFF; env constraints incl. AOU-MSG-TOOLCHAIN-001 | 0.1 | Draft | docs/safety/OCCY_PERCEPTION_DERATE_VALIDATION_GATE.md | 2026-06-04 |
| KIRRA-OCCY-MSGSYNC-001 | Curated Autoware interface version-sync SRAC (real package names + verbatim/byte-identical message closures = identical RIHS hash; pinned reference Jazzy 1.11.0-1noble.20260412; re-verify on any Autoware version change; TOPO-1 interface-isolation + TOPO-2 per-target preconditions). **SUPERSEDES** AOU-MSG-TOOLCHAIN-001 (owner decision 2026-06-05, option C). Phase 2 DONE on bench: verify_hashes.sh PASS (8/8 byte-identical) + `--features ros2` build/test green against curated-only; per-target re-verify OPEN; residual → AOU-MSG-TOOLCHAIN-002 | 0.1 | Draft | docs/safety/MSG_INTERFACE_VERSION_SYNC.md | 2026-06-05 |
| KIRRA-OCCY-UL4600-001 | UL 4600 safety case (GSN "absence of unreasonable risk" top claim consuming AEGIS-SC-000 as a solution node; S2 decomposition+DFA + S3 Governor integrity as evidence) + Safety Performance Indicators (leading/lagging, sourced from the tamper-evident audit chain, tagged EMITTED/DERIVED/GAP) + assurance-case monitoring plan. Cross-cutting (#117) | 0.1 | Draft — pending review | docs/safety/UL4600_SAFETY_CASE.md | 2026-06-10 |
| KIRRA-OCCY-TARA-001 | Cybersecurity TARA (ISO/SAE 21434) — item/perimeter, asset register (C/I/A/authenticity → on-main artifact), threat scenarios, SFOP impact with SG-NNN + UL4600 safety cross-links, attack-potential feasibility, risk→treatment→cybersecurity-goal→control (each tagged IMPLEMENTED/PARTIAL/GAP, re-verified against main), honest residual-risk/gap section (PCR16, rate-limiting, TLS, RBAC granularity, rotation policy, supply-chain). Consumes SECURITY.md / SECURITY_BOUNDARIES.md / v1_security_invariants.md. Cross-cutting (#118) | 0.1 | Draft — pending review | docs/safety/OCCY_TARA.md | 2026-06-10 |
| KIRRA-OCCY-ANGULAR-SOTIF-001 | Angular-velocity bound SOTIF derivation (#136; H1 follow-up). Replaces the deleted placeholder constants with a velocity-dependent ω_max(v) = min(rollover(v), sweep, ftti) from platform params; implemented in `parko/crates/parko-kirra/src/angular_bound.rs` (`AngularVelocityBound::omega_max`), enforced on the parko path. Registration of the existing derivation — values are DRAFT pending formal safety-engineer review (integrator override via `with_angular_bounds`) | 0.1 | Draft — pending review | docs/safety/ANGULAR_VELOCITY_SOTIF.md | 2026-06-10 |
| KIRRA-OCCY-HVCHAN-001 | Hypervisor contract-channel layout + trust-chain spec (#278 design half; the spec ADR-0006 Clause 2 promises). Frozen, versioned, fixed-size `#[repr(C)]` pointer-free `GovernorContractView` over hypervisor shared memory; the normative 7-step write/read trust chain (seqlock snapshot → bounds→CRC→judge → digest → Ed25519-signed release token → actuator verify-before-release); fail-closed failure-semantics table with #279 barrier-layer attribution; hypervisor-config requirements. DRAFT — design-intent until #274/#278 hardware measures it; human safety-engineer review is the gate | 0.1 | Draft — pending review | docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md | 2026-06-11 |

---

## 2. Safety Case Argument Structure

The Kirra safety case is structured using the Goal Structuring Notation (GSN) as defined in GSN Community Standard v3 and as recommended by ISO 26262-2:2018 Annex B. The top-level safety claim and supporting argument are described below.

### 2.1 Top-Level Safety Claim

**G-TOP:** The Kirra Runtime Safety Kernel (kirra-runtime-sdk v1.5.0) is sufficiently safe for use as a real-time command enforcement layer in autonomous vehicle, robot, drone, and industrial control deployments operating under ISO 26262 Part 3 item definition AEGIS-HARA-001.

### 2.2 Context

**C-01:** The Kirra item is as defined in AEGIS-HARA-001 Section 1 (Item Definition): the `kirra-runtime-sdk` crate, the `kirra_verifier_service` binary, the ROS2 safety interlock nodes, the industrial protocol adapters, and the multi-asset safety fabric.

**C-02:** "Sufficiently safe" means that all 17 hazards identified in AEGIS-HARA-001 Section 3 are mitigated to their required ASIL level through the implementation of safety goals SG-001 through SG-016 in AEGIS-SG-001.

**C-03:** The operational context is as defined by operational situations OS1 through OS13 in AEGIS-HARA-001 Section 2.

### 2.3 Strategy

**S-01:** Argument over the satisfaction of all 16 safety goals (SG-001 to SG-016), with evidence of implementation and test verification for each goal.

**S-02:** Argument by decomposition: the safety case is divided into three sub-arguments corresponding to the three enforcement layers of the safety architecture (AEGIS-SA-001):
- Sub-argument SA-L1: Trust Graph Layer correctly derives per-node trust state
- Sub-argument SA-L2: Posture Derivation Layer correctly derives fleet posture and fails closed on staleness
- Sub-argument SA-L3: Enforcement Layer correctly applies kinematic envelope and posture-gated routing

### 2.4 Sub-Argument: Trust Graph Layer (SA-L1)

**G-L1:** The Trust Graph Layer correctly maintains per-node trust state and transitions nodes to Untrusted within FTTI when sensor telemetry is absent or when fault conditions are detected.

Supporting evidence:
- SG-003 (ASIL D): Telemetry watchdog timeout detection (TR-003, TR-003a, TR-003b)
- SG-007 (ASIL D): DAG-based trust propagation to dependent nodes (TR-007, TR-007a)
- Test evidence: `test_watchdog_marks_node_untrusted_after_timeout`, DAG traversal unit tests

**G-L1-1:** The gray/black DAG traversal algorithm correctly identifies all dependency-based trust failures including cycles and depth violations (SG-003, SG-007).
**G-L1-2:** The telemetry watchdog detects sensor silence within AV_TELEMETRY_TIMEOUT_MS + AV_WATCHDOG_SWEEP_MS = 2100 ms (SG-003).

### 2.5 Sub-Argument: Posture Derivation Layer (SA-L2)

**G-L2:** The Posture Derivation Layer correctly aggregates trust states into fleet posture and fails closed (LockedOut) when the posture cache becomes stale.

Supporting evidence:
- SG-005 (ASIL D): PostureCacheStale fail-closed on TTL expiry (TR-005, TR-005a)
- Test evidence: `test_stale_cache_fails_closed_after_virtual_clock_advance`

**G-L2-1:** `resolve_posture_with_reason` returns LockedOut(PostureCacheStale) for any posture cache age >= POSTURE_CACHE_TTL_MS = 5000 ms (SG-005).
**G-L2-2:** The posture engine worker coalesces burst recalculation triggers, preventing thundering herd during multi-sensor fault events.

### 2.6 Sub-Argument: Enforcement Layer (SA-L3)

**G-L3:** The Enforcement Layer correctly applies kinematic envelope validation and posture-gated command routing, preventing all out-of-envelope commands and correctly routing commands based on current fleet posture.

Supporting evidence:
- SG-001 (ASIL D): Velocity clamp Priority 2 in validate_vehicle_command (TR-001, TR-001a)
- SG-002 (ASIL D): Lateral acceleration clamp Priority 6 in validate_vehicle_command (TR-002, TR-002a)
- SG-004 (ASIL C): NaN/Inf guard Priority 0 in validate_vehicle_command (TR-004)
- SG-006 (ASIL D): Unknown command denied before posture check in should_route_command (TR-006)
- Test evidence: proptest suite (306 passing), unit tests for each kinematic check

**G-L3-1:** `validate_vehicle_command` Priority 0 (NaN/Inf guard) executes before all other checks (SG-004).
**G-L3-2:** `validate_vehicle_command` Priority 2 (velocity clamp) executes before the rate-of-change limiter (SG-001, INV-08).
**G-L3-3:** `should_route_command` denies OperationalCommand::Unknown before posture evaluation in all posture states (SG-006, INV-09).

### 2.7 Undeveloped Claims (To Be Developed Before Certification)

The following claims are identified but not yet fully supported by evidence. They represent gaps to be addressed in the certification roadmap (AEGIS-ROAD-001):

**G-PROCESS:** The Kirra software was developed in accordance with a qualified ISO 26262-compliant process, including: qualified tools, configuration management, change management, and systematic verification.

Status: Process documentation in progress. See AEGIS-ROAD-001 Phase 2.

**G-PLATFORM:** The platform on which Kirra executes (OS, hardware, compiler) is qualified to a level commensurate with ASIL D.

Status: Ferrocene compiler qualification in progress. QNX platform assessment pending. See AEGIS-ROAD-001 Phase 3.

**G-COVERAGE:** The test suite achieves the structural coverage requirements (MC/DC) for all ASIL D safety goals.

Status: Coverage measurement infrastructure to be established. See AEGIS-ROAD-001 Phase 2.

---

## 3. Document Interdependencies

```
AEGIS-HARA-001 (Hazards)
      |
      v
AEGIS-SG-001 (Safety Goals)
      |
      +---> AEGIS-SA-001 (Architecture — how goals are implemented)
      |
      +---> AEGIS-RTM-001 (Traceability — goals to code to tests)
      |
      +---> AEGIS-CG-001 (Coding Guidelines — how to preserve goals in code)
      |
      +---> AEGIS-ROAD-001 (Roadmap — what remains before certification)
      +---> AEGIS-STD-001 (Standards Matrix — what to certify against)
            |
            +---> AEGIS-F3269-001 (ASTM F3269 RTA mapping)
            +---> AEGIS-61508-001 (IEC 61508 SIL 3 mapping)
```

---

## 4. Open Issues

| Issue ID | Description | Owner | Target |
|----------|-------------|-------|--------|
| ISSUE-001 | EtherNet/IP adapter (src/adapters/ethernet_ip.rs) not covered in HARA | Safety Lead | Phase 2 |
| ISSUE-002 | ROS2 interlock node safety requirements not traced to SGs | Safety Lead | Phase 2 |
| ISSUE-003 | MC/DC coverage measurement not yet established | Test Lead | Phase 2 |
| ISSUE-004 | Ferrocene compiler qualification not yet initiated | Platform Lead | Phase 3 |
| ISSUE-005 | Process documentation (configuration management, change management) not yet started | Process Lead | Phase 2 |
| ISSUE-006 | TUV pre-assessment not yet scheduled | Program Manager | Phase 4 |

---

## 5. Document Control

| Field | Value |
|-------|-------|
| Prepared by | Kirra Engineering |
| Review status | Pending TUV pre-assessment |
| Next review | 2026-11-23 |
| Supersedes | None |
