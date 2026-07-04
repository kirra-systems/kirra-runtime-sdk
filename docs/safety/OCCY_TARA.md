# Kirra — Cybersecurity TARA (ISO/SAE 21434)

Document ID: KIRRA-OCCY-TARA-001
Version: 0.1
Status: Draft — pending security-engineer review
Classification: ISO/SAE 21434:2021 — Threat Analysis and Risk Assessment (TARA, Clause 15 / Annex H)
Tracker: #118
Date: 2026-06-10

---

> # ⚠️ DRAFT — pending formal security-engineer review
>
> This is a **skeleton** ISO/SAE 21434 TARA. A TARA is a security argument an
> assessor will scrutinise; it has **not** been reviewed or signed off by a
> security engineer. Two honesty rules apply throughout:
>
> 1. **Every control is tagged by on-`main` reality** — `IMPLEMENTED` (a real
>    mechanism verified in code today), `PARTIAL` (present but incomplete), or
>    `GAP` (not present — backlog, not a claim). Same discipline as the UL 4600
>    SPI catalogue (`UL4600_SAFETY_CASE.md` §5.3): controls are cited to a real
>    `file:symbol`, never asserted.
> 2. **Risk ratings and feasibility thresholds are PROVISIONAL** — placeholders
>    for security-engineer calibration, not validated values.
>
> This document does **not** repeat the existing security material; it
> **consumes** `SECURITY.md`, `docs/safety/SECURITY_BOUNDARIES.md`,
> `docs/v1_security_invariants.md`, and the code, and adds the missing
> 21434 analysis layer on top.

---

## 1. Item definition and cybersecurity perimeter

The **item** is the Kirra runtime legitimacy engine and safety governor as
defined in AEGIS-HARA-001 §1 and reused by AEGIS-SC-000 (C-01). 21434 concerns
the *cybersecurity* of that item: an attacker who subverts Kirra can drive the
governed equipment toward **unreasonable risk** (the UL 4600 top claim,
`UL4600_SAFETY_CASE.md` G-UL-TOP) — that is the 21434 ↔ 26262/UL 4600 interface
this TARA makes explicit (§4).

**Trust surface (attack surface) — cited, not repeated:**

| Surface | What it is | Reference (consume) |
|---|---|---|
| Verdict path | The frozen kinematic enforcement contract; the last line before actuation | `kirra_core::kinematics_contract` (re-exported via `src/gateway/kinematics_contract.rs`; talisman blob `33b47b56…`, reviewed-amended H1/M1) |
| Audit chain + signing keys | Tamper-evident hash-chained, Ed25519-signed ledger + key-trust map | `src/audit_chain.rs`, `src/verifier_store.rs` |
| Attestation | Node identity / trust establishment (challenge-response) | `src/attestation.rs`, `src/verifier.rs` |
| Admin / mutation gate | Privileged-route authentication | `src/security.rs`, `SECURITY_BOUNDARIES.md` (SG-015 + handshake carve-out) |
| Fabric / federation | Cross-asset command authority + cross-controller trust reports | `src/fabric/*`, `src/federation*.rs` |
| Protocol adapters | Industrial ingress (DNP3 / Modbus / OPC-UA / EtherNet-IP / CANopen) | `src/protocol_adapter.rs`, `src/adapters/*` |
| HA cluster | Primary/standby promotion + split-brain fence | `src/standby_monitor.rs` |
| Transport / host | The axum HTTP listener + the host platform | `src/bin/kirra_verifier_service.rs` (`KIRRA_VERIFIER_ADDR`) |

The ten structural invariants in `docs/v1_security_invariants.md` are the
**security requirements baseline** this TARA assesses against; `SECURITY.md` is
the disclosure policy and the short invariant list (note: `SECURITY.md`'s
"`verify_attestation` uses HMAC-SHA256" line is **stale** — the code now uses a
per-node **Ed25519** proof, invariant #3 / `attestation::verify_attestation_proof`;
the code is authoritative).

## 2. Asset register

Each asset with its primary cybersecurity property — **C**onfidentiality /
**I**ntegrity / **A**vailability / **Auth**enticity — and the on-`main` artifact
that bears it. (No asset is listed whose control cannot be pointed to in code.)

| # | Asset | Property | Bearing artifact (on main) |
|---|---|---|---|
| A1 | Actuator command verdict | **I**, A | `kinematics_contract::validate_vehicle_command` (talisman) |
| A2 | Fleet posture state | I, A | `posture_engine_v2::resolve_posture_with_reason`, `posture_cache` |
| A3 | Audit ledger (record of truth) | **I**, Auth | `audit_chain::append_audit_event_tx`, `verify_audit_chain_full` |
| A4 | Audit signing keys + key-trust map | **C**, I, Auth | `verifier_store::admit_signing_key`, `audit_trust_anchor`/`audit_key_ledger` |
| A5 | Node trust state / attestation identity | I, Auth | `attestation::verify_attestation_proof`, `RegisteredNode.ak_public_pem` |
| A6 | Challenge nonces | I, A (freshness) | `verifier::generate_challenge_nonce`, `pending_challenges` |
| A7 | Admin token | **C**, Auth | `security::constant_time_compare`, `require_admin_token` |
| A8 | Cross-controller federated trust reports | I, Auth | `federation::verify_federated_report_signature` |
| A9 | Fabric cross-asset command authority | I, A | `fabric::governor::evaluate_command`, `fabric::router` |
| A10 | Industrial-protocol command ingress | I, A | `protocol_adapter`, `evaluate_dnp3_adapter` |
| A11 | HA leadership (single-active) | A, I | `standby_monitor::perform_promotion`, durable epoch CAS |
| A12 | Dependency DAG | A (DoS resistance), I | `verifier::recursive_calculate` (cycle/depth bound) |

## 3. Threat scenarios (per asset)

STRIDE-framed, scoped to the damage that reaches the governed equipment.

| T# | Asset | Threat scenario |
|---|---|---|
| T1 | A1 | **Fabricated kinematic claim** — a client posts an out-of-envelope command hoping the verifier forwards its magnitude verbatim (tamper / spoofing of the actuation value) |
| T2 | A1 | **Verdict-path bypass / substitution** — swap or patch the enforcement contract so clamps don't fire (tamper) |
| T3 | A3 | **Divergence-evidence suppression** — delete/truncate audit rows so a comparator divergence or veto leaves no trace (tamper + repudiation) |
| T4 | A3/A4 | **Key-swap** — re-sign a forged ledger under an attacker key, or roll the trust anchor to an attacker key (spoofing of authenticity) |
| T5 | A5/A6 | **Attestation forgery / nonce prediction** — pass attestation without the private AK, or precompute a signature against a predictable nonce (spoofing) |
| T6 | A7 | **Admin-token compromise / timing extraction** — recover or brute the token; bypass the mutation gate (elevation of privilege) |
| T7 | A8 | **Federated-report forgery / replay** — inject a forged or replayed cross-controller posture (spoofing + replay) |
| T8 | A8/A2 | **Split-brain induction** — feed reordered/stale generations to flip an asset's authoritative posture (tamper) |
| T9 | A9 | **Unmapped-node injection** — submit fabric commands as an unregistered asset to escape posture gating (spoofing) |
| T10 | A10 | **Protocol-adapter abuse** — a broadcast/industrial control that mutates actuators without a durable record (tamper + repudiation) |
| T11 | A11 | **HA leadership hijack / dual-active** — induce two Active verifiers writing divergent state (tamper) |
| T12 | A1/A12 | **DoS** — flood unauthenticated endpoints or submit a cyclic/deep DAG to exhaust the verifier (denial of service) |

## 4. Impact rating (SFOP) + safety cross-link

21434 impact categories: **S**afety / **F**inancial / **O**perational /
**P**rivacy. For any threat whose damage is a **safety** violation, the specific
Safety Goal and the UL 4600 claim it threatens are named — a successful attack
there is a **path to "unreasonable risk."** (Ratings provisional.)

| T# | S | F | O | P | Safety cross-link (SG / UL 4600 claim threatened) |
|---|---|---|---|---|---|
| T1 | Severe | Mod | Major | Negl | SG-001/SG-002 envelope → G-UL-TOP (S-UL-3 enforcement) |
| T2 | Severe | Mod | Major | Negl | SG-001/002/004 → G-UL-TOP (defeats the whole verdict path) |
| T3 | Major | Mod | Major | Negl | SG-010 audit tamper-detect → UL 4600 SPI-G06 / G-UL-MON (blinds the monitoring loop) |
| T4 | Major | Mod | Major | Negl | SG-010 → G-UL-MON (forges the evidence base) |
| T5 | Severe | Mod | Major | Negl | SG-006/SG-007 trust derivation → G-UL-TOP (admits an untrusted node) |
| T6 | Severe | Major | Major | Mod | SG-015 mutation gate → G-UL-TOP (unlocks every privileged mutation) |
| T7 | Major | Mod | Major | Negl | SG-014 federation replay → SA-L1 trust layer |
| T8 | Major | Mod | Major | Negl | SG-005/SG-007 → G-UL-TOP (wrong authoritative posture → wrong gating) |
| T9 | Severe | Mod | Major | Negl | SG-006/SG-007 → G-UL-TOP (escapes posture gate) |
| T10 | Major | Mod | Major | Negl | SG-012 DNP3 mandatory audit → G-UL-MON + H-011 |
| T11 | Major | Mod | Severe | Negl | SG-009 HA → G-PLATFORM availability claim |
| T12 | Major | Mod | Severe | Negl | SG-005 fail-closed-on-stale bounds the safety blast radius; O-impact dominates |

## 5. Attack-path & attack-feasibility rating

**Basis:** ISO/SAE 21434 Annex G **attack-potential** method (Elapsed Time,
Expertise, Knowledge of item, Window of opportunity, Equipment) → feasibility
**Very Low / Low / Medium / High**. (A CVSS basis is the documented alternative;
attack-potential is chosen here as the 21434-native method. Thresholds
PROVISIONAL.)

| T# | Representative attack path | Dominant factor | Feasibility |
|---|---|---|---|
| T1 | POST a command with an inflated magnitude to the fabric/actuator route | None — trivial request | **High** (but see C-A1: blocked) |
| T2 | Replace the contract binary / patch limits in memory | Needs host code-exec + persistence | **Low** |
| T3 | Delete/rewrite `audit_log_chain` rows on disk | Needs DB write access to host | **Low** |
| T4 | Sign a forged ledger / roll the anchor to an attacker key | Needs host access + defeat key-trust | **Very Low** |
| T5 | Forge an attestation proof / predict a nonce | Needs AK private key OR CSPRNG break | **Very Low** |
| T6 | Brute/extract the admin token | Needs token; timing channel | **Low** |
| T7 | Replay or forge a federated report | Needs a trusted controller key / replay window | **Low** |
| T8 | Feed reordered generations across controllers | Needs trusted-peer position + timing | **Low** |
| T9 | Submit commands as an unregistered asset | Needs admin token to register; else rejected | **Low** |
| T10 | Issue a DNP3 broadcast control | Needs industrial-segment access | **Medium** |
| T11 | Starve the primary heartbeat to force promotion | Needs cluster-network position | **Low** |
| T12 | Flood `/health` / `/attestation/challenge` | None — unauthenticated reachability | **High** |

## 6. Risk value, treatment, cybersecurity goal → control

Risk = Impact × Feasibility (21434 5×4 matrix; PROVISIONAL). Treatment ∈
{avoid, reduce, share, retain}. Each cybersecurity goal names the **control**
that meets it, tagged against on-`main` code. **Control status was re-verified
against the current `main` tree** (the #117 grounding habit), correcting two
stale findings (#147 nonce, #245 DNP3) — see notes.

| T# | Risk (prov.) | Treatment | Cybersecurity goal → control | Status |
|---|---|---|---|---|
| T1 | Med | reduce | Verifier authors the clamp; never trusts a client magnitude (#86/#235, #85) → `fabric::governor::evaluate_command` re-derives via `validate_vehicle_command`; returns the enforced `command`; non-finite enforced value → `ENFORCED_COMMAND_UNPRODUCIBLE` deny; `FABRIC_COMMAND_CLAMPED`/`_DENIED` audited (`fabric_command_authoritative_tests`) | **IMPLEMENTED** |
| T2 | Med | reduce | Verdict path is a frozen, WCET-bounded, allocation-free contract; P0 NaN/Inf → P2 ceiling → P6 lateral-accel always run; not loaded from external state (talisman `33b47b56…`, reviewed-amended H1/M1) | **IMPLEMENTED** (integrity rests on host/boot — see T2-residual) |
| T3 | Med | reduce | Hash-chain + signed anchor-head truncation detection (#77) + hash-v2 domain separation; `verify_audit_chain_full` checks `chain_intact` ∧ `signature_valid` ∧ head match; CERT-006 divergences durably recorded (#247) | **IMPLEMENTED** (UL 4600 SPI-G06 surfaces a break; node-local parko chain, `COMPARATOR_DIVERSITY.md` §7a) |
| T4 | Low | reduce | Durable key-trust map (#165): pinned `audit_trust_anchor`, self-attested `audit_key_ledger`, consent-gated adoption; `KeyAdmission` fail-closed on retired/unadopted/reversion keys | **IMPLEMENTED** |
| T5 | Low | reduce | Ed25519 challenge-response: node signs `(node_id, nonce)`, verified vs registered `ak_public_pem` with `verify_strict` + domain separation; **nonce from OS CSPRNG** `getrandom` (#147), single-use burn + `CHALLENGE_TTL_MS`; constant-time compares | **IMPLEMENTED** — *(agent's "wall-clock nonce" was stale `d88d2ba`; `verifier::generate_challenge_nonce` uses `getrandom`)* |
| T6 | Med | reduce | `require_admin_token` fail-closed (absent/empty → 503, mismatch → 401) via `constant_time_compare` (never `==`); identity-gated routes add `require_client_identity`; attestation handshake carve-out justified (SG-015, `SECURITY_BOUNDARIES.md`) | **IMPLEMENTED** (single token, not fine-grained RBAC → §7) |
| T7 | Low | reduce | Federation 5-step pipeline: Ed25519 verify + freshness + replay window + durable nonce burn (`federation_report_nonces`, `synchronous=FULL`) (SG-014) | **IMPLEMENTED** |
| T8 | Low | reduce | Generation-ordered v2 reconciliation (`federation_reconciliation::reconcile_reports`, "higher generation wins") + restart-monotonic `POSTURE_GENERATION` persistence (`init_generation_from_store`) | **IMPLEMENTED** (classic-path within-window acceptance is replay-window-bounded → §7) |
| T9 | Low | reduce | Unregistered asset → no posture record → fabric gate fails closed; asset registration is admin-gated (`/fabric/assets/register`) | **IMPLEMENTED** |
| T10 | Med | reduce | DNP3 broadcast control MUST carry a tamper-evident record; **audit-write failure → 503 block** `DNP3_BROADCAST_AUDIT_UNAVAILABLE` (#245/SG-012); CANopen NMT → posture recalc (#84); malformed → 400 | **IMPLEMENTED** — *(agent's "not fail-closed" was stale `d88d2ba`)* |
| T11 | Low | reduce | Durable epoch compare-and-swap: only one promoter wins the CAS; in-transaction `assert_epoch_held` fences stale-Active writes; heartbeat freshness on a monotonic clock (#79/#80) | **IMPLEMENTED** |
| T12 | Med | reduce/**retain** | Cycle/depth-bounded DAG (`MAX_DEPENDENCY_DEPTH`) and fail-closed-on-stale posture bound the safety blast radius; **no HTTP rate-limiting** → DoS availability risk retained | **PARTIAL** (→ §7) |

## 7. Residual risk and honest gaps

Controls that are PARTIAL or absent, stated plainly. These are the security
backlog; none is papered over.

| Gap | Status | What's missing | Risk retained | Ref |
|---|---|---|---|---|
| **PCR16 measured-boot** | **GAP** | `expected_pcr16_digest_hex` is stored on `RegisteredNode` but never compared to a TPM quote; quote parsing not implemented | Attestation proves key possession, **not** platform/firmware integrity (T2/T5 host-compromise residual) | invariant #2 (aspirational); CLAUDE.md inv #3; `attestation.rs` follow-up note |
| **HTTP rate-limiting / DoS** | **GAP** | No per-IP/-token/-route limiter; unauthenticated `/health`, `/ready`, `/attestation/challenge/*` are floodable | T12 availability risk retained; mitigated only by fail-closed posture | `build_app` route wiring |
| **TLS / transport** | **GAP (operator)** | The axum listener is plain TCP; encryption is delegated to a reverse proxy (code comment: "transmit over TLS in production") | Token/report interception on an unencrypted segment (T6/T7) | `KIRRA_VERIFIER_ADDR` listener |
| **Fine-grained RBAC** | **GAP** | One admin token for all privileged routes; no per-role scoping (e.g. audit-viewer vs key-rotator) | Token compromise = full privilege (T6 blast radius) | `SECURITY_BOUNDARIES.md` |
| **Key-rotation policy** | **PARTIAL** | Rotation mechanism is durable + audited (`record_key_rotation`, `/system/audit/rotate-signing-key`) but operator-initiated only — no schedule/expiry policy | Long-lived keys if operators don't rotate (T4) | `verifier_store::record_key_rotation` |
| **Supply-chain integrity** | **PARTIAL** | `Cargo.lock` pins versions, but no cryptographic provenance / dependency-attestation / `cargo audit` gate in-tree | Transitive-dependency compromise unaddressed by the SDK | (no in-repo control) |
| **Legacy audit v1 rows** | **PARTIAL** | Hash-v2 domain separation protects **new** rows; pre-`HASH_V2_MIGRATION` v1 rows carry the field-relabeling weakness (no silent destructive rewrite) | Bounded to historical rows (T3) | `audit_chain` hash-v2 boundary |
| **Verdict-path & ledger at rest** | **retain** | Contract-binary and SQLite integrity ultimately rest on host/OS access control + (future) measured boot — outside the process's authority | T2/T3 require host compromise; depth-in-defence, not eliminated | host platform |

## 8. Status

**Skeleton / drafted — pending security-engineer review.** Not closed. The
asset register (§2), threat scenarios (§3), SFOP + safety cross-links (§4),
feasibility (§5), and the treatment→control map (§6) are grounded in the current
`main` tree; risk values and feasibility thresholds are PROVISIONAL and require
security-engineer calibration. The §7 gaps (PCR16 measured-boot, rate-limiting,
TLS, RBAC granularity, rotation policy, supply-chain) are the security backlog
to be dispositioned before any rating here is cited as validated coverage.

---

## Document control

| Field | Value |
|-------|-------|
| Prepared by | Kirra Engineering |
| Review status | Draft — pending security-engineer review |
| Method | ISO/SAE 21434 Annex G attack-potential; SFOP impact; controls re-verified against `main` |
| Consumes | `SECURITY.md`, `docs/safety/SECURITY_BOUNDARIES.md`, `docs/v1_security_invariants.md`, `src/security.rs` |
| Related | AEGIS-SC-000, AEGIS-SG-001 (SGs), KIRRA-OCCY-UL4600-001 (safety-case linkage), KIRRA-OCCY-DFA-001 |
| Tracker | #118 |
| Supersedes | None |
