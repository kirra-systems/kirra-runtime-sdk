# HDR-0007: Adopt a Traxxas 1/10 ecosystem as the long-term Kirra mechanical reference

| Field | Value |
|---|---|
| Status | Proposed — platform direction; exact model gated on MR-1 |
| Date | 2026-07-28 |
| Cross-refs | `../mechanical-reference.md` (normative vocabulary + MR-1 gate); HDR-0002 (carrier before custom PCB); `firmware/rosmaster-r2/docs/ROADMAP.md` §Staged migration; `docs/adr/0014-rosmaster-r2-orin-nx-kirra-integration.md` |

## Context

The Yahboom ROSMASTER R2 is the bring-up robot: the staged migration
(vendor MCU → R2CP bridge → Kirra firmware → Rev A carrier) runs on it and
must keep running on it. But making Yahboom-specific chassis geometry,
mounting, replacement parts, and harness arrangements the *long-term*
mechanical standard would tie Kirra hardware to a single robotics-kit
vendor's chassis lifecycle. The electrical and protocol architecture is
already chassis-independent (R2CP, the actuation authorization boundary,
and calibration-owned vehicle geometry carry no Yahboom mounting
assumptions), so the mechanical layer is the only place this dependence
could take root.

## Decision

Adopt the **Traxxas 1/10-scale chassis ecosystem as the long-term Kirra
mechanical reference platform class** (Mechanical Reference A), while the
**Yahboom R2 remains the near-term bring-up vehicle** (Mechanical
Reference B). Specifically:

- Kirra will **not** make Yahboom-specific geometry the long-term
  mechanical standard; Reference B is a temporary adapter/reference
  platform that stays usable throughout the migration.
- The **exact Traxxas chassis model is intentionally unresolved**. A model
  is selected only after the MR-1 review closes measured fit, payload,
  clearance, wheelbase, steering, suspension, and parts-availability
  evidence (`../mechanical-reference.md` §6).
- **Adapter plates are preferred** over distorting the controller PCB
  outline to fit multiple unrelated chassis
  (`../mechanical-reference.md` §7).
- **Electrical and protocol architecture remains independent of the
  chassis selection** — R2CP, the hardware actuation gate, and the
  verifier/consumer authorization chain are unchanged by any mechanical
  outcome of this decision.

## Rationale

- Mature replacement-parts ecosystem and long-term mechanical
  availability from a large, stable RC vendor.
- Multiple chassis options within one platform class, so the selection at
  MR-1 can trade wheelbase, track width, and envelope against measured
  needs instead of accepting one kit's geometry.
- Easier sourcing of suspension, steering, wheels, driveline, battery
  mounts, and body hardware.
- Lower lifecycle dependence on a robotics-kit vendor for chassis spares.

## Limits — what this decision does NOT claim

- It does **not** claim "Traxxas 1/10" is one universal bolt pattern:
  chassis families differ in plate geometry, wheelbase, track width,
  battery tray, receiver-box mounting, body-post spacing, driveline
  clearance, shock towers, screw locations, and flat mounting area
  (`../mechanical-reference.md` §1). Compatibility is claimed only against
  an exact measured model + revision.
- It does not select a model, freeze a board outline, freeze a hole
  pattern, or mark any board Class A — all gated on MR-1 and the §5
  evidence worksheet.
- It does not retire the R2: bring-up, firmware validation, and HIL stay
  on Reference B, and Rev A's first fit target remains the R2.

## Consequences

- `../mechanical-reference.md` becomes the normative home for platform
  classes, compatibility classes (A / B / A+B / Unclassified), the
  evidence worksheet, and MR-1; Rev A is **Unclassified** until measured.
- A chassis change is a *mechanical + calibration* event, never a
  protocol or authorization event: recalibration (Ackermann geometry,
  steering pulse widths, encoder constants) and contract-profile review
  are expected; R2CP and the safety boundary are not renegotiated.
- Future boards state their compatibility class in their own
  documentation, earned by the worksheet, not asserted.
