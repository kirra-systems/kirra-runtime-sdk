# Safety, diagnostics and production design

> **Design status:** portable state/configuration primitives are implemented.
> Electrical safety mechanisms, watchdogs, boot crypto, flash layout and every
> timing claim require target implementation and evidence.

## Safety state machine

```mermaid
stateDiagram-v2
    [*] --> Boot
    Boot --> SelfTest
    SelfTest --> Standby: POST + config valid
    SelfTest --> FaultLatched: failure
    Standby --> Armed: authenticated arm + local preconditions
    Armed --> Active: fresh HOLD_ZERO + explicit activate
    Active --> ControlledStop: timeout / recoverable fault
    ControlledStop --> Standby: speed zero; active fault remains non-armable
    Active --> FaultLatched: severe fault
    Armed --> FaultLatched: severe fault
    FaultLatched --> Standby: active cause absent + local acknowledgement clears recoverable latch
    FaultLatched --> Boot: POST/config fault + reset
    Standby --> FirmwareUpdate: authenticated request
    FirmwareUpdate --> Boot: verified image / reset
```

Only `Active` permits ordinary motion. `ControlledStop` grants authority only to
an internally generated bounded deceleration, remains there until measured
speed is below the calibrated stop threshold, then enters disabled Standby; it
never holds the last command. E-stop, runaway, overcurrent, overtemperature, encoder/steering
plausibility and missed control deadline disable immediately. Configuration and
updates require standby and disabled outputs.

## Fault policy

| Detection | Reaction | Latch / recovery |
|---|---|---|
| Physical E-stop | hardware cut + bridge disable | physical release and acknowledgement |
| Command timeout/auth failure | controlled decel, then disable | automatic to standby; explicit re-arm |
| Brownout/undervoltage | inhibit start or controlled stop | hysteretic automatic, log event |
| Overcurrent/overtemperature | immediate bridge disable | latched |
| Motor runaway | immediate bridge disable | latched |
| Encoder implausible | disable affected drive/all drive | latched |
| Steering implausible | immediate drive disable | latched |
| IMU invalid | inflate covariance; stop if required profile | recoverable with healthy streak |
| Deadline miss | immediate bridge disable | latched |
| Corrupt communication | discard; never refresh heartbeat | healthy authenticated streak |
| Invalid configuration | no arm | valid rollback/configuration |

Current, thermal and steering-feedback policies are capability-gated. If the
stock PCB lacks those sensors, production claims must say “not monitored” and a
board revision is required; firmware must not synthesize health.
`imu_sane` and `communication_healthy` at the safety-manager boundary are
already debounce/hysteresis-filtered monitor verdicts; their monitor services
own the documented healthy streaks. A single raw healthy sample is not passed
as recovery.

## Dual watchdog and deadline supervision

1. A high-priority software supervisor expects independent alive counters from
   control, link and sensor services. Only the supervisor services IWDG.
2. STM32 IWDG runs from LSI and resets independently of the main clock/scheduler.
3. Optional WWDG catches an erroneously fast service pattern.
4. The control task measures start jitter and execution time from a free-running
   timer. One overrun immediately disables PWM before reset.
5. After watchdog reset, outputs remain disabled through Boot/SelfTest; Standby
   is reached only after successful POST and reset-cause persistence.

The control task cannot service the hardware watchdog directly; otherwise a live
control loop could mask a dead safety/link service.

## Plausibility monitors

- **Runaway:** nonzero encoder speed while commanded zero/bridge disabled,
  direction disagreement, or acceleration beyond physical envelope.
- **Encoder:** illegal quadrature/error input where available, impossible delta,
  stagnant encoder under sustained effort, left/right residual versus commanded
  curvature.
- **Steering:** feedback-versus-command residual and rate when feedback exists;
  without feedback only output pulse integrity can be checked.
- **Slip:** compare rear differential/yaw model with gyro yaw; increase odometry
  covariance and derate before latching.
- **IMU:** WHO_AM_I, data-ready progression, finite/range checks, saturation,
  timestamp continuity, six-face gravity norm and gyro-at-rest bias.
- **Battery:** calibrated ADC plausibility, hysteresis and debounce; current and
  temperature only when electrically present.

## Power-on and runtime diagnostics

POST runs with motors disabled and must finish within the 500 ms boot target:

1. reset cause, clock and RAM sentinel;
2. firmware manifest/hash and configuration A/B CRC/schema;
3. GPIO safe-state readback and E-stop;
4. encoder timer progression/stuck inputs without motion;
5. IMU identity/data-ready/self-test;
6. ADC rails and battery plausibility;
7. watchdog test evidence from manufacturing record;
8. communication self-test and unique device identity.

Runtime telemetry includes CPU idle-derived load, every task's stack high-water,
control and link latency histograms, maxima, deadline misses, UART DMA
overrun/framing/CRC errors, sequence gaps, sensor age/health, watchdog near-miss,
reset cause, safety state/fault bits and configuration generation.

## Persistent event log

A fixed-size flash ring stores only state transitions and rare faults, never
high-rate telemetry. Each record has sequence, monotonic boot time, boot counter,
event code, compact context and CRC32C. Writes are rate-limited, occur only from
the lowest-priority flash service, and never run during active motion unless a
pre-erased slot exists. Power loss may lose the newest record but cannot corrupt
previous records. Manufacturing reads and clears the log through authenticated
service mode.

## Preliminary flash map

The stock 256 KiB flash cannot hold two generous application slots plus a
bootloader without aggressive sizing. Proposed compatible map:

| Address | Size | Purpose |
|---|---:|---|
| `0x08000000` | 24 KiB | software write-protected bootloader + public key |
| `0x08006000` | 104 KiB | application A |
| `0x08020000` | 104 KiB | application B |
| `0x0803A000` | 8 KiB | configuration A/B + calibration |
| `0x0803C000` | 16 KiB | event log / boot metadata |

This is a design allocation, not yet a linker script. Page size and actual part
capacity must be read from the unit. A 104 KiB application slot is a hard exit
criterion. If production code does not fit with margin, use external staging
flash or a larger MCU; do not remove rollback or diagnostics to make it fit.

## Authenticated boot and update

The bootloader verifies:

1. manifest magic/schema/product/hardware compatibility;
2. image length and SHA-256 digest;
3. Ed25519 signature under a compiled production public key;
4. best-effort monotonic security version against software-protected boot metadata;
5. trial/confirmed state and reset budget.

Update is receive-to-inactive-slot, hash-as-written, readback, signature verify,
mark trial, reset, POST, then confirm. Unconfirmed images roll back after a
bounded attempt count. Power loss at every write boundary leaves the previous
confirmed slot bootable. Development keys are compile-time distinct and cannot
verify in production builds.

STM32F103 does **not** provide an immutable secure-boot root, tamper-resistant
rollback floor or hardware
key vault. RDP and write protection improve resistance but do not create the
same root as a secure element/TrustZone MCU. RDP level 2 is irreversible and
must not be enabled before manufacturing recovery validation. High-assurance
production requires a control-board revision with hardware root of trust or a
secure element.

`bootloader/include/r2/boot/image_verifier.hpp` defines the crypto seam; a
reviewed library/backend must implement it. No custom signature algorithm is
permitted.

## Fault-injection resistance

- all lengths checked before copy; fixed arrays only;
- unknown enums/flags fail closed;
- NaN/Inf and out-of-domain parameters rejected before arithmetic;
- CRC failure, replay or bad authentication never refreshes command age;
- parser work per delimiter is bounded by 216 decoded bytes;
- flash records are complete replacements with schema, generation and CRC;
- configuration reduction applies absolute limits immediately;
- malformed diagnostics cannot contend with control;
- voltage glitch, reset and partial-flash campaigns are production gates.

## Manufacturing flow

1. record PCB/chassis/MCU/IMU/motor/servo revisions and photographs;
2. continuity-test motor, encoder, E-stop, ADC and steering nets;
3. flash signed manufacturing image through SWD/ROM recovery;
4. run bed-of-nails GPIO, bridge-disabled, encoder and sensor tests;
5. calibrate oscillator, battery ADC, IMU, encoders, wheel geometry and servo;
6. write device identity and signed calibration profile;
7. flash production bootloader/app and verify key separation;
8. run elevated motion, E-stop latency and watchdog/brownout campaigns;
9. export immutable test report, firmware hash and calibration generation;
10. apply read/write protection only after recovery rehearsal.

Every unit must retain traceability from serial number to PCB revision,
bootloader/app hashes, signing key ID, calibration fixture version and test
results.

## ISO 26262-inspired practice

This is not a certification claim. Adopt bidirectional requirements/tests,
safety mechanism independence analysis, freedom-from-interference evidence,
MC/DC for safety decisions, tool/version control, change impact analysis,
production traceability and independent review. Hardware metrics and ASIL claims
require the actual board failure-rate model and external assessment.
