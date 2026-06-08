# PMON-004 Sub-Gate 1 — Live-DDS Full-Node Integration Results

**Date:** 2026-06-08  **Harness:** `crates/kirra-ros2-adapter/tests/perception_mechanism_gate_ros2.rs::run_full_node_integration` (`#[ignore]`, run with `-- --ignored` on a ROS 2 Jazzy dev box).

## Setup
`run_adapter` spawned over real DDS sharing `AdaptorState`; a harness node publishes
trajectory/odom(/objects) on the resolved `~/input/*` topics. Curated workspace:
`autoware_perception_msgs`, `autoware_planning_msgs`, `autoware_map_msgs`,
`autoware_control_msgs`. Verdict observed from the shared slot via `current_verdict("ego")`.

## Result — PASS
**Derate OFF (negative control):** every scenario → `Accept` (perception cap never applied).

**Derate ON:**
| event | objects | verdict | meaning |
|---|---|---|---|
| cold start | none ingested yet | **Clamp** | fail-closed before valid perception |
| scenario b | plausible | **Accept** | clean trajectory passes |
| scenario c1 | single implausible | **Clamp** | perception cap applied |
| scenario c2 | graded implausible | **Clamp** | perception cap applied |
| scenario d | silent past TTL | **Clamp** | staleness → fail-closed |

The ON/OFF delta confirms the perception-derate mechanism gates the trajectory live,
and the governor fails closed both at cold start and on stale perception.

## Disposition
- Resolves the long-standing scenario-b `MRCFallback`: it was a **startup failure**
  (`run_adapter` erroring on a missing `autoware_map_msgs` typesupport, error swallowed
  by `let _ =`) — i.e. delivery/wiring (Branch A), **not** a logic or staleness bug.
- Verdict path (`kinematics_contract.rs`) untouched.

## Open / caveats
- AOU-PERCEPTION-FRAME-001 remains OPEN: twists are synthetic (chosen by the harness);
  absolute map-frame twist from real Autoware is sub-gate 2 (AWSIM). Production keeps
  `KIRRA_PERCEPTION_DERATE_ENABLED` OFF.
- Test is `#[ignore]` (live ROS graph; not CI).
- Known wart: r2r/DDS has no clean shutdown (Phase 4), so the process can SIGABRT at
  exit *after* the test reports `ok` — harmless to the result.
