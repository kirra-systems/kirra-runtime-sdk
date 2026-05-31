# Occy / KIRRA — Speed-Cap Validation Matrix

**Doc ID:** KIRRA-OCCY-SPEED-VAL-001.
**Issue:** #120 (S8 — quantitative envelope characterization), Item C.
**Status:** Pilot — analytical / proven validation of the ADR-0001
assumptions. Empirical track-test validation is a pre-production gate
(§3, §4).

---

## 1. Purpose

ADR-0001 sets the Occy ODD speed cap at **22.35 m/s (50 mph / 80 km/h)**
and grounds the decision in five assumptions: a worst-case detection
range, a reaction-time budget, a comfortable deceleration, a worst-case
object class, and a condition-dependent derate rule (ADR-0002). This
document **validates** each of those assumptions against the available
evidence — without changing the cap. For each row the disposition is
one of:

- **OK-PROVEN** — directly evidenced (cite the source).
- **OK-ANALYTICAL** — analytically bounded from project data.
- **AoU-GAP** — an explicit assumption-of-use clause must be filed on
  the relevant integrator-contract issue (#126 perception, #127 actuation).
- **DEFERRED** — empirical measurement required; pre-production gate.

The cap **stays at 22.35 m/s**. AoU gaps are the integrator-side
contract surface, not Governor-internal limitations; they are filed
as comments on #126 / #127 and tracked there. The other open S8 items
(B IDC detection ranges; D SPFM/LFM/PMHF) feed forward from some of
the gaps surfaced here — cross-refs noted per row.

## 2. Validation matrix

| # | ADR-0001 assumption | Evidence type | Base-tier disposition | D1-tier improvement | Status |
|---|---|---|---|---|---|
| 1 | **R_reliable = 130 m** worst-case detection range (SPEED_ENVELOPE.md:35) | Integrator perception spec; no KIRRA measurement in the base tier | The 130 m worst-case claim rests on the integrator's perception pipeline. A **new #126 clause** captures it: *"Integrator perception shall deliver reliable detection at ≥ 130 m worst-case over the deployment ODD; degraded-condition R characterized per the SPEED_ENVELOPE.md §5–6 derate table."* | D1 IDC channel (radar + thermal + optical) provides KIRRA-measured detection range per sensor under degraded conditions — see Item B (`#120` Item B scope doc). The D1-tier ceiling on R is `min(R_radar, R_thermal, R_optical)` and is characterized empirically. | **AoU-GAP** (base) → Item-B-measured (D1) |
| 2 | **t_react = 0.5 s** reaction-time budget (SPEED_ENVELOPE.md:29) | **Composite:** Governor-verdict component PROVEN by S3 WCET; control + actuator latency residual is an AoU. | See §3 below — split into two sub-claims. The Governor contribution is `p99.9 ≈ 170–352 ns` per-command + ≤ 219 µs jitter ceiling (S3 evidence) → effectively 0 contribution to the 0.5 s budget. The residual ≤ `(0.5 s − Governor WCET) ≈ 499.78 ms` is a new **#127 AoU clause**: *"Integrator actuation pipeline (control compute + bus latency + actuator response) shall complete the safe-stop initiation within 499 ms of the Governor's MRC verdict."* | Same — t_react is platform-agnostic on the Governor side; D1 adds independent perception latency (separate budget, not part of the t_react chain). | **OK-PROVEN** (Governor) + **AoU-GAP** (actuator residual) |
| 3 | **a_comfortable = 3.0 m/s²** routine decel; a_firm = 5.0 m/s² reserve (SPEED_ENVELOPE.md:32–33) | Analytical: `VehicleKinematicsContract::max_brake_mps2 = 4.5 m/s²` (kernel reference profile + `VehicleConfig::default_urban().max_decel_mps2`) covers comfortable; firm reserve sits below the kernel cap. Road-friction degradation is an integrator-side AoU. | Vehicle hardware supports the comfortable decel: 4.5 m/s² brake capability ≥ 3.0 m/s² comfortable basis with 50 % headroom. The **wet/icy/loose-surface friction reduction** is a new #126 clause: *"Integrator vehicle / road combination shall maintain effective deceleration ≥ 3.0 m/s² over the deployment ODD; conditions below this threshold are excluded from the ODD or trigger a sub-ODD weather-derate per ADR-0002."* | Same — brake hardware not in D1's scope. | **OK-ANALYTICAL** (vehicle) + **AoU-GAP** (road friction) |
| 4 | **Worst-case detection class** = pedestrian-class object at R_reliable (SPEED_ENVELOPE.md:35, implicit in "worst-case object") | Integrator perception spec; no KIRRA measurement in the base tier | A **new #126 clause**: *"Integrator perception shall reliably detect ISO-26262-relevant worst-case object classes (pedestrian, cyclist, child-pedestrian, low-contrast debris) at ≥ R_reliable distance within the deployment ODD; FN rate per class characterized per integrator's safety case."* The 130 m + pedestrian-class combination is the dominant safety assumption; both clauses must hold for the cap. | D1 IDC's omission coverage (thermal pedestrian-class detection, radar low-RCS detection) closes the common-cause omission identified in OCCY_DFA.md §3 C5. Per-class FN rate becomes KIRRA-measured. | **AoU-GAP** (base) → Item-B-measured (D1) |
| 5 | **Weather / light derate rule** = `cap = min(subODD_nominal, weather_derate, range_supported)` (ADR-0002) | **Rule itself** PROVEN by ADR-0002 + the kernel composition pattern. **Specific derate values** (what R, a, FN-rate become in heavy rain / fog / night) are empirically bounded by sensor vendor specs (Item B) or track-test data (DEFERRED). | The min-composition rule is OK-PROVEN: the most conservative input wins, regardless of which input degrades. Specific derate values for base-tier integrators come from the integrator's perception spec (per-condition R, FN) and are recorded against #126 as part of their deployment-ODD sign-off — not a single global table. | D1 provides KIRRA-measured per-condition derates from the IDC channel; Item-B scope doc gives the radar / thermal / optical spec-sheet table that becomes a numeric derate dictionary at D1 vendor-selection time. | **OK-PROVEN** (rule) + **DEFERRED** (specific values; Item B + integrator spec) |

## 3. The t_react proof (composite assumption, row 2)

ADR-0001's 0.5 s reaction-time budget is the **total** dead time from
detection onset to actuation onset. It splits into three sub-components:

1. **Perception confirmation** (detection-to-Governor-input latency).
   Integrator-side. Out of scope for the Governor's t_react validation —
   it's measured against the integrator's perception spec and captured
   as a #126 clause.
2. **Governor verdict.** PROVEN by S3 evidence
   (`docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md` §5):
   > "CI-measured steady-state p99.9 = 170–352 ns; max with OS jitter
   > ≤ 219 µs (target hardware re-measure under S8/#120)."

   And `GOVERNOR_VERDICT_WCET_TARGET_MICROS = 100 µs` deployment target
   with a `GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS = 1000 µs`
   CI-regression gate. The per-trajectory slow-loop WCET measured on
   the clean-trajectory test is `29 µs` (S131 Phase 2A report, well
   under the 10 ms per-trajectory budget). The Governor's contribution
   to t_react is effectively negligible against the 0.5 s budget —
   **6+ orders of magnitude headroom** per-command, **4+ orders**
   per-trajectory.

   *Caveat:* the S3 measurements are on the CI build machine. Target-
   hardware re-measurement is filed as part of #120 in the
   GOVERNOR_INTEGRITY_EVIDENCE.md action checklist. For the purposes of
   the cap, the WCET on the target hardware would need to grow by 4–6
   orders of magnitude to threaten the 0.5 s budget — implausible on
   any qualified compute.

3. **Control compute + bus + actuator latency.** The residual after
   Governor verdict. Effectively `≤ (0.5 s − Governor WCET) ≈ 499.78 ms`
   with the S3-measured Governor contribution. An **explicit AoU on
   #127** (the actuation safe-stop AoU): *"Integrator actuation pipeline
   shall complete the safe-stop initiation within 499 ms of the
   Governor's MRC verdict, including control compute, bus transport,
   and actuator response."*

   For most real automotive actuation pipelines this is generous —
   typical brake-by-wire systems initiate in tens to ~200 ms. The
   clause exists to make the contract explicit, not because the budget
   is tight.

## 4. AoU gaps identified (rolled up from §2)

Four AoU clauses to file across two integrator-contract issues:

### To #126 (base Perception Input Contract)

1. **R_reliable detection range** (row 1): *"Integrator perception shall
   deliver reliable detection at ≥ 130 m worst-case over the deployment
   ODD; degraded-condition R characterized per the SPEED_ENVELOPE.md
   §5–6 derate table."*
2. **Worst-case object class** (row 4): *"Integrator perception shall
   reliably detect ISO-26262-relevant worst-case object classes
   (pedestrian, cyclist, child-pedestrian, low-contrast debris) at ≥
   R_reliable distance within the deployment ODD; FN rate per class
   characterized per integrator's safety case."*
3. **Road-friction effective deceleration** (row 3): *"Integrator
   vehicle / road combination shall maintain effective deceleration ≥
   3.0 m/s² over the deployment ODD; conditions below this threshold
   are excluded from the ODD or trigger a sub-ODD weather-derate per
   ADR-0002."*

### To #127 (actuation safe-stop AoU)

4. **Actuator-pipeline residual latency** (row 2): *"Integrator
   actuation pipeline (control compute + bus latency + actuator
   response) shall complete the safe-stop initiation within 499 ms of
   the Governor's MRC verdict."*

## 5. Conclusion

The Occy ODD speed cap of **22.35 m/s (50 mph / 80 km/h) is validated**:

- All five ADR-0001 assumptions are either PROVEN (Governor verdict
  component), OK-ANALYTICAL (vehicle brake capability, composition
  rule), or AoU-GAP with explicit clauses filed.
- The single PROVEN component (Governor verdict latency) carries 4–6
  orders of magnitude headroom against the 0.5 s reaction budget —
  the cap's t_react term is not Governor-limited.
- The AoU-GAP rows fall on the integrator contracts (#126 perception,
  #127 actuation) — they are KIRRA's documented assumptions about the
  integrator's deliverables, not gaps in the Governor's evidence.
- Empirical validation of degraded-condition derate values is DEFERRED
  to either D1-vendor measurement (Item B) or integrator-side
  characterization captured against #126 — neither blocks the cap as
  written.

**The cap is unchanged.** This document is a validation artifact, not
a re-derivation.

## 6. Interaction with S8 Item A (SG2 lateral margin)

S8 Items A and C are **independent and do not interact**:

| Item | What it characterizes | What it sets |
|---|---|---|
| A (SG2 margin) | Transverse containment uncertainty (cross-track) | `CONTAINMENT_LATERAL_MARGIN_M = 0.40 m` |
| C (this doc) | Longitudinal stopping-distance + reaction-time chain | Speed cap = 22.35 m/s (validated, unchanged) |

Item A is a per-cycle cross-track margin; Item C is a per-decision
look-ahead distance. The two formulas share no terms (Item A:
`v_lat × FTTI + ε_loc + ε_per + ε_ctrl`; Item C: `R ≥ v · t_react +
v² / 2a`) and update independent kernel parameters
(`CONTAINMENT_LATERAL_MARGIN_M` vs the integrator-derived runtime
cap composition). No cross-dependency to manage.

Both items rest on **partially overlapping AoU surfaces** — both
require integrator characterization of perception accuracy — but the
specific clauses are distinct:

- Item A's G2 AoU (#123): `ε_localization ≤ 0.10 m 95th-pct lateral`
  → cross-track pose accuracy.
- Item C's #126 clauses 1 + 4: detection range + detection class
  → forward-perception coverage.

The two AoUs are filed against different issues (G2 #123 vs Perception
Input Contract #126), so the integrator surfaces don't conflict either.

---

## Cross-references

- **ADR-0001** — `docs/adr/0001-occy-odd-speed-cap.md` (the
  assumptions this doc validates).
- **ADR-0002** — `docs/adr/0002-condition-dependent-cap-subodds.md`
  (the composition rule, row 5).
- **SPEED_ENVELOPE.md** — `docs/safety/SPEED_ENVELOPE.md`
  (KIRRA-OCCY-SPEED-001) — the SSD derivation chain footer.
- **S3 WCET evidence** — `docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md`
  §5 (the t_react proof source).
- **S8 Item A** — `docs/safety/OCCY_SG2_MARGIN.md`
  (KIRRA-OCCY-SG2-MARGIN-001) — §6 above explains the
  Item-A-vs-Item-C independence.
- **Issues** — #120 (S8 parent), #126 (perception AoU clauses 1/2/3),
  #127 (actuation AoU clause 4), #115 (S3 — provides the t_react
  Governor-component evidence), #99 (weather/posture coupling —
  row 5 runtime derate).
- **Items B + D** — open per S8 discovery sequencing. Item B
  characterizes the D1 IDC channel that closes rows 1 + 4 unilaterally;
  Item D produces the SPFM / LFM / PMHF target-vs-claimed analysis.
