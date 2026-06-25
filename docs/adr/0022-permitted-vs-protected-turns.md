# ADR-0022: Permitted vs protected turns (signal-aware gap-acceptance)

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG5** (junction negotiation — the ego must yield on a permissive movement and may proceed only on a protected one); backstopped by **SG1** (KIRRA's head-on / crossing RSS catches a red-light-runner) |
| Cross-refs | composes ADR-0021 (turn gap-acceptance) with the signal path; code: `crates/kirra-planner/src/behavior.rs` (`SignalState::ProtectedGreen`), `crates/kirra-planner/src/mick.rs` (`turn_is_protected`, `protected_turn_cedes`, the `TurnAt` gate); tests: `behavior.rs` unit + `crates/kirra-planner/tests/permitted_protected_turn.rs` |

## Context

ADR-0021 gave `TurnAt` gap-acceptance for an unprotected turn, but treated every signal the same. Two
green movements are not the same: a **solid green** is *permissive* — "proceed if clear", so a left
turn must still yield to oncoming — while a **green arrow** is *protected* — the conflicting streams
hold a red, so the turn has priority. Without the distinction the ego would either over-yield on a
protected arrow (waiting for traffic that is stopped) or, worse, under-yield on a permissive green.
The `SignalState` set deliberately omitted turn arrows ("they require maneuver intent"); the
`TurnAt` grounding is exactly that maneuver-intent layer.

## Decision

Add `SignalState::ProtectedGreen` (a green turn arrow). Longitudinally it is identical to `Green`
(proceed, no stop) — the protected/permitted difference lives in the turn-maneuver layer:

- A **protected** turn (`turn_is_protected`: the ego lane is traffic-light controlled and its live
  signal is `ProtectedGreen`) asserts priority. Every vehicle closing on the junction is folded into
  the cede set (`protected_turn_cedes`), so **both** gap-acceptance and the planner's predictive
  yield treat them as yielding and the ego proceeds.
- A **permissive** movement — a solid green, a sign, an uncontrolled approach, or (fail-safe) an
  absent/unknown signal — keeps the map's cede set and must gap-accept (ADR-0021): a closing vehicle
  it has no priority over within the critical gap → HOLD.

One cede-set mechanism drives both gates, so the gap decision and the predictive yield cannot
disagree. KIRRA's head-on / crossing RSS independently backstops whatever is committed.

## Consequences

- **Positive:** on the *identical* tight gap, a permitted green HOLDs and a protected arrow GOES —
  the signal decides, as the road rules intend. A protected turn no longer over-yields to a stopped
  cross stream; a permitted one no longer under-yields.
- **Fail-safe direction:** only a `ProtectedGreen` grants the bypass; every other state (including an
  absent or unknown signal, which `derive_controls` already defaults to red) is permissive and
  yields. The longitudinal red/amber/stop behaviour is unchanged, and a red-light-runner during a
  protected turn is still caught by KIRRA's RSS (SG1).
- **Honest scope:** "protected" keys on the ego-lane signal being a `ProtectedGreen`; per-movement
  arrows (a protected left while the through is permissive on the same approach) would need a
  per-turn signal channel, a follow-up. The integrator supplies the live `ProtectedGreen` where the
  controller asserts a protected phase.
