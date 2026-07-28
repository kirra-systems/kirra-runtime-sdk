# HDR-0004: Hardware-combined actuator enable

| Field | Value |
|---|---|
| Status | Proposed — ratified at DR-1 |
| Date | 2026-07-28 |
| Cross-refs | `../safety.md` (normative logic); `docs/hardware/R2_ESTOP_SPEC.md` R1/R2/R6; `firmware/rosmaster-r2/README.md` design constraints; `docs/hardware/TARGET_PLATFORM_MATRIX.md` §0 (power-stage authority removed independently of any MCU) |

## Context

The existing safety spine already demands that the physical E-stop remove
power-stage authority independently of the MCU, and that software safe
states are necessary but not sufficient. On the vendor board, the only
hardware gate is the external E-stop relay chain; the MCU's own outputs
are ungated firmware behavior.

## Decision

Rev A delivers driver enable **only** as the hardware conjunction

```
DRIVER_ENABLE_HW = E_STOP_OK_HW AND WATCHDOG_OK_HW AND MCU_ENABLE_REQUEST
```

combined in hardware on the carrier (`../safety.md`). Firmware contributes
one term — a request, default inactive — and no software-controlled GPIO
alone is a safety gate. The watchdog term comes from an **independent
external supervisor** monitoring the MCU's kick, not from the MCU's
internal watchdogs.

## Consequences

- MCU crash, reset, bootloader mode, absent Nucleo, and firmware wedges
  all converge to disabled actuation without software cooperation
  (`../safety.md` §4 matrix; verified at bring-up stages 6–8 and 13).
- The retained external E-stop relay chain (`docs/hardware/R2_ESTOP_SPEC.md`) composes
  *outside* this gate: motor-supply cut remains the outermost layer, the
  carrier's enable gate is the layer inside it, firmware inside that.
- The gate's realization (discrete logic vs. relay vs. supervisor part) and
  its timing windows are DR-2 outputs backed by measurement, not claims
  made here.
