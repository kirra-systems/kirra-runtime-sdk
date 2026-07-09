# Kirra — UL 4600 Safety Case (GSN top claim) + Safety Performance Indicators

Document ID: KIRRA-OCCY-UL4600-001
Version: 0.1
Status: Draft — pending safety-engineer review
Classification: UL 4600 (autonomous-product safety case) / GSN (Goal Structuring Notation)
Tracker: #117
Date: 2026-06-10

---

> # ⚠️ DRAFT — pending formal safety-engineer review
>
> This is a **skeleton** UL 4600 safety case plus a **first** Safety
> Performance Indicator (SPI) catalogue and monitoring plan. A safety case is
> an argument that a real assessor will scrutinise; this document has **not**
> been reviewed or signed off by a safety engineer.
>
> Two honesty rules apply throughout:
> 1. **GSN nodes cite on-main evidence only.** Every solution/evidence node in
>    §3–§4 names a file that exists today; the §4 register records its real
>    status. Undeveloped claims are labelled, not asserted.
> 2. **SPIs are tagged by data-source reality.** Each SPI in §5 is marked
>    `EMITTED` (a real audit event exists today), `DERIVED` (computable from
>    existing events without new code), or `GAP` (requires new instrumentation
>    — it does **not** exist yet). The issue's suggested SPIs that fall in the
>    `GAP` bucket are called out explicitly in §5.3, not silently claimed.
>
> The SPI **threshold values** in §5 are placeholders for field calibration,
> not validated alarm limits.

---

## 1. Purpose and relationship to the existing safety case

Kirra already carries an **ISO 26262 / GSN** functional-safety case rooted at
`docs/safety/SAFETY_CASE_INDEX.md` (AEGIS-SC-000): a top claim ("sufficiently
safe"), 16 safety goals (SG-001…SG-016) traced to code and tests, a
three-layer decomposition (SA-L1/L2/L3), a Dependent Failure Analysis
(`OCCY_DFA.md`), and Governor integrity evidence (`GOVERNOR_INTEGRITY_EVIDENCE.md`).

This document is the **UL 4600 acquisition layer** that sits *above* that case.
It does **not** replace or duplicate it. UL 4600 (*Standard for Safety for the
Evaluation of Autonomous Products*) asks two things the ISO 26262 case does not
fully provide on its own:

1. A top claim framed as **"absence of unreasonable risk"** over a declared
   ODD, argued by **decomposition + run-time enforcement evidence** (S2 + S3),
   with the existing functional-safety case consumed as a solution node.
2. A **living safety case**: the argument must be kept valid in operation
   through **Safety Performance Indicators (SPIs)** — leading and lagging
   metrics — and an **assurance-case monitoring plan** that feeds field data
   back into the case. UL 4600 treats the safety case as continuously
   re-validated, not a one-time artefact.

Kirra's tamper-evident, hash-chained, Ed25519-signed **audit chain**
(`src/audit_chain.rs`, `src/verifier_store.rs`) is the native, integrity-
protected evidence source for those SPIs. This document makes the audit chain a
first-class safety-case asset.

## 2. Scope, item, and ODD

- **Item:** as defined in AEGIS-HARA-001 §1 and reused by AEGIS-SC-000 C-01 —
  the `kirra-runtime-sdk` crate, the `kirra_verifier_service` binary, the ROS 2
  safety-interlock nodes, the industrial-protocol adapters, the multi-asset
  safety fabric, and (for the Occy planner item) the `parko-*` governor stack.
- **Kirra's role:** Kirra is the **run-time safety governor** — a non-AI
  enforcement layer over AI/ML-driven equipment (aligns with ISO/IEC TR 5469
  usage-class-2; see `ISO_IEC_TR_5469_MAPPING.md`). It does not author motion;
  it bounds it. The UL 4600 "absence of unreasonable risk" claim below is
  therefore scoped to **what the governor can enforce**, not to the correctness
  of the controlled autonomy.
- **ODD:** the operational design domain and its speed envelope are defined in
  `docs/adr/0001-occy-odd-speed-cap.md` (50 mph / 80 km/h cap),
  `docs/safety/OCCY_SOTIF.md` (triggering-condition catalogue), and
  `docs/adr/0002-condition-dependent-cap-subodds.md` (sub-ODD partitions).

## 3. GSN top-level argument (UL 4600 framing)

GSN per the GSN Community Standard v3. This argument is **deliberately thin**:
its leaf strategies hand off to existing artefacts as solution nodes (§4),
except S-UL-4 (operational monitoring), which is the new content of this
document (§5–§6).

```
                         ┌────────────────────────────────────────────┐
   C-UL-ODD  ◀───────────┤ G-UL-TOP                                     │
   ODD per ADR-0001 /    │ The Kirra-governed item poses an absence of  │
   OCCY_SOTIF /          │ unreasonable risk of harm while operating    │
   ADR-0002              │ within its defined ODD.                      │
                         └───────────────────────┬──────────────────────┘
                                                 │
        ┌──────────────────┬────────────────────┼────────────────────┬───────────────────┐
        │                  │                     │                    │                   │
   ┌────▼─────┐      ┌─────▼──────┐       ┌──────▼──────┐      ┌──────▼──────┐     ┌──────▼───────┐
   │ S-UL-1   │      │ S-UL-2     │       │ S-UL-3      │      │ S-UL-4      │     │ (UL-4600     │
   │ Hazards  │      │ Decompo-   │       │ Governor    │      │ Operational │     │  open items  │
   │ mitigated│      │ sition +   │       │ integrity   │      │ feedback /  │     │  → §7, AEGIS-│
   │ to ASIL  │      │ DFA (S2)   │       │ (S3)        │      │ SPIs (live) │     │  ROAD-001)   │
   └────┬─────┘      └─────┬──────┘       └──────┬──────┘      └──────┬──────┘     └──────────────┘
        │                  │                     │                    │
   ┌────▼─────┐      ┌─────▼──────┐       ┌──────▼──────┐      ┌──────▼──────┐
   │Sn-ISO    │      │Sn-DFA      │       │Sn-INTEG     │      │ G-UL-MON    │
   │26262     │      │OCCY_DFA.md │       │GOVERNOR_    │      │ degradation │
   │SAFETY_   │      │(PO-1/PO-2) │       │INTEGRITY_   │      │ is detected │
   │CASE_     │      │            │       │EVIDENCE.md  │      │ + triggers  │
   │INDEX.md  │      │            │       │ + Sn-DIV    │      │ re-eval     │
   │(16 SGs)  │      │            │       │ COMPARATOR_ │      │             │
   │          │      │            │       │ DIVERSITY.md│      │ → §5 SPIs   │
   └──────────┘      └────────────┘       └─────────────┘      │ → §6 plan   │
                                                                └─────────────┘
```

**G-UL-TOP** — The Kirra-governed item poses an *absence of unreasonable risk*
of harm while operating within its defined ODD (C-UL-ODD).

- **S-UL-1 (hazards → ASIL).** All hazards in AEGIS-HARA-001 are mitigated to
  their required ASIL by SG-001…SG-016. *Solution:* **Sn-ISO26262** =
  `SAFETY_CASE_INDEX.md` (AEGIS-SC-000) and its sub-arguments SA-L1/L2/L3.
- **S-UL-2 (decomposition + DFA — the UL 4600 "S2").** The ASIL-D Governor and
  the QM(D) planner are decomposed with analysed independence; the Governor
  provides diagnostic coverage (PO-1) of hazardous-trajectory classes and the
  channels are independent (PO-2, coupling factors C1–C6). *Solution:* **Sn-DFA**
  = `OCCY_DFA.md`.
- **S-UL-3 (Governor integrity — the UL 4600 "S3").** The enforcement Governor
  is shown to be of sufficient integrity: O(1) WCET, MC/DC on safety-critical
  decisions, freedom-from-interference, requirements traceability, and
  **diverse-shadow systematic-fault detection**. *Solutions:* **Sn-INTEG** =
  `GOVERNOR_INTEGRITY_EVIDENCE.md`; **Sn-DIV** = `COMPARATOR_DIVERSITY.md`.
- **S-UL-4 (operational feedback — new in this document).** The argument above
  is kept valid in the field: degradation of any sub-claim is detected through
  SPIs computed over the tamper-evident audit chain, and a breach triggers
  re-evaluation of the affected claim.
  - **G-UL-MON** — Degradation of the safety argument in operation is detected
    and triggers safety-case re-evaluation. *Supported by:* §5 (SPI catalogue)
    + §6 (monitoring plan). The audit chain is the data source; its own
    integrity is a precondition (§5.1).

## 4. Solution / evidence node register (on-main status)

Building this register **is** the goal-by-goal re-triage UL 4600 demands: every
node names an artefact and its real state on `main`. (Status reflects the code
and docs present, not the issue tracker.)

| GSN node | Claim (abbrev.) | Solution artefact (on main) | Status |
|---|---|---|---|
| Sn-ISO26262 | 16 SGs mitigate all HARA hazards to ASIL | `docs/safety/SAFETY_CASE_INDEX.md`, `SAFETY_GOALS.md`, `HARA.md` | **Present**; 16 SGs defined w/ ASIL+FTTI+code refs; 11/16 SGs have test evidence (per `RTM_GAP_REPORT.md`) |
| SA-L1 | Trust graph derives per-node trust within FTTI | `src/telemetry_watchdog.rs`, `src/verifier.rs` (DAG) | **Present + tested** (SG-003, SG-007) |
| SA-L2 | Posture fails closed on staleness | `src/posture_engine_v2.rs::resolve_posture_with_reason` | **Present + tested** (SG-005) |
| SA-L3 | Kinematic envelope + posture-gated routing | `src/gateway/kinematics_contract.rs`, `src/gateway/containment.rs`, `src/posture_cache.rs` | **Present + tested** (SG-001/002/004/006; SG-002 corridor containment landed) |
| Sn-DFA | ASIL decomposition + independence (PO-1/PO-2) | `docs/safety/OCCY_DFA.md` | **Present (analysis)**; PO-2 mitigations C1–C6 partly deployment-dependent (e.g. C1 separate SoC) |
| Sn-INTEG | WCET / MC/DC / FFI / traceability | `docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md`, `src/wcet_gate.rs`, `docs/safety/OCCY_MCDC_EVIDENCE.md`, `docs/safety/OCCY_FFI_EVIDENCE.md` | **Present (host-indicative)**; timing = host CI regression gate at p99.9 ≈ 170–352 ns — NOT certified WCET (the gate asserts p99.9; max is reported ungated; target-measured WCET under QNX/`SCHED_FIFO` is tracked in #274). Coverage = 100% branch-pair on the targeted check-path decisions (`docs/safety/OCCY_MCDC_EVIDENCE.md`), gated decision-coverage floors 77–79%; true MC/DC is toolchain-blocked (#65) — **≥95% MC/DC is a future target, not an achieved figure** |
| Sn-DIV | Diverse-shadow systematic-fault detection | `docs/safety/COMPARATOR_DIVERSITY.md`, `parko/crates/parko-kirra/src/diverse.rs`, `comparator.rs`, `audit_sink.rs` | **Present (DRAFT argument)**; divergence now durably auditable (§7a of that doc) — node-local chain only |
| G-UL-MON | Field degradation detected → re-eval | **this document §5–§6** over `src/audit_chain.rs` | **Skeleton**; SPI catalogue defined, thresholds uncalibrated, some sources are gaps (§5.3) |
| G-PROCESS / G-PLATFORM / G-COVERAGE | dev process / qualified platform / full MC-DC | AEGIS-ROAD-001 | **Undeveloped** (carried from AEGIS-SC-000 §2.7) |

## 5. Safety Performance Indicators

### 5.1 Principles

- **Leading vs lagging.** A *leading* SPI measures stress/degradation **before**
  a safety reaction is forced (it predicts a rising risk of intervention). A
  *lagging* SPI counts a safety reaction that **already occurred** (a veto,
  clamp, fail-closed, or failover). UL 4600 wants both: leading SPIs warn;
  lagging SPIs confirm the argument's residual assumptions still hold.
- **The audit chain is the source of record.** Every SPI below is computed over
  events in the hash-chained `audit_log_chain` table (read via
  `VerifierStore::load_all_posture_events`). Because the chain is hash-linked
  and signed, an SPI value is **tamper-evident**: it cannot be quietly lowered
  by deleting inconvenient events without breaking the chain.
- **Integrity precondition.** An SPI rollup is only trustworthy if the chain
  verifies. Every rollup MUST first run `verify_audit_chain_full(Some(vk))` and
  assert `chain_intact && signature_valid`; a failed verification is itself the
  highest-severity lagging SPI (SPI-G06) and **invalidates the rollup window**.
- **WCET firewall.** The kinematic gateway hot path (`validate_vehicle_command`)
  is O(1) and writes **no** audit row per command — this is deliberate (SG-001/002
  WCET budget). SPIs about *gateway-level* clamps are therefore **gaps** (§5.3),
  not silent claims; command-disposition SPIs that *are* available come from the
  fabric/adapter layer, which audits each verdict outside the WCET path.

### 5.2 SPI catalogue

`event_type` values below are the **verbatim string literals** emitted on main.
Type = (L)eading / (G) lagging. Source = EMITTED / DERIVED / GAP.

**This catalogue is machine-gated (WS-3.3).** The reviewed registry
`ci/spi_registry.json` transcribes every row below, and a root-crate test
(`kirra_verifier::spi_ledger`) asserts each `EMITTED`/`DERIVED` SPI's
`event_type` literals are actually emitted in the workspace's non-test code (the
root `src/` **plus** `crates/*/src` — some `event_type`s are `DerateCode` reason
tokens defined in `kirra-core`, not the verifier crate), that the registry IDs
equal this table's IDs, and provides a tested pure rollup evaluator (count /
ratio + threshold breach, with the §5.1 chain-verify precondition as SPI-G06).
Renaming or removing an audit `event_type` reds the gate — the catalogue can no
longer silently drift from the code.

| SPI ID | Name | Type | Indicates | Audit-chain source (`event_type`) | Direction | Source |
|---|---|---|---|---|---|---|
| SPI-L01 | Comparator divergence rate | L | Latent systematic fault or state drift between primary & diverse governors | `ComparatorDivergence` ÷ `MOTION_COMMAND_ADMITTED` | ↓ lower better; any sustained non-zero ⇒ investigate | EMITTED (parko-kirra; node-local chain) |
| SPI-L02 | RSS near-miss rate | L | Closing on the minimum safe distance (IEEE 2846 / RSS) | `RSS_VIOLATION` ÷ `MOTION_COMMAND_ADMITTED` | ↓ | EMITTED (`append_rss_violation`) |
| SPI-L03 | Sensor-health fault rate | L | Perception/sensor channel degrading toward trust loss | `SENSOR_HEALTH_REPORT_FAULT` per node·hour | ↓ | EMITTED |
| SPI-L04 | Perception-derate rate | L | Detection range / snapshot health below floor | `PERCEPTION_SNAPSHOT_UNHEALTHY`, `DETECTION_RANGE_DEGRADED` | ↓ | EMITTED (`perception_monitor`; `DerateCode` reason tokens in `kirra-core`) |
| SPI-L05 | Degraded-entry frequency | L | ODD/operational stress forcing reduced-capability posture | `SYSTEM_POSTURE_TRANSITION` (→ Degraded) | ↓ | EMITTED (`posture_engine`) |
| SPI-L06 | Posture-flap / recovery-reset rate | L | Instability: repeated fault↔recover cycling (hysteresis resets) | `SYSTEM_POSTURE_TRANSITION` paired w/ `SENSOR_RECOVERY_CONFIRMED` | ↓ | DERIVED |
| SPI-L07 | Federation rejection rate | L | Trust-boundary pressure (misconfig or adversarial peers) | `FEDERATION_REJECTED` | ↓ | EMITTED |
| SPI-G01 | LockedOut / MRC engagement rate | G | Safe-state fallback actually engaged | `SYSTEM_POSTURE_TRANSITION` (→ LockedOut) | ↓; expected non-zero in faults | EMITTED (discrete *MRC-maneuver* event is a GAP — see §5.3) |
| SPI-G02 | Command veto rate | G | Governor blocked a command outright | `FABRIC_COMMAND_DENIED` + `ACTION_FILTER_DENIED` + `ACTION_FILTER_UNKNOWN_TYPE` + `INDUSTRIAL_ACTION_DENIED` ÷ `MOTION_COMMAND_ADMITTED` | ↓ | EMITTED at fabric/adapter layer (GAP at kinematic gateway — §5.3) |
| SPI-G03 | Command clamp rate | G | Governor admitted but bounded a command | `FABRIC_COMMAND_CLAMPED` ÷ `MOTION_COMMAND_ADMITTED` | ↓ | EMITTED at fabric layer (GAP at kinematic gateway — §5.3) |
| SPI-G04 | HA failover count | G | Primary verifier failed; standby promoted | `STANDBY_PROMOTED_TO_ACTIVE` | ↓ | EMITTED (`standby_monitor`) |
| SPI-G05 | Unknown-command denial count | G | Malformed/unknown command reached the gate and was denied | `ACTION_FILTER_UNKNOWN_TYPE`, `ACTION_FILTER_MALFORMED_REQUEST` | ↓ | EMITTED |
| SPI-G06 | Audit-chain integrity failures | G | Tamper or corruption of the evidence base itself | `verify_audit_chain_full` → `!chain_intact \|\| !signature_valid`; plus write-failure logs/counters | **0 required** | EMITTED (on-demand verify); dedicated *write-failure* event is a GAP (§5.3) |

### 5.3 SPIs requiring new instrumentation (honest gaps)

The issue suggests "veto rate, MRC commits, clearance-gate events, posture
transitions." Posture transitions and (fabric-layer) vetoes are available
above; the following are **not emitted today** and are recorded here as
instrumentation backlog, not as claimed signals:

| Gap | Why it's a gap | Needed to instrument | Related |
|---|---|---|---|
| Kinematic-gateway clamp/veto rate | `validate_vehicle_command` is the O(1) WCET hot path and intentionally performs no per-command audit write | An **out-of-band aggregate counter** (e.g. atomic counters flushed periodically to one audit event), never a per-command SQLite write — to preserve the WCET budget | SG-001/002, `wcet_gate.rs` |
| Discrete MRC-maneuver commit | LockedOut posture transition is audited (SPI-G01), but the *maneuver* (MRC profile actually authored to actuators) is not a distinct event | A dedicated audit event at the MRC enforcement point distinguishing "decel-to-stop envelope" vs "safe-stop maneuver" | SS-001/SS-002 (`SAFE_STATE_SPECIFICATION.md`) |
| Clearance-gate engagement | The clearance-confirmation / operator-escalation loop is not implemented | Implement the gate, then audit each engage/clear/hold | #103 |
| Post-incident sequence | No post-collision sequence (impact time, latched mode, command vs actual delta) is recorded | Record the latched post-collision sequence into the chain | #104 |
| Command-source / handoff | `command_source` (autonomous vs teleop) is not a field in the chain | Add `command_source` to the audit record + record handoff verdicts | #111, #112 |

## 6. Assurance-case monitoring plan

UL 4600 requires the safety case to be a *living* artefact. This plan defines
how the §5 SPIs keep G-UL-TOP valid in operation.

1. **Data source & integrity gate.** SPIs are computed by reading the
   `audit_log_chain` (`load_all_posture_events`). **Before** any rollup, run
   `verify_audit_chain_full`; if the chain is not intact and signature-valid,
   raise SPI-G06 at top severity and mark the window **invalid** (do not report
   derived SPI values from an unverifiable chain). Per-node parko divergence
   chains are node-local and signed but not yet centrally reconciled
   (`COMPARATOR_DIVERSITY.md` §7a) — treat them as per-node sources until #165
   key-trust replication lands.
2. **Cadence.**
   - *Automated, per shift / per mission:* compute all §5.2 SPIs over the
     window; emit a rollup; alert on any threshold breach.
   - *Weekly safety review:* trend the leading SPIs (L01–L07); a sustained
     upward leading trend is an early-warning even without a lagging breach.
   - *Quarterly safety-case review:* re-validate the §4 register against
     `main` (re-triage), and re-calibrate thresholds from accumulated data.
3. **Breach → re-evaluation mapping (which SPI reopens which claim).**

   | SPI breach | Re-evaluate |
   |---|---|
   | SPI-L01 (divergence) sustained | Sn-DIV diversity argument; investigate for a real systematic fault |
   | SPI-L02 (RSS near-miss) up | ODD/speed-envelope assumptions (ADR-0001, SPEED_ENVELOPE.md); S-UL-2 |
   | SPI-L03/L04 (sensor/perception) up | SA-L1 trust-derivation assumptions; perception AoU (AOU-PERCEPTION-FRAME-001) |
   | SPI-L05/L06 (degraded/flap) up | ODD fit; recovery-hysteresis tuning (SG-013) |
   | SPI-G01 (LockedOut) up | Whether MRC engagement is masking an unresolved upstream fault |
   | SPI-G02/G03 (veto/clamp) up | Controlled-autonomy quality and S-UL-3 residual-risk assumptions |
   | SPI-G04 (failover) up | HA/platform availability claims (SG-009); G-PLATFORM |
   | SPI-G06 (integrity) **≠ 0** | **Immediate**: evidence-base compromise; freeze SPI-derived claims until resolved |
4. **Roles.** Automated rollup owner (tooling), Safety Lead (weekly trend +
   breach triage), Safety Engineer (quarterly re-validation + threshold
   calibration + sign-off — currently outstanding, see banner).
5. **Change control.** A threshold change, a new SPI, or a §4 status change is a
   safety-case change: record it in this document's history and re-baseline at
   the next quarterly review.

## 7. Limitations and honest scope

- **Not assessor-reviewed.** DRAFT; the GSN argument and the SPI selection both
  require safety-engineer sign-off before being cited as coverage.
- **Thresholds are placeholders.** §5 gives directions and a single hard limit
  (SPI-G06 = 0); all other alarm levels require field calibration.
- **Coverage of the top claim is bounded by enforcement.** G-UL-TOP is scoped
  to what the governor can bound. Hazards outside the governor's authority
  (e.g. a controlled-autonomy failure within the admissible envelope) are the
  subject of the controlled system's own case, not this one.
- **Gaps are real.** §5.3 SPIs are backlog, not signals; the §4 G-PROCESS /
  G-PLATFORM / G-COVERAGE claims remain undeveloped (AEGIS-ROAD-001).
- **Audit-chain scope.** The verifier chain is the system source of record; the
  parko-kirra divergence chain is node-local (no #165 key-trust / central
  forwarding yet).

## 8. Status

**Skeleton / drafted — pending safety-engineer review.** Not closed. The GSN
top claim, the on-main evidence register (§4), the SPI catalogue (§5), and the
monitoring plan (§6) are in place and grounded in the current code; they
require human safety-engineer review, threshold calibration, and closure of the
§5.3 instrumentation gaps before any value here is cited as validated coverage.

---

## Document control

| Field | Value |
|-------|-------|
| Prepared by | Kirra Engineering |
| Review status | Draft — pending safety-engineer review |
| Related | AEGIS-SC-000 (parent index), AEGIS-SG-001, KIRRA-OCCY-DFA-001, KIRRA-OCCY-INTEG-001, KIRRA-CERT006-DIVERSITY-001, KIRRA-TR5469-001 |
| Tracker | #117 |
| Supersedes | None |
