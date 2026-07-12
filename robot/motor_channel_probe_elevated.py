#!/usr/bin/env python3
"""motor_channel_probe_elevated.py — Path-B Step 1: map set_motor channels → wheels.

🔴🔴🔴 RUN ELEVATED — ALL WHEELS OFF THE GROUND, E-STOP IN HAND. 🔴🔴🔴

Identifies which of the four ``Rosmaster.set_motor(s1, s2, s3, s4)`` channels
drive the R2's rear wheels, and the FORWARD-SIGN for each — a robot-specific
WIRING fact that is absent from all software. ``set_motor`` is car-type
independent: it bypasses the firmware kinematics mixer (packet
``[0xFF, 0xFC, len, 0x10, s1, s2, s3, s4, ck]``, no car-type byte — confirmed
byte-identical against the installed Rosmaster_Lib 3.3.9).

This ONLY identifies channels. It writes NO drive kinematics and does NOT touch
the KIRRA consumer. Report the printed table for review before any Ackermann
code is written.

Sole-writer discipline: the KIRRA consumer OWNS /dev/myserial exclusively
(kirra_motor_consumer.py). STOP it before running this — otherwise opening the
port here fails with "device busy", which is the intended fail-safe. Run:
    sudo systemctl stop kirra-consumer   # or kill the consumer node
    KIRRA_MOTOR_PORT=/dev/myserial python3 robot/motor_channel_probe_elevated.py

Safety model:
  * ONE channel at a time, low PWM (default +20, hard-capped at 40).
  * Explicit 0 on the three idle channels every pulse — NEVER the 127 "hold"
    sentinel (127 keeps the previous value instead of stopping).
  * Steering centered FIRST and watched: if the servo moves during a motor
    pulse, that channel is the servo — Ctrl-C immediately (it is a finding).
  * set_motor(0, 0, 0, 0) on EVERY exit path (finally + KeyboardInterrupt).
  * Encoders are read via the receive thread + auto-report (they only update
    from MCU report frames); without that they read a stale 0.
"""

import os
import sys
import time

PROBE_PWM = abs(int(os.environ.get("PROBE_PWM", "20")))  # positive magnitude; sign is recorded separately
PROBE_HOLD_S = 1.5
HARD_CAP = 40  # refuse any probe PWM above this — a fat-finger guard
ENC_MOVE_THRESHOLD = 5  # |encoder delta| above this = "this channel moved"


def ask(question):
    return input(question + " ").strip()


def confirm(question):
    return ask(question + " [y/N]").lower() == "y"


def main():
    if PROBE_PWM > HARD_CAP:
        sys.exit(f"PROBE_PWM={PROBE_PWM} exceeds the {HARD_CAP} probe cap — refusing.")

    try:
        from Rosmaster_Lib import Rosmaster
    except Exception as exc:  # noqa: BLE001 - report and abort, never proceed
        sys.exit(f"Rosmaster_Lib not importable: {exc}")

    port = os.environ.get("KIRRA_MOTOR_PORT", "/dev/myserial")

    print("=" * 66)
    print("  R2 MOTOR-CHANNEL PROBE — 🔴 WHEELS MUST BE OFF THE GROUND 🔴")
    print("=" * 66)
    if not confirm("Robot ELEVATED, all wheels free to spin, e-stop in hand?"):
        sys.exit("Elevate the robot first. Aborting.")
    if not confirm(
        "Is the KIRRA consumer / vendor motor node STOPPED? "
        "(it normally owns /dev/myserial exclusively; this probe needs that port)"
    ):
        sys.exit("Stop the consumer first — it normally owns /dev/myserial. Aborting.")

    print(f"\nOpening motor board on {port} ...")
    try:
        bot = Rosmaster(com=port)
    except Exception as exc:  # noqa: BLE001
        sys.exit(
            f"Could not open {port}: {exc}\n"
            "(If this is 'device busy', the consumer still holds the port — stop it.)"
        )

    results = []
    try:
        # Encoders only update from the MCU's auto-report frames — the receive
        # thread + auto-report are REQUIRED or every get_motor_encoder() is 0.
        bot.create_receive_threading()
        time.sleep(0.1)
        bot.set_auto_report_state(True)
        time.sleep(0.2)
        bot.set_motor(0, 0, 0, 0)

        # Steering safety: center it and confirm it holds before any wheel pulse.
        bot.set_akm_steering_angle(0)
        time.sleep(0.3)
        if not confirm("Steering centered? (it must STAY centered through every pulse)"):
            raise SystemExit("Steering not centered — aborting for safety.")

        print(
            f"\nProbing M1..M4 one at a time, +{PROBE_PWM} PWM for {PROBE_HOLD_S:.1f}s each."
        )
        print(
            "WATCH: if the SERVO moves, or TWO wheels move, or anything lurches → Ctrl-C now.\n"
        )

        for i in range(4):
            ch = i + 1
            if not confirm(f"-- Pulse channel M{ch} now?"):
                results.append((ch, "SKIPPED", "", None))
                continue

            enc_before = bot.get_motor_encoder()
            speeds = [0, 0, 0, 0]
            speeds[i] = PROBE_PWM
            bot.set_motor(*speeds)
            time.sleep(PROBE_HOLD_S)
            bot.set_motor(0, 0, 0, 0)
            time.sleep(0.3)  # let the final report frame land
            enc_after = bot.get_motor_encoder()

            deltas = [b - a for a, b in zip(enc_before, enc_after)]
            moved = [j + 1 for j, d in enumerate(deltas) if abs(d) > ENC_MOVE_THRESHOLD]
            print(f"   encoder delta (m1..m4): {deltas}   moved-index: {moved}")

            wheel = ask(f"   Which WHEEL turned for M{ch}? [FL/FR/RL/RR/servo/none]:").upper()
            direction = ""
            if wheel not in ("NONE", "SERVO", ""):
                direction = ask(f"   Direction at +{PROBE_PWM}? [forward/reverse]:").lower()
            results.append((ch, wheel, direction, deltas))

        _print_summary(results)
    except KeyboardInterrupt:
        print("\n\n⚠ Interrupted — stopping all motors.")
    finally:
        try:
            bot.set_motor(0, 0, 0, 0)
        except Exception:  # noqa: BLE001 - best-effort stop
            pass
        print("\nAll channels commanded to 0. Probe complete.")


def _print_summary(results):
    print("\n" + "=" * 66)
    print("  RESULT — channel → wheel → forward-sign → encoder delta")
    print("=" * 66)
    rear = []
    for ch, wheel, direction, deltas in results:
        fwd_sign = ""
        if direction == "forward":
            fwd_sign = f"+{PROBE_PWM}"
        elif direction == "reverse":
            fwd_sign = f"-{PROBE_PWM}"
        enc = "" if deltas is None else str(deltas)
        print(f"  M{ch}: wheel={wheel or '?':6} forward_sign={fwd_sign or '-':5} encΔ={enc}")
        if wheel in ("RL", "RR"):
            rear.append((ch, wheel, fwd_sign))
    print("-" * 66)
    if rear:
        print("  REAR-DRIVE CHANNELS (for the Ackermann drive):")
        for ch, wheel, fwd_sign in rear:
            print(f"    M{ch} = {wheel}, forward = set_motor value {fwd_sign or '(RECORD SIGN!)'}")
    else:
        print("  No RL/RR recorded — re-check wheel labels before trusting this run.")
    print(
        "\n  Confirm the steering stayed centered / is NOT a motor channel.\n"
        "  SAVE THIS OUTPUT and report it for review before any drive code."
    )


if __name__ == "__main__":
    main()
