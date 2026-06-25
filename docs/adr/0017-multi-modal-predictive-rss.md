# ADR-0017: Multi-modal predictive RSS (space-time mode bound)

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG1** (longitudinal collision — the ego must keep an RSS-safe gap to an object that will become a hazard, not only one that is one now) |
| Cross-refs | gap #3 (competitive analysis); RSS §4 conjunction; producer: `crates/kirra-ros2-adapter/src/prediction.rs` (`predicted_modes_from_objects`, `slow_loop_modes`), map-intention path: `crates/kirra-map/src/lanemap.rs` (`lane_follow_path`), checker: `crates/kirra-ros2-adapter/src/validation.rs` (`predictive_rss_breach`, called by `validate_trajectory_slow_capped`); tests: `crates/kirra-ros2-adapter/src/prediction.rs` unit tests + `crates/kirra-ros2-adapter/tests/validation_tests.rs` |

## Context

The snapshot RSS pass evaluates each object at its **current** position. The §4 conjunction
(danger needs the object unsafe LONGITUDINALLY **and** LATERALLY at once) is what admits a safe
stationary queue — but it also means a vehicle that is **laterally clear right now** but is
**cutting in** (or turning into the ego's path) passes the snapshot filter, even though it will be
abreast and unsafe a second later. A purely-positional bound cannot see the future hazard; it acts
only once the object is already alongside, which may be too late to keep an RSS gap (an SG1 breach).

## Decision

Add a **predictive** RSS pass that rolls each object forward in **time** and checks the
**time-matched** ego pose, worst-cased over a set of motion hypotheses (`PredictedMode`s). A danger
in **any** mode refuses the trajectory — one dangerous hypothesis is enough.

The producer (`predicted_modes_from_objects` / `slow_loop_modes`) emits, per object:

- **CV** (constant velocity) — always; the kinematic snapshot extrapolation.
- **CTRV** (constant turn rate) — only when a tracker yaw feed is **fresh** for that object
  (turn rate > `CTRV_YAW_EPS_RAD_S`); a stale/absent yaw degrades to CV-only, **not** a fault.
- **lane-follow** (map-intention) — only for a moving object with a geometric lane path
  (`LaneGraph::lane_follow_path`); the curving-in hypothesis the straight-line CV misses.

The checker (`predictive_rss_breach`) reuses the SAME ego-frame projection and the SAME §4
lateral-alignment + longitudinal-overlap gating as the snapshot pass — it is the snapshot test
evaluated at the predicted position, not a new safety primitive.

## Consequences

- **Positive:** a predicted cut-in / turn-in is caught *before* the object is abreast; CTRV and
  lane-follow each catch a curving-in object that CV alone misses (`produced_ctrv_mode_…`,
  `produced_lane_follow_mode_…` — the multi-modal payoff).
- **Fail-closed / derate-only:** no modes supplied ⇒ the predictive pass is a **no-op**
  (`predictive_rss_is_a_no_op_when_no_modes_are_supplied`) — byte-identical to the prior snapshot
  behaviour and to the Nominal WCET path. A mode only ever **adds** a refusal, never relaxes one.
- **No §4 regression:** the predictive pass keeps the conjunction, so it does **not** re-introduce
  the lateral-on-proximity-alone over-rejection (`rss_conjunction_still_rejects_a_lateral_cut_in…`,
  `predictive_rss_does_not_regress_a_lane_keeping_neighbor`).
- **Composition proven:** `tests/full_stack_capstone.rs` exercises a dynamic lead and a lane-follow
  merger against the composed checker.
