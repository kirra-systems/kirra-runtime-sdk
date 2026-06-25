# ADR-0025: Joint path+speed optimization (sampling-based, opt-in)

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | n/a as a bound — a planner **comfort/progress** optimization. KIRRA (containment + kinematics) remains the safety authority; the optimizer only ever proposes an in-corridor line, and a bad proposal is MRC'd like any other. |
| Cross-refs | roadmap #3 ("joint path+speed optimization"); code: `crates/kirra-planner/src/lib.rs` (`GeometricPlanner::optimize_guide`, `offset_guide`, `GeometricPlannerConfig::joint_path_optimize`, `JOINT_*` constants); tests in the same module |

## Context

The planner computed **path then speed** (decoupled): the corridor centerline (Chaikin-smoothed)
fixes the geometry, then the forward–backward velocity profile speeds it. Roadmap #3's last item is to
co-optimize the two — choose a path *shape* that admits more speed (a flatter / shorter line through a
bend), trading lateral position for traversal time. The user scoped this to the **sampling-based**
realization (a bounded candidate search), not a QP / iLQR solver.

## Decision

Add an opt-in (`joint_path_optimize`, default `false`) sampling-based spatiotemporal optimizer. After
the reference guide is built, `optimize_guide` tries a bounded vocabulary of `2·N+1` ramped
lateral-offset candidate paths (`offset_guide`: 0 at the ego, ramping to ±δ, held) and keeps the one
with the lowest **time to reach the goal** — scored through the *same* velocity profile, so a flatter
path's higher achievable speed (and a shorter path's lesser distance) both count — plus a small
deviation penalty. The centerline (offset 0) is always a candidate, so the result is never worse than
the baseline.

- **Deterministic + WCET-bounded:** a fixed candidate count, each one velocity-profile pass.
- **Containment:** each candidate's offset is bounded by the corridor half-width minus the footprint
  AND a **swing slack** (the vehicle rectangle reaches further laterally when angled to the corridor
  on a curve). KIRRA independently bounds the result; the optimizer never proposes out of corridor.
- **No-op where it should be:** on a straight road every candidate has the same traversal time ⇒ the
  zero-penalty centerline wins ⇒ the plan is byte-identical (verified). `false` ⇒ unchanged entirely.

## Consequences

- **Positive:** on a bend whose curvature binds the speed, the optimizer picks a flatter line that
  reaches the goal sooner, and KIRRA admits the in-corridor result — path SHAPE and SPEED chosen
  together, the roadmap-#3 capability, as one bounded deterministic search.
- **Honest scope — the load-bearing finding:** the existing **Chaikin smoothing already minimizes
  path curvature**, so a *constant* lateral offset adds little on an already-smoothed gentle corridor;
  the joint gain is material mainly on a **tight** bend (where curvature genuinely limits speed) with
  a corridor wide enough to deviate. A stronger optimizer needs an **apex-varying** offset profile
  (peaks at the apex, returns) rather than a ramped-constant one, and a true **oriented-footprint**
  containment check rather than the swing-slack bound (the x-indexed boundary check is unreliable on a
  tight bend). Both are the follow-up toward a coupled (QP / iLQR) solve — the heavier path #3 also
  names. Default-off keeps this an opt-in experiment until that depth lands.
