#!/usr/bin/env python3
"""steering_cartype5_check.py — confirm the R2 AKM servo actuates only under car-type 5.

🔴 RUN ELEVATED — wheels off the ground, e-stop in hand. 🔴

Path-B bench finding (PR #913 follow-up): set_akm_steering_angle drives the
steering servo ONLY when the board is in car-type 5. The default cross-labeled X3
image (car-type 1) silently ignores it — which is why the first drive-calibration
run saw no servo motion. set_motor drive is car-type INDEPENDENT, so Path B sets
car-type 5 for the servo and still drives via set_motor. (Car-type 5 breaks
set_car_motion — which Path B never uses.)

set_car_type(5) is RAM-volatile (reverts on reboot). The currently-live consumer
still uses set_car_motion, so this restores car-type 1 on exit to avoid leaving
the live drive path broken.

Sole-writer discipline: STOP the KIRRA consumer first (it owns /dev/myserial):
    sudo systemctl stop kirra-consumer   # or kill the consumer node
    KIRRA_MOTOR_PORT=/dev/myserial python3 robot/steering_cartype5_check.py

This ONLY confirms the servo mechanism + sign. It writes no kinematics and does
not touch the consumer. Observe: do the front wheels swing, and does a NEGATIVE
command steer LEFT?
"""

import os
import sys
import time


def confirm(q):
    return input(q + " [y/N] ").strip().lower() == "y"


def main():
    try:
        from Rosmaster_Lib import Rosmaster
    except Exception as exc:  # noqa: BLE001 - report and abort, never proceed
        sys.exit(f"Rosmaster_Lib not importable: {exc}")

    port = os.environ.get("KIRRA_MOTOR_PORT", "/dev/myserial")
    if not confirm("Robot ELEVATED, wheels free, e-stop in hand?"):
        sys.exit("Elevate first.")
    if not confirm("KIRRA consumer STOPPED (it owns /dev/myserial)?"):
        sys.exit("Stop the consumer first.")

    try:
        bot = Rosmaster(com=port)
    except Exception as exc:  # noqa: BLE001
        sys.exit(f"Could not open {port}: {exc} (device busy = consumer still holds it)")

    try:
        # -1 here is the unimplemented-getter sentinel on this image, NOT an
        # AKM-inactive flag — the servo actuates under type 5 regardless.
        print("AKM default angle BEFORE set_car_type(5):", bot.get_akm_default_angle())
        bot.set_car_type(5)          # 5 = R2 / Ackermann — enables the AKM servo path
        time.sleep(0.3)
        print("AKM default angle AFTER  set_car_type(5):", bot.get_akm_default_angle())
        bot.set_akm_steering_angle(0)
        time.sleep(0.6)
        for cmd in (-30, 0, 30, 0, -45, 0, 45, 0):
            print(f"  set_akm_steering_angle({cmd:+d}) -- WATCH the front wheels")
            bot.set_akm_steering_angle(cmd)
            time.sleep(1.3)
    finally:
        try:
            bot.set_akm_steering_angle(0)
        except Exception:  # noqa: BLE001 - best-effort centre
            pass
        try:
            bot.set_car_type(1)      # restore X3/type-1 so set_car_motion drive still works
        except Exception:  # noqa: BLE001 - best-effort restore (reboot also restores it)
            pass
        print("\nSteering centred, car-type restored to 1 (reboot also restores it).")


if __name__ == "__main__":
    main()
