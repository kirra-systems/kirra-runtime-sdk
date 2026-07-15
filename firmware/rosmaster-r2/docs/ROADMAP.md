# Production implementation roadmap

Benchmarks are gates, not estimates. Host timing is indicative; only target and
electrical measurements support real-time/safety claims.

## Phase 1 — hardware reverse engineering

**Deliverables**

- pinned clean-room source register and evidence-ranked interface map;
- PCB revision, MCU/clock/flash/RAM and connector photographs;
- continuity-derived motor/encoder/steering/IMU/ADC/E-stop net map;
- legacy protocol captures only where needed for migration tooling;
- hazard/unknown register and immutable unit bring-up report.

**Risks**

- shared-board lessons differ from the production R2 revision;
- X3 car-type/mecanum assumptions contaminate R2 design;
- no electrical current/thermal/steering feedback exists;
- SWD or high-speed UART is inaccessible.

**Validation tests**

- multimeter/logic-analyzer cross-check of every claimed net;
- bridge-disabled reset/boot scope capture;
- hand-turned encoder and IMU WHO_AM_I capture;
- BOOT0/RESET ROM recovery rehearsal.

**Benchmarks**

- 100% interface claims have direct evidence or are marked unknown;
- zero motor-enable pulses from reset through 500 ms;
- UART BER baseline at 115200 and candidate high rates.

**Exit criteria**

- signed hardware baseline closes every motion-critical unknown;
- independent reviewer reproduces motor/encoder/steering mapping;
- missing safety hardware becomes an explicit board-change requirement.

Current status: repository evidence baseline and portable manifest are complete;
physical unit closure remains required.

## Phase 2 — HAL and drivers

**Deliverables**

- board-revision BSP and compile-time option manifest;
- timer/DMA UART, quadrature, PWM, steering, IMU, ADC, watchdog, flash drivers;
- host HAL mocks and target loopback diagnostics;
- linker map and safe GPIO initialization.

**Risks**

- timer/pin conflicts, 16-bit encoder overflow, servo jitter;
- HAL/library calls contain blocking paths;
- CH340 cannot sustain low-jitter high baud.

**Validation tests**

- register-level unit tests where practical and HIL loopbacks;
- encoder pulse generator at twice maximum edge rate;
- UART split/flood/BER tests; PWM/servo oscilloscope tests;
- reset/brownout at every peripheral initialization step.

**Benchmarks**

- encoder snapshot ≥10 kHz with zero count loss;
- PWM latch 1 kHz with <2 µs channel skew;
- servo pulse jitter <5 µs;
- ≥25% SRAM headroom.

**Exit criteria**

- all HAL conformance tests pass on mocks and target;
- no blocking/dynamic allocation in ISR or control dependencies;
- motor outputs proven off for every reset/fault path.

## Phase 3 — RTOS integration

**Deliverables**

- statically allocated FreeRTOS tasks/queues/timers and priority ceiling;
- task-alive supervisor, IWDG/WWDG policy and stack painting;
- rate-monotonic schedule, trace hooks and overload policy;
- reproducible ARM toolchain/presets.

**Risks**

- priority inversion, hidden heap use, ISR priority errors;
- telemetry/flash starves control;
- watchdog is serviced despite a dead critical task.

**Validation tests**

- forced task stalls, queue floods and priority inversion;
- stack exhaustion guard and scheduler trace review;
- watchdog early/late service and reset-cause tests.

**Benchmarks**

- 1 kHz release jitter p99.999 <50 µs;
- control scheduling overhead <10% of 250 µs budget;
- zero missed deadlines in 24-hour overloaded soak.

**Exit criteria**

- measured response-time analysis matches trace;
- every stack has ≥30% measured margin;
- critical task failure resets or disables motion within its fault-tolerant time.

## Phase 4 — motion control

**Deliverables**

- calibrated Ackermann forward/inverse model and steering map;
- jerk/acceleration/velocity/curvature limits;
- independent left/right feedforward + PID anti-windup;
- encoder filter, slip score, odometry and covariance;
- deterministic simulation and HIL plant.

**Risks**

- linkage nonlinearity, backlash, wheel mismatch and low-speed quantization;
- reverse and near-zero curvature singularities;
- aggressive tuning excites chassis.

**Validation tests**

- property tests over finite command/calibration domains;
- step/ramp/reversal/saturation/zero-speed-yaw cases;
- HIL encoder/IMU fault injection;
- elevated then tethered ground-truth straight/arc trials.

**Benchmarks**

- controller WCET <150 µs inside 250 µs total budget;
- hard envelope never exceeded;
- wheel-speed settling/overshoot meet calibrated plant requirement;
- odometry error/covariance coverage meet declared operating domain.

**Exit criteria**

- all calibrated units pass direction, endpoint and stop tests;
- covariance is conservative against independent ground truth;
- no X3/mecanum behavior remains.

## Phase 5 — safety architecture

**Deliverables**

- state/fault managers, local safe stop and command arbitration;
- hardware E-stop/break, dual watchdog, brownout and battery policy;
- runaway, encoder, steering and IMU plausibility monitors;
- safety requirements, FMEA/FMEDA inputs and traceability.

**Risks**

- stock PCB lacks independent cutoff/current/thermal/steering sensing;
- common-cause clock/power failure;
- nuisance trips cause unsafe operator bypass.

**Validation tests**

- fault matrix for every state and transition;
- wire cut/stuck-at/sensor freeze/runaway/overcurrent/thermal injection;
- brownout/glitch/reset campaigns;
- physical E-stop test at voltage/temperature corners.

**Benchmarks**

- electrical E-stop to disabled power stage <10 ms worst case;
- communication loss enters decel by local deadline and never holds last;
- runaway/deadline severe faults disable within one 1 ms cycle.

**Exit criteria**

- every hazard has prevention/detection/reaction and verification evidence;
- independent review accepts residual risks;
- absent hardware safety mechanisms are added or claims reduced.

## Phase 6 — communications

**Deliverables**

- frozen R2CP v1 schemas and generated golden vectors;
- UART DMA transport, COBS/CRC, sequences, ACK/retry, capability negotiation;
- HMAC session authentication and time synchronization;
- fuzz harness, protocol analyzer and Linux library.

**Risks**

- high baud/USB buffering misses latency;
- authentication exceeds MCU budget;
- sequence/reconnect logic accepts replay;
- management reliability interferes with commands.

**Validation tests**

- decoder fuzzing and sanitizers;
- corruption/truncation/replay/wrap/flood/ACK-loss matrix;
- hostile Linux load and cable disconnect/reconnect;
- independent golden-vector implementation.

**Benchmarks**

- command arrival-to-PWM p99.9 <2 ms under full load;
- decoder bounded to one maximum frame;
- zero accepted stale, replayed, corrupt or unauthenticated commands;
- link CPU <10% and no control deadline impact.

**Exit criteria**

- latency met on selected carrier or requirement/hardware revised;
- protocol/security review complete;
- compatibility and downgrade behavior is explicit.

## Phase 7 — ROS 2 integration

**Deliverables**

- lifecycle `ros2_control` hardware plugin and sole device ownership;
- Ackermann command/state interfaces, IMU/battery/odometry/diagnostics mapping;
- Autoware command adapter after Kirra governance;
- SROS2 profile and optional iceoryx2/Zenoh adapters.

**Risks**

- duplicate publisher/device-owner bypass;
- ROS clocks are mixed with MCU monotonic deadlines;
- RMW or Linux jitter is mistaken for MCU determinism.

**Validation tests**

- launch tests, rogue publisher/device-open attempts and lifecycle restarts;
- ros2_control controller switching and stale command;
- clock skew/step and DDS loss/load campaigns;
- Autoware closed-loop simulation.

**Benchmarks**

- bridge p99.9 processing <250 µs excluding carrier;
- 100–250 Hz command/state with no allocations after activation;
- no MCU deadline misses under maximum ROS/perception load.

**Exit criteria**

- only the lifecycle bridge owns the carrier;
- Kirra-governed Autoware commands reach MCU and bypass attempts stop;
- target RMW selected from measured data.

## Phase 8 — diagnostics

**Deliverables**

- POST/BIST, sensor/motor/link health and fault dictionary;
- CPU/stack/timing/latency histograms and persistent event log;
- ROS diagnostics, trace decoder and service tooling;
- calibration/manufacturing evidence export.

**Risks**

- logging adds jitter or flash wear;
- self-test is vacuous or produces false health;
- diagnostic counters wrap or lose fault context.

**Validation tests**

- inject every diagnostic code and verify ROS/tool rendering;
- power loss during each log operation;
- flash endurance/rate-limit analysis and stack high-water fault paths.

**Benchmarks**

- diagnostics consume <5% CPU and zero dynamic memory;
- no measurable 1 kHz jitter regression;
- previous valid log prefix survives every power interruption.

**Exit criteria**

- field fault can be reconstructed from versioned data;
- self-tests detect their seeded faults;
- event retention/endurance meets service policy.

## Phase 9 — performance optimization

**Deliverables**

- target WCET/jitter, carrier latency, CPU/RAM/flash and energy baselines;
- hot-path trace and copy/allocation inventory;
- optimized fixed-point/table paths only where evidence requires;
- iceoryx2/DDS/Zenoh comparative report on Orin.

**Risks**

- average-case optimization weakens worst case/readability;
- host benchmarks are reported as MCU WCET;
- zero-copy ownership errors replace copy cost.

**Validation tests**

- regression benchmarks under worst sensor/link/ROS load;
- cycle counter plus oscilloscope GPIO markers;
- output equivalence/property tests before/after optimization.

**Benchmarks**

- control WCET <250 µs, p99.999 jitter <50 µs;
- encoder >5 kHz (target 10 kHz), IMU 400–1000 Hz, motor 1 kHz;
- command p99.9 <2 ms; boot-ready <500 ms;
- ≥25% flash/RAM and ≥30% stack margins.

**Exit criteria**

- every stated performance target has reproducible target evidence;
- no optimization weakens safety/testability;
- middleware/iceoryx2 features retained only with measurable value.

## Phase 10 — production hardening

**Deliverables**

- signed A/B bootloader/update, anti-rollback and key ceremony;
- static analysis, MISRA review/deviations, coverage and independent review;
- environmental/HIL endurance, EMC/pre-compliance and manufacturing fixtures;
- release SBOM, reproducible build, traceability and service manuals;
- future-board requirements for CAN-FD, secure root and missing monitors.

**Risks**

- 104 KiB A/B slot pressure;
- STM32F103 lacks hardware root of trust;
- tool/library supply-chain or irreversible protection error;
- prototype assumptions survive into production.

**Validation tests**

- signature/tamper/rollback and power-loss-at-every-update-step;
- bootloader/application fault injection and recovery;
- temperature/voltage/vibration/EMC campaigns;
- 24-hour motion/link soak and production line repeatability.

**Benchmarks**

- verified boot/control-ready <500 ms;
- update power loss always boots last confirmed image;
- required structural/MC/DC coverage achieved for safety decisions;
- zero high-severity unresolved static-analysis/security findings.

**Exit criteria**

- release evidence package approved by safety/security/manufacturing owners;
- recovery and key rotation rehearsed;
- claims match actual hardware capability and external assessment scope.
