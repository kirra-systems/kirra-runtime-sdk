# Aegis Safety Standards Matrix

Document ID: AEGIS-STD-001
Version: 1.0.0
Status: Draft
Date: 2026-05-23

---

## 1. Purpose

This matrix identifies 23 safety and security standards relevant to Aegis deployments across five industry verticals. For each standard, it documents Aegis applicability, current compliance status, the SEooC (Safety Element out of Context) model applicability, and the certification priority.

Priority levels:
- P0: Must have — gates primary commercial targets
- P1: High value — required for specific verticals or derivative compliance
- P2: Applicable — worth tracking; lower immediate urgency

---

## 2. Automotive Vertical

| # | Standard | Title | Priority | Aegis Scope | Current Status | Path |
|---|----------|-------|----------|-------------|----------------|------|
| 1 | ISO 26262 ASIL-D | Functional Safety — Road Vehicles | P0 | Aegis as SEooC safety governor for AV motion commands. validate_vehicle_command(), posture-gated routing, and fail-closed cache semantics map directly to ISO 26262-6 software requirements. | Safety case foundation complete (HARA, SGs, RTM, Architecture). Tool qualification and process compliance pending. | SEooC via TUV SUD. Estimated EUR 150K–250K. |
| 2 | ISO/PAS 21448 SOTIF | Safety of the Intended Functionality | P1 | SOTIF covers hazards from insufficiency of the AI planner's intended behavior (not failures). Aegis addresses SOTIF by bounding all planner outputs to the kinematic contract, converting SOTIF behavioral uncertainty into a hard enforcement boundary. | Gap analysis pending. SOTIF does not replace ISO 26262 for Aegis; it applies to the upstream AI planner. | Document Aegis as a SOTIF mitigation measure for the AI planner's triggering conditions. |
| 3 | ISO/SAE 21434 | Road Vehicles — Cybersecurity Engineering | P1 | Aegis directly implements several 21434 security controls: constant-time token comparison, HMAC-SHA256 attestation, Ed25519-signed audit chain, nonce-based replay prevention, admin token enforcement, and adversarial input detection in the Action Filter. | Informal compliance. No formal TARA (Threat Analysis and Risk Assessment) yet. | Map existing security invariants to 21434 TARA outputs. Estimated 2–4 months. |
| 4 | UN ECE WP.29 R155/R156 | Cybersecurity Management System / Software Update Management | P2 | R155 requires a CSMS covering the vehicle's supply chain. Aegis as a software component must be covered under the OEM's CSMS. R156 requires secure OTA update procedures for software on the vehicle. | Not yet addressed. Applies when Aegis is deployed on a type-approved vehicle. | Address during OEM integration. No standalone Aegis action required; document supply chain security claims. |
| 5 | AUTOSAR Adaptive | AUTOSAR Adaptive Platform (AP) — ARA Safety | P2 | AUTOSAR AP defines the execution environment for ASIL-D software on modern E/E architectures. Aegis would run as an Adaptive Application (AA) in a future automotive integration. The posture engine worker and axum HTTP interface would need to be replaced or wrapped by AUTOSAR communication middleware. | Not applicable until automotive OEM integration. | Track. No action until OEM engagement. |

---

## 3. Drones / UAS Vertical

| # | Standard | Title | Priority | Aegis Scope | Current Status | Path |
|---|----------|-------|----------|-------------|----------------|------|
| 6 | ASTM F3269 | Standard Methods for Run Time Assurance (RTA) of Autonomous and Semi-Autonomous Systems | P0 | Aegis is architecturally a Run Time Assurance monitor. The RTA Monitor = Aegis Safety Kernel; Primary Function = AI Navigation Stack; Backup Control Law = MRC Fallback Profile; Recovery Region = Degraded posture; Safe Region = Nominal posture. This standard directly describes what Aegis does. See AEGIS-F3269-001 for full mapping. | Self-mapping in progress. No test lab required — self-declaration. | Purchase standard (~$80 USD from ASTM). Complete mapping (AEGIS-F3269-001). Estimated 2 weeks. |
| 7 | DO-178C / EUROCAE ED-12C | Software Considerations in Airborne Systems and Equipment Certification | P1 | For drone deployments requiring FAA/EASA airworthiness approval. Aegis as a DAL-B or DAL-C software component would require a DO-178C-compliant development process: requirements traceability (partially done), structural coverage (MC/DC), qualified tools (Rust/Ferrocene), and independence in testing. | Partial — RTM (AEGIS-RTM-001), coding guidelines (AEGIS-CG-001), and property-based tests are DO-178C-compatible artifacts. MC/DC coverage not yet measured. | Establish structural coverage with cargo-tarpaulin or llvm-cov. Ferrocene compiler qualification applies. |
| 8 | DO-333 | Formal Methods Supplement to DO-178C | P2 | DO-333 permits formal verification as a substitute for some structural coverage requirements. The deterministic nature of validate_vehicle_command() and should_route_command() makes them amenable to formal verification. | Not started. | Evaluate post-DO-178C. Proptest property-based tests are a step toward formal argument. |
| 9 | FAA AC 21-49 / BVLOS Operations | FAA Advisory Circular for UAS Beyond Visual Line of Sight | P2 | BVLOS approvals require demonstrated system reliability and safety case evidence. Aegis's HARA, safety goals, and audit chain are directly relevant evidence artifacts. | Not started. Applicable when Aegis-governed drones seek BVLOS approval. | Provide safety case evidence package (HARA + RTM + test results) to BVLOS applicant. |

---

## 4. Robotics Vertical

| # | Standard | Title | Priority | Aegis Scope | Current Status | Path |
|---|----------|-------|----------|-------------|----------------|------|
| 10 | ISO 10218-1/2 | Robots and Robotic Devices — Safety Requirements for Industrial Robots | P1 | Aegis governs motion commands to robot actuators and enforces the kinematic contract (max speed, steering rate). The MRC fallback profile and fail-closed posture semantics align with ISO 10218 protective stop and speed/separation monitoring requirements. The ROS2 interlock (ros2_ws/src/aegis_safety/) is the integration point. | Informal compliance. ROS2 interlock correctly implements emergency stop on LockedOut transition. | Map Aegis safety functions to ISO 10218 risk assessment outcomes. 1–2 months. |
| 11 | ISO/TS 15066 | Robots and Robotic Devices — Collaborative Robots | P1 | For collaborative robot deployments where Aegis governs a robot operating near humans. ISO/TS 15066 defines four collaboration modes (safety-rated monitoring stop, hand-guiding, speed/separation, power/force limiting). Aegis Degraded posture maps to speed/separation mode speed limits. | Speed limit in MRC profile (5.0 m/s, robot: 1.8 * 0.3 = 0.54 m/s) is conservative but needs human proximity context. | Document Aegis MRC profile as speed/separation monitoring speed limit. |
| 12 | IEC 62061 | Safety of Machinery — Functional Safety of Safety-related Control Systems | P1 | IEC 62061 applies to safety-related electrical control systems on machinery. SIL requirements under IEC 62061 are derivative of IEC 61508 and directly applicable to Aegis. An Aegis-governed industrial robot's safety functions (motion stop, speed limiting) are SIL-claimable under 62061. | IEC 61508 mapping (AEGIS-61508-001) provides the foundation. | Apply IEC 61508 SIL 3 claim to 62061 derivatively. |
| 13 | EN ISO 13849-1 | Safety of Machinery — Safety-Related Parts of Control Systems | P2 | Alternative to IEC 62061 for machinery safety. Uses Performance Level (PL a–e) rather than SIL. PLe maps approximately to SIL 3. Applicable for European CE marking. | Not started. Lower priority than IEC 61508 path. | Address post-IEC 61508 via derivative mapping. |

---

## 5. Industrial Infrastructure Vertical

| # | Standard | Title | Priority | Aegis Scope | Current Status | Path |
|---|----------|-------|----------|-------------|----------------|------|
| 14 | IEC 61508 SIL 3 | Functional Safety of Electrical / Electronic / Programmable Electronic Safety-related Systems | P0 | Broadest leverage standard — ISO 26262, IEC 61511, IEC 62061, and IEC 62443 all derive from or reference 61508. A SIL 3 SEooC certificate from Exida or TUV makes Aegis applicable across all industrial verticals simultaneously. The three core safety functions (velocity clamping, posture-gated routing, unknown command denial) are SIL 3 candidates. See AEGIS-61508-001 for preliminary mapping. | Preliminary mapping complete (AEGIS-61508-001). No formal assessment yet. | Exida scoping call for SIL 3 SEooC. Estimated EUR 50K–150K depending on scope. |
| 15 | IEC 61511 | Functional Safety — Safety Instrumented Systems for the Process Industry Sector | P1 | IEC 61511 governs Safety Instrumented Functions (SIFs) in process plants (oil/gas, chemical, water). Aegis governing industrial controllers via the DNP3/EtherNet/IP/CANOpen adapters is a SIF boundary enforcement layer. IEC 61511 is derivative of IEC 61508 — the SIL 3 claim covers it. | The DNP3 adapter (src/adapters/dnp3.rs) audits broadcast commands and detects critical infrastructure groups. | Document after IEC 61508 SIL 3 is achieved. |
| 16 | IEC 62443 | Industrial Automation and Control Systems (IACS) Security | P1 | IEC 62443 defines security levels (SL 1–4) for industrial systems. Aegis's security model (constant-time token comparison, HMAC attestation, Ed25519 signing, nonce-based replay prevention, admin token enforcement) maps to SL 2–3 requirements. | Informal compliance with multiple 62443 controls. No formal assessment. | Document Aegis security invariants against 62443-3-3 system requirements. 1–2 months. |
| 17 | IEC 61131-3 | Programmable Controllers — Programming Languages | P2 | IEC 61131-3 defines PLC programming languages. Relevant only if Aegis is deployed as a software component within a PLC runtime. Not currently applicable. | Not applicable. | Track for future PLC integration scenarios. |
| 18 | NERC CIP | Critical Infrastructure Protection Standards | P2 | NERC CIP applies to bulk electric systems. If Aegis governs systems connected to grid infrastructure (substations, generation control), NERC CIP requirements apply to the asset owner, not to Aegis as a software component. | Not applicable directly. | Provide NERC CIP compliance documentation to integrators on request. |

---

## 6. Cross-Cutting Standards

| # | Standard | Title | Priority | Aegis Scope | Current Status | Path |
|---|----------|-------|----------|-------------|----------------|------|
| 19 | IEC 62304 | Medical Device Software — Software Life Cycle Processes | P2 | IEC 62304 defines software development processes for medical devices. Methodology (SOUP management, unit testing, traceability, configuration management) is directly applicable to Aegis as a model for process compliance regardless of medical deployment. | Not targeted. Referenced for process methodology. | Use IEC 62304 Class C (safety-critical software) as process model for Phase 2 compliance. |
| 20 | MISRA C:2012 / Ferrocene LS | MISRA C Guidelines adapted for Rust via Ferrocene Language Specification | P1 | AEGIS-CG-001 (Rust Safety Coding Guidelines) is explicitly based on MISRA C:2012 principles adapted for Rust and the Ferrocene Language Specification. Ferrocene is the ISO 26262-qualified Rust compiler toolchain. | Coding guidelines adopted (AEGIS-CG-001). Ferrocene compiler not yet in use — currently using standard rustc. | Switch to Ferrocene compiler for ASIL-D builds. Required for ISO 26262 tool qualification. |
| 21 | ISO/IEC 25010 | Systems and Software Quality Models | P2 | Defines software quality characteristics: functional suitability, reliability, security, maintainability, safety. Useful as a quality framework for Aegis but not a certification target. | Not targeted. | Reference for quality attribute definitions. |
| 22 | NIST SP 800-82 Rev 3 | Guide to Operational Technology Security | P2 | NIST 800-82 provides security guidance for industrial control systems. Relevant when Aegis is deployed in OT environments (power, water, manufacturing). The admin token enforcement, attestation, and audit chain directly implement 800-82 access control and audit requirements. | Informal compliance with relevant controls. | Document alignment with NIST 800-82 controls for US government and critical infrastructure customers. |
| 23 | ISO/IEC 27001 | Information Security Management Systems | P2 | ISO 27001 ISMS is a process standard for managing information security. Relevant at the organizational level, not the software component level. Applicable when Aegis is commercialized and requires supply chain security attestation. | Not targeted. | Address at commercial entity formation stage. |

---

## 7. Summary: Priority Matrix

| Priority | Standards | Count | Action |
|----------|-----------|-------|--------|
| P0 | ISO 26262 ASIL-D (#1), ASTM F3269 (#6), IEC 61508 SIL 3 (#14) | 3 | Active certification tracks |
| P1 | ISO/SAE 21434 (#3), DO-178C/ED-12C (#7), ISO 10218-1/2 (#10), ISO/TS 15066 (#11), IEC 62061 (#12), IEC 61511 (#15), IEC 62443 (#16), MISRA C/Ferrocene (#20) | 8 | Derivative from P0 or vertical-specific |
| P2 | UN ECE WP.29 (#4), AUTOSAR AP (#5), DO-333 (#8), FAA BVLOS (#9), EN ISO 13849 (#13), IEC 61131-3 (#17), NERC CIP (#18), IEC 62304 (#19), ISO/IEC 25010 (#21), NIST 800-82 (#22), ISO 27001 (#23) | 12 | Track; address when needed |

---

## 8. SEooC Architecture Summary

The Safety Element out of Context (SEooC) model allows Aegis to be certified once and deployed into many different vehicles, robots, and industrial systems. Under the SEooC model:

1. **Aegis Engineering** certifies the Aegis software component to the required ASIL/SIL level with assumed safety requirements (ASRs) documented in the safety case.
2. **Integrators** perform in-context verification: they confirm that the ASRs are satisfied in their specific deployment (vehicle type, sensor configuration, network topology, failure mode analysis).
3. **Per-deployment cost** is substantially lower than certifying the full item each time.

The SEooC assumption set for Aegis includes:
- The upstream AI planner or navigation stack sends commands in the ProposedVehicleCommand JSON schema
- The deployment provides AEGIS_ADMIN_TOKEN and AEGIS_SUPERVISOR_RESET_KEY as non-empty env vars
- The kinematic contract parameters (max_speed_mps, max_lateral_accel_mps2, wheelbase_m) are correctly configured for the specific vehicle or robot platform
- The deployment platform provides monotonic clock access
- The SQLite database path is on durable storage with adequate write throughput

---

## 9. Document Control

| Field | Value |
|-------|-------|
| Prepared by | Aegis Engineering |
| Next review | 2026-11-23 |
| Supersedes | None |
