# ADR-0021: Unprotected-turn gap-acceptance

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG5** (junction negotiation — the ego must not commit an unprotected turn into traffic it must yield to); backstopped by **SG1** (KIRRA's head-on / crossing RSS catches a misjudged acceptance) |
| Cross-refs | roadmap #2 "Remaining: richer intent — turn/yield negotiation at junctions" (builds on ADR-0019 right-of-way); code: `crates/kirra-planner/src/behavior.rs` (`accept_turn_gap`, `ConflictApproach`, `DEFAULT_TURN_CRITICAL_GAP_S`), `crates/kirra-planner/src/mick.rs` (`turn_conflict_approaches`, the `TurnAt` gate); tests: `behavior.rs` unit tests + `crates/kirra-planner/tests/turn_gap_acceptance.rs` |

## Context

`MickIntent::TurnAt` grounded a turn to "follow the route corridor through the junction," and KIRRA
bounded the *geometry* (a too-tight arc is refused). What it did not yet model is the *timing* of an
**unprotected** turn: a left turn across oncoming traffic, or a turn from a minor road, must wait for
an adequate gap in the stream the ego has to yield to. Following the arc the moment it is resolved
would commit the ego into that stream and rely entirely on KIRRA to MRC it — safe, but it turns
every unprotected turn into a near-miss rather than a negotiated one. ADR-0019 gave the ego the
right-of-way relation (who must yield to whom); this adds the *gap-acceptance* decision on top.

## Decision

Gate `TurnAt` on a standard gap-acceptance test before committing the turn:

- The conflict point is the **junction** (the ego lane's terminus).
- The conflicting stream is every perceived vehicle **closing** on that point which the ego does
  **not** have asserted priority over — i.e. NOT on the right-of-way cede set (`cedes_to_ego_ids`,
  derived in ADR-0019). Each contributes its time-to-conflict (`distance / closing-speed`);
  non-closing vehicles (stopped / receding / tangential) are not conflicts.
- `accept_turn_gap` proceeds only if **every** conflicting approach is more than the critical gap
  (`DEFAULT_TURN_CRITICAL_GAP_S` = 4 s, the HCM left-turn figure) away in time; otherwise the ego
  **HOLDs** at the junction (`PlanOutput::safe_stop`) and waits for a gap.

Fail-closed: the test is `time > critical_gap` so a NaN time (bad data) or a vehicle already at the
conflict (`time ≤ 0`) HOLDs. A **protected** turn (the conflicting vehicle is on the cede set)
bypasses the wait — the ego takes the priority the map grants. KIRRA's head-on / crossing RSS is the
independent backstop on whatever the ego commits.

## Consequences

- **Positive:** the doer now *negotiates* an unprotected turn — it waits for a real gap and takes
  one when it opens, and asserts right-of-way where the map grants it — instead of leaning on KIRRA
  to reject a premature commit. The doer decides WHEN to go; KIRRA still bounds WHAT it does.
- **Fail-safe direction:** the gate only ever turns motion into a HOLD; it cannot author a turn KIRRA
  would not also admit, and a HOLD is trivially admissible. The Nominal corridor-follow path is
  unchanged when there is no conflicting traffic.
- **Honest scope:** the conflict point is the junction-terminus proxy and the conflicting stream is
  "closing on it," a conservative model adequate for the straight-approach junctions the stack
  builds; per-conflict-point geometry (separate left-turn vs cross conflict points) and a
  speed-dependent critical gap are follow-ups. The critical gap is a fixed constant, not yet
  per-maneuver.
