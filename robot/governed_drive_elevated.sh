#!/usr/bin/env bash
# governed_drive_elevated.sh — ONE-COMMAND elevated governed-drive bring-up.
#
# WHY THIS EXISTS: the doer/checker/actuation stack comes up under systemd
# (verifier + kirra-ros-stack + kirra-consumer + planner/taj/mick sidecars), but
# occy_doer also needs two INPUTS that are NOT part of that stack and are easy to
# lose over a choppy SSH session when hand-started as background jobs:
#   1. /scan  — the YDLIDAR TG30 driver (its own `ros2 launch`, not a unit), and
#   2. /odom  — which the R2 has NO real source for yet (the vendor base node that
#               publishes it is killed to free /dev/myserial for the KIRRA
#               consumer; consumer-published odom is the follow-up, task #84).
# This script (re)starts both durably (setsid, survives this shell), turns on
# occy DEBUG so its hold reason is visible, fires a forward goal, and streams the
# loop — all in one invocation, so no multi-line pasting is needed.
#
# 🔴 WHEELS ELEVATED. The /odom here is a STATIC origin crutch: occy never sees
# the goal get closer, so it commands forward INDEFINITELY. This proves the
# governed loop end-to-end (occy -> checker -> release token -> consumer -> motor)
# with the wheels up. A FLOOR drive that stops at the goal needs real odom.
#
#   bash robot/governed_drive_elevated.sh ["go forward one meter"]
#
# Ctrl-C tears down the /scan + /odom helpers this script started.
set -uo pipefail

INTENT_TEXT="${1:-go forward one meter}"
MICK_URL="${KIRRA_MICK_URL:-http://127.0.0.1:8102}"
WS_SETUP="${KIRRA_ROS_WS_SETUP:-$HOME/kirra-runtime-sdk/ros2_ws/install/setup.bash}"
LOGDIR="$(mktemp -d)"
STARTED_LIDAR=0
ODOM_PID=""

banner() { echo; echo "== $* =="; }
cleanup() {
  [[ -n "${ODOM_PID}" ]] && kill "${ODOM_PID}" 2>/dev/null
  [[ "${STARTED_LIDAR}" -eq 1 ]] && pkill -f ydlidar_ros2_driver 2>/dev/null
  echo "  (torn down the /odom${STARTED_LIDAR:+ + /scan} helper this script started)"
}
trap cleanup EXIT INT TERM

# ROS setup scripts reference unset vars — relax nounset around the source.
set +u
source /opt/ros/humble/setup.bash
# shellcheck disable=SC1090
source "${WS_SETUP}"
set -u
export ROS_DOMAIN_ID="${ROS_DOMAIN_ID:-28}"
echo "ROS_DOMAIN_ID=${ROS_DOMAIN_ID}  ws=${WS_SETUP}"

pub_count() { ros2 topic info "$1" 2>/dev/null | awk -F': ' '/Publisher count/{print $2}'; }

# ---- 1. /scan (YDLIDAR) ----------------------------------------------------
banner "1/4  /scan (YDLIDAR TG30)"
if [[ "$(pub_count /scan)" == "0" || -z "$(pub_count /scan)" ]]; then
  echo "  no /scan publisher — launching ydlidar driver (setsid, log: ${LOGDIR}/lidar.log)"
  setsid ros2 launch ydlidar_ros2_driver ydlidar_launch.py >"${LOGDIR}/lidar.log" 2>&1 &
  STARTED_LIDAR=1
  for _ in $(seq 1 15); do sleep 1; [[ "$(pub_count /scan)" != "0" && -n "$(pub_count /scan)" ]] && break; done
fi
if [[ "$(pub_count /scan)" != "0" && -n "$(pub_count /scan)" ]]; then
  echo "  ✔ /scan has a publisher"
else
  echo "  ❌ /scan STILL has no publisher — check ${LOGDIR}/lidar.log (is /dev/ydlidar present?)"
fi

# ---- 2. /odom --------------------------------------------------------------
banner "2/4  /odom"
# Remove any stale static crutch left by a PRIOR run (a `ros2 topic pub … /odom`
# process) BEFORE counting publishers — otherwise an orphaned crutch would be
# mistaken for the consumer's real wheel-odometry below. The consumer's real
# /odom is a python process, not `ros2 topic pub`, so it survives this.
pkill -f "topic pub.*[/]odom" 2>/dev/null && sleep 1 || true
if [[ "$(pub_count /odom)" != "0" && -n "$(pub_count /odom)" ]]; then
  # The consumer's real wheel-odometry (KIRRA_R2_ODOM_ENABLED) is publishing —
  # do NOT add a static crutch (two publishers would fight). This is the path
  # that lets the drive STOP at the goal.
  echo "  ✔ /odom already published (real wheel-odometry) — no static crutch"
else
  echo "  no /odom publisher — starting static origin crutch (10 Hz)."
  echo "  🔴 static odom has NO feedback: occy never sees the goal approach, so it"
  echo "     drives forward FOREVER — wheels-up proof only. For a floor drive that"
  echo "     STOPS at the goal, run the consumer with KIRRA_R2_ODOM_ENABLED=1."
  setsid ros2 topic pub -r 10 /odom nav_msgs/msg/Odometry '{header: {frame_id: odom}}' \
    >"${LOGDIR}/odom.log" 2>&1 &
  ODOM_PID=$!
  sleep 1
  [[ "$(pub_count /odom)" != "0" && -n "$(pub_count /odom)" ]] \
    && echo "  ✔ static /odom publishing (pid ${ODOM_PID})" \
    || echo "  ❌ /odom not publishing — see ${LOGDIR}/odom.log"
fi

# ---- 3. occy DEBUG + goal --------------------------------------------------
banner "3/4  occy DEBUG + goal via Mick"
T="$(ros2 service type /occy_doer/set_logger_levels 2>/dev/null || true)"
if [[ -n "${T}" ]]; then
  ros2 service call /occy_doer/set_logger_levels "${T}" \
    "{levels: [{name: 'occy_doer', level: 10}]}" >/dev/null 2>&1 \
    && echo "  ✔ occy_doer log level -> DEBUG (${T})" \
    || echo "  ⚠ could not set occy DEBUG (non-fatal)"
else
  echo "  ⚠ /occy_doer/set_logger_levels not found — is occy_doer up? (non-fatal)"
fi
INTENT_JSON="$(python3 -c 'import json,sys; print(json.dumps({"text": sys.argv[1]}))' "${INTENT_TEXT}")"
echo "  POST /intent  ${INTENT_JSON}"
curl -s "${MICK_URL}/intent" -XPOST -H 'content-type: application/json' -d "${INTENT_JSON}"; echo

# ---- 3b. corridor probe (one-shot: is it boxed in?) ------------------------
banner "3b/4  corridor probe — nearest obstacle + planner verdict (read-only)"
echo "  (replays occy's scan->Taj->planner call once; 'obstacle within 1 m ahead'"
echo "   means occy's 0.00 is the checker CORRECTLY refusing, not a fault)"
python3 "$HOME/kirra-runtime-sdk/robot/inspect_corridor.py" 2>&1 \
  | grep -iE 'nearest|corridor|object|planner|MRC|obstacle|EMPTY|v=|verdict|stop|forward' \
  || echo "  (corridor probe failed — non-fatal; sidecars up?)"

# ---- 4. live view ----------------------------------------------------------
banner "4/4  LIVE — occy proposal / verdict / hold reason (Ctrl-C to stop)"
echo "  If /cmd_vel_raw stays 0.00, read the 'hold:' reason below it:"
echo "    hold: awaiting pose/goal  -> an input (odom/goal) is missing"
echo "    hold: stale-scan          -> /scan stopped"
echo "    hold: goal-reached        -> it thinks it's already there"
echo "    <a real 'v=0.00' plan>    -> boxed in: the lidar sees an obstacle in the corridor"
echo
# occy proposal (scalar) alongside its DEBUG reasoning from the journal.
( journalctl -u kirra-ros-stack -f -n 0 | grep --line-buffered -iE 'hold:| v=' | sed 's/^/[occy] /' ) &
JLOG=$!
trap 'kill "${JLOG}" 2>/dev/null; cleanup' EXIT INT TERM
ros2 topic echo /cmd_vel_raw --field linear.x
