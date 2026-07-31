#!/usr/bin/env bash
# Minimal reproduction for the leading hypothesis: the subscriber never receives
# because it runs as a DIFFERENT UID than the publisher.
#
# WHY THIS TEST EXISTS
#   Every observation that WORKED was same-user (jetson → jetson):
#     - ros2 topic echo /kirra/release --once          (jetson)
#     - the minimal standalone Python subscriber       (jetson)
#     - kirra_release_publisher.py                     (jetson)
#   The one thing that FAILS is started with sudo, i.e. as root:
#     sudo bash -c '... exec python3 /opt/kirra/robot/kirra_motor_consumer.py'
#   kirra-consumer.service specifies User=<robot user>, so root is NOT the
#   designed configuration — it is an artefact of the manual start line.
#
#   Fast DDS carries discovery over UDP multicast (which is why the graph shows a
#   matched, reliable, volatile pair) but carries DATA over shared memory for
#   same-host peers. The /dev/shm segments are created with the creating
#   participant's ownership. A writer must WRITE into the reader's segment, so a
#   jetson-owned publisher writing into a root-owned reader segment can be
#   refused at the filesystem layer while discovery still succeeds — timers keep
#   firing (they are local) and the subscription is simply never marked ready.
#
#   That is a hypothesis, not a conclusion. This script settles it.
#
# WHAT IT PROVES
#   Runs the SAME pure-ROS subscriber (matrix mode a: no vendor library, no
#   serial, no motors) twice against one jetson-owned publisher — once as the
#   invoking user, once as root. If the user run passes and the root run fails,
#   the UID boundary is the cause and no Python change is warranted. If both
#   pass, the hypothesis is dead and the vendor matrix (modes b-d) is next.
#
# SAFETY
#   Touches no serial port, constructs no Rosmaster, commands no motor. The
#   payload is 3 bytes, never the 272-byte governed frame.
#
# USAGE
#   source /opt/kirra/robot/ros_env.sh && kirra_source_ros
#   robot/diagnostics/uid_asymmetry_test.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SECS="${SECS:-8}"

if [[ "$(id -u)" -eq 0 ]]; then
  echo "❌ run this as the ROBOT USER, not root — the script elevates for the" >&2
  echo "   second leg itself. Running the whole thing as root tests nothing." >&2
  exit 2
fi

echo "== invoking user: $(id -un) (uid $(id -u)) =="
echo "== RMW_IMPLEMENTATION=${RMW_IMPLEMENTATION:-<unset, Humble default rmw_fastrtps_cpp>} =="
echo "== ROS_DOMAIN_ID=${ROS_DOMAIN_ID:-<unset>} =="

echo
echo "== /dev/shm before (DDS segment ownership) =="
ls -la /dev/shm 2>/dev/null | head -30 || echo "   <unreadable>"

# ---- publisher, as the invoking user ---------------------------------------
python3 -u "${HERE}/probe_publisher.py" >/tmp/kirra_uid_pub.log 2>&1 &
pub_pid=$!
# shellcheck disable=SC2064  # expand pub_pid now, deliberately
trap "kill -TERM ${pub_pid} 2>/dev/null || true" EXIT
sleep 2
echo
echo "== publisher running as $(id -un), pid ${pub_pid} =="

# ---- leg 1: subscriber as the invoking user --------------------------------
echo
echo "######## LEG 1: subscriber as $(id -un) ########"
set +e
python3 -u "${HERE}/ros_callback_matrix.py" a --seconds "${SECS}"
rc_user=$?
set -e

# ---- leg 2: subscriber as root ---------------------------------------------
# The ROS environment does not survive sudo, so it is re-established inside the
# elevated shell exactly the way the failing consumer start line does.
echo
echo "######## LEG 2: subscriber as root ########"
set +e
sudo -E bash -c "
  source /opt/kirra/robot/ros_env.sh
  kirra_source_ros
  export ROS_DOMAIN_ID='${ROS_DOMAIN_ID:-28}'
  export PYTHONPATH='/opt/kirra/robot'
  exec python3 -u '${HERE}/ros_callback_matrix.py' a --seconds '${SECS}'
"
rc_root=$?
set -e

echo
echo "== /dev/shm after (note the owner column) =="
ls -la /dev/shm 2>/dev/null | head -30 || echo "   <unreadable>"

# ---- verdict ----------------------------------------------------------------
describe() { case "$1" in 0) echo "PASS (subscription dispatched)";; 1) echo "FAIL (timer fired, subscription did NOT)";; 2) echo "INCONCLUSIVE (executor never span)";; *) echo "ERROR (exit $1)";; esac; }

echo
echo "======== VERDICT ========"
echo "  subscriber as $(id -un): $(describe ${rc_user})"
echo "  subscriber as root:      $(describe ${rc_root})"
echo

if [[ ${rc_user} -eq 0 && ${rc_root} -eq 1 ]]; then
  echo "✔ HYPOTHESIS CONFIRMED: the UID boundary is the cause."
  echo "  Same code, same topic, same domain, same publisher — only the"
  echo "  subscriber's user differs, and only the root run fails to receive."
  echo "  The fix is to run the consumer as the robot user, which is what"
  echo "  kirra-consumer.service already specifies (User=<robot user>)."
  echo "  No change to the consumer's Python is warranted by this result."
  exit 0
fi

if [[ ${rc_user} -eq 0 && ${rc_root} -eq 0 ]]; then
  echo "✘ HYPOTHESIS REJECTED: root receives too, so the UID boundary is not it."
  echo "  Next: the vendor matrix, which this test deliberately excluded —"
  echo "  robot/diagnostics/run_callback_matrix.sh a b c d"
  exit 1
fi

echo "? INCONCLUSIVE: the user leg did not cleanly pass, so the comparison"
echo "  proves nothing. Check /tmp/kirra_uid_pub.log — if the publisher never"
echo "  started, neither leg was a real test."
exit 1
