# Kirra mechanical reference — platforms, classes, and evidence

> **Status: platform-direction document.** This fixes the mechanical
> *strategy* and *vocabulary*; it selects no chassis model, freezes no
> board outline, and freezes no hole pattern. Every dimension below is
> `Pending` or `Requires measurement` until the MR-1 gate (§6) and the
> DR-2/DR-3 reviews close it. Same evidence discipline as
> `pin-allocation.md` and `connector-map.md`.

## 1. Platform class vs. exact chassis model

Two different things, never conflated:

- A **mechanical platform class** is an ecosystem — a family of chassis
  sharing sourcing, spares, and general architecture. "Traxxas 1/10" is a
  platform class.
- An **exact chassis model** is one make/model/revision with measurable
  geometry. Only an exact model has mounting coordinates.

**"Traxxas 1/10" is not one universal bolt pattern.** Different Traxxas
1/10 chassis families differ in chassis-plate geometry, wheelbase, track
width, battery tray, receiver-box mounting, body-post spacing,
center-driveline clearance, shock-tower geometry, screw locations, and
available flat mounting surfaces. No document in this repository may claim
a universal Traxxas 1/10 footprint; mounting claims attach to an exact
model + revision, measured, or they are `Pending`.

## 2. Mechanical Reference A — Kirra 1/10 Platform

The **long-term mechanical reference**: a selected chassis from the
Traxxas 1/10 ecosystem (HDR-0007). Initial status — all deliberately open:

| Field | Status |
|---|---|
| Platform family | Traxxas 1/10 (adopted as direction — HDR-0007) |
| Exact chassis model | **Pending** (selected only at MR-1, §6) |
| Mounting coordinates | Pending measurement |
| Wheelbase | Pending |
| Track width | Pending |
| Available board envelope | Pending |
| Connector clearance envelope | Pending |
| Thermal/airflow envelope | Pending |

## 3. Mechanical Reference B — Yahboom ROSMASTER R2 Bring-Up Adapter

The **current hardware validation platform**:

| Field | Status |
|---|---|
| Purpose | Rev A bring-up, firmware validation, and HIL |
| Mechanical role | Temporary adapter/reference platform |
| Long-term status | **Not** the final Kirra mechanical standard |

The existing Yahboom vehicle **must remain usable during the migration** —
the firmware roadmap's staged migration (stages 0–4) runs on it, and
nothing in the mechanical strategy invalidates it. Note the R2's chassis
dimensions and mounting-hole coordinates are **not documented anywhere in
this repository** (`docs/hardware/HARDWARE_FINDINGS_R2X3.md` records
electronics behavior, explicitly "NOT immutable chassis identity");
Reference B mounting evidence is gathered with the same §5 worksheet when
Rev A's R2 fit is designed.

## 4. Board compatibility classes

Vocabulary for every current and future Kirra board:

| Class | Meaning |
|---|---|
| **Class A** | Compatible with the selected Kirra/Traxxas 1/10 mechanical reference (Reference A — exists only after MR-1). |
| **Class B** | Compatible with the Yahboom ROSMASTER R2 adapter pattern (Reference B). |
| **Class A+B** | Supports both, through native holes, slots, or a documented adapter plate. |
| **Unclassified** | Mechanical fit has not been verified. |

A class is **earned by evidence** (measured dimensions + 3D fit + the §5
worksheet), never asserted from intent. **Rev A is currently
`Unclassified`** — it is designed *toward* Class B (or A+B via adapter
plate) but is not marked Class A, B, or A+B until dimensions and a 3D fit
are verified.

## 5. Mechanical evidence worksheet

One worksheet instance per (board, reference platform) pair. All
unsupported values remain `Pending` / `Requires measurement`.

| Field | Reference A (Kirra 1/10) | Reference B (Yahboom R2) |
|---|---|---|
| Reference | A | B |
| Exact chassis make/model | Pending (MR-1) | Yahboom ROSMASTER R2 — unit in hand |
| Revision | Pending | Requires inspection (unit revision unrecorded) |
| Measurement method | Pending | Pending |
| Chassis material | Pending | Requires inspection |
| Usable flat area | Pending | Requires measurement |
| Hole coordinates | Pending measurement | Requires measurement |
| Hole diameter/thread | Pending | Requires measurement |
| Board maximum X/Y/Z envelope | Pending | Requires measurement |
| Wheelbase | Pending | Requires measurement (calibration inputs exist; mounting-grade measurement does not) |
| Track width | Pending | Requires measurement |
| Battery clearance | Pending | Requires measurement |
| Driveline clearance | Pending | Requires measurement |
| Steering clearance | Pending | Requires measurement |
| Suspension clearance | Pending | Requires measurement |
| Body clearance | Pending | Requires measurement |
| USB/ST-LINK access | Pending | Requires measurement |
| Cable bend radius | Pending | Requires measurement |
| Connector insertion clearance | Pending | Requires measurement |
| Cooling/airflow | Pending | Requires measurement |
| Vibration isolation | Pending | Requires measurement |
| Photo/drawing reference | Pending | Pending |
| Status | **Pending — blocked on MR-1** | **Pending — Unclassified until measured** |

## 6. MR-1 — Chassis Selection Review (new gate)

A mechanical review gate, added alongside (not replacing) DR-1…DR-4
(`design-reviews.md`). MR-1 selects the exact Reference A chassis. It may
run in parallel with DR-1/DR-2, but **no Class A mounting pattern may be
frozen before MR-1 passes**, and DR-3 (which freezes mechanical fit) can
freeze only fits whose reference platform evidence exists — Reference B
fit needs the §5 Reference B column closed; Class A fit needs MR-1.

MR-1 requires, all recorded:

- exact Traxxas make/model and revision;
- dimensional measurements (the §5 Reference A column);
- weight/payload estimate (carrier + Nucleo + Jetson + battery + sensors);
- wheelbase and track-width suitability;
- steering geometry review;
- drivetrain compatibility;
- available electronics envelope;
- battery and Jetson mounting plan;
- suspension travel clearance;
- cable-routing plan;
- parts availability;
- adapter-plate concept;
- a 3D model or measured drawing.

## 7. PCB and adapter strategy

Principles binding on Rev A layout (DR-3) and successors:

1. **Do not force the main PCB to imitate every chassis hole pattern.**
   Prefer a stable controller-board outline plus replaceable mechanical
   **adapter plates** per reference platform; board and adapter plate are
   independently replaceable.
2. **Keep high-stress chassis fasteners away from fragile connector
   areas.**
3. **Standoffs or vibration-isolating mounts** where appropriate; the
   vibration approach per platform is a §5 worksheet field.
4. **The Nucleo headers never carry chassis loads** — mechanical retention
   of the Nucleo is the carrier's job (mounting provisions per HDR-0001),
   not the Morpho pins'.
5. **Mounting holes get copper and component keep-outs.**
6. **Verify underside clearance to conductive chassis material** (plates
   and standoff choices per platform; a §5 field).
7. **Preserve access**, mounted, to: ST-LINK/USB, reset, SWD, status
   LEDs, the E-stop connector, the R2CP connector, and test points.
8. **Harnesses are never structural restraints** — strain relief is
   mechanical and belongs to the board/plate/chassis
   (`connector-map.md` §1), never to wire tension.

## 8. Vibration and cable strain

- Every off-board harness gets strain relief at both ends; connector
  retention (latch) is not strain relief.
- Cable routing respects the minimum bend radius of the chosen wire
  (recorded per harness at DR-2 in `connector-map.md`); routing paths and
  bend radii on the vehicle are §5 worksheet fields per platform.
- Vibration exposure differs sharply between Reference B (indoor bench
  robot) and a Traxxas 1/10 platform (higher speeds, suspension
  transients); mounting and isolation decisions are made per platform and
  re-reviewed at MR-1 — bench-robot fastening practice is not assumed to
  transfer.

## 9. 3D-fit evidence

A mechanical fit claim requires, per platform:

- a STEP/3D model (or measured drawing) of the board + adapter plate +
  chassis interface, reviewed at DR-3/DR-4
  (`manufacturing-checklist.md`);
- a physical trial fit on the actual chassis unit before fabrication
  release of any adapter plate;
- photos/drawings archived as the §5 worksheet's evidence.

## 10. What is frozen today

Nothing. **No board outline and no hole pattern is frozen.** This document
freezes only the strategy (platform class direction, the A/B reference
split, the class vocabulary, and the MR-1 gate). Everything dimensional is
`Pending`.

## Cross-references

- `decisions/HDR-0007-traxxas-1-10-mechanical-reference.md` — the platform
  decision and its limits
- `design-reviews.md` — MR-1's position relative to DR-1…DR-4
- `connector-map.md` — harness contract (strain relief, routing, labels)
- `firmware/rosmaster-r2/docs/ROADMAP.md` §Deployment reality — the staged
  migration Reference B continues to serve
- Runtime note: vehicle geometry in the stack is **calibration-owned**
  (e.g. the firmware's Ackermann geometry validates a configured
  `wheelbase_m`; steering uses calibrated pulse widths; the verifier's R2
  contract profile is an ODD/speed class, not chassis geometry) — so a
  chassis change means **recalibration and contract-profile review, not
  code changes**, and nothing in the runtime depends on Yahboom mounting
  geometry.
