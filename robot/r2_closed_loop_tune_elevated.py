#!/usr/bin/env python3
"""r2_closed_loop_tune_elevated.py — bench-tune + validate the R2 §9 closed-loop
per-wheel speed matcher against the REAL motors + encoders, ELEVATED.

🔴🔴🔴 RUN ELEVATED — ALL WHEELS OFF THE GROUND, E-STOP IN HAND. 🔴🔴🔴

WHY THIS EXISTS. The closed-loop controller (`r2_drive.ClosedLoopSpeedMatcher`)
is host-tested for correctness + fail-closed behaviour, but its GAINS
(KIRRA_R2_SPEED_*) are conservative PLACEHOLDERS — they must be tuned against the
real drivetrain, and the no-runaway / stall→MRC behaviour must be seen on
hardware, BEFORE the loop is ever wired into the actuation consumer. This script
drives the matcher directly (NO governed consumer, NO ROS) so you can:
  1. watch both wheels converge to the SAME target speed (tune KP / slew),
  2. confirm PWM stays BOUNDED (no runaway) as it converges,
  3. prove a STALLED wheel (grab/hold one) trips the MRC fault → motors stop.

This is the closed-loop analogue of r2_drive_calibration_elevated.py: it opens
the board directly, so the KIRRA consumer must be STOPPED first (it owns
/dev/myserial). It writes NOTHING into the consumer and makes no safety claim; it
is a tuning bench. Steering is not used (straight-line tuning, wheels up).

SOLE-WRITER: stop the consumer first, else the port open fails "device busy"
(the intended interlock):
    (stop the kirra consumer)
    set -a; source /etc/kirra/robot.env; set +a
    export KIRRA_R2_M_PER_TICK=0.00025101 KIRRA_R2_V_PER_PWM_RIGHT=0.0194
    python3 robot/r2_closed_loop_tune_elevated.py

Knobs (env; the matcher gains are the ones you are here to tune):
    KIRRA_TUNE_TARGET_MPS   target ground speed for the run (default 0.20; HARD-
                            capped at 0.30 — this is a low-speed elevated bench).
    KIRRA_TUNE_SECONDS      run-window length per pass (default 4.0; capped 10).
    KIRRA_TUNE_RATE_HZ      control rate (default 10).
    KIRRA_R2_SPEED_KP, KIRRA_R2_SPEED_MAX_PWM_STEP, KIRRA_R2_SPEED_STALL_* —
                            the controller gains (see r2_drive.py / env.template).
"""

import math
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from r2_drive import (  # noqa: E402
    ClosedLoopSpeedMatcher,
    R2CalibrationError,
    calibration_from_env,
    speed_match_params_from_env,
)

TARGET_HARD_CAP_MPS = 0.30  # refuse a higher elevated-bench target (fat-finger guard)
SECONDS_HARD_CAP = 10.0


def _confirm(q: str) -> bool:
    return input(q + " [y/N] ").strip().lower() == "y"


def _env_float(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if raw is None or raw.strip() == "":
        return default
    try:
        v = float(raw)
    except ValueError:
        sys.exit(f"{name}: '{raw}' is not a number.")
    if not math.isfinite(v):
        sys.exit(f"{name}: must be finite.")
    return v


def _run_pass(bot, matcher, params, target_v: float, seconds: float, rate_hz: float) -> str:
    """Drive the closed loop for one window. Returns a short outcome tag.

    Prints per-cycle: measured L/R ground speed vs target and the commanded PWMs,
    so the operator can judge convergence + oscillation and watch for a runaway
    (there must be none — PWM is capped + slew-limited). A matcher fault (stall /
    non-finite) stops the motors immediately and ends the pass.
    """
    period = 1.0 / rate_hz
    m_per_tick = params.m_per_tick
    prev = None  # (enc_left, enc_right, t) for the RAW display speed
    print(f"\n  target={target_v:.3f} m/s  for {seconds:.1f}s @ {rate_hz:.0f}Hz  "
          f"(KP={params.kp_pwm_per_mps}, slew={params.max_pwm_step}, "
          f"ema={params.ema_alpha}, pwm_max={params.pwm_max})")
    print("  filt_* = the EMA-filtered speed the controller ACTS on; raw_* = unfiltered.")
    print("   t(s)   filt_L   filt_R   raw_L   raw_R   pwm_L  pwm_R   note")
    t0 = time.monotonic()
    matcher.reset()
    try:
        while True:
            now = time.monotonic()
            elapsed = now - t0
            if elapsed >= seconds:
                break
            enc = bot.get_motor_encoder()  # [m1(RL), m2, m3, m4(RR)]
            pwm_left, pwm_right, fault = matcher.step(target_v, enc[0], enc[3], now)
            if fault is not None:
                bot.set_motor(0, 0, 0, 0)
                print(f"  {elapsed:6.2f}   -----    -----    -----   -----     0      0   FAULT: {fault} → STOPPED")
                return f"fault:{fault}"
            bot.set_motor(pwm_left, 0, 0, pwm_right)
            # filt_* is the matcher's own EMA (what the P-term saw); raw_* is our
            # independent unfiltered delta — the gap shows the aliasing the filter removes.
            if prev is not None:
                dt = now - prev[2]
                rvl = (enc[0] - prev[0]) * m_per_tick / dt if dt > 0 else 0.0
                rvr = (enc[3] - prev[1]) * m_per_tick / dt if dt > 0 else 0.0
                fvl = matcher._ema_left if matcher._ema_left is not None else rvl
                fvr = matcher._ema_right if matcher._ema_right is not None else rvr
                print(f"  {elapsed:6.2f}  {fvl:7.3f}  {fvr:7.3f}  {rvl:6.3f}  {rvr:6.3f}  "
                      f"{pwm_left:5d}  {pwm_right:5d}")
            prev = (enc[0], enc[3], now)
            # pace to the next tick
            sleep_left = period - (time.monotonic() - now)
            if sleep_left > 0:
                time.sleep(sleep_left)
    finally:
        bot.set_motor(0, 0, 0, 0)
    return "completed"


def main() -> int:
    try:
        from Rosmaster_Lib import Rosmaster
    except Exception as exc:  # noqa: BLE001
        sys.exit(f"Rosmaster_Lib not importable: {exc}")

    # Build the controller params from env, fail-closed (needs the measured R2
    # profile + KIRRA_R2_M_PER_TICK + KIRRA_R2_V_PER_PWM_RIGHT).
    try:
        cal = calibration_from_env(os.environ)
        params = speed_match_params_from_env(os.environ, cal)
    except R2CalibrationError as exc:
        sys.exit(f"closed-loop params incomplete: {exc}\n"
                 "(source /etc/kirra/robot.env and export KIRRA_R2_M_PER_TICK + "
                 "KIRRA_R2_V_PER_PWM_RIGHT — see env.template.)")

    target = _env_float("KIRRA_TUNE_TARGET_MPS", 0.20)
    if not (0.0 < target <= TARGET_HARD_CAP_MPS):
        sys.exit(f"KIRRA_TUNE_TARGET_MPS must be in (0, {TARGET_HARD_CAP_MPS}] — got {target}.")
    seconds = min(_env_float("KIRRA_TUNE_SECONDS", 4.0), SECONDS_HARD_CAP)
    rate_hz = _env_float("KIRRA_TUNE_RATE_HZ", 10.0)
    if not (rate_hz > 0.0):
        sys.exit("KIRRA_TUNE_RATE_HZ must be > 0.")
    port = os.environ.get("KIRRA_MOTOR_PORT", "/dev/myserial")

    print("=" * 66)
    print("  R2 CLOSED-LOOP TUNE — 🔴 WHEELS MUST BE OFF THE GROUND 🔴")
    print("=" * 66)
    if not _confirm("Robot ELEVATED, all wheels free to spin, e-stop in hand?"):
        sys.exit("Elevate the robot first. Aborting.")
    if not _confirm("Is the KIRRA consumer / vendor motor node STOPPED? (it owns /dev/myserial)"):
        sys.exit("Stop the consumer first. Aborting.")

    print(f"\nOpening motor board on {port} ...")
    try:
        bot = Rosmaster(com=port)
    except Exception as exc:  # noqa: BLE001
        sys.exit(f"Could not open {port}: {exc}\n(If 'device busy', the consumer still holds it — stop it.)")

    try:
        # Encoders only update from the MCU auto-report — REQUIRED or every read is 0.
        bot.create_receive_threading()
        time.sleep(0.1)
        bot.set_auto_report_state(True)
        time.sleep(0.2)
        bot.set_motor(0, 0, 0, 0)

        matcher = ClosedLoopSpeedMatcher(params)

        print("\n── PASS 1: convergence + no-runaway ──")
        print("  WATCH: both meas_L and meas_R should climb to ~target and hold; the")
        print("  PWMs must stay BOUNDED (no ramp to the cap). If either wheel races or")
        print("  the PWM pins to the cap and stays, Ctrl-C and lower KP / raise slew.")
        _confirm("Ready to drive (elevated)?") or sys.exit("Aborted before drive.")
        outcome = _run_pass(bot, matcher, params, target, seconds, rate_hz)
        print(f"  pass 1: {outcome}")
        conv = _confirm("Did BOTH wheels converge to ~target with BOUNDED PWM (no runaway)?")

        stall_seconds = min(max(seconds, 6.0), SECONDS_HARD_CAP)
        print("\n── PASS 2: stall → MRC fault ──")
        print("  As soon as it starts, FIRMLY grip ONE rear wheel (gloved) to a COMPLETE")
        print(f"  STOP and HOLD it there (~2 s) — you have {stall_seconds:.0f}s. That wheel's")
        print("  pwm will climb a little as the controller tries; that is expected + safe")
        print("  (it is capped). KEEP HOLDING until you see 'FAULT: wheel_stall_...' and")
        print("  BOTH motors stop. A light touch that only SLOWS the wheel will NOT trip")
        print("  it — its filt_ column must reach ~0 and stay there for ~1 s.")
        if _confirm("Ready to run the stall test (grip one wheel to a FULL stop)?"):
            outcome2 = _run_pass(bot, matcher, params, target, stall_seconds, rate_hz)
            print(f"  pass 2: {outcome2}")
            stalled = outcome2.startswith("fault:") and _confirm(
                "Did holding a wheel trip a FAULT and STOP both motors (no ramp-to-cap)?"
            )
        else:
            stalled = False
            print("  pass 2: SKIPPED")

        print("\n" + "=" * 66)
        if conv and stalled:
            print("  ✅ CLOSED-LOOP TUNE PASSED (elevated): converged + bounded + stall→MRC.")
            print("     Record the working KIRRA_R2_SPEED_* gains; they are the ones the")
            print("     consumer will use once wired (KIRRA_R2_CLOSED_LOOP=1). Next: the")
            print("     governed-consumer elevated run, then tethered floor.")
        else:
            print("  ⚠  NOT signed off. Adjust gains (KP / slew) or re-run the stall test;")
            print("     do NOT enable closed-loop on the consumer until BOTH pass.")
        print("=" * 66)
    except KeyboardInterrupt:
        try:
            bot.set_motor(0, 0, 0, 0)
        except Exception:  # noqa: BLE001
            pass
        print("\n\nInterrupted - motors stopped.")
    finally:
        try:
            bot.set_motor(0, 0, 0, 0)
        except Exception:  # noqa: BLE001
            pass
        print("\nMotors commanded to 0. Tune session complete.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
