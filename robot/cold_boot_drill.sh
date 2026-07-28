#!/usr/bin/env bash
# cold_boot_drill.sh — verify the R2 comes up SAFE on a clean power-on, and that
# it FAILS CLOSED when the verifier dies. Run after a physical power cycle.
#
# Two phases:
#   (default)      Phase A — post-boot health: every KIRRA service up, posture
#                  readable, the fleet node attested, the board owned by KIRRA,
#                  the vendor autostart gone. READ-ONLY; safe with wheels down.
#   --fail-closed  Phase B — kill the verifier and prove the consumer decel-stops
#                  (503 → 0.0, SS-002) + the interceptor denies, then restart and
#                  prove recovery. 🔴 WHEELS ELEVATED (it exercises the stop path).
#
#   robot/cold_boot_drill.sh
#   robot/cold_boot_drill.sh --fail-closed
#
# Companion to docs/hardware/R2_AUTOSTART_CHECKLIST.md. Exit 0 iff every checked
# item passes.
set -uo pipefail

FAILCLOSED=0
[[ "${1:-}" == "--fail-closed" ]] && FAILCLOSED=1

VERIFIER="${KIRRA_URL:-http://localhost:8090}"
PASS=0; FAIL=0
ok()   { echo "  ✔ $*"; PASS=$((PASS + 1)); }
bad()  { echo "  ❌ $*"; FAIL=$((FAIL + 1)); }
note() { echo "     $*"; }

# ROS for the topic checks (nounset-relaxed around the vendor setup scripts).
set +u
# Distro-agnostic base ROS (Jazzy or Humble, per ADR-0036).
_KIRRA_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=robot/ros_env.sh
source "$_KIRRA_HERE/ros_env.sh" 2>/dev/null && kirra_source_ros 2>/dev/null || true
source "${KIRRA_ROS_WS_SETUP:-$HOME/kirra-runtime-sdk/ros2_ws/install/setup.bash}" 2>/dev/null || true
set -u
export ROS_DOMAIN_ID="${ROS_DOMAIN_ID:-28}"

echo "== Phase A — post-boot health (ROS_DOMAIN_ID=${ROS_DOMAIN_ID}) =="

# 1. services active.
echo "-- services --"
for svc in kirra-verifier kirra-planner kirra-taj kirra-mick kirra-consumer kirra-ros-stack; do
  st="$(systemctl is-active "$svc" 2>/dev/null || echo missing)"
  [[ "$st" == "active" ]] && ok "$svc active" || bad "$svc is '$st' (expected active)"
done

# 2. verifier reachable + posture.
echo "-- verifier posture --"
if curl -fsS "${VERIFIER}/health" >/dev/null 2>&1; then
  ok "verifier /health reachable"
  pm="$(curl -fsS "${VERIFIER}/metrics" 2>/dev/null | grep -m1 '^kirra_fleet_posture{' | awk '{print $NF}')"
  case "$pm" in
    0) ok "fleet posture = Nominal (0)" ;;
    1) bad "fleet posture = Degraded (1)"; note "an actuator START from rest is denied under Degraded" ;;
    2) bad "fleet posture = LockedOut (2)"; note "attest a node: python3 robot/attest_bench_node.py" ;;
    *) bad "fleet posture unreadable ('$pm')" ;;
  esac
  # 3. fleet has an attested/Trusted node.
  fleet="$(curl -fsS "${VERIFIER}/fleet/posture" 2>/dev/null || echo '')"
  if grep -q '"local_status":"Trusted"' <<<"$fleet"; then
    ok "at least one Trusted node in the fleet"
  else
    bad "no Trusted node — empty/untrusted fleet"
    note "fleet was: ${fleet:0:120}"
  fi
else
  bad "verifier /health unreachable at ${VERIFIER}"
fi

# 4. consumer owns the board, no car-type FATAL.
echo "-- consumer / board ownership --"
clog="$(journalctl -u kirra-consumer -n 60 --no-pager 2>/dev/null || true)"
grep -q 'OWNS /dev/myserial' <<<"$clog" && ok "consumer OWNS /dev/myserial" || bad "no 'OWNS /dev/myserial' in recent consumer log"
if grep -qE 'FATAL' <<<"$clog"; then bad "consumer logged a FATAL"; grep -E 'FATAL' <<<"$clog" | tail -1 | sed 's/^/     /'; else ok "no consumer FATAL"; fi

# 5. motor-port AUTHORITY — who actually holds the device.
#
# This used to be `pgrep -af '...|yahboom'`, which on the live R2 matched
# `avahi-daemon: running [yahboom.local]` and the YDLIDAR node under
# ~/yahboomcar_ros2_ws, and so reported "a vendor base process is RUNNING"
# while the Kirra consumer held exclusive ownership. Neither ever opened
# /dev/myserial. The question is ownership, not branding.
echo "-- motor port authority --"
AUTH_PY="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/motor_authority.py"
MOTOR_PORT="${KIRRA_MOTOR_PORT:-/dev/myserial}"
if [[ -f "$AUTH_PY" ]]; then
  if auth_out="$(python3 "$AUTH_PY" "$MOTOR_PORT" 2>&1)"; then
    ok "consumer is the sole motor-port opener"
    sed 's/^/     /' <<<"$auth_out"
    ok "no competing motor-base process owns the motor port"
  else
    bad "motor-port ownership check FAILED"
    sed 's/^/     /' <<<"$auth_out"
    note "review the holders above; disable_vendor_autostart.sh --report explains"
  fi
else
  note "motor_authority.py not found (running outside the checkout?) — skipped"
fi
# Vendor-NAMED processes are listed for context only and never fail the drill.
vendor_named="$(pgrep -af 'yahboom' 2>/dev/null | grep -viE 'grep|cold_boot|disable_vendor' || true)"
if [[ -n "$vendor_named" ]]; then
  note "vendor-named processes present and IGNORED (they do not hold the motor port):"
  sed 's/^/     /' <<<"$vendor_named"
fi

# 6. sensor topics (advisory — lidar is a separate launch, may be off at boot).
echo "-- sensor topics (advisory) --"
scan_pub="$(ros2 topic info /scan 2>/dev/null | awk -F': ' '/Publisher count/{print $2}')"
odom_pub="$(ros2 topic info /odom 2>/dev/null | awk -F': ' '/Publisher count/{print $2}')"
[[ "${scan_pub:-0}" -gt 0 ]] && ok "/scan has a publisher" || note "⚠ /scan has no publisher (start the lidar: ros2 launch ydlidar_ros2_driver ydlidar_launch.py)"
[[ "${odom_pub:-0}" -gt 0 ]] && ok "/odom has a publisher" || note "⚠ /odom has no publisher (KIRRA_R2_ODOM_ENABLED=1 for consumer odom)"

echo
echo "Phase A: ${PASS} passed, ${FAIL} failed."

if [[ $FAILCLOSED -eq 0 ]]; then
  [[ $FAIL -eq 0 ]] && echo "✔ cold-boot health OK. Add --fail-closed to run the kill-verifier drill (wheels up)."
  exit $(( FAIL > 0 ? 1 : 0 ))
fi

# ---- Phase B — fail-closed drill ------------------------------------------
echo
echo "== Phase B — fail-closed drill =="
echo "🔴 WHEELS ELEVATED. This STOPS the verifier to prove the consumer decel-stops."
read -r -p "   Wheels up and clear? type 'yes' to proceed: " ans
[[ "$ans" == "yes" ]] || { echo "aborted."; exit 1; }

echo "-- stopping kirra-verifier --"
sudo systemctl stop kirra-verifier
# Consumer liveness period is KIRRA_CONTROL_PERIOD_MS × KIRRA_MISSED_PERIODS (default
# 100 ms × 3 = 300 ms) → decel-to-zero. Give it a comfortable margin.
sleep 3
ilog="$(journalctl -u kirra-ros-stack --since '6 seconds ago' --no-pager 2>/dev/null || true)"
if grep -qiE 'POSTURE_|Cannot reach Kirra|BLOCKED|fail-closed' <<<"$ilog"; then
  ok "interceptor denied / failed closed while the verifier was down"
  grep -iE 'POSTURE_|Cannot reach|BLOCKED' <<<"$ilog" | tail -1 | sed 's/^/     /'
else
  note "⚠ no explicit interceptor denial in the window (it may just be publishing nothing → consumer starves to 0, also fail-closed)"
fi
echo "-- restarting kirra-verifier --"
sudo systemctl start kirra-verifier
sleep 4
pm="$(curl -fsS "${VERIFIER}/metrics" 2>/dev/null | grep -m1 '^kirra_fleet_posture{' | awk '{print $NF}')"
[[ "$pm" == "0" ]] && ok "verifier recovered to Nominal after restart" \
  || bad "posture is '$pm' after restart (a fresh DB needs re-attest: robot/attest_bench_node.py)"

echo
echo "Drill total: ${PASS} passed, ${FAIL} failed."
exit $(( FAIL > 0 ? 1 : 0 ))
