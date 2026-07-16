# R2 Path-B Ackermann drive — proposal (pure core landed + host-tested; INERT — review before consumer wiring / robot testing)

> **Status: PROPOSAL. The pure translation core is implemented + host-tested;
> it is INERT — no consumer wiring, no calibration, nothing has driven the
> robot.** This is the design for an open-source R2 drive that bypasses the
> (broken, on this cross-labeled X3 image) firmware `set_car_motion` kinematics
> and instead drives the two rear motors directly + steers the servo, doing the
> Ackermann math in KIRRA-governed code. Written for review; the calibration
> gaps in §5 MUST be measured on the bench before this can drive correctly.
>
> **Slices 1-2 landed (real code, still inert + OFF by default):**
> - **Slice 1** — the L1 Ackermann geometry + L2 calibration + fail-closed
>   last-hop of §§3-7 as a PURE module, `robot/r2_drive.py`
>   (`translate(v, omega, cal) -> R2Actuation`), with exhaustive host tests
>   (`robot/r2_drive_test.py`). No serial I/O, no ROS/vendor imports —
>   host-verified without a robot.
> - **Slice 2** — the §6 consumer wiring, behind the off-by-default
>   `KIRRA_DRIVE_MODE` flag (default `x3_set_car_motion` = byte-identical
>   existing path; `r2_ackermann` = Path B). In r2 mode the consumer sets
>   car-type 5 at init (+verifies, fail-closed), applies the measured centre
>   trim, swaps the verify-gated last hop to `set_motor` + AKM steering, and
>   never calls `set_car_motion`. Proven end-to-end against stubs in
>   `robot/teardown_smoke_test.py` case (5); the x3 path is unchanged (cases
>   1-4 still pass).
>
> It STILL cannot drive: `R2DriveCalibration` (built from `KIRRA_R2_*` via
> `calibration_from_env`) REFUSES construction on any missing/invalid field, so
> with the §5 measurements open the r2 path fails closed at startup. What
> remains: the §5 bench calibration, the §9 sign-offs, and on-hardware
> validation (§8) — then flip `KIRRA_DRIVE_MODE=r2_ackermann`.

## 1. Why Path B

`Rosmaster.set_car_motion` is a pure pass-through: it forwards `(car_type, vx,
vy, vz)` to the MCU and the **firmware** does the car-type kinematics (analyzed
in the read-only investigation; confirmed against the installed Rosmaster_Lib
3.3.9). The X3 image on this R2 does not implement the type-5 (Ackermann) drive
path, so `set_car_motion(v, 0, 0)` moves only one wheel. Path A (flash Yahboom's
R2 firmware) remains the low-effort route (`robot/install/PLATFORM_R2_PENDING.md`). **Path B**
needs no vendor firmware: `set_motor` (FUNC `0x10`) carries **no car-type byte**
and addresses the four motor channels directly, bypassing the mixer entirely;
`set_akm_steering_angle` (FUNC `0x31`) drives the steering servo independently.
The Ackermann kinematics then live in our verifiable code, which is arguably a
*better* safety story than closed vendor firmware.

## 2. Confirmed hardware facts (bench probe)

From `robot/motor_channel_probe_results.txt` (PR #911 probe, elevated):

| `set_motor` arg | Channel | Wheel | Forward sign |
|---|---|---|---|
| `s1` | M1 | rear-left | **+** |
| `s2` | M2 | (unpopulated) | — |
| `s3` | M3 | (unpopulated) | — |
| `s4` | M4 | rear-right | **+** |

- **Straight-ahead forward drive = `set_motor(+v, 0, 0, +v)`.** Both rear channels
  take POSITIVE for robot-forward — no mirror-sign flip (motor polarity is
  already matched).
- Steering is **not** a motor channel (AKM path); it stayed centered through all
  four pulses.
- Encoder cross-check agreed with the visual on both driven channels.

### 2a. Steering requires car-type 5 (bench-confirmed, PR #913 follow-up)

The first calibration run saw **no servo motion** during the Phase C steering
sweep. Cause (confirmed on the bench, `r2_drive_calibration_results.txt`): the
AKM steering servo **only actuates when the board is in car-type 5**. Under the
default cross-labeled X3 image (car-type 1) `set_akm_steering_angle` is silently
ignored. After `set_car_type(5)` the front wheels swing as commanded.

- This is **not** a Path-B problem — it's the reason Path B works. Type 5 is
  known to *break* `set_car_motion` drive (`robot/install/PLATFORM_R2_PENDING.md`), but Path B
  **never uses `set_car_motion`**; it drives with `set_motor`, which is car-type
  **independent** (the Phase A drive sweep ran fine without ever touching
  car-type). So the Path-B trio is: `set_car_type(5)` (servo on) +
  `set_motor(+v,0,0,+v)` (drive, any type) + `set_akm_steering_angle(cmd)` (steer).
- `get_akm_default_angle()` returns the sentinel **−1** on this image **even with
  the servo actuating** — it is an unimplemented getter, **not** an
  "AKM-inactive" flag. The physical centre trim must therefore be read from the
  protractor sweep, never from this call.
- `set_car_type` is **RAM-volatile** (reverts on reboot). Because the *currently
  live* consumer still uses `set_car_motion`, any bench tool that sets type 5
  must **restore type 1 on exit** (or reboot) so the live drive path is not left
  broken. The calibration script does this in its `finally`.

## 3. The drive stack (three layers)

```
  (governed cmd_vel: v [m/s], ω [rad/s])   ← KIRRA has ALREADY bounded this
        │
  L1 Ackermann kinematics  ── δ = atan(L·ω / v) ;  rear speeds
        │
  L2 calibration           ── m/s → PWM% ;  road-wheel rad → servo command-units
        │
  L0 raw actuation         ── set_motor(pwm,0,0,pwm) + set_akm_steering_angle(cmd)
```

L1 is exact geometry. **L2 is the gap** — until the two calibrations in §5 are
measured, L0 cannot honor a physical `m/s` / `rad` command.

## 4. Ackermann kinematics (L1)

Inputs are the **already-governed** command `(v, ω)` = `(linear.x, angular.z)`.
`L` = wheelbase (≈0.229 m measured, to re-confirm). Bicycle model:

- **Turn radius** of the rear-axle centre: `R = v / ω` (ω ≠ 0).
- **Front road-wheel steering angle**: `δ = atan(L · ω / v)` (from `tan δ = L/R`).
  - `ω = 0` → `δ = 0` (straight).
  - `v = 0, ω ≠ 0` is **not Ackermann-achievable** (a car cannot spin in place):
    KIRRA must refuse/clamp such a command *upstream*; this layer treats it as a
    fault → MRC stop (§6), never an unbounded steer.
- **Sign**: `ω > 0` (left / CCW) → `δ > 0` → steer **left**. The servo command is
  `[-45,+45]` with **negative = left**, so `steer_cmd ≈ −K·δ`. The sign **must be
  verified on the bench** (drive slow, confirm a left command turns left) before
  any floor run.
- **Clamp** `δ` to `±δ_max` (max road-wheel angle at full lock, measured).
- **Rear-wheel speeds**: rear-axle centre moves at `v`. Optional true Ackermann
  differential `v_{L,R} = v ∓ t·ω/2` (`t` = rear track). **v0 proposal: equal PWM
  on both rear wheels** (`t` not needed); the differential is small at low `ω`
  and is a later refinement (needs `t`, and — to be exact in m/s — the speed
  calibration).

## 5. Calibration gaps — MEASURE, do not invent

These are the values that stand between this design and correct driving. Each is
a bench measurement (elevated), recorded like `steering_bench_results.txt`, never
guessed:

1. **PWM ↔ ground speed.** Sweep `set_motor` %; measure wheel surface speed (or
   encoder ticks/s → m/s using the wheel radius). Yields `pwm = v / V_PER_PWM`.
   *Without this the drive cannot honor a `m/s` command* — only a normalized
   fraction of full PWM. Floor-blocking.
2. **Road-wheel angle ↔ servo command-units.** `set_akm_steering_angle` is
   `[-45,+45]` **command units** about the servo centre default (see 3) —
   command units ≠ road-wheel degrees. Measure road-wheel angle (protractor) at
   several commands → `K` (command-units per radian) + linearity. Floor-blocking
   for turns.
3. **Steering centre trim** (`set_akm_default_angle`, range `[60,120]`) so `δ = 0`
   is physically straight. The library initialises this default to **100**;
   `robot/install/PLATFORM_R2_PENDING.md` earlier noted the `[60,120]` **midpoint (90)** — both
   are provisional. The physical straight-ahead centre is the value MEASURED here
   from the protractor sweep — **not** `get_akm_default_angle`, which returns the
   sentinel **−1** (unimplemented) on this image even with the servo actuating
   (see §2a).
4. **Max road-wheel angle at full lock** → `δ_max`.
5. **Rear track `t`** — only if the Ackermann rear differential (§4) is chosen.
6. **Wheelbase `L`** ≈0.229 m — re-confirm on this platform.

These become the measured fields of the `r2` contract profile
(`robot/install/PLATFORM_R2_PENDING.md` Track-A A2), never invented defaults.

## 6. KIRRA integration & fail-closed

**This module is the R2 *doer* last-hop — the analog of the X3 firmware mixer,
but in our code.** It does NOT relax the safety architecture:

- **The verify chokepoint is unchanged.** In `kirra_motor_consumer.py`, actuation
  happens only after the Rust verify core releases the token (ADR-0033). Path B
  swaps exactly one call: the released last-hop
  `set_car_motion(linear, 0.0, angular)` becomes the Ackermann pair
  `set_motor(pwm,0,0,pwm)` + `set_akm_steering_angle(cmd)` (via
  `r2_drive.apply_actuation(bot, translate(...))`). The translation runs
  **after** verify — the same place the X3 firmware mixing runs after verify.
  The token still gates the enforced bytes. **Landed (slice 2)** behind
  `KIRRA_DRIVE_MODE=r2_ackermann`; the default (`x3_set_car_motion`) is
  byte-identical.
- **The consumer must set car-type 5 once at init** (§2a) so the AKM servo
  actuates, and it MUST NOT call `set_car_motion` thereafter (type 5 breaks it —
  which is fine, Path B does not use it). This replaces the current
  `KIRRA_EXPECTED_CAR_TYPE` type-1 assertion for the R2 platform. Because type 5
  is RAM-volatile, the consumer re-asserts it on every start (it does not rely on
  a persisted board setting).
- **`safe_stop` becomes** `set_motor(0,0,0,0)` **+** `set_akm_steering_angle(0)`
  (replacing `set_car_motion(0,0,0)` at `:160`), preserving SS-002 stop-on-fault.
- **Fail-closed at the last hop**: any non-finite `(v, ω)` or `δ`, a `v=0,ω≠0`
  command, or an out-of-range clamp saturating → MRC (`set_motor(0,0,0,0)` +
  centre). Never a bare unbounded actuation.
- **KIRRA class re-validation.** The checker's envelope/WCET reasoning currently
  assumes the X3 `(linear, angular)` → `set_car_motion` semantics. For R2 it must
  be re-validated under `KIRRA_VEHICLE_CLASS=r2` with the measured contract
  profile (`robot/install/PLATFORM_R2_PENDING.md`). The interceptor wheelbase cross-check
  (`ros2_ws/.../cmd_vel_interceptor.py`) uses `L` from that profile.

## 7. Reference algorithm (now realized as a pure module)

Slice 1 committed this as real, host-tested code: `robot/r2_drive.py`
(`translate(v, omega, cal) -> R2Actuation`) + `robot/r2_drive_test.py`. It is
still INERT — the consumer does not call it (§6 wiring pending) and no
`R2DriveCalibration` can be built until §5 is measured (the dataclass fails
closed on any missing field). The pseudocode below matches the module 1:1.

```
# Inputs: v (m/s), omega (rad/s) — ALREADY governed/bounded by KIRRA upstream.
# Calibrations (measured, from the r2 profile): L, V_PER_PWM, K, delta_max,
# center_trim, PWM_MAX.
# ONCE at consumer init (NOT per command): bot.set_car_type(5)  # enables the AKM
#   servo (§2a); drive via set_motor is car-type independent. Never call
#   set_car_motion after this.
def r2_ackermann_last_hop(v, omega):
    if not (isfinite(v) and isfinite(omega)):     return mrc_stop()
    if abs(v) <= STOP_EPS and abs(omega) > W_EPS: return mrc_stop()  # not Ackermann-achievable
    # steering
    delta = 0.0 if abs(v) <= STOP_EPS else atan(L * omega / v)
    delta = clamp(delta, -delta_max, +delta_max)
    steer_cmd = clamp(round(-K * delta), -45, +45)   # sign STILL TO VERIFY on bench (Phase C sign-check pending)
    # drive (equal-PWM v0)
    pwm = clamp(round(v / V_PER_PWM), -PWM_MAX, +PWM_MAX)
    # actuate (order: steer, then drive; both fail-closed on raise)
    bot.set_akm_steering_angle(steer_cmd)
    bot.set_motor(pwm, 0, 0, pwm)          # M1=rear-left, M4=rear-right, both +fwd

def mrc_stop():
    bot.set_motor(0, 0, 0, 0)
    bot.set_akm_steering_angle(0)
```

## 8. Validation plan (staged, elevated → floor)

0. **Calibrate (elevated)** — measure every §5 value; record to
   `r2_drive_calibration_results.txt`. Verify the steering sign (§4).
1. **Elevated drive** — feed governed commands through the last-hop; confirm:
   straight = both rear wheels same direction/speed; left/right steer sign;
   fail-closed on injected NaN / `v=0,ω≠0`; `safe_stop` zeros both.
2. **Floor, tethered, low speed** — straight + gentle turns; confirm KIRRA bounds
   and the verify gate hold end-to-end (a `first_run_elevated.sh`-style guided
   acceptance, adapted for R2).
3. Only then: promote to the standing R2 config (`KIRRA_VEHICLE_CLASS=r2`,
   `KIRRA_EXPECTED_CAR_TYPE` per platform, interceptor wheelbase = profile `L`).

## 9. Open decisions (need sign-off before implementation)

- **Equal-PWM vs Ackermann rear differential** for v0 (differential needs `t`).
- **Open-loop vs encoder-closed-loop** speed (m/s tracking vs PWM proportional).
  Open-loop v0 will not perfectly track a `m/s` command (§ probe: ~12% wheel
  mismatch at equal PWM) — acceptable for low-speed v0, or close the loop.
- **Where the Ackermann translation lives** — RESOLVED (slice 1): a small
  dedicated pure module, `robot/r2_drive.py`, that the consumer will call after
  verify. Chosen over inlining in `kirra_motor_consumer.py` so the translation
  is host-testable in isolation (the ADR-0033 chokepoint stays thin). The verify
  gate remains ahead of it. The remaining §9 items below are still open.
- **`v=0, ω≠0` policy** — confirmed as an upstream KIRRA refusal + last-hop MRC
  (Ackermann cannot execute it).

---

Cross-refs: `robot/motor_channel_probe_results.txt` (the map),
`robot/install/PLATFORM_R2_PENDING.md` (Path A / the `r2` profile),
`robot/kirra_motor_consumer.py:157,177` (the verify-gated actuation last-hop).
Human review; do not merge.
