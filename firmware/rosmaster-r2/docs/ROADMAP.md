# Production implementation roadmap

Benchmarks are gates, not estimates. Host timing is indicative; only target and
electrical measurements support real-time/safety claims.

## Deployment reality — where the live R2 actually is (2026-07)

The phases below describe the *target*. This section records what is deployed
on the bring-up robot today, so the gap is legible without reading eleven
phases. Three states, deliberately separated.

### Current implemented state — Yahboom base, Kirra chokepoint

The Jetson Orin NX (Ubuntu 22.04 / JetPack 6.2, ROS 2 Humble) drives a **stock
Yahboom/Rosmaster motor controller** over `/dev/myserial`. None of the firmware
in this tree is on the robot. What *is* live is the ADR-0033 chokepoint:

```
Rabbit / Mick → planner → verifier / governor
              → kirra_motor_consumer.py   (verifies the Ed25519 release token)
              → /dev/myserial             (TIOCEXCL, 0600, single writer)
              → Yahboom MCU               (vendor firmware, unverified)
```

Live evidence from bring-up: `serial exclusivity: OK`,
`TIOCEXCL claimed on /dev/myserial`, `KIRRA consumer OWNS /dev/myserial`,
`r2_ackermann drive: open-loop equal-PWM`.

So the **authorization** boundary is already Kirra's, in Linux userspace. The
**actuation** boundary is still the vendor's: the Yahboom MCU accepts whatever
bytes reach it, and the single-writer property is what stops anything else
sending them. Closed-loop wheel matching and R2 odometry are deliberately off
pending validation.

> The consumer is a *sole-writer* guard, not an authenticated link. It cannot
> stop a process that gains the port; it stops there being a second process
> with the port. That distinction is why the udev rule, `TIOCEXCL` and the
> boot sentinel are all load-bearing, and why they must stay after the MCU
> work lands.

### Near-term bridge state — R2CP over the vendor board

The next boundary move is the **Jetson-side R2CP bridge**. This is the single
largest gap between the spec in `PROTOCOL.md` and anything runnable, and it is
host-side work — no board bring-up required:

1. ✅ a host encoder/decoder for the canonical frame — `crates/kirra-r2cp`
   (`lib.rs`). `wire.cpp` remains the reference implementation and the
   conformance oracle; where the two disagree, `wire.cpp` is right;
2. ✅ a differential test harness — `tests/differential_vs_wire_cpp.rs` compiles
   the real `wire.cpp` into an oracle subprocess and compares encoder bytes and
   decoder VERDICTS in both directions, so the two implementations cannot drift
   silently;
3. ✅ a **simulated MCU** speaking R2CP over a PTY — `sim.rs` (the rules) and
   `pty.rs` (the binding). `kirra-r2cp-sim` prints a `/dev/pts/N` path a real
   bridge can open;
4. ⬜ the consumer gaining an R2CP drive mode alongside `r2_ackermann`/`x3`,
   gated by `KIRRA_DRIVE_MODE` exactly as the existing modes are.

Only then does firmware flashing become a *swap* rather than a leap.

**What the PTY stage does and does not prove.** It exercises the bridge against
a peer that enforces the protocol's refusals — replay (`sequence <=
last_accepted`), staleness, state gating, the watchdog, malformed payloads —
over a real device path with real read boundaries and a real line discipline.
Passing is evidence about the **bridge**. It is not evidence about the firmware,
and a PTY cannot model baud rate, framing errors, line noise, cable pull,
brown-out or on-target timing. HIL is what tests the link.

> **Open obligation — COMMAND_ACK result codes.** `PROTOCOL.md` §COMMAND_ACK
> names the eight results in prose ("accepted, clamped, stale, replay,
> unauthenticated, invalid, disarmed and faulted") but binds no numbers, and no
> firmware emits an ACK yet. `kirra_r2cp::sim::ack_result` assigns them in prose
> order **provisionally**, and says so at its definition. When the firmware
> implements COMMAND_ACK it must either adopt those values or change them in
> both places; the differential harness is what will catch a silent
> disagreement. Until then a bridge test asserting a result code is asserting
> against a proposal, not against the protocol.
>
> `SafetyState` is NOT in that position: its wire bytes are mirrored from the
> `safety_manager.hpp` discriminants (`boot`=0 … `firmware_update`=7), which is
> why `sim::SafetyState::to_wire` writes the numbers out instead of deriving
> them from its own smaller enum.

### Final Kirra-owned state — and what it does NOT remove

```
… → verifier / governor → consumer → R2CP (authenticated) → Kirra MCU firmware
                                                          → motors, steering,
                                                            encoders, watchdog,
                                                            e-stop
```

**Replacing the firmware does not remove the Jetson verifier or governor.**
`PROTOCOL.md` already states this and it bears repeating here because it is the
easiest thing to get wrong: R2CP's `AUTH_TAG` authenticates the *link* — it
proves the bridge sent the bytes. It says nothing about whether the checker
authorized the motion they encode. Until either the MCU verifies the Kirra
release token itself, or a dedicated Kirra signer emits an authorization MAC
under a key the bridge never holds, ADR-0033's token-verifying consumer, the
serial ACL and the startup sentinel remain the trust boundary. A Kirra-owned
MCU moves the *watchdog and safe-state* into hardware; it does not move the
*authorization decision* out of the governor.

What the MCU does buy: a hardware watchdog with a bounded FTTI, boot-safe motor
state, an e-stop input that is not mediated by Linux, encoder/battery/fault
telemetry the checker can trust, and a firmware image whose provenance we
control (`bootloader/image_verifier.hpp`).

### Rev A carrier hardware — the stage-2 bench board

The hardware side of stages 2–3 now has a documentation foundation:
**Kirra Control Board Rev A** (`docs/hardware/kirra-control-board-rev-a/`),
a safety-focused carrier for an STM32 NUCLEO-G474RE — the concrete first
vehicle for the STM32G4 control-board revision already planned in
`ARCHITECTURE.md` §Decision summary. It retains the external motor
drivers/motors/encoders/steering, speaks R2CP to the Jetson, and adds a
hardware-combined driver enable (E-stop AND independent watchdog AND
firmware request). Status — documentation only, nothing is fabricated:

- Rev A documentation foundation — **started**;
- pin allocation — **blocked** on the exact Nucleo (MB1367) revision and
  external measurements;
- schematic — not started;
- PCB layout — not started;
- manufacturing release — **not approved**.

A NUCLEO-G474RE BSP in this tree is future work, gated exactly like the
F103 BSP (`drivers/README.md`): no register code before physical
verification.

Mechanical platform direction
(`docs/hardware/kirra-control-board-rev-a/mechanical-reference.md`,
HDR-0007) — the R2 stays the bring-up vehicle; the long-term mechanical
reference moves off Yahboom-specific geometry:

- Traxxas 1/10 mechanical reference — **adopted as a platform direction**;
- exact chassis model — **pending MR-1** (Chassis Selection Review);
- Yahboom R2 adapter reference — retained for Rev A bring-up;
- Class A mounting definition — not frozen;
- adapter plate — not designed;
- 3D fit verification — not started.

### Staged migration

| Stage | Boundary | Verifier/governor | Vendor MCU |
|---|---|---|---|
| 0 — today | consumer + `TIOCEXCL` | required | present, unverified |
| 1 — bridge | consumer speaks R2CP to a **simulated** MCU | required | present |
| 2 — HIL | R2CP to Kirra firmware on a bench board | required | bench only |
| 3 — swap | R2CP to Kirra firmware on the robot | required | removed |
| 4 — bound | MCU verifies Kirra authorization | required | — |

Rollback at every stage is `KIRRA_DRIVE_MODE` plus reflashing the vendor image;
stages 0–2 are rollback-free because the robot never stops using the vendor
board. Do not skip stage 1: it is the only stage that can fail cheaply.

### Compatibility and test strategy

- **Conformance**: the host encoder is tested against `wire.cpp`, not against a
  second specification. One decoder is normative.
- **Simulated MCU** (stage 1) carries the FDIT matrix — truncated frames, CRC
  flips, replayed sequences, stale timestamps, oversize payloads — reusing the
  existing `fuzz/corpus/` seeds so host and target see identical bytes.
- **HIL** (stage 2) adds what simulation cannot: real UART timing, brown-out,
  cable pull, watchdog expiry under load, and the on-target WCET measurements
  the crypto phase gate requires.
- The existing `tools/check.sh` and the `rosmaster-r2-firmware` CI lane stay the
  gate for firmware changes; the bridge gets a lane of its own when it lands.

### Diagnostics that must survive the migration

Bring-up produced four false positives from *vendor-name* matching — a lidar in
`yahboomcar_ros2_ws`, the `yahboom.local` mDNS hostname, an OLED unit, and
`ollama.service` with the workspace on its `PATH`. Acting on them disabled
Ollama and killed the lidar. `robot/motor_authority.py` replaced that with
**serial-authority** detection: who actually holds the configured motor device,
compared against the consumer's systemd MainPID.

That check is written against a device path, not a protocol, so it keeps
working unchanged when the byte stream on `/dev/myserial` becomes R2CP. The
single-writer invariant is what it verifies, and that invariant outlives the
vendor board.

## Phase 0 — system safety and security baseline

**Deliverables**

- system requirements, operating domain, hazards, threat model and trust boundary;
- fault-tolerant times, safe-state definitions and requirements-to-test IDs;
- Kirra release-authorization binding, device-key custody and recovery policy;
- toolchain/evidence strategy and change-control baseline.

**Risks**

- architecture work encodes unstated safety assumptions;
- link authentication is mistaken for command authorization;
- performance targets lack a measurable endpoint.

**Validation tests**

- independent hazard/threat-model review and traceability walk;
- fault-response timing analysis for every safety mechanism;
- red-team bypass analysis from ROS/DDS and direct device access.

**Benchmarks**

- every safety goal has a quantitative FTTI and verification method;
- 100% of motion-authority paths terminate at one verified MCU/consumer boundary.

**Exit criteria**

- safety, security and systems owners approve the baseline;
- unresolved assumptions are explicit gates on all later phases.

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

Current status: the initial repository evidence baseline and non-enabling
portable capability manifest are complete; source page-level extraction and
physical-unit closure remain required.

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
- Kirra release-token verification (or device-key authorization MAC), serial
  ACL/startup sentinel and explicit R2CP key custody;
- SROS2 profile and optional iceoryx2/Zenoh adapters.

**Risks**

- duplicate publisher/device-owner bypass;
- ROS clocks are mixed with MCU monotonic deadlines;
- RMW or Linux jitter is mistaken for MCU determinism.

**Validation tests**

- launch tests, rogue publisher/device-open attempts and lifecycle restarts;
- unsigned/tampered/replayed Kirra authorization and direct serial-open attempts;
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

- signed A/B bootloader/update, best-effort F103 software rollback floor and key
  ceremony; tamper-resistant anti-rollback requires a secure element/revised MCU;
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
