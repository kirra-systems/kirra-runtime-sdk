# Curated Autoware interface — build & maintenance

This directory holds the tooling that retires the ad-hoc laptop **trim** (the
hand-replaced `autoware_planning_msgs` overlay) with a **sanctioned,
version-controlled, hash-verified** curated interface: the **real Autoware
package names** carrying **only** the verbatim message closures the Kirra
governor consumes.

- Curated packages (scaffolded under `ros2_ws/src/`):
  - `autoware_perception_msgs` — the `PredictedObjects` closure (`~/input/objects`).
  - `autoware_planning_msgs` — the `Trajectory` closure (`~/input/trajectory`).
- `autoware_control_msgs/Control` is the *intended* `~/input/control_cmd` type
  but is **not r2r-bound yet** (node.rs comment only) — it is the **future third
  closure** to add when that binding lands. Not curated now.
- `nav_msgs/Odometry` + the base deps (`geometry_msgs`, `std_msgs`,
  `builtin_interfaces`, `unique_identifier_msgs`) come from **ros-base** and are
  **not** curated.

**Wire-compatibility rule (non-negotiable):** real package names + **verbatim,
byte-identical** `.msg` closures ⇒ identical RIHS type hash ⇒ DDS delivers
genuine Autoware messages to the governor. **Never hand-edit a `.msg`** — any
edit silently breaks compatibility. The only sanctioned change path is
re-extract + re-verify (below) and a bumped reference in the SRAC
(`docs/safety/MSG_INTERFACE_VERSION_SYNC.md`, KIRRA-OCCY-MSGSYNC-001).

## Scripts

- `extract_closures.sh [REF_SHARE]` — on a host with a reference Autoware
  install (default `/opt/ros/jazzy/share`): copies the verbatim, fully-
  transitive `.msg` closure of each seed (`PredictedObjects`, `Trajectory`)
  into the matching curated package and regenerates each `CMakeLists.txt`.
- `verify_hashes.sh [REF_SHARE]` — the gate: byte-diffs every curated `.msg`
  against the reference; non-zero exit on any mismatch.
- `crossdistro_hash_check.sh [REF_HUMBLE] [REF_JAZZY]` — the ADR-0036
  Humble↔Jazzy wire-safety bench check: curated == each reference, then the
  cross-distro closure diff (step 3). Needs BOTH distros' msg shares.
- `closure_diff.py --ref-a DIR --ref-b DIR --seed pkg/Msg …` — **M3 (#1042)**:
  walks the FULL recursive closure of each seed across two reference `share/`
  trees and byte-compares every message in it, **base packages included**
  (`builtin_interfaces`, `std_msgs`, `geometry_msgs`, …). This is what step 3 of
  `crossdistro_hash_check.sh` now runs — a differing *nested* base message
  (leaf identical, RIHS hash drifted) is no longer invisible. `--leaf-only`
  reproduces the old leaf-only comparison for contrast.
- `closure_diff_selftest.sh` — the **CI-gated** proof (pure python3, no ROS):
  runs `closure_diff.py` over the synthetic `testdata/{humble,jazzy}/` fixtures
  and asserts the closure check catches a nested drift the leaf-only check
  misses (and does not false-alarm on an identical tree). Wired into CI as
  `cross-distro closure comparator self-test (M3)`. The FULL dual-distro
  comparison against real `/opt/ros/{humble,jazzy}/share` stays a bench tool
  until pinned dual-distro msg shares are containerized (the #1042 remainder).

## Build sequence (governor host needs NO apt Autoware packages)

```sh
# Phase 2 (laptop / target), on a host with the reference Autoware msgs:
bash scripts/curated_interface/extract_closures.sh            # populate msg/
bash scripts/curated_interface/verify_hashes.sh               # MUST PASS

# build + source ONLY the curated overlay
cd ros2_ws
source /opt/ros/jazzy/setup.bash
colcon build --packages-select autoware_perception_msgs autoware_planning_msgs
source install/setup.bash

# the real proof: governor builds + Layer-2 passes with ONLY the curated subset
cd ..
cargo build -p kirra-ros2-adapter --features ros2
cargo test  -p kirra-ros2-adapter --features ros2
```

This **supersedes the ad-hoc trim** (`~/aw_msgs_overlay`): the governor host
carries no apt Autoware packages, only the audited curated overlay. Once the
build + Layer-2 tests pass against the curated subset alone, retire the trim.

> **Phase-1 scaffold note:** in the committed repo the `msg/` dirs are empty
> (placeholder READMEs only) and the `CMakeLists.txt` interface lists are empty,
> so the curated packages are **not buildable until populated** by
> `extract_closures.sh`. The verbatim `.msg` are committed in **Phase 2** from
> the laptop, where the reference install lives — Claude Code cannot reach
> `/opt/ros/jazzy`.
