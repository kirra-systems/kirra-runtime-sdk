# KIRRA robot install — Layer A (base-agnostic, validated today)

A documented, reproducible install that turns a **fresh Yahboom-flashed Orin
NX** into a working KIRRA robot, capturing the state validated on hardware
during the 2026-07 bringup (first governed motion + TG30 lidar). This is the
seed of the golden-image product and the backup that makes a reflash
non-destructive (see `REFLASH.md`).

> **Companion installer — the Rabbit / voice operator layer.** This README covers
> the base-image install (the fenced verifying consumer + FFI + lidar). The
> conversational **Rabbit** layer (voice, persona, proactive narration, OTA voice
> check) is staged separately by `install_robot_units.sh`, which installs the
> `rabbit_*` scripts + the `kirra-ros-stack` / `kirra-rabbit-watch` /
> `kirra-rabbit-greet` / `kirra-ota-check` units (STAGED, not enabled). See
> `docs/hardware/RABBIT_BRINGUP_RUNBOOK.md` (bring-up), `RABBIT_AUDIO_STACK.md`
> (STT/TTS + the systemd-audio caveat), and `docs/rabbit/RABBIT_VOICE_LINES.md`
> (every spoken line). Rabbit is Channel-A speech + the one fenced `/intent` door —
> it carries no actuation authority.

**Layer split (the honesty constraint):**

- **Layer A (this directory, scripted)** — everything base-agnostic and
  validated: the fenced verifying consumer, the FFI verify core, the mint
  binary, the lidar config, the env, systemd staging. Produces a robot in the
  **current validated mode**: car-type 1 (mecanum register), straight-line
  capable, wheels governed by the ADR-0033 chokepoint.
- **Layer B (`PLATFORM_R2_PENDING.md`, STUBBED)** — the steering/R2 platform
  config. 🔴 **NOT implemented.** The robot does NOT currently have working
  steering + drive together (`set_car_type(5)` gives steering but breaks
  drive); the fix requires Yahboom's Ultimate-Orin NX R2 image, not yet
  obtained. Nothing in Layer A touches steering or car-type 5.

---

## What gets installed (inventory, with sources)

| Piece | Source | Installed to |
|---|---|---|
| Verifying motor consumer | `robot/kirra_motor_consumer.py` (env contract :24-37; car-type guard :128-155) | `/opt/kirra/robot/` |
| FFI binding (ctypes, no crypto in Python) | `robot/kirra_ffi.py` (lib discovery :90-105) | `/opt/kirra/robot/` |
| Verify core cdylib | `crates/kirra-consumer-ffi` → `cargo build --locked --release -p kirra-consumer-ffi` | `/opt/kirra/lib/libkirra_consumer_ffi.so` |
| Governor stand-in minter (DEV) | `crates/kirra-release-token/src/bin/kirra_ros_release_mint.rs` | `/opt/kirra/bin/` |
| Bench/acceptance scripts | `robot/first_run_elevated.sh`, `robot/live_loop_elevated.sh`, `robot/steering_bench_elevated.sh`, `robot/kirra_release_publisher.py` | `/opt/kirra/robot/` |
| Env (single source of truth) | `robot/install/env.template` | `/etc/kirra/robot.env` |
| Consumer systemd unit (staged, NOT enabled) | `robot/install/systemd/kirra-consumer.service` | `/etc/systemd/system/` |

**System deps** (expected on the vendor image; the installer fail-louds when
absent): ROS 2 Humble, the vendor `Rosmaster_Lib` Python library, the
`ydlidar_ros2_driver`, `python3`. **Build dep**: rustup/cargo — the repo's
`rust-toolchain.toml` pins the toolchain (1.94.1) automatically; the aarch64
build runs natively on the Orin, or build elsewhere and use `--skip-build`.

**Validated configs captured here** (do not re-derive — these were hard-won):

- **Lidar**: `ydlidar_ros2_driver`, port `/dev/ydlidar`, **baud 512000**, TG30,
  ~10 Hz, BEST_EFFORT `/scan` (`installer/platform_map.toml:31-37`).
  ⚠ NOT `sllidar_ros2` at 115200 — that was the superseded doc bug
  (`docs/hardware/ROSMASTER_X3_BRINGUP.md` §6 carries the correction note).
- **Env block** (`env.template`, from `docs/hardware/ROSMASTER_X3_BRINGUP.md:145-165`):
  freshness 200 ms, control period 100 ms, missed periods 3, stop decel
  0.5 m/s², demo envelope **0.15 m/s / 0.4 rad/s**, motor port `/dev/myserial`,
  `ROS_DOMAIN_ID=28`, `KIRRA_EXPECTED_CAR_TYPE=1` (the current validated mode).
- **Device symlinks** (vendor udev): `/dev/myserial → ttyUSB1`,
  `/dev/ydlidar → ttyUSB0` on the validated unit — confirm per-robot with
  `capture_from_robot.sh`.

---

## Procedure

### 0. Prereqs (manual, cannot be scripted — flagged honestly)

1. **Flash the vendor Yahboom base image** (the current unit runs the
   cross-labeled X3 image; Layer A is agnostic to which Yahboom base).
2. Vendor pieces present on that image: ROS 2 Humble, `Rosmaster_Lib`,
   `ydlidar_ros2_driver`, the udev rules for `/dev/myserial` / `/dev/ydlidar`.
   If a fresh image is missing any of these, restore from the captured copies
   (`capture_from_robot.sh` output; see `REFLASH.md`) — the installer checks
   and refuses loudly rather than guessing.
3. Install rustup (or plan to build off-device + `--skip-build`).
4. Clone this repo on the robot (`~/kirra-runtime-sdk` is the assumed root).

### 1. Run the installer

```bash
# Bench (dev key — 🔴 DEV/DEMO ONLY, never a production unit):
robot/install/install_kirra.sh --dev-key

# Production posture (pin the real governor key afterwards):
robot/install/install_kirra.sh
```

Idempotent: re-running never overwrites an existing `/etc/kirra/robot.env`.

### 2. Verifier-side note (separate machine or same box)

The verifier stack install is `deploy/systemd/install.sh` (verifier + Occy +
Taj + Mick as systemd units). **Known gap** (from the installer gap analysis):
it generates the secrets (`deploy/systemd/install.sh:61-79`) but NOT
`KIRRA_VEHICLE_CLASS`, which is required with no default — the verifier
**fails closed at startup** until you append it to `/etc/kirra/kirra.env`:

```bash
echo 'KIRRA_VEHICLE_CLASS=courier' | sudo tee -a /etc/kirra/kirra.env  # interim reviewed class, installer/platform_map.toml:29
```

Minting (needed for the live loop) additionally requires
`KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` (+ `KIRRA_GOVERNOR_SIGNING_KEY_ALLOW_DEV=1`
for the dev key) in the same file.

### 3. Verification (how you know it worked)

```bash
# (a) The consumer starts and OWNS the board — the validated terminal mode:
set -a; source /etc/kirra/robot.env; set +a
source /opt/ros/humble/setup.bash
python3 /opt/kirra/robot/kirra_motor_consumer.py
```

Pass criteria:
- prints `KIRRA consumer OWNS /dev/myserial (sole writer) ...` — no env FATAL,
  no car-type FATAL (register must read 1 on the current image);
- 🔴 the vendor base node (`yahboomcar_bringup`) is NOT running — the consumer
  is the sole `/dev/myserial` writer.

```bash
# (b) The lidar publishes (second terminal):
ros2 topic hz /scan            # steady ~10 Hz (TG30)
ros2 topic echo /scan --once   # finite, room-plausible ranges (not all 0/inf)
```

```bash
# (c) First governed motion — 🔴 WHEELS ELEVATED:
/opt/kirra/robot/first_run_elevated.sh
```

### 4. Config capture (per-robot values)

Run **on the robot, in a known-working state**, and commit the output:

```bash
robot/install/capture_from_robot.sh
```

Captures the working ydlidar yaml, the udev rules, the device-symlink truth,
and version stamps into `robot/install/captured/<hostname>/`. These are the
values that must match the specific robot and are NOT derivable from this
repo — capturing them is what makes `REFLASH.md` real.

---

## What Layer A does NOT cover (explicit)

- **Steering / car-type 5 / the R2 platform** — Layer B, blocked on the vendor
  R2 image: `PLATFORM_R2_PENDING.md`.
- **The consumer as an enabled service** — the unit is staged but not enabled;
  the validated mode is a terminal run. Enable only after an elevated re-test.
- **Production key ceremony** — `--dev-key` is bench-only; production units
  enroll a real governor key (`docs/safety/GOVERNOR_KEY_PROVISIONING.md`).
- **The ros2_ws safety nodes / live-loop launch** — covered by
  `ros2_ws/src/kirra_safety` + `docs/hardware/ROSMASTER_X3_BRINGUP.md` §6b;
  this directory installs the robot-side chokepoint, not the full loop.
- **A `.img` snapshot** — generated later, from the validated R2 base.
