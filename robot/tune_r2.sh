#!/usr/bin/env bash
# tune_r2.sh — one-command launcher for the R2 closed-loop ELEVATED tune, so a
# fresh shell doesn't need the KIRRA_R2_* exports re-pasted every time.
#
#   ./robot/tune_r2.sh
#
# 🔴 RUN ELEVATED — wheels off the ground, e-stop in hand. Stop the KIRRA
# consumer first (it owns /dev/myserial). This sets the measured R2 profile +
# closed-loop calibration (env.template / r2_drive_calibration_results.txt) and
# runs r2_closed_loop_tune_elevated.py. Every value is override-friendly (:=), so
# to sweep a gain just export it first, e.g.:
#   KIRRA_R2_SPEED_KP=60 ./robot/tune_r2.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
cd "$REPO"

# --- measured R2 drive/steer profile (only the drive values are used by the
#     tuner, but the shared calibration loader wants the full set) ---
: "${KIRRA_R2_WHEELBASE_M:=0.229}";           export KIRRA_R2_WHEELBASE_M
: "${KIRRA_R2_V_PER_PWM:=0.0145}";            export KIRRA_R2_V_PER_PWM
: "${KIRRA_R2_PWM_MAX:=40}";                  export KIRRA_R2_PWM_MAX
: "${KIRRA_R2_STEER_UNITS_PER_RAD:=66}";      export KIRRA_R2_STEER_UNITS_PER_RAD
: "${KIRRA_R2_DELTA_MAX_RAD:=0.68}";          export KIRRA_R2_DELTA_MAX_RAD
: "${KIRRA_R2_STEER_SIGN:=-1}";               export KIRRA_R2_STEER_SIGN
: "${KIRRA_R2_CENTER_TRIM:=90}";              export KIRRA_R2_CENTER_TRIM
# --- closed-loop calibration (measured, RR confirm) ---
: "${KIRRA_R2_M_PER_TICK:=0.00025101}";       export KIRRA_R2_M_PER_TICK
: "${KIRRA_R2_V_PER_PWM_RIGHT:=0.0194}";      export KIRRA_R2_V_PER_PWM_RIGHT
# --- controller gains — CONSERVATIVE starting points; sweep via env to tune ---
: "${KIRRA_R2_SPEED_KP:=40}";                 export KIRRA_R2_SPEED_KP
: "${KIRRA_R2_SPEED_EMA_ALPHA:=0.3}";         export KIRRA_R2_SPEED_EMA_ALPHA
: "${KIRRA_MOTOR_PORT:=/dev/myserial}";       export KIRRA_MOTOR_PORT

echo "── R2 closed-loop tune: KP=$KIRRA_R2_SPEED_KP ema=$KIRRA_R2_SPEED_EMA_ALPHA "\
"target=${KIRRA_TUNE_TARGET_MPS:-0.20} port=$KIRRA_MOTOR_PORT"
exec python3 "$REPO/robot/r2_closed_loop_tune_elevated.py"
