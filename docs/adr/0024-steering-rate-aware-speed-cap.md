# ADR-0024: Steering-rate-aware (curvature-transition) speed cap

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | n/a as a bound — a **comfort** refinement kept below the checker's steering-rate ceiling (SG3 stays the safety authority); it makes a sharp transition checker-admissible rather than relying on the per-pose clamp. |
| Cross-refs | roadmap #3 ("an explicit steering-rate cap for very sharp transitions"); code: `crates/kirra-planner/src/lib.rs` (`steering_rate_speed_cap`, `GeometricPlannerConfig::{max_steering_rate_rads, wheelbase_m}`, the `velocity_profile` static-limit loop); tests in the same module |

## Context

The velocity profile already caps speed for a curve's curvature κ via the comfort lateral-accel
bound `√(a_lat/κ)` — that bounds the steering **angle** `δ = atan(L·κ)`. But it does not bound the
steering **rate** `δ̇`. A sharp **transition** — κ changing fast along the path (a tight entry/exit,
an S-bend) — demands `δ̇ ∝ v·dκ/ds`, which can exceed the comfortable (and the checker's hard)
steering-rate envelope even where κ itself is modest. The doer would then propose a transition the
checker clamps per-pose, instead of slowing for it.

## Decision

Add a curvature-transition speed cap to the profile's per-station static limit. From the bicycle
relation `δ = atan(L·κ)`, `δ̇ = [L/(1+(L·κ)²)]·(dκ/ds)·v`; solving `δ̇ = δ̇_max` for `v`:

```
v_cap = δ̇_max · (1 + (L·κ)²) / (L · |dκ/ds|)
```

(`steering_rate_speed_cap`), folded into the profile's `min(...)` alongside the curvature cap, with
`dκ/ds` from the Menger curvature at adjacent stations. `max_steering_rate_rads` is kept **below**
the checker's hard steering-rate ceiling, so a capped plan is checker-admissible — the doer slows
the transition rather than being clamped. The backward decel-feasibility pass then makes the slow-
down reachable (the ego eases off *before* the transition).

## Consequences

- **Positive:** a sharp entry/exit is taken at a speed whose steering rate stays in the comfort
  envelope; the proposal is admissible where it would otherwise be clamped. Composes with the
  curvature (lateral-accel) cap and Chaikin smoothing — smoothing lowers `dκ/ds`, this bounds the
  speed for whatever transition remains.
- **No-op where it should be:** `dκ/ds ≈ 0` (a straight or constant-curvature path) ⇒ the cap is
  `∞` ⇒ the profile is byte-identical, so the WCET-critical straight path is unchanged. Setting
  `max_steering_rate_rads = 0` disables it.
- **Comfort, not safety:** the checker's per-pose steering-rate ceiling (SG3) remains the bound; this
  only shapes the doer's speed so it stays inside it. Honest scope: a single comfort steering-rate
  constant and a fixed wheelbase — a speed-/platform-dependent rate and a coupled path+speed (MPC)
  solve are follow-ups (roadmap #3 "joint path+speed optimization").
