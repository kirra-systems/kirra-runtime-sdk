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
service** — a thin HTTP sidecar over the real `kirra-taj` crate
(`cargo run -p kirra-mick --example taj_service`, listens on `:8101`) — and publishes the
**assured-clear-distance speed cap** (the speed from which the robot can still stop within
the clear distance ahead, RSS Rule 4 / the ADR-0014 "lidar safety buffer").

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

Enable it:

```bash
cargo run -p kirra-mick --example taj_service        # start the Taj sidecar (:8101)
ros2 launch kirra_safety kirra_with_robot.launch.py \
    kirra_token:=$KIRRA_ADMIN_TOKEN use_perception_cap:=true
```

## Quick Start

```bash
cd ros2_ws
colcon build --packages-select kirra_safety
source install/setup.bash
ros2 launch kirra_safety kirra_with_robot.launch.py kirra_token:=$KIRRA_ADMIN_TOKEN
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
