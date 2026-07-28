# HDR-0005: Continuous ground plane

| Field | Value |
|---|---|
| Status | Proposed — ratified at DR-1 |
| Date | 2026-07-28 |
| Cross-refs | `../power-and-grounding.md` (normative); DR-3 (`../design-reviews.md`) |

## Context

A recurring failure mode on mixed-signal control boards is a well-meant
"star ground" or split-plane rule that severs return paths under switching
signals, radiating and coupling exactly the noise it meant to prevent.
Rev A carries switching logic signals (PWM, UART, watchdog kick) alongside
low-level sense inputs (E-stop sense, encoder channels, optional steering
feedback) — but **no motor current** (RA-X3), which removes the classic
argument for splitting.

## Decision

Rev A uses **one continuous ground plane** with placement- and
routing-level control of switching-current return paths, rather than a
generic star-ground rule or split planes. Sensitive conditioning is
protected by placement and return-path discipline on the single plane
(`../power-and-grounding.md` §1).

## Consequences

- DR-3 reviews return paths explicitly: every switching signal has an
  adjacent return; no slots/moats under signal crossings; connector
  pinouts reserve grounds next to fast or safety signals where practical.
- If a future revision ever routes power electronics (it must not on
  Rev A), this decision is re-examined then — it is scoped to a
  logic-only carrier.
