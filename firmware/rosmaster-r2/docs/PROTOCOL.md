# R2 Control Protocol (R2CP) v1

## Scope

R2CP is the bounded point-to-point protocol between the Linux hardware bridge
and the MCU. It is independent of ROS, DDS and the Yahboom legacy protocol.
UART is the first carrier; CAN-FD may carry the same logical messages later.

## Carrier

- UART: 8-N-1, DMA RX/TX, validated ≥921600 baud for the latency target.
- Frame: COBS-encoded canonical bytes followed by `0x00`.
- Maximum decoded frame: 216 bytes; maximum payload: 192 bytes.
- Multi-byte integers: little-endian; IEEE-754 binary32 is allowed only in
  specified payload fields and must be finite.
- CRC: CRC32C Castagnoli over header + payload.
- Queueing: commands use one latest-value slot; reliable management traffic uses
  a bounded four-entry window.

## Canonical header

| Offset | Size | Field |
|---:|---:|---|
| 0 | 2 | magic `0x3252` |
| 2 | 1 | protocol major |
| 3 | 1 | protocol minor |
| 4 | 1 | message type |
| 5 | 1 | flags |
| 6 | 2 | payload length |
| 8 | 4 | sequence |
| 12 | 8 | source monotonic time, µs |
| 20 | N | payload |
| 20+N | 4 | CRC32C |

The checked implementation is `protocol/src/wire.cpp`. COBS bounds
resynchronization to one delimiter. A malformed/oversize/CRC-invalid frame is
discarded without changing command freshness or sequence state.

## Flags

| Bit | Name | Meaning |
|---:|---|---|
| 0 | ACK_REQUIRED | Receiver must return command or management ACK |
| 1 | RESPONSE | Response to a request |
| 2 | AUTH_TAG | Payload ends with a 16-byte session authentication tag |
| 3 | FAULT_LATCHED | Sender has a latched fault |
| 4–7 | — | Must be zero in v1 |

CRC detects corruption; it does **not** authenticate. In production,
`MOTION_COMMAND`, configuration mutation, calibration and update messages require
`AUTH_TAG`. The tag is HMAC-SHA-256 truncated to 128 bits over the complete
canonical header and payload-before-tag under a per-device session key. Session
keys are derived from a provisioned device key and both hello nonces using
HKDF-SHA-256. Keys never traverse R2CP. Commands without a valid tag are not
liveness events. Crypto requires a reviewed library and on-target WCET tests;
the framing layer intentionally contains no home-grown crypto.

## Message catalogue

| ID | Name | Direction | Reliability |
|---:|---|---|---|
| `0x01` | HELLO | either | retry |
| `0x02` | CAPABILITIES | MCU→SBC | response |
| `0x03/04` | TIME_SYNC_REQUEST/RESPONSE | SBC↔MCU | correlated |
| `0x10` | MOTION_COMMAND | SBC→MCU | latest-value + ACK |
| `0x11` | COMMAND_ACK | MCU→SBC | best effort |
| `0x20` | ROBOT_STATE | MCU→SBC | best effort |
| `0x21` | ODOMETRY | MCU→SBC | best effort |
| `0x22` | IMU | MCU→SBC | best effort |
| `0x23` | BATTERY | MCU→SBC | best effort |
| `0x24` | DIAGNOSTICS | MCU→SBC | best effort |
| `0x25` | FAULT_EVENT | MCU→SBC | retry until acknowledged |
| `0x30/31` | CONFIG_GET/SET | either | request/response |
| `0x32` | CALIBRATION | SBC→MCU | transactional |
| `0x40` | ENTER_BOOTLOADER | SBC→MCU | authenticated, standby only |
| `0x41` | FIRMWARE_BLOCK | SBC→bootloader | selective ACK |
| `0x42` | FIRMWARE_COMMIT | SBC→bootloader | authenticated |

Unknown IDs, reserved flag bits or unsupported mandatory capabilities are
rejected explicitly. Major mismatch prevents arming. Minor mismatch is allowed
only through advertised capabilities.

## Core payloads

All command values are SI units and finite.

### HELLO

`nonce[16], implementation_id:u32, firmware_semver:u32, minimum_major:u8,
maximum_major:u8, maximum_frame:u16`.

### CAPABILITIES

Fixed bitset plus board revision, MCU ID, IMU type, carrier baud, control rate,
encoder snapshot rate, telemetry rates, active configuration generation,
bootloader version and feature limits. Capability discovery reports facts; it
does not relax a required Linux deployment profile.

### MOTION_COMMAND

`command_id:u32, valid_for_us:u32, velocity_mps:f32, curvature_per_m:f32,
acceleration_limit_mps2:f32, jerk_limit_mps3:f32, mode:u8, reserved[3],
auth_tag[16]`.

`valid_for_us` is bounded to 20–1000 ms by configuration. MCU reception time and
source-time mapping both must show freshness. `mode` is STOP, TRACK or HOLD_ZERO;
there is no raw PWM or servo-pulse command in normal operation.

### COMMAND_ACK

`command_id:u32, received_sequence:u32, applied_at_us:u64, result:u16,
safety_state:u8, reserved:u8, active_faults:u64`.

ACK proves protocol handling, not physical movement. Result distinguishes
accepted, clamped, stale, replay, unauthenticated, invalid, disarmed and faulted.

### ROBOT_STATE / ODOMETRY

Robot state carries safety state, fault bits, command age, applied speed/
curvature and raw extended encoder counts. Odometry carries x/y/yaw, longitudinal
speed, yaw rate and upper-triangular 3×3 covariance (six binary32 fields), all
timestamped at acquisition/integration rather than transmission.

### IMU / BATTERY / DIAGNOSTICS

IMU carries calibrated acceleration/angular velocity/magnetic field,
temperature, sample sequence and health bits. Battery carries voltage and only
reports current/temperature when hardware capabilities prove those channels.
Diagnostics carries reset cause, CPU/stack high-water, loop histogram, deadline
misses, frame errors/gaps, sensor health and persistent-event cursor.

### CONFIGURATION / CALIBRATION

GET names a schema and field group. SET carries expected generation, a complete
replacement record and authentication tag; partial updates are rejected. The MCU
validates ranges, writes the inactive flash slot, reads it back and atomically
selects the newest valid generation. Calibration profiles are records, not
arbitrary code. Motion remains disarmed when `calibrated=false`.

## Sequencing, loss and retransmission

Each direction has an independent wrapping 32-bit sequence. A candidate advances
when modulo subtraction is nonzero and within the negotiated maximum jump.
Duplicates/replays never update freshness. Gaps increment diagnostics.

Motion commands are never retransmitted by the MCU and never queued. The SBC
sends a new command at 100–250 Hz and treats missing ACKs as health degradation.
The MCU enters controlled stop at its local deadline regardless of ACK traffic.

Configuration, fault log and firmware transfer use request IDs, bounded
retries, idempotent blocks and selective ACK bitmaps. No retry can block the
1 kHz control task.

## Time synchronization

Four timestamps implement an NTP-like exchange:

```text
SBC t1 ─ request ─> MCU records t2
SBC t4 <─ response(t1,t2,t3) ─ MCU t3
```

Linux fits offset and drift using minimum-delay samples. MCU time is a 64-bit
monotonic microsecond counter and is never stepped. UTC/PTP time is metadata on
Linux; safety deadlines use MCU monotonic time only. Offset uncertainty is
published and inflates odometry timestamp covariance.

## Version negotiation and startup

1. physical outputs disabled;
2. MCU completes POST and loads a valid configuration;
3. HELLO nonces and capabilities exchanged;
4. protocol major, deployment profile and authentication established;
5. time-sync uncertainty reaches threshold;
6. Linux requests arm; MCU requires local safety preconditions;
7. first fresh STOP/HOLD_ZERO command establishes sequence;
8. explicit activation permits TRACK.

Any restart returns to disabled standby. A transport reconnect cannot replay a
pre-restart command into motion.

## Fuzz and fault-injection obligations

The decoder must be fuzzed over arbitrary byte streams and checked for bounded
runtime, no out-of-bounds access and delimiter recovery. Integration tests inject
bit flips, truncation, overlength, zero runs, duplicate/wrapped/jumped sequences,
stale timestamps, bad tags, ACK loss, DMA splits and floods. Only valid fresh
authenticated commands may refresh liveness.
