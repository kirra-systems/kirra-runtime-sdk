# Rev A manufacturing checklist (DR-4 release gate)

> Every box must be checked, with evidence linked, before a fabrication
> order is placed. An unchecked box blocks release — there are no
> "documentation-only" passes. This checklist releases *fabrication of a
> bench board*; it does not release motion (see `bringup-plan.md`).

## Identity and evidence

- [ ] Exact carrier board revision recorded (name, revision, date, git tag
      of the design sources).
- [ ] Exact NUCLEO-G474RE MB1367 board revision recorded (marking + photo).
- [ ] Schematic source documents archived (the matching ST MB1367
      schematic, driver/encoder/steering measurement records, Jetson UART
      domain evidence).
- [ ] CubeMX allocation exported and archived alongside the frozen
      `pin-allocation.md` worksheet (no `Pending` fields remain).
- [ ] Connector drawings verified against physical mating parts.

## Electrical rule closure

- [ ] ERC clean (or every remaining warning individually waived with
      rationale).
- [ ] DRC clean (same waiver rule).
- [ ] No undocumented unconnected nets — every no-connect is deliberate
      and annotated.
- [ ] No raw motor-current paths anywhere on the board (RA-X3).
- [ ] No unsafe default enable state: `MCU_ENABLE_REQUEST` pulled
      inactive; `DRIVER_ENABLE_HW` disabled for unpowered, resetting,
      unprogrammed, and Nucleo-absent boards (verified on the schematic
      against `safety.md` §4).
- [ ] Safety truth table (`safety.md` §1, §4) reviewed against the final
      schematic by a non-author.

## Physical and access

- [ ] Test points present for every safety-critical and communication
      signal (`interfaces.md` §1.2 list checked one by one).
- [ ] Mounting holes measured against the chassis and the Nucleo footprint.
- [ ] USB/ST-LINK access checked with the Nucleo mounted (connector
      clearance and cable path).
- [ ] STEP model reviewed (carrier + Nucleo + mated connectors).

## Outputs

- [ ] BOM reviewed: manufacturer part numbers for every line, no
      placeholders, availability checked.
- [ ] Assembly variants documented (e.g. unpopulated optional footprints:
      steering feedback, ESD options, filter values left DNP pending
      measurement).
- [ ] Gerbers reviewed (visual pass against the layout, layer by layer).
- [ ] Drill files reviewed.
- [ ] Fabrication notes reviewed (stack-up, finish, tolerances, marking).
- [ ] Release notes written (what Rev A is, known limitations, waived
      findings and their expiry).
- [ ] Release tag created on the design sources; the tag is what gets
      fabricated.
