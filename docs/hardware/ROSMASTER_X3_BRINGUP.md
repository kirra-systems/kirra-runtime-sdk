# KIRRA on the Yahboom Rosmaster X3 — Governed Motor Bringup

**The ADR-0033 chokepoint, made physical.** The verifying consumer owns the
motor serial port and drives the wheels **only** for commands carrying a valid
ADR-0033 release token. This is the software fence (#891) made hardware: the
consumer *is* the motor bringup — we do **not** stand up the vendor `/cmd_vel`
driver and retrofit a fence onto it.

> 🔴 **The very first drive test runs with the robot ELEVATED — wheels off the
> ground.** See [§5](#5-first-run-acceptance-test--wheels-off-the-ground).

---

## 0. Confirmed hardware facts

| Thing | Value |
|---|---|
| Motor board | `/dev/myserial` → `ttyUSB1`, driven by `Rosmaster_Lib` |
| Motor API | `Rosmaster.set_car_motion(v_x, v_y, v_z)` |
| X3 hardware range | `v_x, v_y ∈ [-1.0, 1.0]` m/s, `v_z ∈ [-5, 5]` rad/s |
| Lidar | `/dev/rplidar` → `ttyUSB0`, `RPLIDAR_TYPE=4ROS`, `sllidar_ros2` |
| ROS | Humble, `ROBOT_TYPE=x3`, `ROS_DOMAIN_ID=28` |

---

## 1. Architecture — one verify core, in Rust, over FFI

The verify core is Rust; the motor driver is Python. Per **ADR-0033 decision
(c)** we bridge with a **C-ABI over the existing Rust core**, never a Python
re-implementation of the crypto/watermark/freshness/liveness (that would be two
sources of truth):

```
 signed frame (topic)                     Rust: libkirra_consumer_ffi
 payload(32)||token(96)   ──ctypes──▶  MotorConsumer<CaptureSerial>
        │                                 = RosReleaseGate (verify→decode→
        │                                   freshness→watermark) + SS-002
        │                                   liveness/decel + #892 alarm
        ▼                                        │ decides a twist (or refuses)
 kirra_motor_consumer.py  ◀──decision──────────┘
        │ set_car_motion(v_x, 0, v_z)   (only if the core said "write")
        ▼
   /dev/myserial  (Rosmaster_Lib)   ← this node is the SOLE writer
```

**No verification logic lives in Python.** `robot/kirra_ffi.py` marshals bytes in
and a decision out; every gate decision is made by the reused
`kirra-actuation-consumer::MotorConsumer` (`crates/kirra-actuation-consumer/src/lib.rs`),
whose gate is `RosReleaseGate` (`crates/kirra-release-token/src/ros_twist.rs`).

### The FFI surface (reused from #891, and how it was extended)

The **verify core is reused verbatim** — no crypto, no watermark, no freshness,
no liveness, no refusal taxonomy was reimplemented. What this PR **adds** is the
C-ABI marshalling layer that #891 named as future work (ros_twist.rs module
docs: *"the C-ABI surface for the Python node"*), because none existed yet:

- **New crate `crates/kirra-consumer-ffi`** (`cdylib` + `rlib`): instantiates
  `MotorConsumer<CaptureSerial>` and exposes six `extern "C"` functions —
  `kirra_consumer_new` (fail-closed: NULL on bad key / decel / deadline /
  envelope), `_on_frame`, `_on_tick`, `_health`, `_alarm_explanation`, `_free`.
  `unsafe` is isolated **here only**; the verify core keeps
  `#![forbid(unsafe_code)]`.
- **One added accessor** on the reused consumer: `MotorConsumer::serial_mut`
  (`kirra-actuation-consumer/src/lib.rs`) — lends the serial seam so the FFI can
  reset its one-slot capture between calls. It reaches the seam **only**, never
  the gate/watermark/liveness.
- **`CaptureSerial`** (in the FFI crate) is the `MotorSerial` seam: it records
  the twist the core decided to write and applies the **demo-envelope clamp**.
  The core owns the *decision*; Python performs the *actuation*.
- **Governor stand-in minter** `kirra_ros_release_mint`
  (`crates/kirra-release-token/src/bin/`) wraps the existing `issue_ros_release`
  for demos/tests — so even the *signing* side of the bench stays the Rust
  implementation, never Python.

---

## 2. Verify-before-drive (ADR-0033)

Every actuated command passes the five-step gate (`RosReleaseGate::release`):
token exists → Ed25519 over the exact bytes (ROS domain) → finite decode →
freshness window → strictly-advancing sequence. Refusal taxonomy (wire-stable
codes) and the failure that produced each:

| Refusal | Meaning |
|---|---|
| `NO_TOKEN` | unsigned command / rogue publisher → **no motor write** |
| `DIGEST_MISMATCH` | bytes substituted after signing |
| `SIGNATURE_INVALID` | wrong/rotated key or tamper |
| `UNDECODABLE` | non-finite twist (never actuate NaN/Inf) |
| `STALE` | outside freshness window (replay / clock skew) |
| `SEQUENCE_NOT_ADVANCED` | replay / reorder |

🔴 **Loud key-mismatch diagnostic (#892).** A *sustained* run of
`SIGNATURE_INVALID` (≥10 consecutive, ~½–1 s at 10–20 Hz) **latches** a distinct
alarm — visibly different from ordinary staleness, never a silent safe-stop. The
node logs `KEY_MISMATCH_ALARM_EXPLANATION` (the operator sentence naming the
likely cause — rotation done out of order — and the recovery). A valid release
clears it.

---

## 3. The demo velocity envelope (backstop, not the safety mechanism)

The consumer clamps the actuated twist to a demo envelope **far below** the X3
hardware max. **Chosen demo values (demo-scoped, NOT the hardware limit):**

| Knob | Demo value | X3 hardware max |
|---|---|---|
| `KIRRA_DEMO_VX_MAX` (linear) | **0.15 m/s** | 1.0 m/s |
| `KIRRA_DEMO_VZ_MAX` (angular) | **0.4 rad/s** | 5 rad/s |
| lateral `v_y` | **0** (skid-steer demo; no lateral) | 1.0 m/s |

These are **required config with no default** — `kirra_consumer_new` returns a
fail-closed NULL (and the node aborts) if they are unset or not finite > 0. The
clamp is **defense-in-depth**: Kirra's checker is the safety authority; the clamp
only guarantees a bug can't command 1.0 m/s. It lives in the Rust `CaptureSerial`
seam so it is tested and cannot be forgotten by the Python layer.

---

## 4. Fail-closed liveness + guaranteed stop (SS-002)

- **No valid release within ≈3 control periods** → the core drives an **active
  decel-to-zero ramp** via `set_car_motion`, then output silence. **Never
  hold-last** (the Cruise-drag failure SS-002 exists to prevent).
- **A refusal does NOT reset the liveness window** — a flood of invalid tokens
  starves into the safe stop exactly as silence does.
- **Consumer exit / SIGINT / SIGTERM / exception / panic** → `set_car_motion(0,
  0, 0)` in the shutdown path, guaranteed (signal handlers + a `finally` belt).
  The robot stops if the consumer dies.

---

## 5. First-run acceptance test — WHEELS OFF THE GROUND

> 🔴 **Run `robot/first_run_elevated.sh` with the robot ELEVATED, all wheels
> free to spin.** This is the first time governed commands drive real motors; a
> wiring/clamp/verify bug elevated is a wheel spinning in the air, not a robot
> lunging off the bench. **Only after all three phases pass elevated does the
> robot touch the floor.**

### Build + pin the key

```bash
# Build the verify-core .so and the governor stand-in minter.
cargo build -p kirra-consumer-ffi --release
cargo build -p kirra-release-token --bin kirra_ros_release_mint --release

# The dev signing seed for the bench (DEV ONLY). Pin its public key.
export KIRRA_DEV_SEED=2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a
export KIRRA_GOVERNOR_VK_HEX=$(target/release/kirra_ros_release_mint --seed $KIRRA_DEV_SEED pubkey)
```

### Start the consumer (it OWNS the motor board)

```bash
export KIRRA_FRESHNESS_WINDOW_MS=200
export KIRRA_CONTROL_PERIOD_MS=100
export KIRRA_MISSED_PERIODS=3
export KIRRA_STOP_DECEL_MPS2=0.5      # from the deployed class MRC profile
export KIRRA_DEMO_VX_MAX=0.15
export KIRRA_DEMO_VZ_MAX=0.4
export KIRRA_MOTOR_PORT=/dev/myserial
export ROS_DOMAIN_ID=28
python3 robot/kirra_motor_consumer.py
```

🔴 **Do NOT launch `yahboomcar_bringup` (the vendor base node).** It would be a
second, unfenced writer to `/dev/myserial`. This consumer holding the port is the
structural guarantee; the script prints the active `ros2 node list` so you can
confirm it is absent.

### Run the elevated acceptance

In a second terminal:

```bash
robot/first_run_elevated.sh
```

It guides three phases and asks you to confirm each (you are the acceptance
sensor — it cannot see the wheels):

- **(a)** valid governed command → wheels spin at the **clamped** demo speed;
- **(b)** unsigned command → **zero** wheel motion + `REFUSED (NO_TOKEN)` in the
  consumer log;
- **(c)** kill the consumer (Ctrl-C) → wheels **stop immediately**.

### Host pre-check (no robot required)

Before touching hardware, prove the Python↔Rust boundary on any host:

```bash
cargo build -p kirra-consumer-ffi
cargo build -p kirra-release-token --bin kirra_ros_release_mint
python3 robot/ffi_smoke_test.py
```

This mirrors the elevated (a)/(b)/(c) — plus replay, wrong-key, and the decel
ramp — through the same FFI, minus the wheels. It runs in CI.

---

## 6. Lidar — separately (real perception for the governed loop)

Kept a **separate launch step** from the motor consumer. This is the input
Taj→Occy→Kirra needs so the governed loop is real, not synthetic.

```bash
export RPLIDAR_TYPE=4ROS
ros2 launch sllidar_ros2 sllidar_launch.py \
    serial_port:=/dev/rplidar
# Confirm real scans:
ros2 topic hz /scan
ros2 topic echo /scan --once
```

---

## 7. Explicitly NOT in this bringup

- **Speech / mic / TTS** — no audio hardware yet; voice is the last layer, on top
  of a robot that already drives and refuses.
- **sros2** — Tier-2.
- **Autonomous navigation / SLAM / nav2** — the demo is *governed command →
  checker → wheels*, and a refusal. Not nav.
- **Any second writer to `/dev/myserial`** — the vendor base node stays off.

---

## 8. Report summary (per the deliverable)

- **FFI surface:** reused the #891 verify core verbatim (`RosReleaseGate` /
  `MotorConsumer`); **extended** it with the previously-absent C-ABI marshalling
  layer (`kirra-consumer-ffi`) that ADR-0033 decision (c) called for, plus one
  seam accessor (`serial_mut`) and the governor stand-in minter. No crypto /
  watermark / freshness / liveness reimplemented anywhere.
- **Demo envelope:** `vx_max = 0.15 m/s`, `vz_max = 0.4 rad/s`, `v_y = 0` —
  demo-scoped backstop, required config with no default, far below the X3
  hardware max (1.0 m/s / 5 rad/s).
- **No vendor motor node co-runs:** the consumer is the sole opener/writer of
  `/dev/myserial`; the bringup and the elevated script both assert the vendor
  base node is absent.
- **Shutdown-stop guarantee:** `set_car_motion(0,0,0)` on SIGINT/SIGTERM/
  exception/normal exit (signal handlers + `finally`), plus the SS-002 liveness
  decel-to-zero when releases stop arriving.
