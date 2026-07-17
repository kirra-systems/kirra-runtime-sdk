#!/usr/bin/env bash
# stage1_doer_dryrun.sh — stand up the WHOLE Stage-1 doer dry run in ONE window.
#
# Stage 1 proves Mick (LLM) + Taj (perception) + Occy (planner) produce a sane
# /cmd_vel_raw proposal WITHOUT the verifier/interceptor-mint/consumer, so nothing
# can actuate — safe to run over SSH. See docs/hardware/R2_LIVE_LOOP_BRINGUP.md §1.
#
# WHY THIS EXISTS: juggling five SSH shells (mick, launch, odom, intent, echo) on a
# phone keyboard is the actual failure mode — a half-typed export or a Ctrl-C on the
# wrong window and the pipeline looks broken when it isn't. This script owns every
# piece EXCEPT your lidar driver, backgrounds them with logs, then foregrounds a live
# tail of occy's decision + the proposal. One Ctrl-C tears down everything it started
# (never your lidar).
#
#   Window 1 (your usual way):  the 4ROS lidar driver  -> /scan
#   Window 2:                   ./robot/stage1_doer_dryrun.sh
#
# NO verifier, NO consumer, NO wheels. Actuation is Stage 2 (live_loop_elevated.sh),
# physically present only.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
cd "$REPO"

# --- env: ROS + domain (guarded; matches run_consumer_r2.sh discipline) --------
if [ -z "${ROS_DISTRO:-}" ] && [ -f /opt/ros/humble/setup.bash ]; then
  # shellcheck disable=SC1091
  source /opt/ros/humble/setup.bash
fi
if [ -f "$REPO/ros2_ws/install/setup.bash" ]; then
  # shellcheck disable=SC1091
  source "$REPO/ros2_ws/install/setup.bash"
fi
: "${ROS_DOMAIN_ID:=28}"; export ROS_DOMAIN_ID

MICK_URL="${KIRRA_MICK_URL:-http://127.0.0.1:8102}"
OLLAMA_URL="${KIRRA_OLLAMA_URL:-http://127.0.0.1:11434}"
INTENT_TEXT="${1:-creep forward one meter}"
LOGDIR="${KIRRA_STAGE1_LOGDIR:-/tmp/kirra_stage1}"
mkdir -p "$LOGDIR"

# --- teardown: one Ctrl-C kills only what THIS script started -------------------
PIDS=()
cleanup() {
  echo; echo "── tearing down Stage-1 (lidar untouched) ──"
  for pid in "${PIDS[@]:-}"; do
    [ -n "${pid:-}" ] && kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

banner() { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }

# --- 0. Ollama reachability (Mick's brain) — warn only, Mick fail-closes anyway -
banner "0/5  Ollama"
if curl -sf "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
  echo "  ollama up ($OLLAMA_URL)"
else
  echo "  ⚠ ollama NOT reachable at $OLLAMA_URL — start it (ollama serve) + pull the model."
  echo "    Mick will 422 on /intent until it's up; the rest of the stack still runs."
fi

# --- 1. mick_service (text -> typed intent); start if down ----------------------
banner "1/5  mick_service"
if curl -sf "$MICK_URL/health" >/dev/null 2>&1; then
  echo "  mick_service already up ($MICK_URL) — reusing it"
else
  MICK_BIN=""
  for prof in release debug; do
    [ -x "$REPO/target/$prof/mick_service" ] && { MICK_BIN="$REPO/target/$prof/mick_service"; break; }
  done
  [ -n "$MICK_BIN" ] || { echo "FATAL: mick_service not built (cargo build -p kirra-sidecars --bin mick_service --release)"; exit 1; }
  echo "  starting $MICK_BIN -> $LOGDIR/mick.log"
  "$MICK_BIN" >"$LOGDIR/mick.log" 2>&1 &
  PIDS+=("$!")
  for _ in $(seq 1 30); do curl -sf "$MICK_URL/health" >/dev/null 2>&1 && break; sleep 0.3; done
  curl -sf "$MICK_URL/health" >/dev/null 2>&1 && echo "  mick_service up" || { echo "FATAL: mick_service did not come up — see $LOGDIR/mick.log"; exit 1; }
fi

# --- 2. the launch stack (planner + taj sidecars + occy_doer + interceptor) -----
banner "2/5  ros2 launch (occy doer + perception cap)"
echo "  -> $LOGDIR/launch.log  (Cannot-reach-Kirra errors here are EXPECTED — no verifier in Stage 1)"
ros2 launch kirra_safety kirra_with_robot.launch.py \
     use_occy_doer:=true use_perception_cap:=true \
     >"$LOGDIR/launch.log" 2>&1 &
PIDS+=("$!")

echo -n "  waiting for /occy_doer to appear"
for _ in $(seq 1 40); do
  if ros2 node list 2>/dev/null | grep -q '/occy_doer'; then echo " — up"; break; fi
  echo -n "."; sleep 0.5
done
ros2 node list 2>/dev/null | grep -q '/occy_doer' || { echo; echo "FATAL: /occy_doer never appeared — see $LOGDIR/launch.log"; exit 1; }

# --- 3. static /odom at origin (dry run: the motor node is OFF) ------------------
banner "3/5  /odom static ego (10 Hz, origin)"
ros2 topic pub -r 10 /odom nav_msgs/msg/Odometry '{header: {frame_id: odom}}' \
     >"$LOGDIR/odom.log" 2>&1 &
PIDS+=("$!")
echo "  publishing /odom"

# --- 4. occy DEBUG on (so it prints its hold reason each tick) -------------------
banner "4/5  occy DEBUG + goal via Mick"
ros2 service call /occy_doer/set_logger_levels rcl_interfaces/srv/SetLoggerLevels \
     "{levels: [{name: 'occy_doer', level: 10}]}" >/dev/null 2>&1 \
     && echo "  occy_doer log level -> DEBUG" || echo "  ⚠ could not set occy DEBUG (non-fatal)"

# fire the LLM intent (go_to becomes the goal, grounded ego->odom at receipt)
echo "  POST /intent  text=\"$INTENT_TEXT\""
curl -s "$MICK_URL/intent" -XPOST -H 'content-type: application/json' \
     -d "{\"text\":\"$INTENT_TEXT\"}" ; echo
echo -n "  /intent/last -> "; curl -s "$MICK_URL/intent/last" ; echo

# --- 5. live view: occy decision + the bounded proposal -------------------------
banner "5/5  LIVE  (Ctrl-C to tear it all down)"
echo "  Left: occy_doer hold/plan lines (from the launch log)."
echo "  Right: /cmd_vel_raw — Occy's KIRRA-bounded proposal (want a small linear.x)."
echo "  Obstacle ~0.5 m in front of the lidar -> proposal should collapse to ~0 (Taj cap)."
echo
# occy's per-tick line, filtered out of the launch log
( stdbuf -oL tail -n0 -F "$LOGDIR/launch.log" 2>/dev/null \
    | grep --line-buffered -E 'occy_doer|hold:|v=|new goal|mick intent' \
    | sed -u 's/^/[occy] /' ) &
PIDS+=("$!")
# the proposal itself
ros2 topic echo /cmd_vel_raw
