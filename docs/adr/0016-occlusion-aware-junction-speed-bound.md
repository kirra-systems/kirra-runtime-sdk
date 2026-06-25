# ADR-0016: Occlusion-aware speed bound at junctions (RSS Rule 4, applied laterally)

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG1** (collision — the ego must be able to stop for an emerging cross-vehicle), **SG9** (acts on stale/invalid world state — H9 trigger explicitly names *occlusion*) |
| Cross-refs | gap #1 (competitive analysis); RSS Rule 4 (caution under limited visibility); code: `crates/kirra-planner/src/behavior.rs` (`TrafficControl::OccludedApproach`, `assured_clear_distance_speed_cap`), `crates/kirra-map/src/lanemap.rs` (`LaneGraph::{with_occluded_approach, derive_occluded_approaches, sight_distance}`, `Occluder`, `corner_sight_distance`), `crates/kirra-planner/src/mick.rs` (`derive_controls`); tests: `crates/kirra-planner/tests/occluded_junction.rs` |

## Context

KIRRA already enforces the **forward** assured-clear-distance bound (a stopped hazard in the ego's
own lane beyond its visibility). At a **junction** the dangerous occlusion is **lateral**: a
building / hedge / parked car blocks the view of *cross* traffic, so the ego may not see a vehicle
that will emerge from the unseen approach. H9 (ASIL D) names occlusion directly; without a bound,
the ego could enter a blind junction at a speed from which it cannot stop for emergent cross-traffic
(an SG1 collision).

## Decision

Treat a blind junction approach as **RSS Rule 4 applied laterally**: cap the approach speed to the
**assured-clear-distance speed** — the most the ego may carry and still brake to a stop within the
distance it can actually see toward the conflict.

- The lane map carries a per-approach-lane **sight distance**; absent ⇒ open view ⇒ no cap. It is
  either hand-supplied (`LaneGraph::with_occluded_approach`) **or derived from occluder geometry**
  (`LaneGraph::derive_occluded_approaches` over a set of `Occluder` footprints — buildings / hedges /
  parked cars from the map + perception). The derivation models a corner footprint's
  junction-facing edge: the assured-clear sight toward the conflict is the residual gap from that
  edge to the conflict line (`corner_sight_distance`), worst-cased over occluders and **tighten-only**
  (it never relaxes a stricter hand-set datum).
- `derive_controls` emits a `TrafficControl::OccludedApproach { conflict_line_x_m, sight_distance_m }`
  for the ego's approach lane, **alongside** any sign/signal (a blind STOP approach gets both).
- `evaluate_controls` caps the speed to `assured_clear_distance_speed_cap(sight, decel)` while the
  conflict is still ahead, composed by the existing lowest-cap rule. The doer therefore **creeps**
  a blind junction, fast where the view is open and slow where it is not.

The cap uses the **same RSS Rule 4 formula** as the checker, with a comfortable decel ≤ the checker's
brake, so a capped doer plan is **checker-admissible** — the planner slows; KIRRA still bounds the
result, and any cross-vehicle that becomes visible is caught by its RSS.

## Consequences

- **Positive:** the ego is physically able to stop for emergent cross-traffic at a blind junction;
  a deployment with no sight-distance datum is byte-identical to prior behaviour (no cap).
- **Honest scope:** the sight distance can now be **derived from occluder footprints**
  (`derive_occluded_approaches`), closing the original follow-up. The model is an axis-aligned
  footprint (AABB) on the straight +x approach frame the junction model already uses; a general
  convex polygon and curved-approach projection remain follow-ups. A map with no footprints (and no
  hand-set datum) is byte-identical to prior behaviour (open view, no cap).
- **Composition proven:** `tests/full_stack_capstone.rs` drives a two-junction route while creeping
  the occluded first approach, KIRRA admitting throughout.
