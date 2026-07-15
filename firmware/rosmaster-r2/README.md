# Kirra R2 MCU Platform

Clean-room firmware foundation for the Yahboom ROSMASTER R2 Ackermann robot.
Yahboom material is used only to identify hardware interfaces. No vendor
firmware source or architecture is incorporated.

## Status

This milestone provides:

- an evidence-ranked R2/shared-board hardware manifest;
- replaceable, allocation-free C++17 HAL interfaces;
- Ackermann forward/inverse kinematics;
- jerk-limited velocity control and anti-windup PID;
- a fail-closed safety state machine;
- COBS + CRC32C versioned framing and replay/loss detection;
- dual-slot, CRC-protected, versioned calibration storage with rollback;
- fixed-memory timing diagnostics;
- host tests and a deterministic plant integration fixture;
- architecture, protocol, safety, calibration, manufacturing and production
  roadmap documentation.

It does **not** claim a flashable board image. The R2 motor-channel harness,
steering connector, board revision, IMU variant, battery divider, current and
temperature sensing, E-stop electrical path and clock must be inspected on the
target PCB before an STM32 board-support package can be safely completed.

## Repository layout

```text
bootloader/   authenticated A/B update contract and memory plan
hal/          platform-independent hardware contracts and evidence manifest
drivers/      STM32F103 driver requirements and bring-up gates
protocol/     deterministic SBC–MCU wire protocol
kinematics/   R2 Ackermann model
control/      limiters, feedforward and closed-loop wheel control
diagnostics/  fixed-memory health and timing metrics
safety/       local safety authority and fault state machine
firmware/     configuration and application composition
tests/        host unit/integration tests and HAL mocks
simulation/   deterministic controller/plant fixture
tools/        build, analysis and documentation entry points
docs/         design and production evidence
```

## Host verification

```bash
cmake -S firmware/rosmaster-r2 -B /tmp/r2-build \
  -DR2_ENABLE_SANITIZERS=ON
cmake --build /tmp/r2-build --parallel
ctest --test-dir /tmp/r2-build --output-on-failure
/tmp/r2-build/r2_deterministic_sim
```

The host benchmark is functional evidence only. Timing claims require the
STM32F103RCT6 target, production compiler flags and electrical measurements.

## Design constraints

- No dynamic allocation after startup; core types allocate nothing.
- No exceptions or RTTI.
- The 1 kHz loop never parses frames, writes flash, logs text or waits on I/O.
- Timer encoder mode counts every edge in hardware; a 5–10 kHz snapshot service
  extends and timestamps counters without per-edge interrupts.
- Absolute speed/steering boundaries are applied before rate limits.
- Missing calibration, stale commands, bad frames and inconsistent sensors
  cannot arm motion.
- The physical E-stop must remove power-stage authority independently of the MCU.

Start with `docs/HARDWARE_REFERENCE.md`, then `docs/ARCHITECTURE.md`.
