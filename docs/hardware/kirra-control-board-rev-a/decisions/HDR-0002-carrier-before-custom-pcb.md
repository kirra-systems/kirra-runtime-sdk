# HDR-0002: Carrier board before custom MCU PCB

| Field | Value |
|---|---|
| Status | Proposed — ratified at DR-1 |
| Date | 2026-07-28 |
| Cross-refs | `firmware/rosmaster-r2/docs/ROADMAP.md` §Staged migration ("Do not skip stage 1: it is the only stage that can fail cheaply"); HDR-0001; HDR-0006 |

## Context

Two hardware paths lead away from the vendor board: (a) a full custom
controller PCB (integrated MCU, drivers, power distribution), or (b) an
interface/safety carrier around an off-the-shelf MCU module, keeping the
existing driver electronics. The firmware roadmap's migration principle is
to stage boundary moves so each stage can fail cheaply.

## Decision

Build the **carrier board first** (Rev A), and defer the custom MCU PCB to
a later revision. Rev A takes on exactly the parts Kirra must own to move
the actuation gate into hardware — MCU hosting, R2CP transport, E-stop
input, independent watchdog, combined enable, signal conditioning — and
nothing that is already working externally (drivers, motors, power).

## Consequences

- Rev A's failure modes are cheap: a bad carrier risks a small board and a
  Nucleo, not a motor-driver or power-distribution design.
- The custom controller (integrated MCU package, integrated drivers,
  production power distribution, hardware root of trust) is explicitly the
  *future* state in `../README.md` and inherits Rev A's verified interface
  evidence when it comes.
- Some Rev A choices are transitional by design (Morpho-header dependence,
  external driver connectors) and must not be treated as production
  commitments.
