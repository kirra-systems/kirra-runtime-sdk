#!/usr/bin/env python3
"""r2_drive_calibration_elevated.py — Path-B Step 2: measure the §5 calibration gaps.

🔴🔴🔴 RUN ELEVATED — ALL WHEELS OFF THE GROUND, E-STOP IN HAND. 🔴🔴🔴

Captures — never invents — the bench measurements that stand between the
Path-B Ackermann drive proposal (docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md §5)
and a correct physical drive. It writes NO drive kinematics, wires NOTHING into
the KIRRA consumer, and makes no safety claim. Its only output is a recorded
table for human review, saved to ``r2_drive_calibration_results.txt``.

The confirmed channel map (robot/motor_channel_probe_results.txt, PR #911) is the
sole input assumption:
    set_motor(s1, s2, s3, s4)  →  s1 = M1 = REAR-LEFT (+fwd), s4 = M4 = REAR-RIGHT (+fwd)
    steering on the AKM path (set_akm_steering_angle, FUNC 0x31), NOT a motor channel.

Four phases (each optional; skip any you cannot do this bench):
  A  PWM ↔ encoder-speed sweep    — both rear wheels together up a low PWM ladder;
                                     ticks/s per wheel (also re-confirms L/R balance).
  B  encoder scale                — hand-rotate ONE rear wheel a known number of
                                     turns → ticks/rev; with the measured wheel
                                     diameter this converts ticks/s → m/s.
  C  steering command ↔ road-wheel angle — sweep set_akm_steering_angle over
                                     [-45..+45]; operator reads the physical
                                     road-wheel angle per command → K, linearity,
                                     δ_max, sign check; records the live centre.
  D  geometry                      — tape-measured wheelbase L (re-confirm ~0.229 m)
                                     and rear track t.

Sole-writer discipline: the KIRRA consumer OWNS /dev/myserial exclusively
(kirra_motor_consumer.py). STOP it before running this — otherwise opening the
port here fails with "device busy", which is the intended fail-safe:
    sudo systemctl stop kirra-consumer   # or kill the consumer node
    KIRRA_MOTOR_PORT=/dev/myserial python3 robot/r2_drive_calibration_elevated.py

Safety model (identical discipline to the probe):
  * Elevated + consumer-stopped are CONFIRMED before the port opens.
  * Drive phase A uses low PWM only (default ladder 15..40, hard-capped at 40).
  * Steering is centred FIRST and returned to centre after phase C.
  * set_motor(0,0,0,0) + set_akm_steering_angle(0) on EVERY exit path
    (finally + KeyboardInterrupt).
  * Encoders are read via the receive thread + auto-report (they only update from
    MCU report frames); without that they read a stale 0.
  * The drive sweep holds each rung briefly and stops between rungs — no sustained
    run, no ramp.
"""

import os
import sys
import time

# --- hard caps / knobs (env-overridable, but the cap is never exceeded) -------
HARD_CAP = 40  # refuse any drive PWM above this — a fat-finger guard
DRIVE_HOLD_S = 1.5  # per-rung hold for the PWM↔speed sweep
DRIVE_SETTLE_S = 0.3  # let the final report frame land after stopping
# default low ladder; each rung drives BOTH rear wheels (M1 & M4) together
DEFAULT_LADDER = "15,20,25,30,35,40"
# steering commands to sweep in phase C (command units, negative = left)
DEFAULT_STEER_CMDS = "-45,-30,-15,0,15,30,45"

RESULTS_PATH = os.environ.get(
    "CALIB_RESULTS_PATH",
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "r2_drive_calibration_results.txt"),
)


def ask(question):
    return input(question + " ").strip()


def confirm(question):
    return ask(question + " [y/N]").lower() == "y"


def ask_float(question):
    """Prompt until a finite float is given, or blank to skip (returns None)."""
    while True:
        raw = ask(question + " (blank to skip)")
        if raw == "":
            return None
        try:
            val = float(raw)
        except ValueError:
            print("   not a number — try again.")
            continue
        # reject NaN/inf explicitly (val != val is the NaN test)
        if val != val or val in (float("inf"), float("-inf")):
            print("   non-finite — try again.")
            continue
        return val


def _parse_int_list(raw, name):
    out = []
    for tok in raw.split(","):
        tok = tok.strip()
        if tok == "":
            continue
        try:
            out.append(int(tok))
        except ValueError:
            sys.exit(f"{name}: '{tok}' is not an integer.")
    return out


# --- phase A: PWM ↔ encoder-speed sweep ---------------------------------------
def phase_a(bot, lines):
    print("\n" + "=" * 66)
    print("  PHASE A — PWM ↔ encoder-speed (both rear wheels together)")
    print("=" * 66)
    if not confirm("Run phase A (low-PWM drive sweep)?"):
        lines.append("PHASE A (PWM↔speed): SKIPPED")
        return

    ladder = _parse_int_list(os.environ.get("CALIB_LADDER", DEFAULT_LADDER), "CALIB_LADDER")
    over = [p for p in ladder if abs(p) > HARD_CAP]
    if over:
        sys.exit(f"ladder rung(s) {over} exceed the {HARD_CAP} PWM cap — refusing.")
    if not ladder:
        lines.append("PHASE A (PWM↔speed): SKIPPED (empty ladder)")
        return

    print(f"  Ladder: {ladder} PWM, {DRIVE_HOLD_S:.1f}s per rung, both M1 & M4 = +PWM.")
    print("  WATCH: wheels must spin FREELY; anything lurches or the servo moves → Ctrl-C.\n")
    lines.append("PHASE A (PWM↔speed): both rear wheels driven together (M1=RL, M4=RR = +PWM)")
    lines.append(f"  hold_per_rung_s = {DRIVE_HOLD_S}")
    lines.append("  pwm    d_m1(ticks)  d_m4(ticks)  ticks_per_s_m1  ticks_per_s_m4  L/R_ratio")

    for pwm in ladder:
        if not confirm(f"-- Drive both rear wheels at +{pwm} PWM?"):
            lines.append(f"  {pwm:<5}  SKIPPED")
            continue
        enc_before = bot.get_motor_encoder()
        t0 = time.monotonic()
        bot.set_motor(pwm, 0, 0, pwm)  # M1 & M4 only; M2/M3 unpopulated
        time.sleep(DRIVE_HOLD_S)
        bot.set_motor(0, 0, 0, 0)
        dt = time.monotonic() - t0
        time.sleep(DRIVE_SETTLE_S)
        enc_after = bot.get_motor_encoder()

        d = [b - a for a, b in zip(enc_before, enc_after)]
        d_m1, d_m4 = d[0], d[3]
        tps_m1 = d_m1 / dt if dt > 0 else 0.0
        tps_m4 = d_m4 / dt if dt > 0 else 0.0
        ratio = (d_m1 / d_m4) if d_m4 != 0 else float("nan")
        print(f"   Δ m1..m4 = {d}   dt={dt:.3f}s   ticks/s: m1={tps_m1:.1f} m4={tps_m4:.1f}")
        lines.append(
            f"  {pwm:<5}  {d_m1:<11} {d_m4:<11} {tps_m1:<15.1f} {tps_m4:<15.1f} {ratio:.3f}"
        )

    lines.append("  NOTE: set_motor bypasses the firmware speed PID — expect open-loop scatter.")
    lines.append("        L/R_ratio != 1.0 quantifies the straight-line drift at equal PWM.")


# --- phase B: encoder scale (ticks/rev) ---------------------------------------
def phase_b(bot, lines):
    print("\n" + "=" * 66)
    print("  PHASE B — encoder scale (hand-rotate one rear wheel)")
    print("=" * 66)
    if not confirm("Run phase B (encoder ticks/rev)?"):
        lines.append("PHASE B (encoder scale): SKIPPED")
        return

    which = ask("  Rotate which rear wheel by hand? [RL/RR]:").upper()
    idx = 0 if which == "RL" else 3 if which == "RR" else None
    if idx is None:
        print("  unrecognised wheel — skipping phase B.")
        lines.append("PHASE B (encoder scale): SKIPPED (bad wheel label)")
        return
    turns = ask_float(f"  How many full turns will you rotate {which}?")
    if turns is None or turns == 0:
        lines.append("PHASE B (encoder scale): SKIPPED (no turn count)")
        return

    input(f"  Ready — press ENTER, then slowly rotate {which} exactly {turns} full turns, then ENTER again.")
    enc_before = bot.get_motor_encoder()
    input("  Rotating... press ENTER when the turns are complete.")
    enc_after = bot.get_motor_encoder()
    d = [b - a for a, b in zip(enc_before, enc_after)]
    d_wheel = d[idx]
    ticks_per_rev = d_wheel / turns if turns else float("nan")
    diam = ask_float("  Measured wheel diameter (m)?")
    print(f"   {which}: Δticks={d_wheel} over {turns} turns → {ticks_per_rev:.1f} ticks/rev")

    lines.append(f"PHASE B (encoder scale): wheel={which}")
    lines.append(f"  full_turns          = {turns}")
    lines.append(f"  delta_ticks_all     = {d}")
    lines.append(f"  ticks_per_rev       = {ticks_per_rev:.3f}")
    if diam is not None:
        circ = 3.141592653589793 * diam
        m_per_tick = circ / ticks_per_rev if ticks_per_rev else float("nan")
        lines.append(f"  wheel_diameter_m    = {diam}")
        lines.append(f"  wheel_circumf_m     = {circ:.5f}")
        lines.append(f"  m_per_tick          = {m_per_tick:.8f}")
        lines.append("  => combine with PHASE A ticks/s to get m/s per PWM (the V_PER_PWM map).")
    else:
        lines.append("  wheel_diameter_m    = (not measured — m/s conversion pending)")


# --- phase C: steering command ↔ road-wheel angle -----------------------------
def phase_c(bot, lines):
    print("\n" + "=" * 66)
    print("  PHASE C — steering command ↔ road-wheel angle")
    print("=" * 66)
    if not confirm("Run phase C (steering sweep)?"):
        lines.append("PHASE C (steering↔angle): SKIPPED")
        return

    # Record the live centre default (the physical straight-ahead trim).
    live_center = None
    try:
        live_center = bot.get_akm_default_angle()
    except Exception as exc:  # noqa: BLE001 - not fatal; just record that we couldn't
        print(f"   (get_akm_default_angle unavailable: {exc})")
    lines.append("PHASE C (steering↔angle): set_akm_steering_angle sweep, command units [-45,+45], neg=left")
    lines.append(f"  live_default_angle (get_akm_default_angle) = {live_center}")
    print(f"   live steering default (centre trim) = {live_center}")

    cmds = _parse_int_list(os.environ.get("CALIB_STEER_CMDS", DEFAULT_STEER_CMDS), "CALIB_STEER_CMDS")
    over = [c for c in cmds if abs(c) > 45]
    if over:
        sys.exit(f"steering command(s) {over} exceed the [-45,+45] envelope — refusing.")

    print("  For each command the servo will move; measure the ROAD-WHEEL angle with a")
    print("  protractor (degrees; LEFT of straight = negative, to match the command sign).")
    lines.append("  command_units   measured_road_wheel_deg (neg=left)   note")

    for cmd in cmds:
        if not confirm(f"-- Steer to command {cmd:+d}?"):
            lines.append(f"  {cmd:<+8}  SKIPPED")
            continue
        bot.set_akm_steering_angle(cmd)
        time.sleep(0.6)
        deg = ask_float(f"   Measured road-wheel angle at command {cmd:+d} (deg, neg=left)?")
        note = ask("   note? (e.g. 'full lock', 'binding') [blank ok]:")
        deg_s = "(skipped)" if deg is None else f"{deg:+.2f}"
        lines.append(f"  {cmd:<+8}  {deg_s:<34} {note}")

    # Return to centre and confirm before leaving the phase.
    bot.set_akm_steering_angle(0)
    time.sleep(0.4)
    lines.append("  SIGN CHECK: confirm a NEGATIVE command physically steered LEFT (records the")
    lines.append("              steer_cmd = -K*delta sign in the proposal §4).")
    lines.append("  δ_max = the largest |road-wheel angle| reached before full lock / binding.")
    lines.append("  K     = fit command-units per RADIAN from the linear region of the sweep.")


# --- phase D: geometry --------------------------------------------------------
def phase_d(lines):
    print("\n" + "=" * 66)
    print("  PHASE D — geometry (tape measure; no actuation)")
    print("=" * 66)
    if not confirm("Run phase D (wheelbase + track)?"):
        lines.append("PHASE D (geometry): SKIPPED")
        return
    L = ask_float("  Wheelbase L, front axle → rear axle (m)? (~0.229 expected)")
    t = ask_float("  Rear track t, RL contact → RR contact (m)?")
    lines.append("PHASE D (geometry):")
    lines.append(f"  wheelbase_L_m = {L if L is not None else '(not measured)'}")
    lines.append(f"  rear_track_t_m = {t if t is not None else '(not measured — only needed for Ackermann differential)'}")


def _write_results(lines):
    header = [
        "R2 DRIVE CALIBRATION RESULTS — Path-B Step 2 (measured, not invented)",
        "=" * 70,
        "",
        "Provenance: robot/r2_drive_calibration_elevated.py, run ELEVATED.",
        "Channel map assumed (robot/motor_channel_probe_results.txt, PR #911):",
        "  set_motor s1=M1=REAR-LEFT(+fwd), s4=M4=REAR-RIGHT(+fwd); steering on AKM path.",
        "These feed the §5 gaps in docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md.",
        "",
    ]
    with open(RESULTS_PATH, "w") as fh:
        fh.write("\n".join(header + lines) + "\n")
    print(f"\nResults written to {RESULTS_PATH}")
    print("Review them, then commit for human review (do NOT wire the drive yet).")


def main():
    try:
        from Rosmaster_Lib import Rosmaster
    except Exception as exc:  # noqa: BLE001 - report and abort, never proceed
        sys.exit(f"Rosmaster_Lib not importable: {exc}")

    port = os.environ.get("KIRRA_MOTOR_PORT", "/dev/myserial")

    print("=" * 66)
    print("  R2 DRIVE CALIBRATION — 🔴 WHEELS MUST BE OFF THE GROUND 🔴")
    print("=" * 66)
    if not confirm("Robot ELEVATED, all wheels free to spin, e-stop in hand?"):
        sys.exit("Elevate the robot first. Aborting.")
    if not confirm(
        "Is the KIRRA consumer / vendor motor node STOPPED? "
        "(it normally owns /dev/myserial exclusively; this tool needs that port)"
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

    lines = []
    try:
        # Encoders only update from the MCU's auto-report frames — the receive
        # thread + auto-report are REQUIRED or every get_motor_encoder() is 0.
        bot.create_receive_threading()
        time.sleep(0.1)
        bot.set_auto_report_state(True)
        time.sleep(0.2)
        bot.set_motor(0, 0, 0, 0)

        # Steering centred first and confirmed before any drive.
        bot.set_akm_steering_angle(0)
        time.sleep(0.3)
        if not confirm("Steering centred? (it must STAY centred through the drive phases)"):
            raise SystemExit("Steering not centred — aborting for safety.")

        phase_a(bot, lines)
        phase_b(bot, lines)
        phase_c(bot, lines)
        phase_d(lines)

        _write_results(lines)
    except KeyboardInterrupt:
        print("\n\n⚠ Interrupted — stopping all motors and centring steering.")
        if lines:
            _write_results(lines)  # persist whatever was captured before the interrupt
    finally:
        try:
            bot.set_motor(0, 0, 0, 0)
        except Exception:  # noqa: BLE001 - best-effort stop
            pass
        try:
            bot.set_akm_steering_angle(0)
        except Exception:  # noqa: BLE001 - best-effort centre
            pass
        print("\nAll channels commanded to 0, steering centred. Calibration complete.")


if __name__ == "__main__":
    main()
