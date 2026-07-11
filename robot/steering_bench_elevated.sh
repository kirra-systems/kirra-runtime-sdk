#!/usr/bin/env bash
# steering_bench_elevated.sh — Track-A steering bench for the Rosmaster R2
# (Ackermann). Guides BOTH:
#   A1 — the MEASUREMENTS that gate the r2 contract profile (v_z semantics,
#        steering servo limit, slew, footprint), and
#   A5 — the steering ACCEPTANCE phases (zero-yaw straight, small left/right,
#        direction correctness) run after the A4 units fix lands.
#
# 🔴🔴🔴 ROBOT ELEVATED — ALL WHEELS OFF THE GROUND, STEERING FREE TO SWING. 🔴🔴🔴
#
# WHY: this session commands real steering. Elevated, a wrong direction or a
# saturated servo is an observation; on the floor it is a crash.
#
# The operator IS the measurement instrument: answers are recorded to a log
# file (robot/steering_bench_results.txt) that feeds the r2 profile PR — the
# numbers in that profile must come from THIS session, never invented.
#
# Prereqs: the consumer running and owning /dev/myserial (vendor node OFF),
# the mint binary built, KIRRA_GOVERNOR_VK_HEX pinned (see the bringup doc §5).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PUB="python3 ${HERE}/kirra_release_publisher.py"
LOG="${HERE}/steering_bench_results.txt"

say()  { echo; echo "== $*"; }
rec()  { echo "$*" >> "$LOG"; }
ask()  { # ask "prompt" varname  → records "varname: answer"
  read -r -p "$1 " _ans
  rec "$2: ${_ans}"
  echo "  recorded: $2 = ${_ans}"
}
confirm() {
  read -r -p "$1 [y/N] " ans
  case "$ans" in y|Y) return 0 ;; *) echo "✗ FAILED/ABORTED — noted in log"; rec "FAIL: $1"; exit 1 ;; esac
}
pub() { # pub linear angular seconds — publish governed frames
  KIRRA_PUB_LINEAR="$1" KIRRA_PUB_ANGULAR="$2" timeout "$3"s ${PUB} --valid || true
}

: > "$LOG"
rec "steering bench session $(date -Is)"

echo "=================================================================="
echo "  R2 STEERING BENCH — 🔴 WHEELS OFF THE GROUND, STEERING FREE 🔴"
echo "=================================================================="
read -r -p "Robot ELEVATED, wheels free, steering linkage unobstructed? [y/N] " a
[ "$a" = "y" ] || [ "$a" = "Y" ] || { echo "Elevate first. Aborting."; exit 1; }
confirm "Vendor base node ABSENT (ros2 node list shows no yahboomcar bringup)?"

# ───────────────────────────── A1.1 — v_z SEMANTICS ─────────────────────────
say "A1.1 — what does set_car_motion's third argument actually do on the R2?"
echo "Publishing linear=0.10 m/s, angular=0.20 for 5 s. WATCH the platform:"
echo "  (a) the STEERING SERVO deflects to a fixed angle → arg is a STEERING ANGLE"
echo "  (b) left/right wheels spin at DIFFERENT speeds / body yaws → arg is a YAW RATE"
pub 0.10 0.20 5
ask "Which did you observe — 'angle' or 'yaw-rate' (or 'other: …')?" v_z_semantics
echo "Repeating at angular=0.40 (double). Did the deflection/response roughly DOUBLE?"
pub 0.10 0.40 5
ask "Proportional to the commanded value? y/n + notes:" v_z_proportional

# ───────────────────────────── A1.2 — SERVO LIMIT ────────────────────────────
say "A1.2 — steering servo hard limit (max_steering_deg)"
echo "Stepping angular up until the servo stops deflecting further (saturation)."
for w in 0.4 0.8 1.2 1.6 2.0; do
  echo "  angular=${w} for 4 s…"
  pub 0.10 "$w" 4
  read -r -p "  still increasing deflection? [y/n] " inc
  rec "deflection_increasing_at_angular_${w}: ${inc}"
  [ "$inc" = "n" ] && break
done
echo "Measure the MAXIMUM wheel deflection angle from straight-ahead"
echo "(phone inclinometer against the wheel, or protractor photo)."
ask "max_steering_deg (LEFT):"  max_steering_deg_left
ask "max_steering_deg (RIGHT):" max_steering_deg_right

# ───────────────────────────── A1.3 — SLEW ──────────────────────────────────
say "A1.3 — steering slew (max_steering_rate_deg_s)"
echo "Alternating full-left/full-right every 2 s for 8 s; time one full swing"
echo "(left-stop to right-stop) with a stopwatch/slow-mo video."
for i in 1 2; do pub 0.10 2.0 2; pub 0.10 -2.0 2; done
ask "one full swing took (seconds):" full_swing_s
echo "  (max_steering_rate_deg_s ≈ (left_limit+right_limit)/full_swing_s — computed in the PR, not here)"

# ───────────────────────────── A1.4 — FOOTPRINT ─────────────────────────────
say "A1.4 — footprint (tape measure; robot can stay elevated)"
ask "wheelbase_m (axle to axle; expect ≈0.229):"        wheelbase_m
ask "width_m (outer wheel to outer wheel):"             width_m
ask "length_m (bumper to bumper):"                      length_m
ask "overhang_front_m (front axle to front bumper):"    overhang_front_m
ask "overhang_rear_m (rear axle to rear bumper):"       overhang_rear_m

# ───────────────────────────── A5 — ACCEPTANCE ──────────────────────────────
say "A5 — steering acceptance (meaningful AFTER the A4 units fix; record anyway)"
echo "(a) straight line: linear=0.10, angular=0 for 4 s — steering must stay centered."
pub 0.10 0.0 4
confirm "Steering stayed CENTERED (no deflection)?"
echo "(b) small LEFT: angular=+0.2 for 4 s."
pub 0.10 0.2 4
ask "Wheels deflected LEFT? (y/n):" a5_left_correct
echo "(c) small RIGHT: angular=-0.2 for 4 s."
pub 0.10 -0.2 4
ask "Wheels deflected RIGHT? (y/n):" a5_right_correct
rec "A5 note: direction convention verified against REP-103 (+z yaw = left turn)"

say "DONE — results in ${LOG}. Send that file back; the r2 profile PR is built"
echo "from EXACTLY these numbers (plus the reviewed policy values). Floor driving"
echo "remains prohibited until the live-loop elevated re-test passes."
