# HDR-0006: Retain external motor drivers in Rev A

| Field | Value |
|---|---|
| Status | Proposed — ratified at DR-1 |
| Date | 2026-07-28 |
| Cross-refs | `firmware/rosmaster-r2/docs/HARDWARE_REFERENCE.md` §Motor bridge (H-bridge inspection obligations); HDR-0002; `../requirements.md` RA-X1/RA-X3 |

## Context

The robot's existing external motor-driver hardware, motors, encoders, and
steering gear work today under the vendor MCU. Integrating drivers onto the
Kirra board would pull motor current, thermal design, and power
distribution into Rev A — the exact scope HDR-0002 defers — and would
invalidate the working, already-characterized actuator set for no Rev A
benefit.

## Decision

Rev A **retains the existing external motor drivers** (and the motors,
encoders, and steering hardware they serve). The carrier interfaces to
them at logic level only: control outputs, the hardware-combined enable,
and read-only fault inputs. Motor current never crosses the carrier.

## Consequences

- The vendor-hardware inspection obligations transfer rather than
  disappear: the retained drivers' enable polarity, input thresholds,
  fault semantics, and coast/brake truth table must be bench-characterized
  (bring-up stages 8 and 11) before DR-2 assumptions freeze — the same
  discipline `firmware/rosmaster-r2/docs/HARDWARE_REFERENCE.md` applies to the vendor board.
- Driver replacement or integration becomes a future-revision decision,
  taken with Rev A's measured evidence in hand.
- Rev A's connectors and harnessing must mate with the existing driver
  wiring; connector drawings are verified against the physical harness at
  DR-4 (`../manufacturing-checklist.md`) — no Yahboom connector pinout is
  assumed from documentation.
