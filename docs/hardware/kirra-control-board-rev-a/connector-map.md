# Rev A connector map

> **Status: mechanical scaffold — no families or part numbers selected.**
> This document fixes the connector *inventory, conventions, and labeling
> scheme* so the schematic and harness work start from one shared map. It
> is deliberately not electrical (signals are defined in `interfaces.md` /
> `pin-allocation.md`) and not firmware. Connector families and mating
> part numbers are DR-2 selections, made only after the retained vendor
> harness is characterized on the unit — **no Yahboom connector pinout is
> assumed from documentation** (HDR-0006). Designators and per-connector
> pinouts freeze at DR-2; physical access and orientation freeze at DR-3;
> drawings are verified against physical mating parts at DR-4
> (`manufacturing-checklist.md`).

## 1. Connector inventory

Designators are stable from this document forward; splitting or merging a
connector (e.g. folding the driver fault lines into the driver control
connectors) is a DR-2 decision recorded here, never a silent change.

| Designator | Purpose | Signals carried (worksheet functions) | Mates with | Family | Keyed? | Locking? | Status |
|---|---|---|---|---|---|---|---|
| J1 | Jetson R2CP link | `R2CP_RX`, `R2CP_TX`, ground reference | Jetson-side harness — attach point Pending (`architecture.md` §6) | Pending | Required | Required | Pending |
| J2 | Logic power in | `LOGIC_5V_IN`, `LOGIC_GND` | External protected logic supply | Pending | Required | Required | Pending |
| J3 | E-stop loop | NC loop in/out, `E_STOP_SENSE` reference | E-stop chain per `../R2_ESTOP_SPEC.md` | Pending | Required | Required | Pending |
| J4 | Left encoder | `ENCODER_L_A`, `ENCODER_L_B`, encoder supply/return | Existing vendor encoder harness — Requires measurement | Pending | Required | Required | Pending |
| J5 | Right encoder | `ENCODER_R_A`, `ENCODER_R_B`, encoder supply/return | Existing vendor encoder harness — Requires measurement | Pending | Required | Required | Pending |
| J6 | Left driver control | `PWM_LEFT_IN1/IN2`, `DRIVER_ENABLE_HW`, `DRIVER_FAULT_L_N`, ground | Retained external left driver — Requires measurement | Pending | Required | Required | Pending |
| J7 | Right driver control | `PWM_RIGHT_IN1/IN2`, `DRIVER_ENABLE_HW`, `DRIVER_FAULT_R_N`, ground | Retained external right driver — Requires measurement | Pending | Required | Required | Pending |
| J8 | Steering | `STEERING_PWM`, optional `STEERING_FB`, servo supply pass-through decision Pending | Existing steering gear harness — connector Unverified (`firmware/rosmaster-r2/docs/HARDWARE_REFERENCE.md` §Steering) | Pending | Required | Required | Pending |
| J9 | Debug | `SWDIO`, `SWCLK`, `NRST`, ground (header or test-point group; primary path is the Nucleo's ST-LINK) | Debug probe | Pending | — | — | Pending |

Notes:

- Whether `DRIVER_ENABLE_HW` is distributed on J6/J7 or on a dedicated
  enable connector is a DR-2 decision driven by the retained drivers'
  measured enable-input wiring.
- The E-stop loop connector (J3) carries the loop, not motor power; the
  motor-supply relay chain stays external per `../R2_ESTOP_SPEC.md`.
- Status LEDs and test points are board features, not connectors, and are
  not listed here.

## 2. Family-selection rules (applied at DR-2)

1. **Keyed and locking everywhere** harness-side (RA-F13); friction-fit
   unlatched headers are not acceptable for off-board safety or
   communication signals.
2. **No cross-pluggable neighbors.** Two connectors whose accidental swap
   is electrically possible must differ in family, pin count, or key — the
   left/right encoder pair (J4/J5) and driver pair (J6/J7) are the
   highest-risk swaps and get explicit attention: same family is
   acceptable only with distinct keying or positive labeling per §4, and
   the DR-3 review must state which mechanism prevents the swap.
3. **Vendor-mating side is measured, not chosen.** J4–J8 must mate with
   the existing harness ends; their families follow from what is
   physically on the robot (bench characterization at bring-up stage 11
   feeds back into this table before fabrication of any harness).
4. Current ratings are logic-level only (RA-X3 — no motor current), but
   the encoder/servo supply pins still get a rating check at DR-2.

## 3. Pin-numbering and orientation conventions

- **Pin 1 convention:** every connector's pin 1 is marked on silkscreen
  (numeral + polarity mark), and pinout drawings are always drawn **viewing
  the board-mounted connector's mating face**, pin 1 identified. Harness
  drawings state their viewing direction explicitly on every sheet —
  "viewed from wire side" vs "viewed from mating face" confusion is the
  classic harness error this section exists to prevent.
- **Ground placement:** where the family permits, ground occupies the same
  relative position across connectors of the same family, and fast or
  safety signals get an adjacent ground (`power-and-grounding.md` §1).
- **Cable orientation:** harnesses are built so the latch/key faces a
  consistent, documented direction per connector; a cable that can be
  mechanically reversed must be electrically tolerant of it or keyed so it
  cannot happen.

## 4. Harness labels

Every harness end is labeled at build time:

```
KCB-A / <designator> / <purpose> / <harness serial>
e.g.  KCB-A / J4 / ENC-L / H-0003
```

- Labels face outward after installation (readable without unplugging).
- The label's designator must match this table — a harness relabeled for a
  different connector is a new harness with a new serial.
- Left/right pairs additionally carry color bands (colors fixed at DR-2,
  same scheme for encoders and drivers: one color = left, one = right,
  never reused for anything else on the robot).

## 5. Mating part numbers

Filled at DR-2, verified against physical parts at DR-4:

| Designator | Board connector MPN | Mating connector MPN | Contact/crimp MPN | Tooling | Source/evidence | Status |
|---|---|---|---|---|---|---|
| J1–J9 | Pending | Pending | Pending | Pending | Pending | Pending |

The table expands to one row per designator when the first family is
selected; a row without a recorded crimp tool is not buildable and blocks
DR-4.
