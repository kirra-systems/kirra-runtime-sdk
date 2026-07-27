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
# The phases (ADR-0033 Step 4 acceptance):
#   (a)  a VALID governed command → wheels spin at the CLAMPED demo speed
#   (b)  an UNSIGNED command      → ZERO wheel motion, refused at the wire parse
#   (b2) a CORRUPT SIGNATURE      → ZERO wheel motion, refused by the crypto gate
#   (c)  kill the consumer        → wheels STOP immediately (SS-002)
#
# (b) and (b2) are both refusals but prove DIFFERENT gates: (b) never reaches the
# verify core (evidence-bound V2 has no unsigned wire form, so a token-less
# payload dies at the strict 272-byte parse), while (b2) is well-formed and is
# refused by the Ed25519 check itself.
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

# Evidence-bound V2 preflight: the publisher binds the pinned platform profile
# digest into every frame, and the consumer refuses any frame whose digest
# differs. Check it HERE so a missing export fails before the guided procedure
# starts, rather than aborting the publisher mid-phase. Export the SAME value
# the consumer terminal uses.
if [ -z "${KIRRA_PROFILE_DIGEST:-}" ]; then
  echo "FATAL: KIRRA_PROFILE_DIGEST is unset — export this robot's 64-lowercase-hex" >&2
  echo "       platform profile digest (the same value the consumer pins) before" >&2
  echo "       running this test. See robot/run_consumer_r2.sh for how to obtain it." >&2
  exit 1
fi

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
echo "Publishing UNSIGNED frames for 5 s... watch the consumer terminal for"
echo "'REFUSED (MALFORMED_FRAME_176B)'. Evidence-bound V2 has no unsigned wire"
echo "form, so a token-less payload is refused at the strict 272-byte parse and"
echo "never even reaches the verify core (stricter than V1's NO_TOKEN)."
timeout 5s ${PUB} --unsigned || true
confirm "Did the wheels stay COMPLETELY STILL?"
confirm "Did the consumer log 'REFUSED (MALFORMED_FRAME_176B)' (no motor write)?"

echo
echo "── Phase (b2): CORRUPT SIGNATURE (expect ZERO motion + the crypto gate) ──"
echo "Publishing well-formed 272-byte frames with a flipped signature bit for 5 s."
echo "These DO reach the verify core, so this proves the Ed25519 check itself —"
echo "watch for 'REFUSED (SIGNATURE_INVALID)'."
timeout 5s ${PUB} --corrupt-sig || true
confirm "Did the wheels stay COMPLETELY STILL?"
confirm "Did the consumer log 'REFUSED (SIGNATURE_INVALID)' (no motor write)?"

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
echo "  ✅ FIRST-RUN TEST PASSED (elevated). All phases confirmed:"
echo "     (a) governed → clamped motion, (b) unsigned → refused at the wire"
echo "     parse + still, (b2) corrupt signature → refused by the crypto gate"
echo "     + still, (c) consumer death → wheels stop."
echo "  Only NOW may the robot be placed on the floor for further testing."
echo "=================================================================="
