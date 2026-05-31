# Occy / KIRRA — CARLA Scenario Suite for #131

**Doc ID:** KIRRA-OCCY-CARLA-SUITE-001.
**Issue:** #131 (Option-B per-trajectory wiring).
**Cross-refs:** `docs/safety/OCCY_131_OPTIONB_DESIGN.md` (KIRRA-OCCY-OPTIONB-001)
§9 specifies the four scenarios; this doc is the **integrator-facing
runbook** that operationalizes them.
**Status:** specification for the integrator. The kirra-runtime-sdk
ships the adapter + the kernel; the integrator's environment runs the
simulator + Autoware.

---

## A. Prerequisites

| Component | Pin / version | Source |
|---|---|---|
| CARLA simulator | 0.9.15+ | `carla.org/downloads` |
| Autoware | Jazzy build (matching CARLA Jazzy bridge) | `autowarefoundation/autoware` |
| ROS 2 | `Jazzy Jalisco` (Humble works with r2r 0.9.5 too) | `apt install ros-jazzy-desktop-full` |
| lanelet2 | `apt install ros-${ROS_DISTRO}-lanelet2 libboost-serialization-dev` | ROS repos |
| kirra-ros2-adapter binary | built with `--features ros2` | this repo (Phase 4 commit) |
| Lanelet2 map | a CARLA-mapped Lanelet2 file (e.g. Town05.osm + .osm.bin) | CARLA Autoware bridge ships these |
| Vehicle profile | `VehicleConfig::default_urban()` (or integrator's profile) | adapter |

Build the adapter binary:

```sh
source /opt/ros/${ROS_DISTRO}/setup.bash
cd kirra-runtime-sdk
cargo build --release -p kirra-ros2-adapter --features ros2 --bin kirra_ros2_adapter_node
```

The binary lands at `target/release/kirra_ros2_adapter_node`.

---

## B. Lanelet2 fixture generation + Phase 2B sanity test

The Lanelet2 unit tests in `crates/kirra-ros2-adapter/src/corridor/lanelet2.rs`
need a fixture that the kernel CI does not generate. Run this **once per
integrator env**:

```sh
# 1. Generate the binary fixture from the OSM-XML template — see
#    crates/kirra-ros2-adapter/tests/fixtures/README.md for the full
#    recipe (it's a ~15-line OSM-XML + a 5-line Python snippet).
cd kirra-runtime-sdk/crates/kirra-ros2-adapter/tests/fixtures
sh generate_fixture.sh   # the recipe from fixtures/README.md

# 2. Confirm Phase 2B's Lanelet2CorridorSource against the fixture.
cd kirra-runtime-sdk
cargo test -p kirra-ros2-adapter --features ros2
```

`cargo test` runs the three `lanelet2_tests` (`test_load_and_extract_corridor`,
`test_unknown_lanelet_id_returns_error`, `test_corridor_source_trait_impl`)
in addition to the Phase 1/2A/3/4 default-lane tests.

---

## C. The four injection scenarios (design §9)

Each scenario:
1. Boot Autoware-in-CARLA with its standard launch.
2. Start the Governor adapter:
   ```sh
   ./target/release/kirra_ros2_adapter_node \
       --corridor-source lanelet2 \
       --map-bin /opt/autoware/maps/town05/lanelet2_map.osm.bin \
       --lanelet-ids 1001,1002,1003
   ```
3. **Remap the vehicle interface's command input** from
   `/control/command/control_cmd` (Autoware's gated output) to
   `/kirra_governor/output/control_cmd` (the adapter's output). One
   remap line in the integrator's `vehicle_interface.launch.xml`.
   This is the entire integration delta on the Autoware side.
4. Drive a baseline mission (start → goal) and confirm the Governor
   passes through clean trajectories (look for
   `verdict=Accept elapsed_us=…` log lines).
5. Inject the fault; observe both the published topic and the
   structured logs.

### Scenario 1 — Perception dropout (SG9, subscription staleness)

| | |
|---|---|
| Inject | Stop publishing `~/input/objects` AND `~/input/odometry` for > 500 ms. (Easiest: `ros2 node kill /perception_objects` for a moment.) |
| Expected adapter output | `subscription_staleness_mrc` warning + `OutgoingControlCommand { linear_velocity_mps: 0.0, steering_angle_rad: 0.0, accel_mps2: -max_decel_mps2 }` on `~/output/control_cmd` within 500 ms of the dropout. |
| Expected vehicle behaviour | Service-brake ramp (Autoware's planning may continue, but the integrator remap means the vehicle interface sees only the Governor's MRC). |
| Log signature | `subscription_staleness_mrc timeout_ms=500 asset_id=...` |

### Scenario 2 — Trajectory clipping the Lanelet2 corridor (SG2)

| | |
|---|---|
| Inject | Spawn a roadwork obstacle, OR programmatically modify the planner's output to include one pose 3 m outside the drivable bounds (CARLA's static-obstacle API + scenario_runner script). |
| Expected adapter output | Slow-loop emits `verdict=MRCFallback` with cause `DrivableSpaceDeparture`; fast loop on the next tick publishes MRC. |
| Expected vehicle behaviour | Service-brake; Autoware's behavior_path_planner will re-route once the obstacle is registered, but the Governor's MRC fires immediately. |
| Log signature | `trajectory_verdict verdict=MRCFallback elapsed_us=… asset_id=…` followed by repeated `fast_loop_verdict verdict=MRCFallback` cycles |

### Scenario 3 — Cut-in agent (SG1 RSS over horizon)

| | |
|---|---|
| Inject | Spawn a stationary `vehicle.tesla.model3` 4 m ahead of the ego, in-lane. Set the planner to a 10 m/s approach. |
| Expected adapter output | `validate_trajectory_slow` runs `longitudinal_safe_distance(10, 0, 0.5, 2.5, 4.5, 4.5)` → required gap ≫ 4 m → `TrajectoryVerdict::MRCFallback`. The fast loop publishes MRC on every tick until the perceived object clears (or the planner re-plans to a stopping trajectory). |
| Expected vehicle behaviour | Service-brake stop short of the cut-in. |
| Log signature | `trajectory_verdict verdict=MRCFallback elapsed_us=…` |

### Scenario 4 — Over-aggressive trajectory (SG3 kinematics)

| | |
|---|---|
| Inject | Modify the planner to issue a velocity step that implies `accel > max_accel_mps2` (e.g. `v[0]=5, v[1]=30, dt=0.5 → implied_accel = 50 m/s²` vs. the kernel default `2.5 m/s²`). Easiest as a one-shot scenario_runner override. |
| Expected adapter output | Per-pose `validate_vehicle_command` returns `DenyBreach(InvalidTimeDelta)` (if dt is also non-physical) or `DenyBreach(NanInf*)` for finite-but-extreme inputs that produce non-finite intermediates; either short-circuits the slow loop to `MRCFallback`. For a clean over-aggressive accel that passes Priority-0/1, the kernel returns `ClampLinear`; the slow loop records the clamp and aggregates to `TrajectoryVerdict::Clamp` → the fast loop publishes the derated velocity. |
| Expected vehicle behaviour | Either MRC service-brake (Deny) or derated acceleration (Clamp). |
| Log signature | `trajectory_verdict verdict=MRCFallback` or `verdict=Clamp` |

---

## D. Pass criteria

For each of the four scenarios:
1. **Within one planning cycle** (≤ 100 ms after the fault is injected),
   the `~/output/control_cmd` topic must receive an `OutgoingControlCommand`
   with `linear_velocity_mps == 0.0` and `accel_mps2 == -max_decel_mps2`
   (the MRC), OR a derated `linear_velocity_mps` (scenario 4's Clamp arm).
2. The tracing JSON stream must contain the expected log signature
   from the table above, with the asset id matching the test ego.
3. The vehicle interface's actual brake command (visible on
   `/vehicle/status` or equivalent integrator-side topic) reflects the
   MRC within one control cycle.

---

## E. Contrast run — "why you need an independent Governor"

Run each scenario **with the Governor bypassed** — restore the original
vehicle_cmd_gate remap so the vehicle interface listens to Autoware's
output directly:

| Scenario | Bypassed Autoware behavior | Governor-mediated behavior |
|---|---|---|
| 1. Perception dropout | Planner may continue with stale objects until the dropout self-clears | MRC within 500 ms |
| 2. Corridor clipping | behavior_path_planner detects (eventually) and re-plans; can take 1–2 s | MRC immediately on detected departure |
| 3. Cut-in | RSS check exists in some Autoware configurations; in others the planner allows the unsafe approach | MRC immediately |
| 4. Over-aggressive accel | vehicle_cmd_gate's limit filter may smooth, but with `max_decel_mps2` defaults higher than Autoware's | Reject → MRC or Clamp |

The contrast is the integrator's "evidence of independent value"
artifact — pair it with the design doc's §1 motivation paragraph and
KIRRA-OCCY-MANUAL-001 §5 (the SEooC claim) when presenting the safety
case.

---

## F. What this suite does NOT cover

- **Quantitative WCET measurement** — the per-trajectory + fast-loop
  budgets (~10 ms / 200 µs) are already gated by
  `wcet_gate::ci_gate_tests` on the kernel CI. CARLA adds end-to-end
  latency that's not in scope for the safety case (it's an integrator
  performance concern, not a Governor safety property).
- **Localization-error injection** — covered separately by S8 / #120
  characterization. SG2 lateral margin is the output of that work.
- **Multi-asset coordination** — single-asset suite for the pilot.
  Phase 5+ adds multi-asset.

---

## G. Re-running

The Lanelet2 fixture regeneration step (B) is one-shot per integrator
env (or when boost-serialization version changes). The four scenario
runs (C) are repeated on every Governor release branch.
