# Rev A design-review gates

> Reviews are gates, not estimates — the same rule the firmware roadmap
> applies to benchmarks. A phase is complete when its review passes with
> recorded evidence, never because its documents exist. **No fabrication
> order occurs before DR-4.**

## DR-1 — Architecture Review

Freezes:

- **mission** (`README.md` §Mission),
- **scope** — inclusions and exclusions (`requirements.md` §2–3),
- **interfaces** — the interface set and philosophy (`interfaces.md`),
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

## Review mechanics

- Reviewers: at least one person who did not author the artifact under
  review, mirroring the independent-review pattern used across the firmware
  roadmap's exit criteria.
- Findings and waivers are recorded with the review; a waived finding
  states its rationale and expiry (e.g. "accept for Rev A, must fix in the
  custom controller").
- Re-entering an earlier gate (e.g. a pin change after DR-2) reopens that
  gate for the affected scope.
