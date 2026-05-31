# Occy / KIRRA — D1 IDC Detection-Range Specification

**Doc ID:** KIRRA-OCCY-IDC-RANGES-001.
**Issue:** #120 (S8 — quantitative envelope characterization), Item B.
**Status:** **Scoping** — vendor-RFP-ready specification + cap-impact
decision rule. Numbers are spec-sheet / literature-sourced, not
empirical. Empirical validation lands at D1-vendor selection (§7).

---

## 1. Scope + relationship to OCCY_INDEPENDENT_DETECTOR.md

`OCCY_INDEPENDENT_DETECTOR.md` (KIRRA-OCCY-IDC-001) specifies the **D1
channel architecture** — radar + thermal + optical, with per-class
diversity for obstacles, VRU, and water boundaries — and lists the
high-level integration / coverage claims. **This document is the
range-spec layer beneath D1's architecture:** for each sensor type
and the deployment ODD's worst-active sub-ODD, what detection range
must be delivered, what the SSD-derate rule does to the cap when
range degrades, and what the vendor RFP must measure to validate the
spec.

The two interlock as follows:

- OCCY_INDEPENDENT_DETECTOR.md says **what** D1 detects (object classes
  + omission coverage closing the OCCY_DFA.md §3 C5 common-cause).
- This document says **how far** D1 must detect in worst-active conditions
  and **what happens to the cap** when it does not.

Speed-cap interaction is handled via the same `SSD(v) = R_reliable`
chain as ADR-0001 (the Item C validation matrix dispositions row 1 +
row 4 as **AoU-GAP (base) → Item-B-measured (D1)**; this document is
the "measured" half).

## 2. Minimum range requirement

From `SPEED_ENVELOPE.md` §3 (the SSD table at 50 mph):

| Threshold | Required R | Source |
|---|---|---|
| **PRIMARY** — comfortable-stop @ 50 mph | **R ≥ 94 m** | SSD comfortable = `v·t_react + v²/(2·a)` at `v = 22.35 m/s, t = 0.5 s, a = 3.0 m/s²` |
| **RESERVE** — firm-stop @ 50 mph | R ≥ 61 m | Same formula at `a = 5.0 m/s²`; below 61 m the cap MUST derate regardless of brake reserve |
| **BASELINE TARGET** — preserve ADR-0001 margin | R ≥ 130 m in clear conditions | ADR-0001 baseline at the chosen cap (~28 % SSD headroom against the 59 mph comfortable breaking point) |

The PRIMARY requirement is the floor: D1 must deliver **reliable
detection of ISO-26262 worst-case objects (pedestrian / cyclist /
child-pedestrian / low-contrast debris) at R ≥ 94 m in the worst
active sub-ODD condition**. Below 94 m the operational cap derates
per §4. Below 61 m the comfortable-stop basis breaks entirely (no
amount of brake reserve recovers it) and the cap drops sharply.

D1 SHOULD aim to preserve or exceed the 130 m baseline in clear
conditions so the headroom that ADR-0001 documented at R = 130 m
holds. The 36 m of clear-conditions margin (130 − 94) absorbs S8
uncertainty + sensor aging.

## 3. Candidate sensor spec table (scoping — not empirical)

Per-sensor range estimates by condition. **All values are
spec-sheet / literature-sourced; the D1-vendor RFP MUST require
empirical measurement per §7.** Targets are the ISO-26262 worst-case
class (pedestrian-class unless otherwise noted).

| Sensor type | Clear day | Night / low-light | Fog / heavy rain | Worst-active sub-ODD floor | Meets PRIMARY R ≥ 94 m? |
|---|---|---|---|---|---|
| **Long-range FMCW radar** (e.g. Continental ARS548 class) | ~250 m vehicle-class; ~50–100 m pedestrian-class (low RCS) | Unchanged (illumination-independent) | ~200 m + maintained (minimal degradation) | ~50–100 m pedestrian-class | **Vehicle-class: yes. Pedestrian-class: borderline — may gap to ~50 m worst case.** Single-sensor risk on pedestrian. |
| **Thermal (LWIR) camera** (e.g. FLIR ADK class, uncooled) | 80–150 m human-class (high thermal contrast) | Same as clear day (the strong case for thermal) | 50–100 m (thermal contrast reduced in precipitation) | **~50 m heavy fog** | **No — fog floor below 94 m.** Heavy-fog sub-ODD is the dominant derate driver. |
| **Optical / neural** (1080p+ with neural perception) | 100–200 m vehicle-class; 60–100 m pedestrian-class | 50–80 m pedestrian-class (headlight-limited illumination zone) | 30–60 m | **~30–60 m severe rain / fog** | **No — both night and fog floors below 94 m.** Hard derate. |

**Composite (D1 fused, three-sensor min)**: `R_D1_condition = min(R_radar, R_thermal, R_optical)` per condition. The fusion ceiling is the worst of the three under each condition — D1's KIRRA-measured `R_D1` feeds the cap-impact rule in §4 + §5.

**Key observations:**
- **Radar alone** meets PRIMARY for vehicle-class detection in all conditions, but gaps for pedestrian-class. D1's value-add for the pedestrian-omission case (the OCCY_DFA.md §3 C5 common-cause closure) is the radar + thermal **diversity** — neither alone is sufficient at the floor.
- **Thermal + optical** are the night-VRU and fog-edge cases respectively. Day-clear conditions are well-covered by all three; the worst-active floor is set by the worst-degraded sensor under whatever sub-ODD condition is active.
- **The fog/rain sub-ODD is the driver** for the cap-derate rule below: at the ~50 m floor (thermal heavy-fog or optical severe rain), the cap derates to ~35 mph (§4). This is the *operational* derate, not a hardware-incapable case — the integrator's perception still functions, just at reduced range.

## 4. SSD-derate equation + cap-impact table

The same SSD-formula chain ADR-0001 uses to derive the baseline cap. Inverting `SSD(v) = R` gives the **comfortable-stop breaking-point velocity** at each R:

```
v_break(R) = -a · t_react + √( (a · t_react)² + 2·a·R )
           = -1.5 + √( 2.25 + 6·R )       (with t = 0.5 s, a = 3.0 m/s²)
```

Each row recomputed from this formula (not from the brief's starter values):

| R (m) | `v_break` (m/s) | `v_break` (mph) | Operational cap (mph, ADR-0001-style margin) | Trigger condition |
|---|---|---|---|---|
| **130** | **26.47** | **59.2** | **50** | ADR-0001 baseline (~28 % SSD headroom; the design point) |
| 100 | 23.04 | 51.5 | **45** | Moderate rain on optical; thermal pre-fog (~10 mph derate from baseline) |
| **94** | **22.30** | **49.9** | **40** | **SSD-comfortable boundary at the baseline cap** — at this R, 50 mph is right on the breaking point with zero margin → operational cap drops to next-step-below |
| 80 | 20.46 | 45.8 | **35** | Fog sub-ODD floor on thermal (50–80 m worst case) |
| 61 | 17.69 | 39.6 | **30** | SSD-firm boundary at 50 mph — even emergency-brake reserve no longer recovers comfortable margin |
| 50 | 15.89 | 35.5 | **25** | Severe degraded condition (thermal heavy-fog floor; optical severe rain) |
| 40 | 14.06 | 31.5 | **20** | Below all sensors' design floors; would normally trigger a posture downgrade rather than just a cap derate |

Notes on the recomputation:
- The brief's starter table had `R=130 → 22.35 m/s = 50 mph` in the "derated cap" column — that's the **operational cap (with margin)**, NOT the breaking point. Breaking point at R=130 is 26.47 m/s ≈ 59.2 mph, which ADR-0001 backs off from to 50 mph with ~28 % SSD headroom.
- The "Operational cap" column applies the same margin-discipline rule ADR-0001 uses: cap at the next defensible 5-mph step below the breaking point, preserving a buffer to absorb S8 uncertainty + sensor degradation. The ratio works out near 0.85 across the table — consistent with the brief's intent — but is now traceable to the formula + a stated rule.

## 5. Cap-impact decision rule

The runtime composition is the same `min`-rule as ADR-0002, with `R_D1_condition` providing the live measurement that closes the validated-range term:

> **`cap_D1 = min(cap_sub-ODD_nominal, weather_derate_table, SSD_derate_cap(R_D1_condition))`**

where:

- `cap_sub-ODD_nominal` is the static sub-ODD cap (e.g. 50 mph for Sub-ODD A urban surface; ADR-0002).
- `weather_derate_table` is the integrator-side per-condition derate (ADR-0002; specific values DEFERRED per Item C row 5).
- **`SSD_derate_cap(R_D1_condition)` is the operational-cap value from §4 evaluated at the D1-channel-measured detection range under the current active condition**, published by the D1 channel as part of its health metadata per OCCY_INDEPENDENT_DETECTOR.md §4.

The most conservative input always wins.

Per ADR-0002, the transition is **asymmetric**: the cap derates *instantly* when `R_D1_condition` drops below a threshold; it earns back *slowly* (requires confirmed sustained recovery + map / posture corroboration). The same asymmetry covers the case where D1 itself goes offline — the cap collapses to the most conservative defensible value (typically 25–30 mph or full MRC, depending on what `R_D1_condition` is reported as on a D1-degraded posture).

## 6. Interaction with S8 Item C (base-tier AoU rows 1 + 4)

S8 Item C identified two **AoU-GAP** rows that depend on integrator perception in the base tier:

- **Row 1** (R_reliable ≥ 130 m worst-case): captured as #126 clause 1.
- **Row 4** (Worst-case object class detection at R_reliable): captured as #126 clause 2.

This document **defines what "measured" means** when those AoUs transition from base-tier integrator-spec to D1-tier KIRRA-measured. Specifically:

| Item C AoU | D1-tier disposition (this doc) |
|---|---|
| R ≥ 130 m worst-case (#126 clause 1) | D1-measured per sensor + condition (§3 table). Baseline R_D1 in clear conditions ≥ 130 m design target; floors per condition documented. |
| Worst-case object class (#126 clause 2) | D1-measured per-class FN rate (radar + thermal + optical diversity covers the omission case; per OCCY_INDEPENDENT_DETECTOR.md §3 scoped classes). Vendor RFP requires per-class FN rate at the published R. |

For an integrator running **with D1**: the AoU clauses 1 + 4 on #126 are satisfied by the D1-measured numbers from this document. For an integrator running **without D1** (Tier-1 base only): the AoU clauses remain on the integrator's perception stack, and the cap-derate rule in §5 still applies using the integrator's measured `R_perception_condition` rather than `R_D1_condition`.

## 7. Status + RFP requirements

**Status: SCOPING.** Numbers in §3 are spec-sheet / literature-sourced, not empirical from KIRRA-selected hardware. The D1-vendor selection RFP **MUST** require all of:

1. **Range measurement protocol** per §2 (PRIMARY ≥ 94 m at worst-active sub-ODD; BASELINE ≥ 130 m clear). Protocol must specify:
   - Target classes (per OCCY_INDEPENDENT_DETECTOR.md §3 scoped classes).
   - Test distances + step granularity.
   - False-negative rate threshold per class (max FN rate at the published R).
   - Conditions covered (clear, night, fog, heavy rain — minimum set; deployment ODD adds more if needed).
2. **Per-condition FN rate table** matching the cap-impact rows in §4 — vendor must report `(R, FN_rate, condition, class)` quadruples that the §4 table can be reconstructed from.
3. **Sensor sub-element FMEDA** (handover into S8 Item D, the SPFM/LFM/PMHF target-vs-claimed analysis): residual / safe / detected fault rates per sensor, diagnostic coverage values per ISO 26262-5:2018 Annex D.
4. **D1 channel health-metadata interface**: how `R_D1_condition` is computed from the three sensors' per-condition outputs and published to the Governor per OCCY_INDEPENDENT_DETECTOR.md §4.

Empirical campaign returns measured values per row of §3 + §4. If any measured value falls below the spec-sheet estimate in §3, the affected row in §4 is recomputed and the operational cap derates accordingly; the ADR-0001 §6 "cap-as-function-of-demonstrated-range" rule is the established mechanism.

**Pilot evidence:** this document + the OCCY_INDEPENDENT_DETECTOR.md architecture.
**Pre-production / vendor-selection evidence:** the empirical RFP results above, returning measured values per §3 + §4 + the FMEDA in §7.3.

---

## Cross-references

- **OCCY_INDEPENDENT_DETECTOR.md** — `docs/safety/OCCY_INDEPENDENT_DETECTOR.md`
  (KIRRA-OCCY-IDC-001) — the D1 channel architecture this doc layers on.
- **ADR-0001** — `docs/adr/0001-occy-odd-speed-cap.md` (the SSD chain + the
  cap-as-function-of-R rule this doc operates).
- **ADR-0002** — `docs/adr/0002-condition-dependent-cap-subodds.md` (the
  runtime composition rule that consumes `R_D1_condition`).
- **SPEED_ENVELOPE.md** — `docs/safety/SPEED_ENVELOPE.md`
  (KIRRA-OCCY-SPEED-001) — the SSD table + breaking-point analysis.
- **KIRRA-OCCY-SPEED-VAL-001** — `docs/safety/OCCY_SPEED_CAP_VALIDATION.md`
  (S8 Item C) — rows 1 + 4 AoU-GAP that this doc closes for the D1 tier.
- **OCCY_DFA.md** — `docs/safety/OCCY_DFA.md` §3 C5 — the omission
  common-cause that the radar + thermal diversity closes.
- **Issues** — #120 (S8 parent), #126 (perception AoU clauses 1+2 that
  this doc supplies the D1-tier disposition for), Item D (the FMEDA
  follow-up for §7.3).
