# R2 clean-room firmware calibration profile — bench data → PR #946 fields

> **Status: DRAFT mapping for review. Not wired, not committed to any firmware
> build.** This maps the bench-measured R2 facts onto the `PlatformConfiguration`
> / `VehicleGeometry` / `board_manifest` structures of the clean-room MCU firmware
> in PR #946 (`firmware/rosmaster-r2/`). Every value carries its evidence rank and
> a status. Nothing here flips `actuation_enabled` or `calibrated` — the three
> floor-blocking captures at the bottom must land first.

## Provenance and the load-bearing caveat

Sources (all in-repo, bench-verified on the actual R2, Jetson Orin `yahboom`):
`docs/hardware/HARDWARE_FINDINGS_R2X3.md`, `docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md`,
`robot/r2_drive_calibration_results.txt`, `robot/motor_channel_probe_results.txt`.

**These measurements were taken against the stock Yahboom MCU firmware via
`Rosmaster_Lib` (Path B, SBC-side).** PR #946 is a *replacement* MCU firmware that
drives the motors and servo directly. Therefore:

- **Physical / chassis facts transfer** (tyre diameter, encoder ticks/rev, which
  motor channel is which wheel, forward polarity, wheelbase) — properties of the
  robot, not of any firmware.
- **Yahboom-protocol facts do NOT transfer** and must be **re-measured against raw
  PWM** for PR #946: the steering command range `[-45,+45]` and centre default are
  `Rosmaster_Lib` command units about the AKM servo, **not** microseconds. PR #946
  drives the servo as raw TIM7 software PWM, so `servo_*_us` needs a fresh raw-PWM
  sweep — the existing data cannot fill it.
- All values are **single-unit, mostly single-sample.** Keep them behind the
  `board_manifest` continuity gate and the `PlatformConfiguration.calibrated` flag.

## Derived constants (from the measured primitives)

```
wheel_diameter_m   = 0.066675           (2 5/8 in, calipers)         -> wheel_radius_m = 0.0333375
wheel_circumf_m    = pi*D = 0.209465
ticks_per_rev (RL) = 834.5              (single 2-turn sample, LEFT only)
m_per_tick (RL)    = C/834.5 = 2.5101e-4 m/tick
V_PER_PWM (left)   = 0.0145 (m/s)/PWM, offset -0.069, deadband ~PWM 4.7   (OPEN-LOOP, LEFT only)
L/R tick imbalance = ~28% at equal PWM (RL reads fewer) — UNRESOLVED: real drift vs encoder-scale
```

## Table A — `PlatformConfiguration` (`firmware/.../application/configuration.hpp`)

| Field | Value to set | Evidence | Status |
|---|---|---|---|
| `wheel_radius_m` | **0.0333375** | measured (calipers) | ✅ fillable now — closes the review's "unused `wheel_radius_m`" / odometry-scale gap. |
| `left_encoder_counts_per_revolution` | **834** (⚠ true 834.5) | measured, 1 sample | ⚠ fillable, with a granularity caveat — see Gap G1. |
| `right_encoder_counts_per_revolution` | — | **not measured** | ❌ blocking — RR ticks/rev never captured (Phase B RR). |
| `wheelbase_m` | 0.229 *(estimate)* | **unverified** | ⚠ provisional — "expected ~0.229, re-confirm" (Phase D). Do not treat as measured. |
| `rear_track_m` | — | not measured | ❌ only needed if the Ackermann rear differential is chosen over equal-drive v0. |
| `maximum_steering_angle_rad` (δ_max) | — | **not measured** | ❌ blocking for turns — protractor sweep pending (Phase C). |
| `servo_minimum_us` / `servo_center_us` / `servo_maximum_us` | — | **not measured in µs** | ❌ blocking — bench used `Rosmaster_Lib` command units, not raw PWM. Needs a fresh raw-PWM servo sweep for PR #946's direct TIM7 path. |
| `battery_divider_ratio` | — | **refused** | ❌ `HARDWARE_FINDINGS_R2X3.md` explicitly declined to commit the 4.03 divider as an R2 default. Measure per unit. |
| `maximum_speed_mps` / `_acceleration_mps2` / `_deceleration_mps2` / `_jerk_mps3` / `_steering_rate_rad_s` | policy envelope | design, not bench | These are the KIRRA envelope (the `r2` contract profile), not a bench measurement. Set from the class contract, not from this data. |
| `command_timeout_ms` | keep default `100` | design | unchanged. |
| `calibrated` | **stays `false`** | — | Must remain false until RR ticks/rev + δ_max + servo-µs land. Per the config review, an uncalibrated record must immobilise, not feed these zeros into control. |

## Table B — direction / rear-wheel engagement → HAL/BSP + `board_manifest`

Motor-channel → wheel map (bench probe `robot/motor_channel_probe_results.txt`,
PR #911) vs the firmware's `kSharedBoardPinManifest` / `kUnresolvedR2Harness`:

| Fact (bench-measured) | Firmware home | Status |
|---|---|---|
| **M1 = rear-left**, forward = **+** (`set_motor s1`) | `kUnresolvedR2Harness.rear_left_candidate_channel = "M1"` (already set); pins `motor_m1_a/b = PC6/PC7 = TIM8_CH1/CH2` | ✅ matches manifest. |
| **M4 = rear-right**, forward = **+** (`set_motor s4`) | `rear_right_candidate_channel = "M4"` (already set); pins `motor_m4_a/b = PB0/PB1 = TIM1_CH2N/CH3N` (Evidence: `corroborated`) | ✅ matches; note M4 pins are only `corroborated`, not `official`. |
| **Both rear channels take POSITIVE for robot-forward — no mirror-sign flip** | HAL/BSP motor-direction sign map (H-bridge A/B duty from a signed command). **No field for this exists in `board_manifest`** — see Gap G3. | ⚠ HAL calibration fact with no manifest home. |
| M2/M3 **unpopulated** on this unit | manifest lists them as shared-board candidates | ✅ consistent (they're expansion-board capability, not R2 harness). |
| Encoders: `encoder_m1 = TIM2 (PA15/PB3)`, `encoder_m4 = TIM3 (PA6/PA7)` | manifest (Evidence: `official_shared_board`) | ✅ — but **encoder counting SIGN** (which way is forward) is a BSP calibration, unmeasured for the raw path. |
| Steering servo on the **AKM path**, candidate `servo_s1 = PC3 = TIM7 software PWM` | `kUnresolvedR2Harness.steering_connector = "UNKNOWN"` | ❌ physical connector unresolved; bench drove it through `Rosmaster_Lib`, never resolved to a `servo_sN` pin. |
| `continuity_verified` / `actuation_enabled` | `kUnresolvedR2Harness{... false, false}` | ✅ **stays false** — single-unit, RR/δ_max/connector unresolved. |

## Steering-sign cross-check (a verification item, not a value)

- Bench (Path-B doc §4): in `Rosmaster_Lib` units, **negative command = left**, and
  the doc marks `steer_cmd ≈ −K·δ` as **"sign STILL to verify on the bench."**
- PR #946 `ackermann.cpp` convention: `ω>0` (CCW) → `δ>0` → steer **left**.
- **Action:** these two sign conventions must be reconciled on the raw-PWM servo
  path — confirm a positive `δ` from the firmware physically turns the wheels left
  — before `maximum_steering_angle_rad` or any turn is enabled. It is a cross-check
  the bench work sets up, not a settled constant.

## Schema gaps this exposes in PR #946 (raised in the #946 review)

- **G1 — integer `counts_per_revolution` can't hold 834.5.** The `uint32` field
  truncates the measured half-count (0.06% odometry scale error, and it's a single
  2-turn sample). Consider a higher-resolution encoder-scale representation, or
  document the rounding + require a multi-sample mean.
- **G2 — no PWM↔speed / motor-output scale field.** The bench's real drive
  calibration (`V_PER_PWM ≈ 0.0145 (m/s)/PWM`, offset, deadband, **per wheel**) has
  **no home in `PlatformConfiguration`.** The firmware needs a measured m/s→duty
  map (and the ~28% L/R imbalance means it's per-wheel, not shared) to honour a
  physical `m/s` command; today that scale would have to be hard-coded in the BSP.
  Add per-wheel `mps_per_duty` (slope + offset/deadband) fields, or an explicit
  closed-loop-only contract. **Most actionable gap.**
- **G3 — no motor-direction / encoder-sign field** in `board_manifest`. The "both
  forward = +" and per-encoder counting sign are load-bearing HAL facts with
  nowhere provenance-ranked to live.
- **G4 — the ~28% equal-PWM L/R imbalance is unresolved** (real drift vs
  encoder-scale). It bears directly on "equal command ⇒ straight" and on odometry
  covariance. One capture (RR ticks/rev) closes it; until then, naive equal-drive
  is not guaranteed straight.

## Remaining floor-blocking captures (unchanged from the Path-B plan)

1. **Phase B on the RIGHT wheel** (+ its diameter): fills
   `right_encoder_counts_per_revolution`, and resolves G4 (real drift vs scale).
2. **Phase C protractor sweep, raw-PWM path**: `maximum_steering_angle_rad`
   (δ_max), the road-wheel-angle→`servo_us` map (`servo_min/center/max_us`), and
   the **left/right sign check**.
3. **Phase D**: tape-measured `wheelbase_m` (and `rear_track_m` iff the rear
   differential is chosen).

Only after these — and with `calibrated`/`actuation_enabled` still gated — do these
values become the measured fields of the `r2` firmware profile.
