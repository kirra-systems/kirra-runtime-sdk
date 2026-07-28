# HDR-0001: Use NUCLEO-G474RE for Rev A

| Field | Value |
|---|---|
| Status | Proposed — ratified at DR-1 |
| Date | 2026-07-28 |
| Cross-refs | `firmware/rosmaster-r2/docs/ARCHITECTURE.md` §Decision summary; `firmware/rosmaster-r2/docs/ROADMAP.md` Phase 10 (F103 limitations); HDR-0002 |

## Context

The firmware architecture already records the direction: retain the
STM32F103RCT6 only for vendor-board compatibility, and "plan an STM32G4/H7
control-board revision for CAN-FD, hardware crypto and stronger
diagnostics." Phase 10 records the F103's limits (no hardware root of
trust, A/B slot pressure). This HDR does not re-decide the G4 direction —
it selects the concrete first vehicle for it.

## Decision

Rev A hosts an **STM32 NUCLEO-G474RE** module as its MCU. The Nucleo is
carried, not redesigned: mechanical mounting plus Morpho-header connection.

Reasons for a Nucleo module over a bare G474 on the carrier:

- removes MCU power-supply, crystal, boot-strap, and programmer design
  from Rev A's risk surface (the ST-LINK is on the module);
- keeps Rev A's unresolved-evidence surface small — the carrier's unknowns
  are the robot-side interfaces, not the MCU core;
- the G474's timer/ADC/CAN-FD capability covers the firmware tree's driver
  closure plan (encoder timers, PWM, ADC, watchdog interplay) with the
  CAN-FD option kept open for a later carrier revision.

## Consequences

- Every pin claim depends on the **exact MB1367 board revision** in hand;
  the pin-allocation process (`../pin-allocation.md` §2) starts there and
  no allocation is asserted before that evidence exists.
- A G474 BSP in `firmware/rosmaster-r2/` becomes future firmware work,
  gated identically to the F103 BSP (no register code before physical
  verification).
- The final custom Kirra controller (integrated MCU package) remains a
  separate future decision — see HDR-0002.
