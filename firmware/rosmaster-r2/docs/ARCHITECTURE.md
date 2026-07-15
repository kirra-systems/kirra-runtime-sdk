# R2 firmware architecture

> **Design status:** portable core implemented and host-tested. RTOS, STM32 BSP,
> crypto, Linux bridge and every target timing/memory figure remain requirements
> pending physical-board and HIL evidence.

## Decision summary

- **MCU:** retain STM32F103RCT6 for hardware compatibility; plan an STM32G4/H7
  control-board revision for CAN-FD, hardware crypto and stronger diagnostics.
- **RTOS candidate:** FreeRTOS with static allocation, tickless idle outside
  active control, and direct-to-task notifications; confirm footprint/latency on
  target. Zephyr is the preferred future-board candidate.
- **Hot link:** fixed-layout R2CP frames, COBS, CRC32C, sequence/timestamps and
  bounded ACKs over DMA UART at a validated high baud rate.
- **ROS integration:** a Linux lifecycle bridge is the sole device owner. The MCU
  is not a DDS participant.
- **ROS middleware candidates:** CycloneDDS and Fast DDS require an on-Orin
  workload/security comparison. Zenoh is optional at the cross-host/fleet boundary.
- **Local Linux IPC:** iceoryx2 only across processes carrying high-rate fixed
  samples; direct calls remain faster and simpler inside one process.

## Layering and dependency rule

```text
Application / composition
        ↓
Communication  ←→  Diagnostics
        ↓
Safety manager (sole local motion authority)
        ↓
Motion controller / odometry
        ↓
Hardware services (time, command, calibration, health)
        ↓
Device managers (IMU, motors, encoders, battery, update)
        ↓
Drivers
        ↓
RTOS abstraction
        ↓
HAL / board-support package
```

Dependencies point downward; every boundary uses a narrow interface. Drivers do
not know ROS messages, the controller does not know UART, and protocol parsing
cannot call motor registers. The portable core is C++17 with no heap, exceptions
or RTTI. The STM32 BSP is C/C++ and is the only layer permitted to include STM32
headers or FreeRTOS APIs.

## Responsibility split

### MCU

Owns PWM, steering, quadrature capture, wheel-speed control, local kinematic
limits, 1 kHz command shaping, odometry integration, sensor timestamps, IMU and
battery sampling, physical E-stop observation, command timeout, safety/fault
state, watchdogs, diagnostics, calibration flash and authenticated update.

### Linux SBC

Owns perception, SLAM, localization, mapping, planning, behavior/mission logic,
logging, fleet DDS/Zenoh, Autoware adapters and rich UI. It proposes velocity and
curvature; it cannot bypass MCU limits or stale-command shutdown.

Kirra's verifier/governor still bounds autonomy commands on Linux. Production
authority requires the MCU to verify a Kirra authorization over the exact
enforced command, or the ADR-0033 interim topology in which a release-token
verifying, privileged Linux consumer is the sole serial owner and enforces its
ACL/startup sentinel. R2CP HMAC authenticates the link; it is not by itself a
Kirra verdict. The MCU adds an independent physical envelope and liveness
boundary; it does not copy the high-level policy.

## RTOS decision

| Criterion | FreeRTOS | Zephyr | NuttX | ThreadX |
|---|---|---|---|---|
| F103 48 KiB fit | Excellent | Feasible, tighter | Poorer | Excellent |
| Determinism | Excellent with static design | Excellent | Good | Excellent |
| STM32 drivers | Vendor + custom BSP | Strong upstream | Strong | Vendor |
| Tooling/tests | Moderate, add host seams | Strongest | Good | Good |
| Community | Very large | Large/growing | Medium | Large vendor base |
| POSIX | No | Partial | Strong | No |
| Integration risk | Lowest | Medium | High on this MCU | Medium |

**FreeRTOS is the provisional baseline** because expected RAM margin, mature
F103 ports and control of every allocation may outweigh Zephyr's superior
DeviceTree and test ecosystem.
Static tasks, queues and timers are mandatory. NuttX's POSIX surface is overhead
without value on a 48 KiB controller. ThreadX is viable only when a commercial,
version-matched safety/support package is a product requirement.

Zephyr should be re-evaluated for a future ≥256 KiB SRAM board, especially when
CAN-FD, MCUboot, tracing and upstream sensor drivers become decisive.

## Task and interrupt model

| Context | Rate / trigger | Priority | Budget | Responsibility |
|---|---:|---:|---:|---|
| Hardware E-stop / timer break | asynchronous | hardware | <10 ms electrical | disable bridge independent of scheduler |
| Encoder timers | every edge in hardware | hardware | no ISR/edge | quadrature count |
| Encoder snapshot ISR | 10 kHz | highest ISR | 5 µs | extend counters, timestamp |
| Control task | 1 kHz | highest task | 250 µs | snapshot, safety, kinematics, PID, latch PWM |
| IMU DMA completion | 400–1000 Hz | high ISR | 10 µs | publish buffer index |
| IMU service | 400–1000 Hz | high task | 150 µs | validate, calibrate, timestamp |
| Link RX DMA/idle ISR | frame/idle | medium ISR | 10 µs | delimit DMA span only |
| Link task | event-driven | medium task | 300 µs | decode, validate, latest-command mailbox |
| Odometry task | 200 Hz | medium | 200 µs | integrate pose/covariance |
| Battery/thermal task | 100 Hz | low | 100 µs | filter and limit checks |
| Diagnostics task | 10 Hz | low | 500 µs | health snapshot/telemetry |
| Configuration/update | command, standby only | lowest | bounded chunks | transactional flash |

The control task consumes a single-producer/single-consumer latest-value mailbox.
No command queue is allowed to accumulate stale actuation. IMU and encoder
buffers are double-buffered; ownership changes by index, not payload copy.

## Timing chain

```text
SBC timestamp
  → UART DMA (bounded frame)
  → idle ISR publishes span
  → parser validates COBS/length/CRC/version/sequence/freshness
  → latest-command mailbox
  → next 1 kHz release point
  → absolute envelope
  → jerk/accel/steering-rate shaping
  → wheel PID + feedforward
  → timer preload
  → synchronous PWM update event
```

The target `<2 ms` is measured from a GPIO marker immediately before the Linux
bridge submits the first frame byte to a GPIO marker at PWM latch, p99.9 under
SBC load. It includes complete serialization, USB/UART buffering, parsing and
the next control release; it excludes upstream planning. A roughly 70-byte v1
motion frame alone takes about 6.1 ms at 115200 baud, so that carrier cannot pass.

## Memory budget

Unverified F103 requirement budget; linker map and stack painting must replace
these allocations:

| Region | Budget |
|---|---:|
| Control/kinematics/safety static data | 5 KiB |
| Protocol RX/TX DMA + decoded frames | 3 KiB |
| IMU/encoder/odometry buffers | 3 KiB |
| Diagnostic/event buffers | 4 KiB |
| FreeRTOS kernel objects | 3 KiB |
| All task/ISR stacks | 16 KiB |
| Reserved/headroom | **14 KiB (29%)** |
| Total SRAM | 48 KiB |

Flash proposal is in `SAFETY_AND_PRODUCTION.md`. Link failure occurs if RAM
headroom drops below 25%; production validation also checks measured stack
high-water with fault paths active.

## Motion and odometry

The inverse bicycle model computes curvature and a front virtual road-wheel
angle. Rear wheel targets include track differential. Steering pulse conversion
uses a measured monotonic calibration table; no vendor “angle units” survive.

Pipeline:

1. validate finite command and freshness;
2. clamp the absolute velocity/curvature/steering envelope;
3. apply acceleration, deceleration, jerk and steering-rate limits;
4. calculate left/right rear wheel speed;
5. filter encoder speed with a bounded IIR/outlier gate;
6. apply feedforward + PID with back-calculation anti-windup;
7. evaluate wheel disagreement/slip against yaw gyro;
8. latch both PWM channels using a verified TIM1/TIM8 master/slave update route,
   or measure/document bounded sequential skew if the route is unavailable;
9. integrate midpoint odometry and covariance at 200 Hz.

Covariance starts from calibrated encoder quantization, wheel-radius uncertainty,
track/wheelbase uncertainty and gyro noise. It inflates with slip score, encoder
disagreement, IMU invalidity and elapsed time; the MCU never reports a fixed
optimistic covariance.

## DDS, micro-ROS and Zenoh

| Option | Decision |
|---|---|
| Full DDS on MCU | Rejected: inappropriate for 48 KiB SRAM and the hard loop |
| DDS-XRCE / micro-ROS | Conditional telemetry experiment only; Agent/session state must not gate actuation |
| Custom R2CP | Selected for safety command/status: bounded memory and wire time |
| CycloneDDS | ROS 2 RMW candidate; compare on the actual Orin graph |
| Fast DDS | Compatibility candidate; likely easiest on existing Humble image |
| Zenoh bridge | Use for Wi-Fi/multi-robot/fleet only when benchmarks show a benefit |

The Linux bridge maps R2CP to `ros2_control`:

- command: `hardware_interface::LoanedCommandInterface` for velocity/steering;
- state: wheel position/velocity, steering, IMU and battery;
- lifecycle activation performs capability/version/time-sync/calibration checks;
- deactivation sends stop, waits for ACK, then observes standby;
- `ros2_control` update rate is 100–250 Hz; the MCU interpolates at 1 kHz.

Autoware `AckermannControlCommand` is translated on Linux after the Kirra
governor. An optional Twist adapter computes curvature but rejects zero-speed yaw.

## iceoryx2

iceoryx2 does not run on this F103 and does not replace the UART. It can help on
Linux when perception, governor, control bridge and recorder must be separate
processes and exchange large fixed-layout samples. Use loaned shared-memory
chunks, bounded subscribers and a newest-sample policy.

Retain it only if target measurements show lower CPU and p99.9 latency than ROS
intra-process or direct calls under the real load. It cannot provide a stock
Linux worst-case guarantee; prior Kirra evidence saw microsecond normal latency
but tens-of-milliseconds maxima. One process with direct calls is preferable
when separation is not required.

Timing histograms are single-writer objects. ISR/control owners copy them into
immutable diagnostic snapshots under a bounded critical section or versioned
SPSC handoff; diagnostics never reads live 64-bit counters concurrently on the
Cortex-M3.

## Extension strategy

- **CAN-FD:** add a `Transport` implementation and preserve R2CP payloads;
  arbitration and bus-off become safety inputs.
- **EtherCAT/TSN:** Linux/gateway concerns for a platform revision, not an F103
  feature. Use distributed clocks/PTP only with hardware timestamping.
- **PTP:** SBC disciplines an MCU monotonic-clock affine mapping; control uses
  only monotonic local time and never steps it.
- **Tracing:** SEGGER SystemView/Percepio hooks behind a compile-time interface;
  disabled production hot-path cost must be zero.
- **Acceleration:** FPGA/DSP outputs are untrusted proposals checked by the same
  local limits.
