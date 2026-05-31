# Occy / KIRRA — Quantitative Hardware Safety Metrics (SPFM / LFM / PMHF)

**Doc ID:** KIRRA-OCCY-QUANT-001.
**Issue:** #120 (S8 — quantitative envelope characterization), Item D.
**Status:** Pilot — **target-vs-claimed** framework using IEC TR 62380 /
SN 29500 class FIT estimates + ASIL-D class diagnostic-coverage targets
+ KIRRA-evidenced safety-mechanism mapping. All numbers in this
document are **CLAIMED-CLASS** (no specific vendor FMEDA; no empirical
hardware-element measurements). Vendor-specific FMEDA, fault-injection
LFM validation, and operational PMHF return at the pre-production
gates listed in §8.

---

## 1. Targets (ISO 26262-5:2018, ASIL D)

From `docs/safety/OCCY_FAULT_MODEL.md` §8 (was: "deferred to S8"; now
cross-references this document):

| Metric | ASIL D target | Aggregation |
|---|---|---|
| **SPFM** — Single-Point Fault Metric | **≥ 99 %** per sub-element | Per-sub-element gate |
| **LFM** — Latent Fault Metric | **≥ 90 %** per sub-element | Per-sub-element gate |
| **PMHF** — Probabilistic Metric for Hardware Failures | **< 10⁻⁸ / h = 10 FIT** | Element-level (sum across sub-elements) |

Per ISO 26262-5:2018 §8 + §9.

## 2. Methodology

PMHF upper-bound formula (conservative — used for the pilot):

> **PMHF ≈ Σᵢ [ λ_total_i × (1 − SPFM_i) ]**  across all Governor sub-elements.

The safe-failure-fraction (`λ_S / λ_total`) further reduces this in
the full ISO 26262-5 §9 calculation, but the safe-vs-dangerous split
is hardware-specific; using the upper-bound `(1 − SPFM)` form keeps
the pilot estimate conservative without requiring vendor-specific
FMEDA splits.

FIT rates are baseline IEC TR 62380 / Siemens SN 29500 class
estimates (handbook reliability data, conservative). They are
**explicitly not vendor-specific FMEDA** and are labelled
**CLAIMED-CLASS** throughout. Vendor selection replaces each row's
λ_total + SPFM with the supplier's FMEDA values, transitioning the
row to **CLAIMED-CITED** then (post-empirical) **MEASURED**.

## 3. Sub-element table

The Governor's HW element decomposes into five sub-elements. Per-row
values (FIT = failures per 10⁹ hours):

| # | Sub-element | λ_total (FIT) | Primary diagnostic mechanism | SPFM claimed | LFM claimed | Residual PMHF = λ_total × (1 − SPFM) (FIT) | Evidence status |
|---|---|---|---|---|---|---|---|
| 1 | **Lockstep SoC** (ASIL-D-class processor running the Governor binary) | 500 | DCLS (dual-core lockstep) cross-checks each instruction; lockstep CPU comparison covers most random logic faults. KIRRA's WCET gate catches execution-time anomalies that the lockstep comparator may miss (mis-fetched but consistent instructions). | **≥ 99 %** | **≥ 90 %** | **5.0** | CLAIMED-CLASS (Renesas R-Car V3M Safety Manual §4 / AURIX TC39x Safety Manual §3 — typical ASIL-D-class targets; cite the applicable section at vendor selection) |
| 2 | **ECC RAM** (4 GB; Governor runtime data) | 100 | Single-bit error correction (safe — corrected at memory controller); double-bit error detection (safe shutdown). KIRRA's NaN/Inf trap at the verdict input (Priority-0 in `validate_vehicle_command`) provides secondary defence against SEU-induced bit-flips on the verdict payload that survive ECC. | **≥ 99 %** | **≥ 90 %** | **1.0** | CLAIMED-CLASS (JEDEC JESD89 + standard automotive-grade DDR ECC) |
| 3 | **Communication bus** (Automotive Ethernet PHY + MAC, or CAN-XL physical layer) | 30 | CRC / FCS error detection at the frame layer (catches transmission corruption); per-topic timeout → fail-closed (KIRRA's posture-cache staleness + subscription-staleness paths surface this). | **≥ 95 %** | **80 %** | **1.5** | CLAIMED-CLASS (AEC-Q100 grade; IEEE 802.3 / ISO 11898 standards) |
| 4 | **Power supply** (voltage regulator + sequencer supplying the Governor SoC) | 100 | PMIC built-in voltage monitoring (comparator); power-good signal gates SoC startup. **Single-supply configuration covers approximately 90 % of failure modes**; achieving ≥ 99 % SPFM for ASIL D requires a redundant or supervised dual-supply architecture (see §4 mitigation). | **≈ 90 %** (single supply) | 70 % | **10.0** | CLAIMED-CLASS (AEC-Q101; single-supply is the **gap** identified below) |
| 5 | **Hardware watchdog** (independent oscillator-based; separate from `telemetry_watchdog.rs`) | 10 | Independent RC-oscillator timing; resets the SoC if the Governor binary fails to kick the window. IEC 60730 class oscillator + window timing. | **≥ 98 %** | **≥ 85 %** | **0.2** | CLAIMED-CLASS (IEC 60730 watchdog class) |

**Sum: 5.0 + 1.0 + 1.5 + 10.0 + 0.2 = 17.7 FIT.**

## 4. Rolled-up PMHF estimate + the power-supply gap

| Configuration | Power-sub-element SPFM | Power residual (FIT) | Total PMHF (FIT) | vs ASIL D target < 10 FIT |
|---|---|---|---|---|
| **Single supply (pilot baseline estimate)** | 90 % | 10.0 | **17.7** | **FAIL (1.77× target)** |
| **Redundant / supervised dual supply (deployment requirement)** | ≥ 99 % | ≤ 1.0 | **8.7** | **PASS (87 % of target)** |

The single-supply configuration **exceeds the PMHF target by 7.7 FIT**.
The power sub-element alone (10 FIT residual at 90 % SPFM) is the
dominant contributor; bringing it to ASIL-D class
(SPFM ≥ 99 % → ≤ 1 FIT residual) closes the gap with ~13 %
headroom against the target.

**Deployment requirement (also called out in §6):** the Governor's D3
compute must be powered by an **ASIL-D-class redundant or supervised
supply** (e.g. dual-PMIC with voted outputs, or a single PMIC with
comprehensive built-in self-test covering ≥ 99 % of fault modes).
Single-supply deployments are insufficient for the ASIL-D PMHF claim.

## 5. LFM rolled-up claim

LFM at the element level is the weighted average of per-sub-element
LFM, with weights = λ_dangerous_i (≈ λ_total × (1 − safe_fraction);
for the pilot, take λ_dangerous ≈ λ_total because the safe split is
vendor-specific). For the five sub-elements:

| Sub-element | λ_total (FIT) | LFM claimed | Contribution |
|---|---|---|---|
| Lockstep SoC | 500 | 90 % | 0.90 × 500 = 450 |
| ECC RAM | 100 | 90 % | 90 |
| Comm bus | 30 | 80 % | 24 |
| Power supply | 100 | 70 % | 70 |
| HW watchdog | 10 | 85 % | 8.5 |
| **Total** | **740** | — | **642.5 / 740 = 86.8 %** |

The **single-supply baseline rolled LFM (≈ 86.8 %) is below the
ASIL D 90 % target**. The shortfall is driven by the comm bus (80 %)
and power (70 %) sub-elements:

> **"LFM ≥ 90 % is claimed for the ASIL-D-class sub-elements
> (compute, RAM, hardware watchdog). The communication-bus and
> power sub-elements require additional coverage — supplier FMEDA
> or a periodic self-test protocol — to meet the LFM ≥ 90 % target.
> A redundant / supervised supply per §6 brings the power-sub-element
> LFM to ≥ 90 % by construction (the supervision IS the coverage)."**

With the §6 dual-supply deployment requirement (Power LFM → 90 %),
the rolled LFM becomes `(450 + 90 + 24 + 90 + 8.5) / 740 = 662.5 / 740
= 89.5 %` — still ≈ 0.5 pp short, driven by the comm bus's 80 %.
Closing the comm-bus LFM gap is a **secondary deployment requirement**:
either a higher-coverage protocol stack (TSN with redundancy) or a
periodic self-test sweeping latent PHY faults at startup +
operational intervals.

## 6. Deployment requirement — ASIL-D-class power + comm

Cascading from §4 + §5 — the integrator's hardware deployment for the
Governor element must include:

1. **Redundant / supervised dual-PMIC power supply** with SPFM ≥ 99 %
   and LFM ≥ 90 %. Verified via the supplier's FMEDA (Renesas / Infineon
   / TI ASIL-D PMIC class). This closes both the PMHF gap (single-supply
   ≈ 17.7 FIT → dual-supply ≈ 8.7 FIT) and the power-sub-element LFM gap.
2. **Communication bus with LFM ≥ 90 %** — either an ASIL-D-class TSN
   stack with redundancy, OR a documented periodic self-test protocol
   sweeping latent PHY / MAC faults (recommended interval: startup +
   every 24 h operational). The periodic self-test is an integrator
   deliverable, recorded against the Governor's deployment manual.

These two requirements are the integrator-side commitments that
back the **CLAIMED** SPFM / LFM / PMHF numbers — without them, the
ASIL D quantitative claim is not met regardless of how good the
Governor software-side evidence is. The two requirements are
captured as cross-references on `#127` (the actuation / deployment AoU
issue) and surfaced in the Governor Safety Manual.

## 7. KIRRA-evidenced safety mechanisms — sub-element SPFM contributions

The Governor's existing software safety mechanisms are the
**SPFM-contributing diagnostics** that the per-row coverage claims
in §3 rest on. Each mechanism maps to one or more sub-elements;
together they form the project's PO-1 diagnostic-coverage story (per
`OCCY_DFA.md` §2).

| KIRRA mechanism | Evidence source | Sub-element(s) supported | Contribution |
|---|---|---|---|
| **Bounded verdict WCET** (`wcet_gate::ci_gate_tests`, S3 #115) — p99.9 = 170–352 ns per-command; per-trajectory ≤ 29 µs measured; CI regression threshold = 1000 µs | `docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md` §3 + §5 | Lockstep SoC (1) | Catches execution-time anomalies the lockstep comparator may miss (mis-fetched but consistent instructions; cache-line corruption surviving ECC). Reinforces the SoC's claimed ≥ 99 % SPFM. |
| **`spawn_telemetry_watchdog`** (sensor-liveness dead-man's switch, wired in S131 Phase 4) | `src/telemetry_watchdog.rs` + `src/bin/kirra_verifier_service.rs:1751` | Comm bus (3), Power supply (4) | Catches comm-bus stalls (sensors silent past `AV_TELEMETRY_TIMEOUT_MS = 2 s`) AND power-loss-on-sensor scenarios (sensor SoC reset → no telemetry). Contributes to comm-bus SPFM detection arm. |
| **Subscription staleness** (`AdaptorState::any_subscription_stale`, S131 Phase 4 / 4b) | `crates/kirra-ros2-adapter/src/node.rs:292` | Comm bus (3) | Catches ROS-topic dropout within `SUBSCRIPTION_STALENESS_TIMEOUT_MS = 500 ms` — primary detection mechanism for the comm bus's 95 % SPFM claim. |
| **SG9 fail-closed timeout** (verdict path WCET ceiling) | `OCCY_SAFETY_GOALS.md` SG9 + `wcet_gate::ci_gate_tests` | Lockstep SoC (1) | Per-cycle timeout in the verdict path → fail-closed if WCET budget exceeded; the safe-state collapse on detected compute hang. |
| **Priority-0 NaN / Inf trap** (in `validate_vehicle_command`, P0 — `gateway/kinematics_contract.rs`) | `src/gateway/kinematics_contract.rs:223` | ECC RAM (2), Lockstep SoC (1) | Secondary defence against SEU-induced bit-flips that survive ECC's double-bit detection; also catches NaN-tainted verdict-path arithmetic from any source. Contributes to RAM SPFM "safe" arm. |
| **Posture-cache staleness fail-closed** (`should_route_command` SG9) | `src/posture_cache.rs:213` | Comm bus (3), Lockstep SoC (1) | Cache TTL expiry → cache reads as None → posture defaults to LockedOut → 503 from the actuator gate. Catches both comm hangs and verdict-loop hangs. |
| **MRC selection / fail-safe escalation** | `policy_layer.rs` (`enforce_actuator_safety_envelope` + `enforce_posture_routing`) | All sub-elements (escalation arm) | Every detected dangerous fault collapses to MRC — the "safe" half of `(λ_S + λ_DC)`. Not a per-sub-element SPFM contributor by itself, but the escalation discipline that makes any detected dangerous fault safe-by-construction. |
| **Cold-refresh deadlock fix** (telemetry watchdog `s3-watchdog-deadlock-fix`) | `src/telemetry_watchdog.rs::watchdog_sweep_once` | Lockstep SoC (1), Power supply (4) | Eliminated a fail-silent path where the watchdog could deadlock and never fire. The watchdog liveness now gates the sensor-liveness claim on §3 row 5 (HW watchdog) end-to-end. |

Read together with `OCCY_DFA.md` §2 (the qualitative diagnostic-coverage
table) and `GOVERNOR_INTEGRITY_EVIDENCE.md` (S3 — the WCET / panic-
freedom / no-alloc claims): the KIRRA software-side mechanisms cover
the diagnostic surface above which the hardware-side FMEDA (vendor-
provided at selection) supplies the per-row SPFM and LFM values. The
two halves interlock — the hardware mechanism detects the fault; the
KIRRA mechanism makes it safe.

## 8. Evidence-status legend + pre-production gates

Status taxonomy used throughout §3 + §5:

| Status | Meaning |
|---|---|
| **CLAIMED-CLASS** | The value is the ASIL-D class target; any compliant hardware in this class is expected to meet it; no specific vendor FMEDA cited. **All §3 + §5 rows in this document are CLAIMED-CLASS.** |
| **CLAIMED-CITED** | A specific vendor safety manual is cited (e.g. "Renesas R-Car V3M Safety Manual §4: SPFM = 99.2 %"). Still pre-empirical. Reached at vendor selection (next gate). |
| **MEASURED** | Empirical FMEDA validation on the production silicon, fault-injection-validated LFM, operational PMHF data. Reached at pre-production / homologation. |

**Pre-production gates** that transition rows up the status ladder:

1. **Vendor selection** → obtain specific vendor FMEDA →
   CLAIMED-CLASS → CLAIMED-CITED. Triggered by the D3-compute /
   PMIC / SoC RFP. Output: vendor-specific λ_total + SPFM + LFM
   per row in §3.
2. **Hardware integration** → fault-injection test campaign per
   ISO 26262-5 §10 → CLAIMED-CITED → MEASURED (LFM). Output:
   empirical LFM data per sub-element.
3. **Production certification** → full ISO 26262-5 §9 PMHF audit →
   CLAIMED-CITED → MEASURED (PMHF). Output: operational FIT rates
   from fleet data; final PMHF value vs the 10 FIT target.

## 9. Pilot-level claim

The Governor's hardware safety metrics are **CLAIMED at ASIL-D-class
targets** under the deployment-requirement framing in §6:

- **SPFM**: ≥ 99 % CLAIMED-CLASS for the ASIL-D-class sub-elements
  (lockstep SoC, ECC RAM, HW watchdog); ≥ 95 % CLAIMED-CLASS for
  comm bus (with per-row caveat); ≥ 99 % CONDITIONAL-CLAIMED for the
  power sub-element subject to the redundant / supervised dual-supply
  deployment requirement (§6).
- **LFM**: ≥ 90 % CLAIMED-CLASS for compute + RAM + HW watchdog;
  comm bus + power require the §6 deployment-side coverage to meet
  the 90 % target.
- **PMHF**: **17.7 FIT pilot upper bound** in the single-supply
  configuration (FAILS the 10 FIT target); **8.7 FIT in the dual-supply
  configuration** (PASSES, ~13 % headroom).

**Full empirical PMHF is a pre-production milestone, not a pilot
deliverable.** This document is the framework — vendor selection +
integration + certification fill in the rows.

---

## Cross-references

- **OCCY_FAULT_MODEL.md** §8 — the "deferred to S8" statement that
  this document closes. Cross-referenced in §1.
- **OCCY_DFA.md** §2 — the PO-1 diagnostic-coverage table; the
  KIRRA-side qualitative mapping that §7 quantifies.
- **GOVERNOR_INTEGRITY_EVIDENCE.md** — S3 evidence (WCET, panic-free,
  no-alloc); the source for the WCET-gate SPFM contribution in §7
  row 1.
- **OCCY_INDEPENDENT_DETECTOR.md** + **KIRRA-OCCY-IDC-RANGES-001**
  (S8 Item B) — sensor-element FMEDA is the integrator's / D1
  vendor's responsibility, captured separately and not summed into
  the five-row table here (the Governor element scope is the compute
  + RAM + comm + power + watchdog; the sensor element is its own
  ISO 26262-5 element with its own SPFM / LFM / PMHF).
- **KIRRA-OCCY-SPEED-VAL-001** (S8 Item C) — the §6 deployment
  requirement on power + comm cascades to #127 (actuation AoU
  issue) alongside Item C's actuator-pipeline-latency clause.
- **KIRRA-OCCY-SG2-MARGIN-001** (S8 Item A) — independent of this
  document (Item A is cross-track containment; Item D is element-
  level hardware metrics).
- **Issues** — #120 (S8 parent — items A + B + C + D), #127 (the
  deployment requirements in §6 cascade here as hardware deployment
  AoUs alongside the existing actuation safe-stop AoU).
