# kirra-ros2-adapter

ROS 2 adapter for the Kirra Governor. Implements the Option-B per-trajectory
wiring on top of Autoware's ROS 2 interfaces — see
`docs/safety/OCCY_131_OPTIONB_DESIGN.md` (KIRRA-OCCY-OPTIONB-001) for the
architecture this crate instantiates. Tracking issue: #131.

## Phase 1 scope (this commit)

- `AcceptedTrajectory` state machine + `AdaptorState` (DashMap-backed).
- `CorridorSource` trait + `MockCorridorSource` (Phase 2 adds the Lanelet2 impl).
- `run_adapter` — r2r-backed ROS 2 node skeleton with stubbed subscriptions
  and slow- / fast-loop task stubs. Gated behind the `ros2` feature.

Phase 1 deliberately ships **no verdict logic**: the slow- and fast-loop
stubs log receipt and the slow loop never installs an `AcceptedTrajectory`.
Phase 2 turns the slow loop into a real
`validate_trajectory_containment` + per-pose kinematics + RSS driver;
Phase 3 turns the fast loop into the conformance check; Phase 4 wires MRC
injection + the CARLA demo.

## Build

### Default (no ROS deps) — the safety-kernel CI lane

```sh
cargo build -p kirra-ros2-adapter
cargo test  -p kirra-ros2-adapter
```

Builds only the state machine + corridor trait. No r2r, no Autoware
dependencies. This is what the workspace CI runs.

### With ROS 2 (`ros2` feature) — integrator builds

```sh
source /opt/ros/jazzy/setup.bash    # or humble, kilted — whichever the integrator runs
cargo build -p kirra-ros2-adapter --features ros2
```

Pulls `r2r = "=0.9.5"` (pinned) and compiles the node skeleton in
`src/node.rs`. **Requires `ROS_DISTRO` and `AMENT_PREFIX_PATH` to be set
in the build shell** — r2r's `build.rs` panics otherwise. This is the
expected gate; the safety-kernel CI does not have ROS sourced and so
correctly skips the `ros2` feature.

Supported ROS distros via r2r 0.9.5: Humble, Iron, Jazzy. Pin the
integrator's Autoware release in the integrator's `package.xml`.

## Why r2r (not rclrs)?

Decision recorded in the S131 discovery report:
1. `cargo build` only — no colcon hookup, matches Kirra's existing build.
2. Async-from-the-ground-up (futures + streams) — matches the
   slow-loop / fast-loop tokio model the design assumes.
3. Runtime-agnostic — composes cleanly with the existing tokio
   ecosystem used elsewhere in the workspace.

The decision is reversible: this adapter is the only crate that touches
ROS, so a swap to rclrs would be bounded.
