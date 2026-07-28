# Rev A requirements

> **Status: DR-1 input.** Requirement IDs are stable (`RA-…`); text is frozen
> at DR-1. "Shall" rows are binding on the Rev A schematic; nothing here
> implies the schematic exists yet.

## 1. Mission requirements

| ID | Requirement |
|---|---|
| RA-M1 | Rev A **shall** be a carrier board for an STM32 NUCLEO-G474RE; the Nucleo is the only MCU on the board. |
| RA-M2 | Rev A **shall** replace the Yahboom MCU board while retaining the existing external motor drivers, steering hardware, motors, and encoders. |
| RA-M3 | Rev A **shall** communicate with the Jetson exclusively over R2CP as normatively specified in `firmware/rosmaster-r2/docs/PROTOCOL.md`. Rev A documentation binds no R2CP wire constants. |
| RA-M4 | Rev A **shall** enforce a hardware actuation gate (`safety.md`) whose disable path functions independently of Linux, of the Jetson, and of MCU firmware. |
| RA-M5 | Rev A **shall not** alter the Kirra software trust boundary: verifier/governor authorization, the ADR-0033 consumer, and the single-writer serial discipline remain required and unchanged. |

## 2. Functional inclusions

Rev A **shall** provide, conceptually (pins, parts, and values pending per
`pin-allocation.md` and DR-2):

| ID | Inclusion |
|---|---|
| RA-F1 | Nucleo mechanical mounting and Morpho-header connection. |
| RA-F2 | Jetson-to-MCU R2CP transport (UART path with level translation selected per `interfaces.md` §4). |
| RA-F3 | Hardware E-stop input from a normally closed E-stop loop (composes with `../R2_ESTOP_SPEC.md`). |
| RA-F4 | Independent external watchdog/supervisor, kicked by the MCU, whose permission output gates the driver enable. |
| RA-F5 | MCU enable request line (`MCU_ENABLE_REQUEST`), default inactive. |
| RA-F6 | Hardware-combined driver enable (`DRIVER_ENABLE_HW`) per the `safety.md` logic. |
| RA-F7 | Encoder input interfaces for the retained quadrature encoders (A/B per side, matched conditioning). |
| RA-F8 | Steering output (PWM) and an optional steering-feedback input interface. |
| RA-F9 | Motor-driver control outputs (PWM/direction pairs) to the retained external drivers. |
| RA-F10 | Driver-fault inputs (`DRIVER_FAULT_L_N`, `DRIVER_FAULT_R_N`), read-only to the MCU. |
| RA-F11 | Status LEDs (board power, enable state, fault state at minimum). |
| RA-F12 | SWD/debug access (via the Nucleo's ST-LINK and/or a carrier debug header). |
| RA-F13 | Protected external connectors — keyed and locking where practical. |
| RA-F14 | Test points on every safety-critical and communication signal. |
| RA-F15 | Protected low-current logic-power input (`power-and-grounding.md`). |

## 3. Exclusions

Rev A **shall not** include:

| ID | Exclusion |
|---|---|
| RA-X1 | Integrated high-current motor drivers. |
| RA-X2 | A battery charger. |
| RA-X3 | Raw motor-current routing through the carrier — motor current never crosses the board. |
| RA-X4 | Wireless networking of any kind. |
| RA-X5 | Camera interfaces. |
| RA-X6 | A final integrated MCU package (the Nucleo module is the MCU for Rev A). |
| RA-X7 | Final production power distribution. |
| RA-X8 | Automatic floor-driving approval — nothing about Rev A, including passing all its bring-up stages, authorizes floor motion (`bringup-plan.md` §15). |

The existing external motor drivers remain in use for Rev A
(`decisions/HDR-0006-retain-external-motor-drivers.md`).

## 4. Safety requirements (summary — normative text in `safety.md`)

| ID | Requirement |
|---|---|
| RA-S1 | `DRIVER_ENABLE_HW` **shall** be the hardware conjunction `E_STOP_OK_HW AND WATCHDOG_OK_HW AND MCU_ENABLE_REQUEST`. |
| RA-S2 | Firmware **requests** actuation via `MCU_ENABLE_REQUEST`; it **shall not** possess sole authority to energize the drivers. No software-controlled GPIO alone is a safety gate. |
| RA-S3 | Power-up, reset, bootloader mode, MCU crash, watchdog timeout, E-stop open, disconnected Nucleo, communication loss, and Jetson restart **shall** each result in, or converge to, disabled actuation (`safety.md` §4). |
| RA-S4 | The E-stop loop **shall** be normally closed / energize-to-run, consistent with `../R2_ESTOP_SPEC.md` R2 (fail-safe) and R3 (latching, where the loop includes the latching mushroom). |
| RA-S5 | `E_STOP_SENSE` and the driver-fault inputs are **read-only, advisory** MCU inputs — observability, never the stopping mechanism (mirrors `../R2_ESTOP_SPEC.md` R6). |

## 5. Interface and construction requirements (summary — `interfaces.md`, `power-and-grounding.md`)

| ID | Requirement |
|---|---|
| RA-I1 | No naked MCU pins leave the board; every off-board interface has an appropriate ESD/protection footprint. |
| RA-I2 | Encoder filter components remain configurable (footprints, values TBD) until encoder frequency and voltage are measured. A/B channels per encoder receive matched components and matched routing. |
| RA-I3 | UART level translation is selected only after both voltage domains are verified. Jetson UART voltage **shall** be measured or verified from the carrier schematic — it is not assumed to be 1.8 V or 3.3 V. Generic auto-direction level translators are not assumed suitable for UART. |
| RA-I4 | Continuous ground plane; controlled switching-current return paths; no star-ground topology mandated by rule. |
| RA-I5 | Logic power input includes fuse, reverse-polarity protection, transient suppression, and bulk + high-frequency decoupling. Final protection-component values are a DR-2 output, not chosen here. |
| RA-I6 | The Nucleo external-power configuration (solder bridges/jumpers) **shall** be verified against the exact physical MB1367 board revision before the schematic freezes. |

## 6. Verification linkage

- Requirement-to-evidence closure follows the firmware tree's discipline:
  a claim is either sourced or marked unknown, and unknowns gate actuation
  (`firmware/rosmaster-r2/docs/HARDWARE_REFERENCE.md` §Evidence status).
- Each requirement above maps to a design-review gate in
  `design-reviews.md` and to bring-up stages in `bringup-plan.md`.
- The safety truth table (RA-S1/RA-S3) is verified at bring-up stages 6–8
  and 13 before any motor-power test.
