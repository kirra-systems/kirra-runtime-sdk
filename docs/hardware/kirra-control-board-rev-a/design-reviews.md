# Rev A design-review gates

> Reviews are gates, not estimates — the same rule the firmware roadmap
> applies to benchmarks. A phase is complete when its review passes with
> recorded evidence, never because its documents exist. **No Gerbers are
> generated before DF-1 passes, and no fabrication order occurs before
> DR-4.**

## DR-1 — Architecture Review

Freezes:

- **mission** (`README.md` §Mission),
- **scope** — inclusions and exclusions (`requirements.md` §2–3),
- **interfaces** — the interface set and philosophy (`interfaces.md`),
  including the three signal-role interfaces frozen in
  `system-interfaces.md` (Jetson↔MCU, MCU↔driver, MCU↔safety),
- **safety boundary** — the `DRIVER_ENABLE_HW` logic and safe-behavior
  matrix (`safety.md`),
- **power philosophy** (`power-and-grounding.md` §1–4),
- **connector philosophy** (keyed/locking, no naked pins, test points).

Entry: this documentation set complete; decisions HDR-0001…HDR-0006
recorded. Exit: sign-off recorded; open questions (`architecture.md` §6)
carried as explicit DR-2 obligations, not silently resolved.

## DR-2 — Schematic Review

Freezes:

- **verified pin allocation** — every `pin-allocation.md` row closed by the
  eight-step process, evidence recorded, no `Pending` fields remaining,
- **protection** — final protection/conditioning parts and values on every
  external interface,
- **watchdog** — the external supervisor part, its timeout window, and its
  independence argument,
- **enable logic** — the hardware realization of the `safety.md`
  conjunction, including power-glitch behavior,
- **voltage domains** — all measured/schematic-verified, including the
  Jetson UART domain and the resulting translator selection,
- **connector pinouts** — carrier connectors fully pinned, keyed, and
  documented.

Entry: exact MB1367 revision identified + matching schematic archived;
CubeMX project exists; external measurements complete. Exit: ERC clean;
safety truth table reviewed against the schematic; unresolved items block.

## DR-3 — PCB Review

Freezes:

- **board stack-up**,
- **placement**,
- **routing** — including the continuous ground plane and matched encoder
  A/B pairs (`interfaces.md`, `power-and-grounding.md`),
- **mechanical fit** — carrier in the robot, Nucleo on the carrier,
- **test access** — every required test point present and probeable,
- **connector access** — mating, latching, and harness routing feasible;
  USB/ST-LINK reachable with the Nucleo mounted.

Exit: DRC clean; 3D/mechanical check done; layout review notes resolved.

## DF-1 — Design Freeze (pre-Gerber checkpoint)

A short, mandatory consistency checkpoint after DR-2/DR-3 and **before any
fabrication outputs are generated**. DR-2 freezes the schematic and DR-3
the layout, but neither forces a *final cross-comparison* of the
documentation, the firmware's assumptions, and the schematic as actually
drawn — DF-1 does exactly that, and it exists because that comparison is
what tends to save a board revision. **No Gerbers are generated before
DF-1 passes**; DR-4 then reviews the outputs DF-1 authorized.

DF-1 requires, all recorded:

- **all worksheet entries measured** — no remaining `Pending` /
  `Unverified` / `Requires measurement` item that affects Rev A, across
  `pin-allocation.md`, `connector-map.md`, and the
  `mechanical-reference.md` worksheet columns for the platform(s) this
  build claims;
- **CubeMX pin allocation finalized** — the exported project matches the
  frozen `pin-allocation.md` table exactly;
- **schematic complete** — no placeholder symbols, no TBD values;
- **ERC clean** (or individually waived with rationale);
- **interface documents unchanged since DR-2** — `interfaces.md`,
  `system-interfaces.md`, `connector-map.md`, and `safety.md` diffed
  against their DR-2-approved revisions; any change re-opens the affected
  review scope instead of slipping through;
- **firmware pin manifest updated to match the schematic** — the G474
  board-revision manifest (the BSP input, in the style of
  `firmware/rosmaster-r2/hal/include/r2/hal/board_manifest.hpp`)
  regenerated from the frozen allocation, so firmware and copper cannot
  disagree from day one;
- **independent review completed** — a non-author walks the comparison
  above and signs it.

## DR-4 — Manufacturing Review

Requires, all recorded:

- ERC clean,
- DRC clean,
- reviewed BOM (manufacturer part numbers, no placeholder parts),
- reviewed fabrication outputs (Gerbers, drill files, fab notes),
- reviewed assembly outputs (placement files, assembly drawings, variants),
- 3D fit verification,
- release notes (what this revision is, known limitations, deviations).

Exit: the `manufacturing-checklist.md` is fully checked and a release tag
is created. **Only then** may a fabrication order be placed.

## MR-1 — Chassis Selection Review (mechanical, parallel track)

A separate mechanical gate defined normatively in
`mechanical-reference.md` §6 — it selects the exact Traxxas 1/10 chassis
for Mechanical Reference A. It does not replace or renumber DR-1…DR-4:

- MR-1 may run in parallel with DR-1/DR-2 (the electrical work does not
  wait on a chassis).
- **No Class A mounting pattern may be frozen before MR-1 passes.**
- DR-3 freezes mechanical fit only against reference platforms whose
  evidence exists: the Reference B (Yahboom R2) fit needs the
  `mechanical-reference.md` §5 Reference B measurements; a Class A fit
  additionally needs MR-1.

## Review mechanics

- Reviewers: at least one person who did not author the artifact under
  review, mirroring the independent-review pattern used across the firmware
  roadmap's exit criteria.
- Findings and waivers are recorded with the review; a waived finding
  states its rationale and expiry (e.g. "accept for Rev A, must fix in the
  custom controller").
- Re-entering an earlier gate (e.g. a pin change after DR-2) reopens that
  gate for the affected scope.
