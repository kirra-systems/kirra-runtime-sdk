# kirra_safety

ROS2 safety interlock package for the Kirra runtime legitimacy engine.

Provides four nodes that enforce kinematic contracts, derive a perception speed cap, monitor sensor health, and gate motion commands based on fleet posture:

- **cmd_vel_interceptor** — intercepts `/cmd_vel`, enforces via Kirra, publishes to `/cmd_vel_safe`
- **perception_governor** — turns `/scan` into Taj's corridor and an assured-clear-distance (ACD) speed cap on `/kirra/perception_speed_cap` (see below)
- **sensor_monitor** — reports LiDAR, IMU, camera, and odometry health to Kirra
- **posture_subscriber** — bridges the Kirra SSE posture stream to ROS2 topics and triggers emergency stops on `LockedOut` transitions

## Taj corridor → cmd_vel speed cap (opt-in)

The `perception_governor` node wires **Taj** (the geometric perception layer, ADR-0015)
into the live `cmd_vel` path. It subscribes to `/scan`, forwards each scan to the **Taj
service** — a thin HTTP sidecar over the real `kirra-taj` crate (the `taj_service` binary,
`:8101`) — and publishes the **assured-clear-distance speed cap** (the speed from which the
robot can still stop within the clear distance ahead, RSS Rule 4 / the ADR-0014 "lidar
safety buffer"). The launch starts the Rust sidecars (Occy planner `:8100` + Taj `:8101`)
itself via `ExecuteProcess` — no separate terminal — so a single `ros2 launch` brings up the
whole governed stack on the Orin (single-box). They respawn on a transient crash; the
interceptor fails closed meanwhile.

The `cmd_vel_interceptor` applies that cap to the proposed forward speed **before** the
KIRRA governor: Taj *tightens* the envelope, the governor still *bounds* the result. It is
opt-in (`use_perception_cap`, default off → byte-identical prior path) and **fail-closed** —
a stale/absent cap or an unhealthy corridor holds the robot (the clamp is pure and unit
tested in `test/test_perception_cap.py`):

```
/scan → perception_governor → taj_service (kirra-taj) → /kirra/perception_speed_cap
                                                              │
/cmd_vel_raw → cmd_vel_interceptor [apply cap, then KIRRA /actuator/motion/command] → /cmd_vel
```

Enable it (the launch builds nothing — build the sidecars once, then launch starts them):

```bash
# build the Rust sidecars once (or run scripts/orin_bringup.sh)
cargo build --release -p kirra-mick --example planner_service --example taj_service

# one launch brings up the sidecars + the ROS safety nodes
ros2 launch kirra_safety kirra_with_robot.launch.py \
    kirra_token:=$KIRRA_ADMIN_TOKEN use_perception_cap:=true
```

Launch arguments that control the folded-in sidecars:

| arg | default | meaning |
|---|---|---|
| `start_sidecars` | `true` | start the Rust sidecars from this launch. Set **`false`** if they're already running (e.g. `scripts/orin_bringup.sh --serve`) to avoid double-binding the ports. |
| `start_planner_service` | `true` | start the Occy planner sidecar (`:8100`). |
| `use_perception_cap` | `false` | enable the Taj cap; also gates the Taj sidecar + `perception_governor`. |
| `sidecar_dir` | `~/kirra-runtime-sdk/target/release/examples` | where the `planner_service`/`taj_service` binaries are (or `KIRRA_SIDECAR_DIR`). |
| `planner_addr` / `taj_addr` | `127.0.0.1:8100` / `:8101` | sidecar bind addresses (`taj_addr` must match `taj_url`). |

## Quick Start

```bash
cd ros2_ws
colcon build --packages-select kirra_safety
source install/setup.bash
ros2 launch kirra_safety kirra_with_robot.launch.py kirra_token:=$KIRRA_ADMIN_TOKEN
```

## Gazebo demo — watch Taj + KIRRA stop a robot

`kirra_gazebo_demo.launch.py` drives a simulated robot toward a wall while a naive doer
keeps commanding it forward; Taj's corridor cap + the governor brake it to a controlled
stop. One command brings up the verifier, sidecars, safety nodes, Gazebo, robot, and doer.
See [`docs/testing/GAZEBO_ON_ORIN.md`](../../../../docs/testing/GAZEBO_ON_ORIN.md).

```bash
export KIRRA_ADMIN_TOKEN=test-token KIRRA_SUPERVISOR_RESET_KEY=test-reset-key
ros2 launch kirra_safety kirra_gazebo_demo.launch.py
```

## Full Documentation

See [`docs/ros2_interlock.md`](../../../../docs/ros2_interlock.md) for:

- Full architecture diagram and node descriptions
- Installation steps
- Nav2 topic remapping guide
- Fleet node registration (setup_ros2_fleet.sh)
- Hiwonder ROSOrin quick-start
- Tuning parameters for different robots
- Troubleshooting (fail-closed behavior, posture recovery, SSE reconnection)
- The LiDAR cover demo scenario
