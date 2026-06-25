# ADR-0020: Doer-invariant safety case (KIRRA bounds any doer)

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG1** (collision — KIRRA's RSS/containment verdict is the bound), **SG7** (the same safety check applies regardless of the command's origin / doer) |
| Cross-refs | `docs/COMPETITIVE_PLANNER_ANALYSIS.md` §5.5 ("prove KIRRA bounds a learned planner"); the Mick `Planner` seam (`kirra_planner::plan_for_intent`); checker `kirra_ros2_adapter::validate_trajectory_slow`; tests: `crates/kirra-planner/tests/{adversarial_doer,learned_doer,learned_maneuver}_bounded_by_kirra.rs` + the capstone `learned_doer_invariance_capstone.rs` |

## Context

The Kirra thesis is the doer–checker split: a planner (the DOER) *proposes* a trajectory; KIRRA
(the CHECKER) *bounds* it. The doer is swappable — geometric, learned, LLM-driven — and is never
trusted for safety. The strategic claim that follows (roadmap §5) is that **swapping in a learned
planner does not change the safety case.** The per-doer tests each demonstrated *one* doer bounded:
the geometric Occy, a real learned net (speed-only and 2-D maneuvering), and a black-box reckless
doer. What was asserted only in prose was the property that ties them together.

## Decision

State and test the safety case's doer-invariance as one mechanical property:

> **KIRRA's verdict is a pure function of (trajectory, world). It does not depend on which doer
> authored the trajectory.**

This is structural — `validate_trajectory_slow` takes a trajectory and a world, and has **no doer
parameter** — and `learned_doer_invariance_capstone.rs` makes it observable: a heterogeneous fleet
(geometric Occy, a learned net in safety-aware and progress-only regimes, a reckless black box) is
driven through the SAME intent and world behind the one generic `Planner` seam, and

1. a single **doer-agnostic geometric classifier** (does the trajectory drive into the hazard?)
   predicts KIRRA's admit/reject for every author, identically;
2. the admit/reject split **crosses doer families** — the geometric planner and the aligned learned
   net are both admitted; the misaligned learned net and the reckless black box are both rejected;
3. on a clear road every author is admitted (the bound is **precise**, not blanket); and
4. every rejected author falls back to the same always-available, admissible safe-stop.

## Consequences

- **Positive:** "swap in a learned planner, the safety case is unchanged" is a tested claim, not an
  aspiration. The checker catches a *misaligned learned* doer exactly as it catches a hand-coded
  reckless one, and governs (does not punish) an *aligned* doer of either family.
- **Why it holds (SG7):** the verdict is computed from the proposed trajectory and the world only;
  the doer cannot earn trust by identity. This is the same doer-agnostic discipline SG7 enforces on
  the command-ingress path, here applied to the planning seam.
- **Honest scope:** the "learned net" is the in-repo seeded-ES-fit MLP doer (`LearnedPlanner` /
  `LearnedManeuverPlanner`), not a production deep net — but it is a genuine learned model behind
  the real seam, and the invariance argument is independent of the net's capacity (a bigger net is
  just another author the pure-function verdict is blind to). Swapping in a production net (NVIDIA /
  Hydra-MDP-scale) is integration, not a change to the safety case this ADR records.
