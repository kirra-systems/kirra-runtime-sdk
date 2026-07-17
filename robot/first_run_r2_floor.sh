#!/usr/bin/env bash
# first_run_r2_floor.sh — the R2 Path-B (Ackermann) acceptance: re-validate the
# governed consumer ELEVATED in r2 mode, THEN drive the tethered FLOOR.
#
# This is the doc's §8 step-2 gate (docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md):
# "a first_run_elevated.sh-style guided acceptance, adapted for R2." The §8.1
# elevated acceptance already passed against the bench calibration script
# (r2_drive_calibration_elevated.py). This script is different and stricter: it
# drives the r2 last-hop through the REAL governed consumer — the Ed25519
# verify-before-release chokepoint (ADR-0033) is in the loop, exactly as it will
# be in production. Passing the bench script is NOT this.
#
# 🔴🔴🔴 STAGE A RUNS ELEVATED — WHEELS OFF THE GROUND. 🔴🔴🔴
# Only after Stage A passes does Stage B put the robot on the floor, TETHERED,
# at low speed, with the E-STOP IN HAND.
#
# WHY re-validate elevated even though §8.1 passed: §8.1 proved the geometry via
# the bench script; it did NOT exercise the governed consumer's r2 wiring
# (car-type-5 init + readback, the verify-gated set_motor/AKM last hop, r2
# safe_stop). A wiring/clamp bug here would first show as a spinning wheel in the
# air, not a robot lunging off a tether.
#
# ── Stages ──────────────────────────────────────────────────────────────────
#   STAGE A (ELEVATED, wheels up) — the governed consumer in r2 mode:
#     A1 VALID governed command → wheels drive at the CLAMPED demo speed, front
#        wheels centred (straight); a governed gentle turn steers the correct way
#     A2 UNSIGNED command       → ZERO motion + loud REFUSED in the consumer log
#     A3 kill the consumer      → wheels STOP + steering centres (SS-002)
#   STAGE B (FLOOR, tethered, e-stop in hand) — the same proofs on the ground:
#     B1 VALID governed          → gentle straight creep (tether short, hand on e-stop)
#     B2 governed gentle turn    → steers the correct way while creeping
#     B3 UNSIGNED                → STILL on the floor
#     B4 kill the consumer       → STOPS on the floor
#
# This is a GUIDED script: it drives the publisher and prompts YOU to observe.
# It cannot see the wheels — you are the acceptance sensor. A "no" fails the run.
#
# ── Prereqs (the CONSUMER runs in ANOTHER terminal, in r2 mode) ───────────────
#   1. Build the mint binary (the dev governor stand-in the publisher shells to):
#        cargo build -p kirra-release-token --bin kirra_ros_release_mint --release
#   2. The verify-core cdylib is built/installed (libkirra_consumer_ffi.so;
#        KIRRA_CONSUMER_LIB in the env, or discoverable in target/).
#   3. Nothing else holds /dev/myserial (no vendor yahboomcar_bringup / cmd_vel
#        motor node — the consumer must be the sole writer).
#   4. ROS 2 sourced; ROS_DOMAIN_ID=28 in BOTH terminals.
#   5. Start the consumer with the R2 env exported. The measured profile lives in
#        robot/install/env.template; the r2-mode set is:
#          set -a; source /etc/kirra/robot.env; set +a   # (or export by hand)
#          export KIRRA_DRIVE_MODE=r2_ackermann          # ← the Path-B selector
#          # KIRRA_R2_* (wheelbase/v_per_pwm/pwm_max/steer_units_per_rad/
#          #   delta_max/steer_sign/center_trim/deadband) — the MEASURED profile
#          # KIRRA_GOVERNOR_VK_HEX pinned to THIS publisher's key (dev seed below)
#          # KIRRA_MOTOR_PORT=/dev/myserial ; ADR-0033 timing ; KIRRA_DEMO_VX/VZ_MAX
#        In r2 mode KIRRA_EXPECTED_CAR_TYPE is IGNORED — the consumer sets
#        car-type 5 itself and refuses to start unless the board reads it back.
#        python3 robot/kirra_motor_consumer.py
#      Confirm its startup log shows: car-type 5 set + read back, centre trim
#      applied, "drive_mode=r2_ackermann". If it aborted (bad KIRRA_R2_* / no
#      car-type-5 readback), fix that BEFORE running this script.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PUB="python3 ${HERE}/kirra_release_publisher.py"

confirm() {  # confirm "question" -> exits non-zero on anything but y/Y
  read -r -p "$1 [y/N] " ans
  case "$ans" in
    y|Y) return 0 ;;
    *) echo "✗ operator reported failure — R2 FLOOR-RUN TEST FAILED"; exit 1 ;;
  esac
}

# A governed gentle LEFT turn: same clamped linear, a small +angular. The r2
# last-hop turns this into a centred-plus-steer command; watch the FRONT wheels.
turn_left() { KIRRA_PUB_ANGULAR=0.3 timeout "${1}s" ${PUB} --valid || true; }

echo "=================================================================="
echo "  KIRRA R2 PATH-B ACCEPTANCE — governed consumer, r2_ackermann"
echo "=================================================================="
echo "This runs in TWO stages: A) ELEVATED re-validation, then B) FLOOR."
echo "Do NOT skip to the floor. Stage A must pass first."
echo

# ── Stage A pre-flight ───────────────────────────────────────────────────────
read -r -p "STAGE A: is the robot ELEVATED with all wheels free to spin? [y/N] " ans
[ "$ans" = "y" ] || [ "$ans" = "Y" ] || { echo "Elevate the robot first. Aborting."; exit 1; }
confirm "Is the consumer up in r2 mode (log shows car-type 5 read back + drive_mode=r2_ackermann)?"
if command -v ros2 >/dev/null 2>&1; then
  echo "  active nodes:"; ros2 node list 2>/dev/null | sed 's/^/    /' || true
fi
confirm "Is every vendor /cmd_vel motor node ABSENT (consumer is the sole /dev/myserial writer)?"

# ── A1: valid governed command → clamped straight + a governed turn ──────────
echo
echo "── A1: VALID governed command (expect drive at the CLAMPED demo speed) ──"
echo "Publishing valid STRAIGHT frames (linear only) for 5 s..."
timeout 5s ${PUB} --valid || true
confirm "Did BOTH rear wheels drive the SAME direction, front wheels CENTRED (straight)?"
echo "Now a governed gentle LEFT turn for 5 s (watch the FRONT wheels)..."
turn_left 5
confirm "Did the front wheels steer LEFT while driving (correct sign, governed magnitude)?"

# ── A2: unsigned command → no motion + loud refusal ──────────────────────────
echo
echo "── A2: UNSIGNED command (expect ZERO motion + REFUSED in the consumer log) ──"
echo "Publishing UNSIGNED frames for 5 s... watch the consumer terminal for 'REFUSED'."
timeout 5s ${PUB} --unsigned || true
confirm "Did the wheels stay COMPLETELY STILL (no drive, no steer)?"
confirm "Did the consumer log 'REFUSED' (NO_TOKEN / bad-signature — no motor write)?"

# ── A3: kill the consumer → wheels stop + steering centres ───────────────────
echo
echo "── A3: consumer death → STOP + centre (SS-002) ──"
echo "Re-establish motion so a stop is observable: publishing valid frames for 3 s..."
timeout 3s ${PUB} --valid || true
echo
echo "NOW: in the consumer terminal, press Ctrl-C (or kill the consumer process)."
confirm "After killing the consumer, did the wheels STOP and the steering CENTRE?"

echo
echo "  ✅ STAGE A PASSED (elevated). The governed consumer's r2 last-hop is"
echo "     verify-gated, correctly signed, and fails closed. Restart the consumer"
echo "     (r2 mode) for Stage B."

# ── Stage B pre-flight ───────────────────────────────────────────────────────
echo
echo "=================================================================="
echo "  STAGE B — 🔴 FLOOR, TETHERED, E-STOP IN HAND 🔴"
echo "=================================================================="
echo "Lower the robot onto a clear, flat floor. Keep a SHORT tether/lead on it."
echo "Keep the physical E-STOP in your hand — it removes power-stage authority"
echo "independently of the software (it is the real backstop, not this script)."
echo "Clear ~3 m of straight run ahead. Low speed only (demo cap governs, but be"
echo "ready to e-stop / kill the consumer at any twitch)."
read -r -p "Is the robot on the floor, TETHERED, with the E-STOP in your hand? [y/N] " ans
[ "$ans" = "y" ] || [ "$ans" = "Y" ] || { echo "Set up the tether + e-stop first. Aborting."; exit 1; }
confirm "Is the consumer back up in r2 mode (car-type 5 read back, drive_mode=r2_ackermann)?"

# ── B1: valid governed → gentle straight creep ───────────────────────────────
echo
echo "── B1: VALID governed → gentle straight creep ──"
echo "Publishing valid STRAIGHT frames for 3 s. Expect a slow, straight creep."
echo "Hand on the e-stop. If it veers or accelerates, E-STOP now."
timeout 3s ${PUB} --valid || true
confirm "Did it creep STRAIGHT and slowly (no veer, no lunge), tracking the tether?"

# ── B2: governed gentle turn on the floor ────────────────────────────────────
echo
echo "── B2: governed gentle LEFT turn on the floor ──"
echo "Publishing a governed left turn for 3 s. Expect a gentle left arc at low speed."
turn_left 3
confirm "Did it arc gently LEFT (correct direction) at low speed, under control?"

# ── B3: unsigned on the floor → still ────────────────────────────────────────
echo
echo "── B3: UNSIGNED on the floor → STILL ──"
echo "Publishing UNSIGNED frames for 4 s. Expect NO motion on the ground."
timeout 4s ${PUB} --unsigned || true
confirm "Did the robot stay STILL on the floor (consumer logged REFUSED)?"

# ── B4: kill the consumer on the floor → stop ────────────────────────────────
echo
echo "── B4: consumer death on the floor → STOP ──"
echo "Re-establish creep: publishing valid frames for 3 s..."
timeout 3s ${PUB} --valid || true
echo "NOW: kill the consumer (Ctrl-C its terminal)."
confirm "Did the robot STOP on the floor immediately when the consumer died?"

echo
echo "=================================================================="
echo "  ✅ R2 PATH-B FLOOR-RUN PASSED. Governed motion, correct steering,"
echo "     refusal + kill-to-stop — ELEVATED and on the FLOOR, through the"
echo "     real verify chokepoint."
echo
echo "  Record: save the consumer terminal log as the acceptance artifact and"
echo "  note the run in robot/r2_drive_calibration_results.txt (§8.2 done)."
echo "  Still separately gated before the standing R2 config (doc §8 step 3 / §9):"
echo "    - RR-channel PWM↔m/s confirm (v0 uses the LEFT slope for equal-PWM),"
echo "    - KIRRA_VEHICLE_CLASS=r2 contract re-validation + interceptor wheelbase=L,"
echo "    - the §9 open decisions (equal-PWM vs differential, open- vs closed-loop)."
echo "  KIRRA_DRIVE_MODE=r2_ackermann may now be enabled for tethered low-speed"
echo "  operation; leave it OFF in env.template until the §8-step-3 sign-offs land."
echo "=================================================================="
