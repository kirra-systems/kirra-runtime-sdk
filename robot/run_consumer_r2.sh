#!/usr/bin/env bash
# run_consumer_r2.sh — launch the KIRRA verifying motor consumer in R2 Path-B
# (Ackermann) mode with the measured profile, as ONE paste-proof command.
#
# WHY THIS EXISTS: pasting a dozen `export`s then `python3 ...` into a terminal is
# fragile — a jumbled paste can run the consumer before the exports take, and it
# then defaults to x3 mode and aborts (KIRRA_EXPECTED_CAR_TYPE unset). This script
# sets the entire environment in one process, so there is nothing to mis-paste.
#
#   Terminal 1:   ./robot/run_consumer_r2.sh
#
# 🔴 DEV/DEMO ONLY: it pins the well-known dev governor key (seed 0x2a×32) so the
# bench publisher (robot/kirra_release_publisher.py / first_run_r2_floor.sh) can
# mint against it. NEVER use this on a production/golden unit — that path is a
# real governor key via enrollment (docs/safety/GOVERNOR_KEY_PROVISIONING.md).
#
# Every value below can be overridden by exporting it first (e.g. after
# `source /etc/kirra/robot.env`) — the `:=` defaults only fill what is unset.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
cd "$REPO"

# ROS 2 (guarded — skip if already sourced / absent). Distro-agnostic: Jazzy or
# Humble, per ADR-0036 (override with KIRRA_ROS_SETUP / KIRRA_ROS_DISTRO_PREF).
# shellcheck source=robot/ros_env.sh
source "$HERE/ros_env.sh"
kirra_source_ros || true
: "${ROS_DOMAIN_ID:=28}"; export ROS_DOMAIN_ID

# --- governor key pin: derive the dev seed's pubkey unless already pinned ------
DEV_SEED="${KIRRA_DEV_SEED:-2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a}"
# Resolve the mint binary: an explicit override wins; else search release THEN
# debug (the publisher + teardown harness do the same — a 'built debug' workflow
# must work without an extra override).
MINT="${KIRRA_MINT_BIN:-}"
if [ -z "$MINT" ]; then
  for prof in release debug; do
    if [ -x "$REPO/target/$prof/kirra_ros_release_mint" ]; then
      MINT="$REPO/target/$prof/kirra_ros_release_mint"; break
    fi
  done
fi
if [ -z "${KIRRA_GOVERNOR_VK_HEX:-}" ]; then
  [ -n "$MINT" ] && [ -x "$MINT" ] || { echo "FATAL: kirra_ros_release_mint not found under target/{release,debug} (set KIRRA_MINT_BIN, or build: cargo build -p kirra-release-token --bin kirra_ros_release_mint --release)" >&2; exit 1; }
  KIRRA_GOVERNOR_VK_HEX="$("$MINT" --seed "$DEV_SEED" pubkey)"
fi
export KIRRA_GOVERNOR_VK_HEX

# --- Path-B selector + the MEASURED R2 profile (env.template / calib results) --
: "${KIRRA_DRIVE_MODE:=r2_ackermann}";        export KIRRA_DRIVE_MODE
: "${KIRRA_R2_WHEELBASE_M:=0.229}";           export KIRRA_R2_WHEELBASE_M
: "${KIRRA_R2_V_PER_PWM:=0.0145}";            export KIRRA_R2_V_PER_PWM
: "${KIRRA_R2_PWM_MAX:=40}";                  export KIRRA_R2_PWM_MAX
: "${KIRRA_R2_STEER_UNITS_PER_RAD:=66}";      export KIRRA_R2_STEER_UNITS_PER_RAD
: "${KIRRA_R2_DELTA_MAX_RAD:=0.68}";          export KIRRA_R2_DELTA_MAX_RAD
: "${KIRRA_R2_STEER_SIGN:=-1}";               export KIRRA_R2_STEER_SIGN
: "${KIRRA_R2_CENTER_TRIM:=90}";              export KIRRA_R2_CENTER_TRIM
: "${KIRRA_R2_DRIVE_DEADBAND_PWM:=0}";        export KIRRA_R2_DRIVE_DEADBAND_PWM

# §9 closed-loop calibration (only READ when KIRRA_R2_CLOSED_LOOP is set truthy;
# harmless otherwise). Measured from the RR confirm. Export KIRRA_R2_CLOSED_LOOP=1
# yourself (after the elevated tune) to enable per-wheel speed matching; the gains
# (KIRRA_R2_SPEED_*) fall back to r2_drive's conservative defaults unless set.
: "${KIRRA_R2_M_PER_TICK:=0.00025101}";       export KIRRA_R2_M_PER_TICK
: "${KIRRA_R2_V_PER_PWM_RIGHT:=0.0194}";      export KIRRA_R2_V_PER_PWM_RIGHT

# --- pinned platform profile digest (evidence-bound V2, REQUIRED) -------------
# The consumer refuses any release whose SIGNED profile digest differs from this
# pinned value, so a robot cannot be driven under another platform's envelope.
#
# There is deliberately NO default. Unlike the dev governor key (derivable from
# the dev seed above), this digest is a property of the DEPLOYED PLATFORM: Taj
# computes it from the perception request for this robot, and it must match
# byte-for-byte. Inventing one here would either refuse every release or, worse,
# pin the wrong platform — both are silent-config failures the fail-closed rule
# exists to prevent.
#
# Obtain it from the Taj sidecar's own answer for this robot, e.g.
#   curl -s localhost:8101/perception -d @perception.json -H 'content-type: application/json' \
#     | python3 -c 'import json,sys; print(json.load(sys.stdin)["frame_id"]["profile_digest"])'
# then pin it (64 LOWERCASE hex) in /etc/kirra/robot.env.
if [ -z "${KIRRA_PROFILE_DIGEST:-}" ]; then
  echo "FATAL: KIRRA_PROFILE_DIGEST is unset — the evidence-bound consumer pins the" >&2
  echo "       platform profile digest and refuses releases minted under another." >&2
  echo "       Set it to this robot's 64-lowercase-hex Taj profile digest (see the" >&2
  echo "       comment above this check in robot/run_consumer_r2.sh)." >&2
  exit 1
fi
export KIRRA_PROFILE_DIGEST

# --- ADR-0033 timing + demo caps + hardware (validated first-run values) -------
# NOTE (V2): the consumer now reads this as the MAXIMUM TOKEN LIFETIME the core
# will accept — V2 enforces the token's own signed expires_at_ms, and this bounds
# how long any single mint may remain valid. The variable name is unchanged so
# existing robot.env files keep working.
: "${KIRRA_FRESHNESS_WINDOW_MS:=200}";        export KIRRA_FRESHNESS_WINDOW_MS
: "${KIRRA_CONTROL_PERIOD_MS:=100}";          export KIRRA_CONTROL_PERIOD_MS
: "${KIRRA_MISSED_PERIODS:=3}";               export KIRRA_MISSED_PERIODS
: "${KIRRA_STOP_DECEL_MPS2:=0.5}";            export KIRRA_STOP_DECEL_MPS2
: "${KIRRA_DEMO_VX_MAX:=0.15}";               export KIRRA_DEMO_VX_MAX
: "${KIRRA_DEMO_VZ_MAX:=0.4}";                export KIRRA_DEMO_VZ_MAX
: "${KIRRA_MOTOR_PORT:=/dev/myserial}";       export KIRRA_MOTOR_PORT
# KIRRA_CONSUMER_LIB: prefer an explicit, EXISTING path; else the installed copy;
# else leave unset so the loader auto-searches the repo target/ dirs (kirra_ffi.py).
# Self-heal a stale inherited path (e.g. /opt/kirra/... on a box without it) — a
# dangling value would otherwise fail the loader instead of falling back.
if [ -n "${KIRRA_CONSUMER_LIB:-}" ] && [ ! -f "${KIRRA_CONSUMER_LIB}" ]; then
  echo "   note: KIRRA_CONSUMER_LIB=${KIRRA_CONSUMER_LIB} does not exist — unsetting so the loader auto-searches target/" >&2
  unset KIRRA_CONSUMER_LIB
fi
if [ -z "${KIRRA_CONSUMER_LIB:-}" ] && [ -f /opt/kirra/lib/libkirra_consumer_ffi.so ]; then
  KIRRA_CONSUMER_LIB=/opt/kirra/lib/libkirra_consumer_ffi.so; export KIRRA_CONSUMER_LIB
fi
if [ -z "${KIRRA_CONSUMER_LIB:-}" ] \
   && [ ! -f "$REPO/target/release/libkirra_consumer_ffi.so" ] \
   && [ ! -f "$REPO/target/debug/libkirra_consumer_ffi.so" ]; then
  echo "FATAL: libkirra_consumer_ffi.so not built and no installed copy. Build it: cargo build -p kirra-consumer-ffi --release" >&2
  exit 1
fi

echo "── KIRRA consumer: r2_ackermann, VK=${KIRRA_GOVERNOR_VK_HEX:0:16}…, profile=${KIRRA_PROFILE_DIGEST:0:16}…, port=$KIRRA_MOTOR_PORT, domain=$ROS_DOMAIN_ID"
echo "   (evidence-bound V2: 272-byte frames only; V1 128-byte frames are refused)"
echo "   (car-type 5 will be set + read back at init; Ctrl-C to stop)"
exec python3 "$REPO/robot/kirra_motor_consumer.py"
