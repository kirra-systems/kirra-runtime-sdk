# ADR-0026: Stackelberg interaction model (observed-cooperation gap decision)

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG5** (junction negotiation — the ego must not assert into an agent it cannot trust); backstopped by **SG1** (KIRRA's RSS catches a misjudged interaction) |
| Cross-refs | competitive gap #5 ("no interaction / game-theoretic reasoning — plans against a snapshot"); extends ADR-0021 (turn gap-acceptance); code: `crates/kirra-planner/src/behavior.rs` (`interactive_proceed`, `InteractiveConflict`, `agent_time_to_conflict`, `DEFAULT_AGENT_REACCEL_MPS2`, `AGENT_YIELD_SPEED_MPS`), `crates/kirra-planner/src/mick.rs` (`turn_interactive_conflicts`, the `TurnAt` gate); tests in both modules |

## Context

Occy planned a junction against a **snapshot**: gap-acceptance (ADR-0021) read each conflicting
agent's time-to-conflict as `distance / closing-speed`. That has a latent failure: a **slow** agent
(e.g. crawling at 1 m/s a few metres from the junction) reads as `8 m / 1 = 8 s`, far over the
critical gap — so the ego would **assert into it**, trusting it to stay slow. It might instead be
about to re-accelerate. Competitive gap #5 is exactly this: the planner does not reason about how
agents *respond*.

## Decision

Model the junction as a **Stackelberg leader–follower** interaction: the ego is the leader, each
conflicting agent the follower, and the ego reasons about the follower's worst-case response —
without ever depending on assumed cooperation.

`interactive_proceed` admits the move only if **every** conflicting agent's worst-case time to the
conflict (`agent_time_to_conflict`) exceeds the ego's critical gap, where:

- a **committed** agent (closing ≥ `AGENT_YIELD_SPEED_MPS`) is held at its constant closing speed
  (`d/v`) — classic gap-acceptance, which this subsumes;
- a **slow / potentially-yielding** agent is **not trusted to stay slow**: the ego models it
  re-accelerating from its current speed at `DEFAULT_AGENT_REACCEL_MPS2` (`d = v·t + ½·a·t²` solved
  for `t`) — the safe follower response.

So the ego **exploits a genuinely-yielded position** (a slow agent far enough that even
re-accelerating it cannot arrive within the gap → assert) while **closing the reckless case** (a
slow-but-close agent now HOLDs, where naive `d/v` would have asserted). The `TurnAt` grounding uses
it in place of plain gap-acceptance; the protected-turn / right-of-way bypasses (ADR-0019/0022) are
unchanged. KIRRA's RSS independently backstops whatever is committed.

## Consequences

- **Positive:** the doer now reasons about interaction — it proceeds on an observed yield and waits
  on an untrustworthy one — instead of trusting a snapshot. This both **improves progress** (a
  yielded agent is exploited) and **fixes a latent safety hole** (a slow-but-close agent is no longer
  wrongly admitted).
- **Fail-safe direction:** the model only ever makes a slow agent count for *more* (worst-case
  re-acceleration), never less; a NaN input HOLDs; and KIRRA remains the bound. Asserting on assumed
  (not observed) cooperation is never done.
- **Honest scope:** "observed cooperation" is grounded in the agent's snapshot speed + distance
  (slow ⇒ model re-acceleration), not a richer intent estimate; a reactive IDM-style forward
  simulation, and a true game-theoretic equilibrium (Nash / level-k), remain heavier follow-ups. The
  re-acceleration constant is a fixed conservative value, not per-class.
