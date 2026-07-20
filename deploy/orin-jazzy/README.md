# Orin Jazzy runtime — containerize the Jazzy stack on a JetPack-6 host (ADR-0036, Step 3)

The last thing pinning the robot to 22.04/Humble isn't the code — it's **L4T**.
The Jetson Orin NX runs NVIDIA's L4T, and **JetPack 6 = Ubuntu 22.04 / ROS 2
Humble**. You can't `apt` your way to 24.04; you're gated on what NVIDIA ships.
This directory runs the **Jazzy stack in a container on the unchanged JetPack-6
host**, so you move to Jazzy today instead of waiting for a JetPack-7 (24.04)
native flash.

```
 ┌──────────────────────────── Orin NX host (JetPack 6 / 22.04 / Humble) ────────────────────────────┐
 │  kernel + devices (/dev/myserial, /dev/ydlidar) + the DDS network (host mode)                      │
 │                                                                                                    │
 │   ┌─────────────────────────────┐   DDS over host net    ┌──────────────────────────────────────┐ │
 │   │ (optional) Humble bits /    │  (shared DOMAIN_ID)    │  container: ros:jazzy                  │ │
 │   │  Autoware doer, if present  │◄─────────────────────► │  kirra_governor (checker/adapter)      │ │
 │   └─────────────────────────────┘                        │  + governed consumer (ADR-0033)        │ │
 │                                                           └──────────────────────────────────────┘ │
 └────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

This is the **inverse** of `deploy/autoware-isolation/`: there, Autoware is the
odd-one-out Humble *guest* alongside a native Jazzy host; here (on the robot),
Jazzy is the *guest* on a Humble host. Same cross-distro DDS wire; the checker is
distro-independent and bounds whatever crosses the boundary.

## Scope — the CPU-only safety spine (honest boundary)
The image builds and runs, on a plain `ros:jazzy` arm64 base (no CUDA needed):
- **the ros2 adapter** (`kirra_ros2_adapter_node`, the checker),
- **the ADR-0033 governed motor consumer** + its verify-core `libkirra_consumer_ffi.so`,
- **the 4 curated `autoware_*_msgs`** + `kirra_safety`,
- **the robot Channel-A layer** (`rabbit_*`) + the distro resolver `ros_env.sh`.

**Not here (follow-up):** GPU doers — parko TensorRT/ONNX inference and camera
perception. Those need an `nvcr.io/nvidia/l4t-jetpack`-based Jazzy image with the
CUDA/cuDNN/TensorRT runtime and `--runtime nvidia`. The safety path needs none of
that, so it's deliberately split out; the checker bounds a GPU doer the same way
whether the doer is native, in another container, or on Humble.

## Prerequisites on the host
- Docker with the default runtime (Buildx/BuildKit). For the GPU follow-up:
  `nvidia-container-toolkit` + `--runtime nvidia`.
- The udev symlinks the robot already relies on: `/dev/myserial` (motor board),
  `/dev/ydlidar` (TG30 lidar). Confirm with `robot/install/capture_from_robot.sh`.
- The vendor **`Rosmaster_Lib`** Python package on the host — it is NOT
  redistributable in the image, so the consumer mounts it read-only (see below).

## Bring-up
```bash
cd ~/kirra-runtime-sdk

# 1. env: copy the robot env template and fill it (governor key, R2 calib, ports).
cp robot/install/env.template deploy/orin-jazzy/robot.env
$EDITOR deploy/orin-jazzy/robot.env        # KIRRA_GOVERNOR_VK_HEX, KIRRA_R2_*, ROS_DOMAIN_ID, ...

# 2. build the image (first build compiles r2r + the adapter + the ws — slow once).
docker compose -f deploy/orin-jazzy/docker-compose.yml build

# 3. the checker/adapter (CPU-only, no devices) — validate the wire first:
docker compose -f deploy/orin-jazzy/docker-compose.yml up kirra-adapter

# 4. the governed wheels (🔴 moves the robot — elevate/tether first):
#    point ROSMASTER_LIB_DIR at the host's Rosmaster_Lib, then:
ROSMASTER_LIB_DIR=/usr/local/lib/python3.10/dist-packages/Rosmaster_Lib \
  docker compose -f deploy/orin-jazzy/docker-compose.yml --profile drive up kirra-consumer
```

`kirra-adapter` is safe to run anytime (it only reads/publishes topics and bounds
them). `kirra-consumer` is behind `--profile drive` because it owns the motor
board and moves the robot — the ADR-0033 verify-token chokepoint still gates every
command inside the container, exactly as on bare metal.

## Why host networking + shared DOMAIN_ID
DDS discovery is multicast/shared-memory based; `network_mode: host` puts the
container's ROS graph on the host's network so it discovers (and is discovered by)
any Humble nodes on the box and the isolated Autoware doer — no bridge, provided
the curated boundary interfaces are byte-identical across distros (run the
`scripts/curated_interface/crossdistro_hash_check.sh` gate; ADR-0036 Step 1).

## Device + vendor notes (the parts that are hardware-specific)
- **Motor / lidar**: passed via `devices:` as the udev symlinks. If your host
  exposes the raw `ttyUSB*` instead, set `KIRRA_MOTOR_DEV` / `KIRRA_LIDAR_DEV`.
- **`Rosmaster_Lib`**: mounted read-only from the host into `/opt/vendor` and put
  on `PYTHONPATH`. Adjust `ROSMASTER_LIB_DIR` to the host path (it varies by
  Python minor version). If your consumer runs in `KIRRA_DRIVE_MODE=r2_ackermann`,
  the Path-B last-hop still calls the vendor lib for `set_motor`/steering.
- **Real-time**: for the FIFO-scheduled loops add `cap_add: [SYS_NICE]` (and, if
  you pin CPUs, `cpuset`). Not enabled by default — validate wheels-up first.

## What this is / isn't
- **Is:** the topology + a buildable image + a compose that runs the Jazzy safety
  spine on a Humble Orin host, and the honest device/vendor/GPU boundaries.
- **Isn't:** validated on-Orin here (build it on the Orin or an arm64 builder),
  and it does **not** include the GPU doers — those are the l4t-jetpack follow-up.
  The checker/fence are unchanged; only the runtime environment moved.

## Retirement
When JetPack 7 (24.04/Noble, Jazzy-native) ships, flash the Orin and run the
stack on the host directly — this container was only ever the bridge across the
L4T gap. The Autoware-Humble isolation (`deploy/autoware-isolation/`) retires the
same way when Autoware ships stable Jazzy support.
