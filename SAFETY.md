# Safety — Kirra OS

## Safety statement

> **Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508 SIL 3 requirements. Independent third-party assessment has not yet been performed.**

That sentence is exact and load-bearing. *Designed in alignment with* is not
*compliant with*; a draft mapping is not an assessment; a passing test is not a
certificate. Nothing in this repository has been independently certified,
approved, or assessed by a third party.

---

## Claim taxonomy

Every safety claim carries one of these labels. They are ordered by strength,
and a claim may not be promoted without the evidence the next level demands.

| Label | Means | Evidence required |
|---|---|---|
| **Implemented** | The mechanism exists in shipped source | Source file |
| **Tested** | Implemented, plus automated tests exercising it | Test + CI lane |
| **Draft evidence** | A safety-case document exists, under review | Document marked Draft |
| **Preliminary mapping** | Requirements mapped to a standard, pre-assessment | Mapping document |
| **Planned assessment** | Scheduled, not begun | Roadmap entry |
| **Independently assessed** | A third party has examined and reported | Assessor report |

**No claim in this repository currently carries "Independently assessed."**
If you find one, it is a defect — please report it.

---

## Evidence index

The safety-case foundation. Authoritative registry:
[`docs/safety/SAFETY_CASE_INDEX.md`](docs/safety/SAFETY_CASE_INDEX.md).

> **Document identifier scheme.** `AEGIS-*` IDs are **deliberately retained**
> across the Aegis → Kirra product rename. They are immutable
> cert-configuration lineage, not an incomplete rebrand; newer documents mint
> `KIRRA-*` IDs. Renaming a legacy ID would break the configuration record, so
> the IDs below are reproduced exactly as the documents carry them.

| Document | Doc ID | Status | File |
|---|---|---|---|
| Hazard Analysis and Risk Assessment | AEGIS-HARA-001 | Draft | [`docs/safety/HARA.md`](docs/safety/HARA.md) |
| Safety Goals | AEGIS-SG-001 | Draft | [`docs/safety/SAFETY_GOALS.md`](docs/safety/SAFETY_GOALS.md) |
| Safety Architecture | AEGIS-SA-001 | Draft | [`docs/safety/SAFETY_ARCHITECTURE.md`](docs/safety/SAFETY_ARCHITECTURE.md) |
| Requirements Traceability Matrix | AEGIS-RTM-001 | Draft | [`docs/safety/REQUIREMENTS_TRACEABILITY.md`](docs/safety/REQUIREMENTS_TRACEABILITY.md) |
| Rust Safety Coding Guidelines | AEGIS-CG-001 | Draft | [`docs/safety/CODING_GUIDELINES.md`](docs/safety/CODING_GUIDELINES.md) |
| Safety Standards Matrix | AEGIS-STD-001 | Draft | [`docs/safety/STANDARDS_MATRIX.md`](docs/safety/STANDARDS_MATRIX.md) |
| ASTM F3269 Run Time Assurance Mapping | AEGIS-F3269-001 | Draft | [`docs/safety/ASTM_F3269_MAPPING.md`](docs/safety/ASTM_F3269_MAPPING.md) |
| ASTM F3269-21 Bounded Operation Mapping (current) | KIRRA-RTA-001 | Draft | [`docs/safety/ASTM_F3269_RTA_MAPPING.md`](docs/safety/ASTM_F3269_RTA_MAPPING.md) |
| IEC 61508 SIL 3 Preliminary Claim Mapping | AEGIS-61508-001 | Draft (Preliminary — pre-assessment) | [`docs/safety/IEC_61508_MAPPING.md`](docs/safety/IEC_61508_MAPPING.md) |
| IEC 61508 SIL 3 Requirements Mapping (current) | KIRRA-SIL3-001 | Draft | [`docs/safety/IEC_61508_SIL3_MAPPING.md`](docs/safety/IEC_61508_SIL3_MAPPING.md) |

Every one is **Draft**. The AV-specific (Occy) safety case, the QNX partition
lane documents, and the full registry are indexed in
[`docs/safety/SAFETY_CASE_INDEX.md`](docs/safety/SAFETY_CASE_INDEX.md).

Related: [`docs/safety/UL4600_SAFETY_CASE.md`](docs/safety/UL4600_SAFETY_CASE.md) ·
[`docs/safety/ISO_IEC_TR_5469_MAPPING.md`](docs/safety/ISO_IEC_TR_5469_MAPPING.md) ·
[`docs/safety/ROADMAP_TO_ASIL_D.md`](docs/safety/ROADMAP_TO_ASIL_D.md)

---

## Technical safety mechanisms

**Implemented** and, except where noted, covered by automated tests. None of
these is independently safety-certified.

### Identity and authorization

| Mechanism | Where |
|---|---|
| Per-node Ed25519 attestation — the node signs a `(node_id, nonce)` challenge, checked against its registered public key | [`crates/kirra-safety-authority/src/attestation.rs`](crates/kirra-safety-authority/src/attestation.rs) |
| TPM2 quote verification under a per-node policy (PCR16 measured boot) | [`src/tpm_quote.rs`](src/tpm_quote.rs) |
| Constant-time token comparison | [`src/security.rs`](src/security.rs) |
| Scoped RBAC; fail-closed on absent or empty credentials | [`src/authz.rs`](src/authz.rs), [`docs/safety/PRINCIPAL_TOKENS.md`](docs/safety/PRINCIPAL_TOKENS.md) |
| Release-token enforcement — Ed25519 over exactly the approved bytes, verified before release | [`crates/kirra-inline-governor/`](crates/kirra-inline-governor/), [`docs/adr/0031-release-token-on-the-actuation-path.md`](docs/adr/0031-release-token-on-the-actuation-path.md) |

> **Correction of a long-standing description.** Node attestation is **Ed25519
> per-node challenge-response**, not HMAC-SHA256. The earlier
> `HMAC(admin_token, nonce)` proof was admin-asserted rather than node-proven
> and was removed; a regression test
> (`legacy_admin_token_hmac_proof_is_rejected`) keeps it removed. HMAC-SHA256
> remains in use elsewhere — for the two-box UDP prototype command path in
> [`crates/kirra-governor-service/`](crates/kirra-governor-service/) — which is
> a different mechanism for a different purpose.

### Trust posture and fleet state

| Mechanism | Where |
|---|---|
| Gray/black DAG traversal — cycle detection, diamond memoization | [`src/verifier.rs`](src/verifier.rs) |
| Per-node and fleet posture calculation | [`src/verifier.rs`](src/verifier.rs) |
| Live posture-based command gating | [`src/posture_cache.rs`](src/posture_cache.rs) |
| AV sensor telemetry watchdog | [`src/telemetry_watchdog.rs`](src/telemetry_watchdog.rs) |
| Recovery hysteresis — consecutive healthy reports within a window | [`src/recovery_hysteresis.rs`](src/recovery_hysteresis.rs) |
| Ed25519 federation; signed cross-controller trust reports | [`crates/kirra-fleet-types/`](crates/kirra-fleet-types/) |
| Replay prevention and nonce burning | [`crates/kirra-persistence/`](crates/kirra-persistence/) |
| Federation reconciliation with generation ordering | [`docs/adr/0037-epoch-fenced-generation-ordering.md`](docs/adr/0037-epoch-fenced-generation-ordering.md) |
| Passive-standby promotion; durable epoch fence | [`src/standby_monitor/`](src/standby_monitor/), [`docs/deployment/HA_TOPOLOGY.md`](docs/deployment/HA_TOPOLOGY.md) |

### Motion bounding

| Mechanism | Where |
|---|---|
| Velocity, acceleration and yaw-rate envelopes | [`crates/kirra-core/src/kinematics_contract.rs`](crates/kirra-core/src/kinematics_contract.rs), [`docs/kinematics_envelope_protection.md`](docs/kinematics_envelope_protection.md) |
| Non-finite (`NaN`/`Inf`) rejection before any envelope check | [`crates/kirra-core/src/kinematics_contract.rs`](crates/kirra-core/src/kinematics_contract.rs) |
| Forward kinematics simulation | [`crates/kirra-core/src/kinematics_sim.rs`](crates/kirra-core/src/kinematics_sim.rs) |
| Degraded = controlled decel-to-stop and hold | [`docs/safety/SAFE_STATE_SPECIFICATION.md`](docs/safety/SAFE_STATE_SPECIFICATION.md), [`docs/adr/0011-degraded-http-actuator-503-vs-decel-gate.md`](docs/adr/0011-degraded-http-actuator-503-vs-decel-gate.md) |
| Per-class envelope profiles; no default class | [`docs/CONTRACT_PROFILES.md`](docs/CONTRACT_PROFILES.md) |

### Model-output containment

| Mechanism | Where |
|---|---|
| Action filtering against live posture | [`src/action_filter.rs`](src/action_filter.rs), [`docs/action_filter.md`](docs/action_filter.md) |
| Typed LLM action parsing, fail-closed | [`src/action_policy.rs`](src/action_policy.rs), [`crates/kirra-planner/src/mick.rs`](crates/kirra-planner/src/mick.rs) |
| Conversational layer fenced from actuation | [`ci/check_mick_actuation_fence.py`](ci/check_mick_actuation_fence.py) |

### Boundaries and integration

| Mechanism | Where |
|---|---|
| Industrial protocol adapters (Modbus, DNP3, CANopen, CIP, OPC-UA) | [`docs/protocol_adapters.md`](docs/protocol_adapters.md) |
| ROS 2 / DDS boundaries; `Volatile` actuator topics | [`crates/kirra-ros2-adapter/`](crates/kirra-ros2-adapter/), [`docs/ros2_interlock.md`](docs/ros2_interlock.md) |
| Fail-closed service behaviour | [`docs/safety/SECURITY_BOUNDARIES.md`](docs/safety/SECURITY_BOUNDARIES.md) |
| Transport security (TLS / mTLS, opt-in, fail-closed on half-config) | [`docs/safety/TRANSPORT_SECURITY.md`](docs/safety/TRANSPORT_SECURITY.md) |

### Evidence and reconstruction

| Mechanism | Where |
|---|---|
| SHA-256 hash-chained audit ledger | [`src/audit_chain.rs`](src/audit_chain.rs) |
| Explainable deny verdicts | [`src/verdicts.rs`](src/verdicts.rs) |
| WAL-mode SQLite persistence; disk before memory | [`crates/kirra-persistence/`](crates/kirra-persistence/) |
| Deterministic virtual-clock test harness | [`src/clock.rs`](src/clock.rs), [`src/scenario_runner.rs`](src/scenario_runner.rs) |
| Deterministic incident replay through the real checker | [`docs/REPLAY_INCIDENT_RECONSTRUCTION.md`](docs/REPLAY_INCIDENT_RECONSTRUCTION.md) |

### Verification harnesses

Machine-checked proofs (Kani/CBMC), concurrency models (loom), UB checks
(Miri), fuzzing, mutation testing, MC/DC-style decision coverage, and a
crash-consistency drill run as CI lanes.
→ [`verification/kani/`](verification/kani/),
[`crates/kirra-loom-models/`](crates/kirra-loom-models/),
[`fuzz/`](fuzz/),
[`docs/safety/OCCY_MCDC_EVIDENCE.md`](docs/safety/OCCY_MCDC_EVIDENCE.md),
[`docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md`](docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md)

---

## Assumptions and integration responsibility

Kirra is a Safety Element out of Context. **System-level safety is a property
of the integrated system, not of this repository.** It depends on:

- the **operational design domain** — [`docs/safety/R2_ODD.md`](docs/safety/R2_ODD.md), [`docs/safety/OCCY_SOTIF.md`](docs/safety/OCCY_SOTIF.md)
- the **hardware**, including redundancy — the quantitative metrics analysis derives a redundant-supply deployment requirement
- the **configuration**, including the vehicle class and envelope profile
- the **maps** and their accuracy
- the **sensor contracts** and the rates at which producers actually publish
- the **integration assumptions** — [`docs/safety/ASSUMPTIONS_OF_USE.md`](docs/safety/ASSUMPTIONS_OF_USE.md)
- the **deployment environment**
- the **safety manual** — [`docs/safety/GOVERNOR_SAFETY_MANUAL.md`](docs/safety/GOVERNOR_SAFETY_MANUAL.md)
- **validation evidence** for the integrated system
- **independent assessment**, where the application requires it

An assumption of use that the integrator does not meet is not a Kirra
mechanism that failed — it is a claim that was never supported. Read
`ASSUMPTIONS_OF_USE.md` before deploying anything to hardware.

Residual risk: [`docs/safety/R2_RESIDUAL_RISK.md`](docs/safety/R2_RESIDUAL_RISK.md).

---

## Non-certification notice

**Alignment, mappings, tests, and draft evidence are not equivalent to
independent certification.**

Specifically:

- No ISO 26262 assessment has been performed. No ASIL rating has been awarded
  by any assessor.
- No IEC 61508 assessment has been performed. No SIL rating has been awarded
  by any assessor.
- No third party has approved, certified, or endorsed this software.
- The safety documents are **Draft** and have not been through a formal
  confirmation review.
- Timing figures measured on development hosts are **indicative only**. WCET
  claims require measurement on the target under the documented scheduling
  regime. → [`docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`](docs/safety/WCET_MEASUREMENT_METHODOLOGY.md)
- Toolchain qualification (Ferrocene) is **planned**, not completed.

Certification and assessment activities are tracked as roadmap work:
[`ROADMAP.md`](ROADMAP.md), [`docs/safety/ROADMAP_TO_ASIL_D.md`](docs/safety/ROADMAP_TO_ASIL_D.md).

---

## Public claim rules

These apply to every README, release note, slide, issue comment, and
documentation change.

**Never convert:**

| From | To |
|---|---|
| designed in alignment with | certified · compliant · approved |
| draft mapping | assessed · compliant |
| preliminary claim mapping | SIL 3 product |
| tested mechanism | ASIL-D product · assessor-approved |

...without documented evidence and approval.

**Avoid:**

- "guarantees safety"
- "makes any AI safe"
- "universally safe"
- "impossible to bypass"
- "fully autonomous and safe"
- "certified architecture"

**Prefer:**

- "fail-closed under the documented conditions"
- "designed to enforce"
- "independently checks"
- "bounds according to configured envelopes"
- "supports evidence development"
- "designed in alignment with"
- "subject to assumptions of use"

The distinction being protected: what the software *does* versus what a third
party has *confirmed*. Kirra can honestly claim a great deal about the first
and nothing yet about the second.

---

## Reporting a safety or security issue

Open an issue for defects. For anything with security impact, prefer private
disclosure to the maintainers over a public issue.

A claim in this repository that overstates assurance is itself a defect worth
reporting.
