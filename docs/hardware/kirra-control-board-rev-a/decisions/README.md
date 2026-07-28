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
| HDR-0007 | Adopt a Traxxas 1/10 ecosystem as the long-term Kirra mechanical reference | Proposed (platform direction; exact model gated on MR-1) |

Statuses follow the ADR pattern: `Proposed` until the corresponding review
gate ratifies them (DR-1 for HDR-0001…0006; HDR-0007's platform direction
ratifies at DR-1 while its exact-model selection waits on MR-1 —
`../mechanical-reference.md` §6).
