#!/usr/bin/env bash
# Sweep the callback-dispatch matrix and print the failure boundary.
#
# Runs each mode of ros_callback_matrix.py against a live probe publisher and
# reports which mode is the FIRST to lose subscription callbacks. That mode's
# distinguishing variable is the cause.
#
# SAFETY: no motor is ever commanded. Modes c/d open the serial port but issue
# no motion primitive. Run with the robot on blocks and the consumer stopped.
#
# USAGE
#   source /opt/kirra/robot/ros_env.sh && kirra_source_ros
#   robot/diagnostics/run_callback_matrix.sh            # modes a b c d
#   robot/diagnostics/run_callback_matrix.sh a b c d e f g
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SECONDS_PER_MODE="${SECONDS_PER_MODE:-10}"
MODES=("$@")
if [[ ${#MODES[@]} -eq 0 ]]; then
  MODES=(a b c d)
fi

# ---- process hygiene --------------------------------------------------------
# Enumerate then kill explicitly. A bare `pkill -f kirra_motor_consumer` also
# matches this script's own command line when it is passed as an argument, so
# the PIDs are listed, filtered against $$ and this script's PPID, and only then
# signalled.
kill_matching() {
  local pattern="$1" pid
  local -a victims=()
  while read -r pid; do
    [[ -z "$pid" ]] && continue
    [[ "$pid" == "$$" || "$pid" == "$PPID" ]] && continue
    victims+=("$pid")
  done < <(pgrep -f "$pattern" || true)
  if [[ ${#victims[@]} -gt 0 ]]; then
    echo "   killing ${pattern}: ${victims[*]}"
    sudo kill -KILL "${victims[@]}" 2>/dev/null || kill -KILL "${victims[@]}" 2>/dev/null || true
  fi
}

echo "== process hygiene =="
sudo systemctl stop kirra-consumer.service 2>/dev/null || true
kill_matching '/opt/kirra/robot/kirra_motor_consumer'
kill_matching 'kirra_release_publisher\.py'
kill_matching 'ros_callback_matrix\.py'
kill_matching 'probe_publisher\.py'
sleep 2

echo "== survivors (expect none) =="
pgrep -af 'kirra_motor_consumer|kirra_release_publisher|ros_callback_matrix|probe_publisher' || echo "   none"
echo "== serial ownership (expect none) =="
sudo fuser -v /dev/myserial 2>&1 || echo "   unowned"

# ---- sweep ------------------------------------------------------------------
declare -a RESULTS=()
for mode in "${MODES[@]}"; do
  echo
  echo "############ MODE ${mode} ############"

  python3 -u "${HERE}/probe_publisher.py" >/tmp/kirra_probe_pub.log 2>&1 &
  pub_pid=$!
  sleep 2  # let discovery settle before the subscriber starts

  set +e
  python3 -u "${HERE}/ros_callback_matrix.py" "$mode" --seconds "$SECONDS_PER_MODE"
  rc=$?
  set -e

  kill -TERM "$pub_pid" 2>/dev/null || true
  wait "$pub_pid" 2>/dev/null || true
  sleep 1

  case "$rc" in
    0) RESULTS+=("${mode}: PASS  (sub + timer dispatched)") ;;
    1) RESULTS+=("${mode}: FAIL  (timer dispatched, subscription did NOT)") ;;
    2) RESULTS+=("${mode}: INCONCLUSIVE (executor never span / mode skipped)") ;;
    *) RESULTS+=("${mode}: ERROR (exit ${rc})") ;;
  esac
done

echo
echo "======== MATRIX SUMMARY ========"
printf '%s\n' "${RESULTS[@]}"
echo
echo "The FIRST mode to report FAIL names the cause:"
echo "  a → environmental (RMW/domain), not the vendor library"
echo "  b → an import-time side effect of Rosmaster_Lib"
echo "  c → constructing Rosmaster / opening the serial port"
echo "  d → create_receive_threading(), the vendor blocking-read thread"
echo "  e/f/g → a callback-group or executor-structure issue"
echo "If NO mode fails, the fault is not reproduced by this matrix and the"
echo "deployed file itself is the next suspect — diff it against the repo source."
