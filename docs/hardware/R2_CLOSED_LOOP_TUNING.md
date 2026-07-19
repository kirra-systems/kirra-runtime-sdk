# R2 closed-loop speed matching — bench tuning guide

> Task #84. The pure controller (`robot/r2_drive.py`: `ClosedLoopSpeedMatcher` /
> `speed_match_step`) and its consumer wiring are done and host-tested; the gains
> are conservative defaults, **not hardware-tuned**. This is the turnkey procedure
> to tune them on the R2. Do it AFTER the odom is validated (it reuses the same
> encoder feed) and the first open-loop governed drive works.

## Why closed-loop (what problem it solves)

The R2's two rear wheels are **independently driven with no shared axle**, and at
equal PWM they differ **~34%** in speed (measured, `r2_drive_calibration_results.txt`).
Open-loop equal-PWM therefore drives an **arc**, not a straight line. Closed-loop
speed matching trims each rear wheel's PWM so both track the **same governed
target speed** → the robot drives straight.

It does NOT touch safety: `translate()` is still the fail-closed front (non-finite
/ spin-in-place / at-rest → MRC or stop with zeros), the KIRRA checker still bounds
the command upstream, and closed-loop only REPLACES the two drive PWMs when
`translate` returns `ok` — the steering command and every MRC/stop path are
unchanged. A stalled wheel → MRC stop.

### The control law (per wheel, each cycle)
```
ff   = target_v / v_per_pwm_side          # feedforward from the wheel's own slope
raw  = ff + KP * (target_v - meas_v)      # proportional trim on the speed error
raw  = clamp(raw, prev_pwm ± MAX_PWM_STEP)# slew limit (no jerk)
pwm  = clamp(raw, ±pwm_max)               # hard envelope
```
`meas_v` is the EMA-filtered per-wheel ground speed (Δticks·m_per_tick / dt). No
integral term → the setpoint `|target_v|` is a ceiling P can't wind past.

## Prerequisites (all already done)
- Encoder feed proven (odom hand-spin / roll test — `encL/encR` change under motion).
- Measured constants present in `/etc/kirra/robot.env`:
  - `KIRRA_R2_M_PER_TICK` (encoder scale, m/tick),
  - `KIRRA_R2_V_PER_PWM` (LEFT/RL slope, reused for the left wheel),
  - `KIRRA_R2_V_PER_PWM_RIGHT` (RR slope).
- `KIRRA_DRIVE_MODE=r2_ackermann`, a pinned governor key, posture Nominal.

## Enable it (with the debug log)

Add to `/etc/kirra/robot.env` (then `sudo robot/install/lint_robot_env.sh --fix`
if you hand-edit, and `sudo systemctl restart kirra-consumer`):
```
KIRRA_R2_CLOSED_LOOP=1
KIRRA_R2_CLOSED_LOOP_DEBUG=1        # throttled per-cycle log, tuning only
```
On start the consumer logs `r2_ackermann drive: CLOSED-LOOP speed matching`.

## Reading the debug line

Throttled ~every 5th control cycle:
```
cl tgt=0.120 ema_L=0.098 ema_R=0.131 pwm_L=41 pwm_R=33 steer=0
```
| field | meaning | what you want |
|---|---|---|
| `tgt` | governed target speed (m/s), the setpoint | > 0 while driving |
| `ema_L` / `ema_R` | filtered measured speed of each rear wheel | **both converge to `tgt`** |
| `pwm_L` / `pwm_R` | commanded PWM per wheel | **differ** (that's the imbalance being corrected) |
| `steer` | AKM steering command units | ~0 on a straight goal |

- **Good (converged):** `ema_L ≈ ema_R ≈ tgt`, `pwm_L ≠ pwm_R` and steady. The
  slower wheel (lower slope) carries the higher PWM.
- **Under-corrected (still curves):** `ema_L` and `ema_R` stay apart, PWMs nearly
  equal → KP too low.
- **Oscillating (hunts):** `ema_L`/`ema_R` and PWMs swing above/below `tgt` cycle
  to cycle → KP too high or EMA too fast (alpha too high).
- **`NaN` before the first measured cycle** is normal (feedforward-only seed).

## Gain reference

| Env var | Default | Effect | Tune |
|---|---|---|---|
| `KIRRA_R2_SPEED_KP` | 20 | PWM per (m/s error). The main knob. | ↑ until wheels match; ↓ if it hunts |
| `KIRRA_R2_SPEED_MAX_PWM_STEP` | 5 | per-cycle slew cap | ↑ if correction is sluggish; ↓ if jerky |
| `KIRRA_R2_SPEED_EMA_ALPHA` | 0.4 | measured-speed smoothing (0,1]; 1=raw | ↓ to smooth a noisy `ema_*`; ↑ if lag hurts |
| `KIRRA_R2_SPEED_STALL_CYCLES` | 5 | under-response cycles → MRC fault | ↑ if false stalls on start; ↓ for faster fault |
| `KIRRA_R2_SPEED_STALL_MIN_PWM` | 10 | "commanding real effort" threshold | set just above the physical deadband PWM |
| `KIRRA_R2_SPEED_STALL_MIN_MPS` | 0.02 | "not moving" threshold | just above encoder noise at rest |

Change a value → `sudo systemctl restart kirra-consumer` (params are read at
startup) → re-observe. (A future slice could hot-reload; today it's a restart.)

## Procedure

### Step 1 — 🔴 WHEELS ELEVATED: converge the wheel speeds
Goal: both `ema_*` track `tgt` with no oscillation.
1. Enable closed-loop + debug (above), wheels up.
2. Drive a steady low goal (`bash robot/governed_drive_elevated.sh "go forward one meter"`
   — elevated it just spins the wheels) and watch `journalctl -u kirra-consumer -f | grep '^.*cl tgt'`.
3. Read `ema_L`/`ema_R`:
   - apart + equal PWMs → raise `KIRRA_R2_SPEED_KP` (try 30, 40), restart, re-check.
   - swinging → lower KP (try 12, 8) and/or lower `KIRRA_R2_SPEED_EMA_ALPHA` (0.3, 0.2).
4. Converged when `|ema_L − ema_R|` is small and steady and both ≈ `tgt`.

### Step 2 — floor: verify it drives straight
1. Wheels down, clear ~1.5 m straight lane, e-stop in hand, cables clear of the lidar.
2. `bash robot/governed_drive_elevated.sh "go forward one meter"`.
3. Measure **heading drift** over the run (tape a start line; eyeball or measure
   lateral deviation at 1 m):
   - drifts to one side → KP still too low (slower wheel not catching up) → ↑ KP.
   - snakes / weaves → KP too high or EMA too fast → ↓ KP / ↓ alpha.
4. Cross-check with odom: `journalctl -u kirra-consumer -f | grep 'odom raw'` — `x`
   should climb smoothly to the goal; large per-cycle jumps in the debug `ema_*`
   mean the EMA is too fast.

### Step 3 — lock the gains
Write the converged values into `robot/install/env.template` (and the deployed
`robot.env`), turn `KIRRA_R2_CLOSED_LOOP_DEBUG` off, restart, and re-run the floor
drive once to confirm it holds without the debug spam.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `wheel_stall_left/right` fault → MRC on start | stall thresholds too tight for the start transient | ↑ `KIRRA_R2_SPEED_STALL_CYCLES`, or ↑ `KIRRA_R2_SPEED_STALL_MIN_PWM` above the deadband |
| Still curves, PWMs ≈ equal | KP too low | ↑ `KIRRA_R2_SPEED_KP` |
| Weaves/hunts | KP too high or alpha too high | ↓ KP; ↓ `KIRRA_R2_SPEED_EMA_ALPHA` |
| `ema_*` very noisy | raw aliasing (auto-report not loop-locked) | ↓ alpha (more smoothing) |
| One wheel always pins to `pwm_max` | its slope const wrong | re-confirm `KIRRA_R2_V_PER_PWM` / `_RIGHT` |
| MRC on a legit slow crawl | `STALL_MIN_MPS` above the crawl speed | ↓ `KIRRA_R2_SPEED_STALL_MIN_MPS` |

## Safety notes
- Elevated first (Step 1) before any floor run. E-stop in hand on the floor.
- Closed-loop never relaxes the envelope: `translate` + the KIRRA checker bound
  the command first; closed-loop only redistributes the two drive PWMs toward the
  already-governed target. A non-finite feedback or a stalled wheel → MRC stop.
- The Nominal WCET-critical checker path is unchanged (closed-loop is doer-side,
  after verify).

## References
- `robot/r2_drive.py` — `ClosedLoopSpeedMatcher`, `speed_match_step`, `SpeedMatchParams`.
- `robot/r2_drive_calibration_results.txt` — the measured slopes + the RR confirm.
- `docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md` §9 — the closed-loop proposal.
