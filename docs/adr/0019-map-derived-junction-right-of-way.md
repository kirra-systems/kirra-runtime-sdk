# ADR-0019: Map-derived junction right-of-way

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG5** (junction / commit-zone negotiation — the ego must not assert priority it does not have); backstopped by **SG1** (KIRRA RSS catches any wrong yield) |
| Cross-refs | roadmap #4 "Remaining: the upstream right-of-way *derivation* from the lane graph + controls"; code: `crates/kirra-map/src/lanemap.rs` (`LaneGraph::derive_right_of_way_from_controls`, `with_derived_right_of_way`, `JUNCTION_CONFLICT_RADIUS_M`, `ROW_CROSSING_MIN_RAD`), consumers `cedes_to_ego` / `non_yielding_to_ego` / `junction_context`; tests in the same module |

## Context

The junction cede list — who the ego may proceed against vs. who it must wait for — is consumed by
Occy (`PlanInput.cedes_to_ego_ids`) and by Parko's SG5 `NonYieldingScene`, both read from the lane
graph's `priority_over` relation. That relation was **integrator-supplied** (hand-fed via
`add_right_of_way`). A Lanelet2 map already carries the signs/controls that *imply* right-of-way;
deriving the relation from them — the same "derive-from-the-map" move ADR-0016 made for occlusion —
removes a hand-fed safety input.

## Decision

`derive_right_of_way_from_controls` populates `priority_over` from each approach lane's traffic
**control**, using the MUTCD / Vienna Convention uncontrolled-vs-controlled core: where two
**crossing** approaches share a junction, an approach carrying a STOP or YIELD control yields to a
conflicting approach with **no** control — the uncontrolled (through) road has priority.

Structural signals, both deliberately conservative:
- **Same junction** — termini (stop lines) within `JUNCTION_CONFLICT_RADIUS_M` (12 m).
- **Crossing** — headings ≥ `ROW_CROSSING_MIN_RAD` (45°) apart; parallel same-/opposing-direction
  approaches are a following / head-on relation (RSS), not a right-of-way one.

Only the unambiguous case asserts a relation. Everything else is **left unasserted** — both
uncontrolled, an all-way stop (first-come, not static), or a **traffic light** on either approach
(its priority is the live signal state each tick, owned by the signal path). The derivation is
**additive** (it only adds road-correct assertions; an integrator-set relation is kept) and skips
degenerate / non-finite-terminus lanes.

## Consequences

- **Positive:** the cede list falls out of the map's signs end to end (`junction_context` →
  `cedes_to_ego_ids`) with no hand-feeding; Occy executes the right-of-way, KIRRA backstops.
- **Fail-safe direction:** asserting *too much* priority is the unsafe direction (the ego proceeds
  when it should yield). The rule only asserts where the sign makes it road-correct; every
  ambiguous case yields **no** assertion, so the ego falls back to yielding to that agent, and
  KIRRA's RSS (SG1) catches any residual wrong yield. The conservative default is the safe one.
- **Honest scope:** "same junction" is a terminus-proximity heuristic and "crossing" a heading
  band — adequate for the straight-approach maps the stack builds; an explicit junction-grouping
  primitive (and priority/through-road *signs* beyond stop/yield) is a follow-up. A map with no
  controls (and no hand-set relation) is byte-identical to prior behaviour.
