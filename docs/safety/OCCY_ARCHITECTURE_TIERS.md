# Occy / KIRRA — Two-Tier Architecture (base Governor + optional D1 add-on)

**Doc ID (proposed):** KIRRA-OCCY-ARCH-001.
**Status:** Architecture decision for review. Establishes KIRRA as a
vendor-neutral downstream safety governor (base) with an optional independent
detection add-on (D1) that unlocks premium safety capabilities. Supersedes the
"core IDC" framing — the IDC becomes the optional D1 module.

---

## 1. The two tiers

**Tier 1 — KIRRA Governor (base; downstream SEooC).** Owns no sensors. Consumes
the integrator's world model. Provides conservative/formal checking
(closes *uncertainty*), a **Perception Input Contract** (its assumptions of use,
§4), and an operating envelope **bounded to the delivered, currently-healthy
coverage**. Vendor- and sensor-config-neutral — drops onto any stack. As a
Safety Element out of Context, its ASIL-D claim carries an explicit
assumption-of-use on the integrator's perception for *omission* coverage
(omission is delegated + envelope-bounded, not closed unilaterally).

**Tier 2 — KIRRA D1 Independent Detection Channel (optional add-on).** KIRRA's
own diverse sensing — dedicated radar + thermal/IR + optical/polarization (water)
— on the Governor's independent compute (the settled D1–D3 spec). Plugs in as an
**additive coverage source** and **closes the omission common-cause unilaterally**
(catch-and-veto, not just bound-and-survive) for the omission-critical classes
(obstacle-in-path, VRU incl. night, water surface). Optional: an integrator who
brings only their own perception runs the base tier (bounded by their coverage);
adding D1 unlocks the premium tier.

> Customer choice, in the owner's words: *use your own perception and you run the
> base envelope; add D1 and you get the capabilities that come with it.*

---

## 2. Capability matrix — with vs. without D1

| Capability | Base (no D1) | + D1 add-on |
|---|---|---|
| Omission common-cause (DFA C5/C7) | delegated to integrator perception (assumption-of-use) + envelope-bounded | **closed unilaterally** (independent catch-and-veto) |
| Night / stationary VRU | bounded by integrator perception (weak at night) → likely day-restricted ODD | **independent thermal detection → night ODD unlocked** |
| Standing-water safety (SG4) | conservative untraversable-default, bounded by integrator coverage | **independent water-surface detection → robust SG4, larger water envelope** |
| Operating envelope | capped by integrator's delivered coverage | **extended** (more coverage → higher cap / wider ODD) |
| Safety case | ASIL-D checking + explicit perception assumption-of-use (conditional) | **bulletproof independent ASIL-D omission claim** |
| Sensor-HW-failure independence (C1/C7/C12) | shared with integrator sensors (common-mode) | **genuine modality diversity** |
| Moat / differentiation | software checker over a world model | **independent safety subsystem (True-Redundancy-class)** |

The four things the owner is buying with D1 — night VRU, water safety, envelope
size, safety case, moat — are exactly the rows that change.

---

## 3. Unifying mechanism — why D1 is an add-on, not a fork

Both tiers run the **same** envelope function:

    cap = f( confirmed sub-ODD, conditions, healthy coverage )

D1 is simply *additional healthy coverage*. The Governor bounds the envelope to
whatever coverage is present and healthy — integrator perception alone (base) or
integrator + D1 (premium) — via one code path. Adding, losing, or omitting D1 is
handled by the same sensor-availability-conditioned logic as any other sensor
(S7): more independent coverage → bigger envelope; lose D1 → contract to the
base envelope; never silently compensate. This is what makes D1 a clean optional
module rather than a second product.

---

## 4. Base-tier Perception Input Contract (assumptions of use)

What the integrator's perception must deliver for the base Governor (KIRRA
verifies at runtime and fails safe — derate/MRC — when unmet):

- **Data:** ego state; lanes / drivable space; agents with uncertainty;
  signals; the fields each Governor check consumes.
- **Health / freshness:** per-source age, confidence, availability — so KIRRA
  can assess coverage and fail toward stale/unhealthy.
- **Coverage:** reliable detection of the hazard classes out to the envelope's
  required look-ahead (the SSD = R relation); the integrator demonstrates the
  detection range KIRRA bounds the envelope to (S8-style characterization).
- **Diversity (recommended, not required at base):** if the integrator exposes
  redundant/diverse channels, KIRRA consumes and cross-checks them.
- **Verification & residual:** KIRRA checks freshness/plausibility/coverage
  indicators live and derates when they fail; assumptions it cannot verify at
  runtime are documented assumptions-of-use in the safety case.

### 4.1 SG2 drivable-space containment inputs (Option-B; pending live wiring)

The SG2 corridor-containment check (`gateway::containment::validate_trajectory_containment`,
KIRRA-OCCY-FAULT-001 / OCCY_SAFETY_GOALS.md SG2) requires three inputs that
the integrator's perception / map layer must supply through the Option-B
trajectory + corridor wiring (the follow-up issue tracks the live wire-up;
the check itself is built and unit-tested today):

- **Planned trajectory** — sequence of poses (`x`, `y`, `heading`) over a
  bounded look-ahead horizon (≤ `MAX_TRAJECTORY_HORIZON = 50` per call); pose
  convention is the rear axle to match the bicycle-model used by P6.
- **Drivable-space corridor** — left + right polylines bounding the maneuver
  envelope, sourced from **perception or map prior, NOT the planner** (the
  doer/checker independence requires the corridor be an independent
  signal from the trajectory being checked). The corridor MUST span the
  full maneuver envelope (e.g. both same-direction lanes when a lane change
  is planned) so legitimate lane changes that stay within the corridor pass
  containment.
  - Per side ≤ `MAX_CORRIDOR_VERTICES = 128` vertices.
  - Health: `confidence ∈ [0, 1]` + `age_ms`; thresholds are integrator-set
    (`min_confidence`, `max_age_ms`). The check is conservative — absent /
    stale / low-confidence corridor → `DenyCode::DrivableSpaceDeparture` →
    MRC (the SG4 untraversable-default pattern; see OCCY_FAULT_MODEL §3).
- **Vehicle footprint** — platform geometry on `VehicleKinematicsContract`
  (`width_m`, `length_m`, `overhang_front_m`, `overhang_rear_m`,
  `wheelbase_m`). Same dimensions across Nominal/MRC profiles (the vehicle
  does not shrink in degraded posture).

**Inward margin.** The check enforces a `CONTAINMENT_LATERAL_MARGIN_M`
(default 0.30 m) inside every corridor edge. The real value is derived in
S8 / #120 as worst-case (localization + perception + control) error,
analogous to the SSD ≈ R cap derivation in SPEED_ENVELOPE.md.

This contract is also the **commercial hook**: it shows an integrator exactly
what their perception must deliver — and D1 is KIRRA's answer when it can't (or
when they want the premium envelope / night / water / the ASIL-D omission claim).

---

## 5. Tier-dependent DFA disposition (C5/C7)

- **Base:** omission common-cause mitigated by conservative checking + the input
  contract + envelope-bounding; **residual = delegated** to the perception
  provider (explicit assumption-of-use). Honest and standard for an SEooC.
- **+ D1:** omission common-cause **closed unilaterally** by independent
  catch-and-veto; residual reduces to D1's own characterized FP/FN (S8).

---

## 6. Status of prior decisions

- D1–D3 (sensors / v1 scope / independent compute) → the **D1 add-on module
  spec** (was "core IDC"). OCCY_INDEPENDENT_DETECTOR.md is re-framed as the D1
  add-on.
- Sub-ODD + condition-dependent cap (ADR-0002) → unchanged; the envelope
  function is the shared mechanism that makes D1 additive.
- Speed cap / S1 goals / S4 ODD → unchanged.

Cross-refs: OCCY_INDEPENDENT_DETECTOR.md (D1 spec), OCCY_DFA.md / #114,
OCCY_SAFETY_GOALS.md / #113, OCCY_SOTIF.md + ADR-0002, S7 / #119, S8 / #120,
#124 (D1 add-on), base-tier input-contract issue (new). Register as
KIRRA-OCCY-ARCH-001; capture the two-tier decision as ADR-0003.
