# KIRRA Governor — Safety Manual (SEooC)

**Doc ID (proposed):** KIRRA-OCCY-MANUAL-001.
**Status:** Working draft. Consolidates the in-progress safety case into the
single SEooC deliverable for integrators/assessors. Living document — it
references the detailed docs rather than reproducing them, and §7 is the honest
map of what is not yet complete. Paraphrases standards; clause text to be
verified before formal use.

---

## 1. Element & scope

KIRRA is a **vendor-neutral runtime safety governor** — an independent checker
that supervises an AI planner (Occy). The planner proposes; the Governor accepts,
rejects (→ MRC), or clamps. It is a **Safety Element out of Context (SEooC)**,
living *downstream* of perception and owning no sensors by default. Two tiers:
**base** (consumes the integrator's world model, envelope bounded to delivered
coverage) and an optional **D1 add-on** (KIRRA's own diverse sensing, closes the
omission common-cause). Refs: OCCY_ARCHITECTURE_TIERS.md, ADR-0003.

## 2. Safety claims

Decomposition: **ASIL D = D(D) [Governor] + QM(D) [planner, disciplined-QM]**
(OCCY_DFA.md, ADR-0003). Validity rests on two proof obligations — **PO-1**
diagnostic coverage and **PO-2** independence (the DFA). Per-goal enforcement
(OCCY_SAFETY_GOALS.md, TRACEABILITY_MATRIX.md):

| Goal | What | Disposition |
|---|---|---|
| SG1 longitudinal collision (RSS) | ASIL D | enforced (per-command; per-horizon pending #131) |
| SG2 road/lane departure | ASIL D | **enforced** (per-trajectory; Option-B adapter slow loop, #128/#131) |
| SG3 dynamic envelope | ASIL D | enforced |
| SG4 water / SG5 commit-zone / SG6 post-collision | B/B/A-elevated | **delegated** (AoU / #126), D1 closes omission |
| SG7 teleop parity | ASIL D | enforced (structural, doer-agnostic) |
| SG8 MRC reachability | ASIL D | enforced (Governor side; standing-MRC delegated to planner) |
| SG9 fail-closed | ASIL D | enforced (WCET-bounded timeout) |

## 3. Assumptions of use (the integrator MUST satisfy)

The safety claims hold only under these conditions; violating one voids the
corresponding claim.
- **Perception input contract (#126):** the world model must deliver the required
  data, per-source health/freshness, and coverage — including, for SG2, the
  planned trajectory + the drivable-space corridor (from perception/map, not the
  planner). *Status: filed in the AoU register —* `AOU-PERCEPTION-RANGE-001`
  (≥ 130 m worst-case detection), `AOU-PERCEPTION-CLASS-001` (worst-case object
  classes at ≥ R_reliable), `AOU-VEHICLE-FRICTION-001` (effective decel ≥ 3.0 m/s²);
  see `ASSUMPTIONS_OF_USE.md`. Dispositions remain `AoU-GAP` / `OK-ANALYTICAL`.
- **Actuation output contract (#127):** the actuator must safe-stop on loss of a
  valid verdict within a bounded `T_safe-stop`, and complete safe-stop initiation
  within `T_safe-stop = 499 ms` of the Governor's MRC verdict. *Status: filed as*
  `AOU-ACTUATION-LATENCY-001`, with the ASIL-D-class power / comm-bus deployment
  gates as `AOU-HW-POWER-001` (DR-1) and `AOU-HW-COMMBUS-001` (DR-2); see
  `ASSUMPTIONS_OF_USE.md`. `T_safe-stop` quantified at **499 ms** (Governor-WCET
  S3-PROVEN; actuator residual `AoU-GAP`).
- **Compute / freedom-from-interference (D3):** the Governor runs on compute
  separate from the planner (separate SoC preferred; isolated partition minimum).
- **Configuration constraints:** speed cap = f(validated detection range)
  (ADR-0001); condition-dependent cap + sub-ODDs (ADR-0002); two-tier coverage
  (ADR-0003); platform config = the kinematic contract + vehicle footprint.

## 4. Integrity evidence

| Element | Status |
|---|---|
| Bounded WCET → SG9 timeout (+ CI regression gate) | **done** (S3; re-derive for the trajectory verdict at #131) |
| panic=abort → death ⇒ fail-closed | **done** (structurally confirmed) |
| Requirements traceability matrix + CI gate | **done** |
| MC/DC structural coverage | pending |
| Freedom-from-interference evidence doc | pending |
| Qualified toolchain (Ferrocene) | pending (target-support eval) |

Ref: GOVERNOR_INTEGRITY_EVIDENCE.md.

## 5. Safe states & fault handling

Fail-safe posture: any unmaskable fault → fail-closed → MRC; HA gives partial
fail-operational recovery. Fault model (OCCY_FAULT_MODEL.md) covers process
death (immediate MRC + ~10 s recovery), WCET/SG9 timeout, compute fault, mutex
poison, audit-queue-full (forensic only), stale input, posture Unknown, and
sensor degradation (envelope contraction → MRC floor). Degraded-mode: the
permitted envelope is a function of independently-assessed healthy coverage;
default-deny / fail-toward-smaller; planner adapts style, Governor enforces
envelope.

## 6. ODD & operating constraints

Deployment ODD: urban/surface driverless, day+night+all-weather, **≤50 mph /
80 km/h** (Sub-ODD A active; controlled-access highway Sub-ODD B defined, not
activated — #125). Three hard must-nots: enter flooded water (SG4), strike trains
(SG5), drag a person (SG6). SOTIF triggering-condition catalog + the
condition-dependent cap apply. Refs: OCCY_SOTIF.md, SPEED_ENVELOPE.md,
ADR-0001/0002.

## 7. Limitations & open evidence (honest residual map)

- Per-command enforcement plus the per-trajectory Option-B adapter path (#131);
  any remaining RSS-over-horizon hardening continues under **#131**.
- SG2 drivable-space containment is **enforced live** — wired into the Option-B
  adapter slow loop (`kirra-trajectory` validation; a containment failure
  collapses the per-asset slot so the fast loop publishes MRC) (#128/#131).
- Base-tier **omission** common-cause is **delegated** (an AoU); the D1 add-on
  (#124) closes it unilaterally.
- Coverage gap: **G1 occlusion-aware caution** (#122).
- Common-cause: **G2 localization integrity** (#123).
- **Placeholder values pending S8 (#120):** IDC detection ranges, the speed-cap
  range assumption, and the quantitative metrics (SPFM/LFM/PMHF). (The SG2
  lateral margin is now characterized — `CONTAINMENT_LATERAL_MARGIN_M = 0.40 m`,
  KIRRA-OCCY-SG2-MARGIN-001 / `docs/safety/OCCY_SG2_MARGIN.md`.)
- **Pending integrity evidence:** MC/DC, the FFI doc, Ferrocene adoption.
- **AoU contracts written:** perception (#126) and actuation (#127) are filed in
  the AoU register (`ASSUMPTIONS_OF_USE.md`): `AOU-PERCEPTION-RANGE-001`,
  `AOU-PERCEPTION-CLASS-001`, `AOU-VEHICLE-FRICTION-001`,
  `AOU-ACTUATION-LATENCY-001`, plus deployment gates `AOU-HW-POWER-001` (DR-1) and
  `AOU-HW-COMMBUS-001` (DR-2). Dispositions remain honest (`AoU-GAP` /
  `OK-ANALYTICAL` / `OK-PROVEN`) — the integrator obligations are recorded, not
  discharged.

## 8. Integration guidance

Deliver the perception input contract (§3); guarantee the actuation safe-stop;
run the Governor on separate compute; provide the platform config (kinematics +
footprint); configure the ODD/envelope; choose the tier (base, or +D1 for
omission-independence / night-VRU / water / a larger envelope / the unilateral
ASIL-D omission claim).

Refs: OCCY_SAFETY_GOALS.md, OCCY_DFA.md, OCCY_SOTIF.md, SPEED_ENVELOPE.md,
OCCY_ARCHITECTURE_TIERS.md, OCCY_INDEPENDENT_DETECTOR.md, GOVERNOR_INTEGRITY_EVIDENCE.md,
OCCY_FAULT_MODEL.md, TRACEABILITY_MATRIX.md, ADR-0001/0002/0003, #120/#122/#123/#124/#126/#127/#131.
Register as KIRRA-OCCY-MANUAL-001.
