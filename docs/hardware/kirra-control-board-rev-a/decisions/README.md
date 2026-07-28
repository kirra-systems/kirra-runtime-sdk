# Hardware decision records (HDRs)

Small, hardware-scoped decision records for the Kirra Control Board,
modeled on the repository's existing `docs/adr/` convention (numbered,
one decision per file, status header, context → decision → consequences).

Why a separate series instead of new ADRs: the `docs/adr/` series records
*architecture* decisions for the software stack, and several of the
decisions below merely **bind Rev A to choices already recorded there or in
the firmware tree** — they are hardware instantiations, not new
architecture. Keeping them here avoids diluting the ADR series while
preserving the same record discipline. An HDR must not restate a decision
already established in `docs/adr/` or `firmware/rosmaster-r2/docs/` — it
cites it and records only the hardware-scoped delta.

Numbering: `HDR-NNNN-slug.md`, monotonically increasing, never reused.

| ID | Title | Status |
|---|---|---|
| HDR-0001 | Use NUCLEO-G474RE for Rev A | Proposed (DR-1) |
| HDR-0002 | Carrier board before custom MCU PCB | Proposed (DR-1) |
| HDR-0003 | R2CP host link | Proposed (DR-1) |
| HDR-0004 | Hardware-combined actuator enable | Proposed (DR-1) |
| HDR-0005 | Continuous ground plane | Proposed (DR-1) |
| HDR-0006 | Retain external motor drivers in Rev A | Proposed (DR-1) |

Statuses follow the ADR pattern: `Proposed` until the corresponding review
gate (here DR-1) ratifies them.
