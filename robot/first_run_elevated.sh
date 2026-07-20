#!/usr/bin/env bash
# first_run_elevated.sh — the FIRST-RUN acceptance test for the KIRRA verifying
# motor consumer on the Yahboom Rosmaster X3.
#
# 🔴🔴🔴 RUN THIS WITH THE ROBOT ELEVATED — WHEELS OFF THE GROUND. 🔴🔴🔴
#
# WHY ELEVATED: this is the first time governed commands drive real motors. A
# wiring/clamp/verify bug could spin the wheels unexpectedly. Elevated, a wrong
# motion is a spinning wheel in the air, not a robot lunging off a bench. Only
# AFTER all three phases pass elevated does the robot touch the floor.
#
# The three phases (ADR-0033 Step 4 acceptance):
#   (a) a VALID governed command  → wheels spin at the CLAMPED demo speed
#   (b) an UNSIGNED command        → ZERO wheel motion, loud REFUSED in the log
#   (c) kill the consumer          → wheels STOP immediately (SS-002)
#
# This is a GUIDED script: it drives the publisher and prompts YOU to observe the
# wheels. It cannot see the wheels — you are the acceptance sensor. Answer
# honestly; a "no" fails the run.
#
# Prereqs:
#   - The consumer node is running in ANOTHER terminal (see the bringup doc):
#       ros2 run ... kirra_motor_consumer.py   (or: python3 robot/kirra_motor_consumer.py)
#     with all KIRRA_* config exported and KIRRA_GOVERNOR_VK_HEX pinned to the
#     dev key below (KIRRA_DEV_SEED's pubkey).
#   - Built: cargo build -p kirra-release-token --bin kirra_ros_release_mint --release
#   - ROS 2 (Humble or Jazzy) sourced; ROS_DOMAIN_ID=28.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PUB="python3 ${HERE}/kirra_release_publisher.py"

confirm() {  # confirm "question" -> exits non-zero on anything but y/Y
  read -r -p "$1 [y/N] " ans
  case "$ans" in
    y|Y) return 0 ;;
    *) echo "✗ operator reported failure — FIRST-RUN TEST FAILED"; exit 1 ;;
  esac
}

echo "=================================================================="
echo "  KIRRA X3 FIRST-RUN TEST — 🔴 WHEELS MUST BE OFF THE GROUND 🔴"
echo "=================================================================="
read -r -p "Is the robot ELEVATED with all wheels free to spin? [y/N] " ans
[ "$ans" = "y" ] || [ "$ans" = "Y" ] || { echo "Elevate the robot first. Aborting."; exit 1; }

echo
echo "Confirm the vendor base node is NOT running (it would be a second, unfenced"
echo "writer to the motor board):"
if command -v ros2 >/dev/null 2>&1; then
  echo "  active nodes:"; ros2 node list 2>/dev/null | sed 's/^/    /' || true
fi
confirm "Is 'yahboomcar_bringup' / any vendor /cmd_vel motor node ABSENT?"

# ---- Phase (a): valid governed command → clamped motion --------------------
echo
echo "── Phase (a): VALID governed command (expect wheels spinning at the CLAMPED demo speed) ──"
echo "Publishing valid frames for 5 s..."
timeout 5s ${PUB} --valid || true
confirm "Did the wheels SPIN (slowly, at the clamped demo speed)?"

# ---- Phase (b): unsigned command → no motion + loud refusal ----------------
echo
echo "── Phase (b): UNSIGNED command (expect ZERO motion + REFUSED in the consumer log) ──"
echo "Publishing UNSIGNED frames for 5 s... watch the consumer terminal for 'REFUSED (NO_TOKEN)'."
timeout 5s ${PUB} --unsigned || true
confirm "Did the wheels stay COMPLETELY STILL?"
confirm "Did the consumer log 'REFUSED (NO_TOKEN)' (no motor write)?"

# ---- Phase (c): kill the consumer → wheels stop ----------------------------
echo
echo "── Phase (c): consumer death → wheels STOP (SS-002 shutdown guarantee) ──"
echo "First, re-establish motion so a stop is observable:"
echo "Publishing valid frames for 3 s (wheels should spin again)..."
timeout 3s ${PUB} --valid || true
echo
echo "NOW: in the consumer terminal, press Ctrl-C (or: kill the consumer process)."
confirm "After killing the consumer, did the wheels STOP immediately?"

echo
echo "=================================================================="
echo "  ✅ FIRST-RUN TEST PASSED (elevated). All three phases confirmed:"
echo "     (a) governed → clamped motion, (b) unsigned → refused + still,"
echo "     (c) consumer death → wheels stop."
echo "  Only NOW may the robot be placed on the floor for further testing."
echo "=================================================================="
