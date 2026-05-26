# Aegis Safety Kernel — Hazard Analysis and Risk Assessment (HARA)

Document ID: AEGIS-HARA-001
Version: 1.0.0
Status: Draft
Classification: ISO 26262 Part 3
Date: 2026-05-23

---

## 1. Item Definition

**Item name:** Aegis Runtime Safety Kernel
**Item version:** 1.5.0
**Item description:** Aegis is a runtime safety enforcement layer that intercepts proposed actuator commands from AI planners, autonomous navigation stacks, and LLM-based controllers, and enforces hard physical constraints before commands reach vehicle actuators, robot motors, drone flight controllers, or industrial machinery. Aegis operates as a fail-closed middleware process between the AI planning layer and the physical actuator interface.

**Scope:** This HARA covers the Aegis software item deployed as:
- An autonomous vehicle motion command governor (AV/UGV)
- A robot kinematic safety enforcer (ROS2/mecanum platforms)
- A drone command gating layer
- An industrial controller safety wrapper (CANOpen, DNP3, EtherNet/IP, Modbus, OPC-UA)
- A multi-asset fleet safety fabric

**Item boundary:**
- In scope: All software within the `aegis-runtime-sdk` crate, the `aegis_verifier_service` binary, the ROS2 safety interlock nodes, the industrial protocol adapters, and the multi-asset safety fabric
- Out of scope: The AI planner generating commands, the physical actuator hardware, the vehicle chassis, sensor hardware

---

## 2. Operational Situations

| ID | Description |
|----|-------------|
| OS1 | Normal autonomous operation — all sensors nominal, fleet posture Nominal |
| OS2 | Degraded sensor operation — one or more sensors faulted, fleet posture Degraded |
| OS3 | Multiple sensor failure — two or more sensors faulted simultaneously |
| OS4 | AI planner producing out-of-envelope commands (overspeed, excessive steering, NaN/Inf) |
| OS5 | Communication loss between AI planner and Aegis |
| OS6 | Aegis process failure or crash (primary instance) |
| OS7 | Database corruption or audit chain failure |
| OS8 | Network partition in HA deployment (primary-standby) |
| OS9 | Clock skew in distributed deployment |
| OS10 | Deliberate adversarial input — prompt injection via LLM or forged industrial message |
| OS11 | Recovery attempt after sensor fault (hysteresis evaluation period) |
| OS12 | Multi-asset fabric cross-asset trust propagation during partial fleet lockout |
| OS13 | Industrial protocol broadcast command (DNP3, CANOpen NMT) during degraded posture |

---

## 3. Hazard Identification and Classification

Severity, Exposure, and Controllability are rated per ISO 26262-3:

**Severity:** S0 (no injury) / S1 (minor, reversible) / S2 (severe, reversible) / S3 (life-threatening, fatal)
**Exposure:** E0 (never) / E1 (very low) / E2 (low) / E3 (medium) / E4 (high, significant portion of operating time)
**Controllability:** C0 (always controllable) / C1 (easily controllable) / C2 (normally controllable) / C3 (difficult to control)

**ASIL determination:** Per ISO 26262 Table 4 (Annex B):

| S / E / C | C0 | C1 | C2 | C3 |
|-----------|----|----|----|----|
| S1-E1     | QM | QM | QM | QM |
| S1-E2     | QM | QM | QM | A  |
| S1-E3     | QM | QM | A  | B  |
| S1-E4     | QM | A  | B  | B  |
| S2-E1     | QM | QM | QM | A  |
| S2-E2     | QM | QM | A  | B  |
| S2-E3     | QM | A  | B  | C  |
| S2-E4     | A  | B  | C  | D  |
| S3-E1     | QM | QM | A  | B  |
| S3-E2     | QM | A  | B  | C  |
| S3-E3     | A  | B  | C  | D  |
| S3-E4     | B  | C  | D  | D  |

---

### Hazard Table

| Hazard ID | Description | Operational Situation | Potential Harm | S | E | C | ASIL | Safety Goal |
|-----------|-------------|----------------------|----------------|---|---|---|------|-------------|
| H-001 | Aegis passes a motion command with linear_velocity_mps exceeding the active contract's max_speed_mps | OS1, OS4 | Vehicle exceeds safe speed; collision with pedestrians, infrastructure, or other vehicles | S3 | E4 | C3 | D | SG-001 |
| H-002 | Aegis passes a command implying lateral acceleration exceeding max_lateral_accel_mps2, computed by bicycle model | OS1, OS4 | Vehicle rollover or loss of traction causing collision | S3 | E4 | C2 | D | SG-002 |
| H-003 | Aegis fails to detect sensor node trust state transition within AV_TELEMETRY_TIMEOUT_MS; fleet posture remains Nominal when it should be Degraded | OS2, OS3 | AI planner operates at full speed/maneuverability with degraded sensor input; collision risk | S3 | E3 | C3 | D | SG-003 |
| H-004 | Stale posture cache (age >= POSTURE_CACHE_TTL_MS) causes MRC profile not applied during sensor degradation | OS2, OS9 | Commands evaluated against Nominal contract when Degraded contract should be active | S2 | E4 | C3 | D | SG-005 |
| H-005 | NaN or Inf in motion command field passes through enforcement without detection | OS4, OS10 | Undefined arithmetic behavior in downstream actuator; uncontrolled motion | S3 | E2 | C3 | C | SG-004 |
| H-006 | Aegis process crashes; no command gating occurs and commands pass directly to actuator | OS6 | Unfiltered AI planner commands reach actuators without kinematic enforcement | S3 | E3 | C2 | D | SG-008 |
| H-007 | PassiveStandby instance fails to promote when primary crashes within PROMOTION_TIMEOUT_MS | OS6, OS8 | Gap in enforcement coverage during failover window | S2 | E2 | C3 | B | SG-009 |
| H-008 | SHA-256 audit chain tampered; safety violations undetected in post-incident analysis | OS7, OS10 | Inability to reconstruct events leading to incident; regulatory and legal exposure | S1 | E4 | C3 | B | SG-010 |
| H-009 | LLM hallucination produces action with OperationalCommand::Unknown; Action Filter does not deny | OS4, OS10 | Unclassified command reaches actuator; undefined physical behavior | S3 | E3 | C3 | D | SG-006 |
| H-010 | CANOpen NMT stop/pre-op/reset command not triggering posture recalculation | OS13 | Industrial controller enters stop state; Aegis posture remains Nominal; unsynchronized state | S2 | E3 | C3 | C | SG-011 |
| H-011 | DNP3 broadcast control command executed without being audited in chain | OS13, OS10 | Critical infrastructure control without tamper-evident record | S2 | E2 | C2 | B | SG-012 |
| H-012 | Cross-asset trust propagation failure in fabric — leader asset LockedOut, follower assets unaware and continuing motion | OS12 | Convoy vehicles or linked robots continue at Nominal speed without awareness of fleet lockout | S3 | E3 | C3 | D | SG-007 |
| H-013 | Recovery hysteresis bypassed by sensor replay attack; faulty sensor promoted to Trusted prematurely | OS11, OS10 | Flapping sensor with manipulated telemetry triggers early Nominal posture restoration | S2 | E2 | C3 | B | SG-013 |
| H-014 | Federation report with forged or replayed generation counter accepted by reconciliation engine | OS10 | Peer controller posture downgraded or upgraded via adversarial input | S2 | E2 | C3 | B | SG-014 |
| H-015 | Rate-of-change limiter applied before hard boundary clamp; velocity spike slips through in single cycle | OS1, OS4 | Transient over-speed command reaches actuator before next enforcement cycle | S2 | E3 | C3 | C | SG-001 |
| H-016 | AEGIS_ADMIN_TOKEN absent or empty; mutation route returns 503 but system misconfigured as fail-open | OS1, OS10 | Unauthorized node registration or trust manipulation | S2 | E2 | C3 | B | SG-015 |
| H-017 | DDS actuator topic configured with TransientLocal durability; stale command delivered to reconnecting actuator | OS5 | Actuator receives outdated command from DDS history cache after reconnect | S2 | E3 | C3 | C | SG-016 |

---

## 4. Safety Goal Summary

See AEGIS-SG-001 for full safety goal definitions.

---

## 5. Document Control

| Field | Value |
|-------|-------|
| Prepared by | Aegis Engineering |
| Review status | Pending TUV pre-assessment |
| Next review | 2026-11-23 |
| Supersedes | None |
