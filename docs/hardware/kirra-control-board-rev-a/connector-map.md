# Rev A connector map — the harness contract

> **Status: structural, not prescriptive.** This document locks down the
> connector *conventions* (§1) and the *rules* (§4) now, and deliberately
> leaves every electrical/mechanical selection (§2) `Pending` until DR-2 —
> the same evidence discipline as `pin-allocation.md`. It is not
> electrical (signals are defined in `interfaces.md` / `pin-allocation.md`)
> and not firmware. **No Yahboom connector pinout is assumed from
> documentation** (HDR-0006): the vendor-mating side of every harness is
> measured on the unit, never chosen from memory.

## 1. Locked structural conventions (frozen with this document)

1. **Designator scheme.** Board connectors are `J1, J2, …`, stable from
   this document forward. A designator is never reused for a different
   function; splitting or merging a connector is a DR-2 decision recorded
   here, never a silent change.
2. **One connector per function.** Each designator carries exactly one
   function from the §3 table. No multi-function combo connectors; the one
   open placement question (`DRIVER_ENABLE_HW` on the driver-control
   connectors vs. a dedicated enable connector) is resolved at DR-2 and
   recorded as a table change.
3. **Pin-1 marking convention.** Every connector's pin 1 is marked on
   silkscreen (numeral + polarity mark) and appears in every drawing of
   that connector.
4. **Viewing convention — board-side vs. harness-side.** Every pinout
   drawing states its view explicitly: **board-side** drawings view the
   board-mounted connector's mating face; **harness-side** drawings view
   the free harness connector's mating face (which mirrors it). A drawing
   without a stated view is invalid. This kills the classic
   "wire side vs. mating face" mirror-image harness error.
5. **Signal naming.** Connector signals use the exact worksheet symbols
   (`pin-allocation.md` — `E_STOP_SENSE`, `ENCODER_L_A`, …). Schematic
   nets, harness drawings, and firmware BSP symbols all use the same
   names; no connector document invents aliases.
6. **Cable-label format.** Every harness end is labeled at build time:
   `KCB-A / <designator> / <purpose> / <harness serial>` — e.g.
   `KCB-A / J4 / ENC-L / H-0003`. Labels face outward after installation.
   A harness relabeled for a different connector is a new harness with a
   new serial. Left/right pairs additionally carry color bands (colors
   fixed at DR-2; one color = left, one = right, never reused elsewhere
   on the robot).
7. **Keyed and locking, required.** Every off-board connector is keyed and
   locking. Friction-fit unlatched headers are not acceptable for
   off-board safety or communication signals.
8. **Hot-plugging is prohibited.** No Rev A connector is hot-plug rated:
   mating and unmating happen with logic power off and motor supply
   isolated. Any exception (candidates: J1 link, J9 debug) requires an
   explicit DR-2 decision recorded in the §3 table; safety-path
   connectors (J3, J6, J7) are never hot-plug.
9. **Shield/drain handling.** If a harness is shielded, the shield/drain
   terminates at **one end only** — the board end, to a designated shield
   termination (never a signal pin), and is **never used as a signal or
   power return**. Whether any Rev A harness needs a shield is a DR-2
   outcome of the signal-integrity measurements, not a default.
10. **Strain relief.** Every off-board harness is strain-relieved at both
    ends by a mechanical feature (clamp, tie-down point, grommet, or
    service loop anchored to board/plate/chassis). A connector latch is
    retention, not strain relief, and **harnesses are never structural
    restraints** (`mechanical-reference.md` §7–8). Strain-relief
    provisions on the carrier are a DR-3 layout item.
11. **Cable routing and bend radius.** Each harness records its wire's
    minimum bend radius at DR-2 (once gauge/insulation are selected), and
    routing on the vehicle must respect it — routed paths and clearances
    are per-platform fields in the `mechanical-reference.md` §5 worksheet.
    No harness routes across suspension travel, steering sweep, or
    driveline clearance zones without a reviewed path.
12. **Required evidence before a connector family is frozen** (per
    connector, recorded in the §3 *Evidence source* column):
    the manufacturer drawing for the exact housing/terminal series; a
    physical mating check against the real vendor-harness end (J4–J8);
    measured voltage and current for every pin; confirmed crimp-tool
    availability; and the §4 hard-rule checks passed.

## 2. Pending until DR-2 (deliberately unselected)

For every connector: **connector family, pitch, mating housing, crimp
terminal, wire gauge, current rating, voltage rating, polarization,
retention method, and exact pin order.** None of these appears as a value
anywhere in this document until its evidence exists; a plausible family
named early is the connector version of a guessed pin assignment.

## 3. Connector inventory

Definitional columns (Function, Signals, Keyed, Locking, Hot-plug) are
filled; everything evidence-dependent is `Pending`. *Board-side view* /
*Harness-side view* hold references to the two per-§1.4 drawings once they
exist.

| Designator | Function | Signals | Board-side view | Harness-side view | Pin-1 marker | Keyed | Locking | Hot-plug allowed | Expected voltage/current | Wire gauge | Connector family | Mating part number | Evidence source | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| J1 | Jetson R2CP link | `R2CP_RX`, `R2CP_TX`, GND | Pending | Pending | Required (§1.3) | Required | Required | No (DR-2 may revisit) | Requires measurement (domain not assumed — `interfaces.md` §4) | Pending | Pending | Pending | Pending | Pending |
| J2 | Logic power in | `LOGIC_5V_IN`, `LOGIC_GND` | Pending | Pending | Required | Required | Required | No | Requires measurement (board consumption + margin) | Pending | Pending | Pending | Pending | Pending |
| J3 | E-stop loop | NC loop in/out, `E_STOP_SENSE` reference | Pending | Pending | Required | Required | Required | **Never** | Requires measurement (`../R2_ESTOP_SPEC.md` §5) | Pending | Pending | Pending | Pending | Pending |
| J4 | Left encoder | `ENCODER_L_A`, `ENCODER_L_B`, encoder supply/return | Pending | Pending | Required | Required | Required | No | Requires measurement | Pending | Pending | Pending | Pending | Pending |
| J5 | Right encoder | `ENCODER_R_A`, `ENCODER_R_B`, encoder supply/return | Pending | Pending | Required | Required | Required | No | Requires measurement | Pending | Pending | Pending | Pending | Pending |
| J6 | Left driver control | `PWM_LEFT_IN1/IN2`, `DRIVER_FAULT_L_N`, GND; `DRIVER_ENABLE_HW` placement Pending (§1.2) | Pending | Pending | Required | Required | Required | **Never** | Requires measurement (driver logic inputs) | Pending | Pending | Pending | Pending | Pending |
| J7 | Right driver control | `PWM_RIGHT_IN1/IN2`, `DRIVER_FAULT_R_N`, GND; `DRIVER_ENABLE_HW` placement Pending (§1.2) | Pending | Pending | Required | Required | Required | **Never** | Requires measurement | Pending | Pending | Pending | Pending | Pending |
| J8 | Steering | `STEERING_PWM`, optional `STEERING_FB`; servo supply pass-through decision Pending | Pending | Pending | Required | Required | Required | No | Requires measurement (servo rail; connector Unverified — `firmware/rosmaster-r2/docs/HARDWARE_REFERENCE.md` §Steering) | Pending | Pending | Pending | Pending | Pending |
| J9 | Debug | `SWDIO`, `SWCLK`, `NRST`, GND (primary path: Nucleo ST-LINK) | Pending | Pending | Required | Pending | Pending | No (DR-2 may revisit) | Logic-level — confirm at DR-2 | Pending | Pending | Pending | Pending | Pending |

## 4. Hard rules (release-blocking)

1. **Pin-numbering verification.** Connector pin numbering must be
   verified against the manufacturer drawing **and** checked on a physical
   mating pair before fabrication release. A datasheet read-through alone
   does not close this; the §3 *Evidence source* cell records both checks.
2. **No interchangeable hazardous mating.** No safety-critical connector
   may share an interchangeable housing and pin count with a connector
   carrying a hazardous or incompatible voltage unless mechanical keying
   makes cross-connection impossible.
3. **No cross-pluggable neighbors.** Beyond rule 2, any two connectors
   whose accidental swap is electrically possible must differ in family,
   pin count, or key. The left/right pairs (J4/J5, J6/J7) are the
   highest-risk swaps: same family is acceptable only with distinct keying
   or the §1.6 color-band labeling, and the DR-3 review must state which
   mechanism prevents each swap.
4. **Vendor-mating side is measured, not chosen.** J4–J8 families follow
   from what is physically on the robot (bench characterization at
   bring-up stage 11 feeds this table); no vendor pinout is assumed.

Rules 1–2 are checked at DR-4 (`manufacturing-checklist.md` connector
items); rules 3–4 at DR-2/DR-3.

## 5. Mating-part and tooling worksheet

Expanded to one row per designator when the first family is selected;
verified against physical parts at DR-4. A row without a recorded crimp
tool is not buildable and blocks DR-4.

| Designator | Board connector MPN | Mating housing MPN | Crimp terminal MPN | Retention method | Wire gauge / min bend radius | Crimp tool | Manufacturer drawing ref | Physical mating check | Status |
|---|---|---|---|---|---|---|---|---|---|
| J1–J9 | Pending | Pending | Pending | Pending | Pending | Pending | Pending | Pending | Pending |
