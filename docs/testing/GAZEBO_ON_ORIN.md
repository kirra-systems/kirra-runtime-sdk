# Watch Taj + KIRRA stop a robot in Gazebo (on the Orin)

A single `ros2 launch` spins up a Gazebo robot that drives itself toward a wall while a
naive "doer" keeps commanding it forward — and **Taj's corridor cap + the KIRRA governor
brake it to a controlled stop before the wall.** It's the doer-checker thesis made
watchable, and it runs entirely on the Jetson Orin NX (single-box, no second machine, no
CARLA). Gazebo (Classic) is ARM-native and renders fine on the Orin for one small robot.

## What's in the demo

```
doer_commander ─/cmd_vel_raw→ cmd_vel_interceptor ─/cmd_vel→ Gazebo robot
   (naive: 1.2 m/s forward)         │   ▲
                                    │   └── Taj cap (perception_governor → taj_service)
   Gazebo lidar ─/scan─────────────┘        applied BEFORE the KIRRA governor
```

- **`worlds/kirra_corridor.world`** — a short corridor with side walls and a red end wall.
- **`urdf/kirra_bot.urdf`** — a differential-drive robot with a forward 180° lidar
  (`/scan`); its diff-drive plugin consumes `/cmd_vel` (the KIRRA-enforced command).
- **`doer_commander`** — the untrusted proposer: commands a constant 1.2 m/s forward and
  never brakes. KIRRA + Taj are what stop the robot.
- The **Kirra safety stack** (`kirra_with_robot.launch.py`, included): the interceptor with
  the Taj cap on, the `perception_governor`, and the Rust planner/Taj sidecars.

## Prerequisites (on the Orin)

```bash
# ROS 2 + Gazebo Classic ROS integration (JetPack 6 / Ubuntu 22.04 / Humble)
sudo apt-get install -y ros-humble-gazebo-ros-pkgs ros-humble-robot-state-publisher

# Build the Rust sidecars + verifier once (or run scripts/orin_bringup.sh)
cd ~/kirra-runtime-sdk
cargo build --release -p kirra-mick --example planner_service --example taj_service
cargo build --release --bin kirra_verifier_service

# Build the ROS package
cd ros2_ws && colcon build --packages-select kirra_safety && source install/setup.bash
```

## Run it

```bash
export KIRRA_ADMIN_TOKEN=test-token KIRRA_SUPERVISOR_RESET_KEY=test-reset-key
ros2 launch kirra_safety kirra_gazebo_demo.launch.py
```

That one command starts **everything**: the KIRRA verifier, the planner + Taj sidecars, the
safety nodes, Gazebo + the robot, and the doer. (If you already have the verifier/sidecars
up via `scripts/orin_bringup.sh --serve`, pass `start_verifier:=false` — the included stack
still starts its own sidecars, so use the bring-up OR the launch, not both, to avoid
double-binding the ports.)

## What you'll see

The robot accelerates down the corridor at the doer's commanded speed, then — as it nears
the red wall — **slows and stops a short standoff before it**, and holds, even though the
doer is still commanding 1.2 m/s. The deceleration is Taj's assured-clear-distance cap
(the clear distance shrinks as the wall approaches) applied ahead of the governor.

Watch the decision live:

```bash
ros2 topic echo /kirra/enforcement_action     # e.g. Allow:v=1.20  →  Allow:v=0.74|PERCEPTION_CAP  →  ...|PERCEPTION_CAP (v→0)
ros2 topic echo /kirra/perception_speed_cap    # the ACD cap (m/s) falling as the wall nears
ros2 topic echo /kirra/perception_health       # OK:clear=…m:cap=…
ros2 topic echo /cmd_vel                        # the enforced command actually driving the wheels
```

`rviz2` with the `/scan` and `/odom` displays makes the lidar fan and the stop obvious.

## Knobs

| arg / param | default | effect |
|---|---|---|
| `forward_speed_mps` (launch) | `1.2` | the doer's naive commanded speed. |
| `gui` (launch) | `true` | `false` runs gzserver headless (data only — useful over SSH). |
| `start_verifier` (launch) | `true` | set `false` if the verifier is already running. |
| `decel_mps2` / `margin_m` (perception_governor params) | `1.5` / `0.4` | the ACD model: lower decel or larger margin → the cap bites earlier / stops further back. |
| `confidence_floor` | `0.5` | corridor confidence below this → unhealthy → 0 cap (fail-closed). |

To make the derate more dramatic, raise `forward_speed_mps` or lower `decel_mps2` (a gentler
assumed brake caps the speed from further out). Remember: **tune the doer / the ACD model,
never the checker's envelope** — KIRRA's bound is the invariant.

## Fail-closed, live

Kill the Taj sidecar mid-run (`pkill -f taj_service`) and the robot **stops**: the cap topic
goes stale, the interceptor fails closed (the `PERCEPTION_STALE` reason appears on
`/kirra/enforcement_action`), and the doer's forward command no longer reaches the wheels. A
perception fault holds the robot rather than letting it coast into the wall.

## Note on Gazebo versions

This demo targets **Gazebo Classic** (`gazebo_ros_pkgs`) for the fewest moving parts. The
modern **Gazebo (gz sim, Harmonic) + `ros_gz`** path works too — swap the `gzserver`/`gzclient`
processes for `ros_gz_sim`'s `gz sim`, add a `ros_gz_bridge` for `/scan` and `/cmd_vel`, and
keep everything else (the safety stack, the doer, the URDF sensors) the same.
