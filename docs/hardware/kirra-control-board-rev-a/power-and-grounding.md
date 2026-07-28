# Rev A power and grounding

> **Status: DR-1 philosophy.** Principles freeze at DR-1; component values,
> protection parts, and copper decisions are DR-2/DR-3 outputs. No final
> protection-component value is selected in this document.

## 1. Grounding

- **Continuous ground plane**, not a generic star-ground rule. A single
  unbroken reference plane under all signals; no slots or moats that force
  return currents into detours.
- **Controlled switching-current return paths**: every switching signal
  (PWM outputs, UART, watchdog kick, encoder inputs) gets a return path
  directly under or adjacent to its trace; connector pinouts reserve
  adjacent grounds for fast or safety signals where practical (DR-3 checks
  this).
- Analog/sense conditioning (E-stop sense divider, optional steering
  feedback) is placed and routed to avoid switching return currents flowing
  under it — placement discipline on the one plane, not a split plane.

## 2. Motor current stays off the board

- **Motor current does not flow through the carrier.** The carrier's
  connectors carry logic-level control, enable, sense, and fault signals
  only (requirement RA-X3).
- **Battery power distribution remains external.** The battery, its
  main switch, the motor-supply feed, and the `../R2_ESTOP_SPEC.md`
  safety-relay chain stay outside Rev A; the carrier neither routes nor
  fuses them.

## 3. Logic power input

The carrier accepts **protected low-current logic power**
(`LOGIC_5V_IN` / `LOGIC_GND`), and the input stage includes, conceptually:

- an input **fuse** (rating: DR-2, from measured board consumption plus
  margin);
- **reverse-polarity protection**;
- **transient suppression** (TVS; working voltage: DR-2, after the supply
  source and its transients are characterized);
- **bulk and high-frequency decoupling** appropriate to the loads.

Final component values are deliberately not chosen here — they depend on
measured consumption, the chosen upstream supply, and the Nucleo's own
input stage, all of which are DR-2 evidence items.

## 4. Power-state safety

Power sequencing must preserve `safety.md` §4: at any point during ramp-up,
brown-out, or ramp-down, `DRIVER_ENABLE_HW` must not glitch active. The
enable gate and the `MCU_ENABLE_REQUEST` pull are powered/referenced such
that an unpowered or partially powered carrier reads as *disabled*. This is
a DR-2 schematic review item and a bring-up stage 2–3 verification.

## 5. Nucleo power configuration — verify against the physical board

The NUCLEO-G474RE's external-power options (supply source selection,
solder-bridge and jumper configuration, what the ST-LINK section powers)
**vary by MB1367 board revision**. The external-power configuration used by
the carrier **must be verified against the exact physical MB1367 revision
in hand and its matching ST schematic** — the same step 1–2 evidence the
pin-allocation process requires (`pin-allocation.md` §2). No jumper or
solder-bridge setting is asserted in this documentation.
