#!/usr/bin/env bash
# live_loop_elevated.sh — the LIVE-LOOP acceptance test: lidar → Taj → Occy →
# interceptor → verifier → signed release → verifying motor consumer → wheels.
#
# 🔴🔴🔴 RUN THIS WITH THE ROBOT ELEVATED — WHEELS OFF THE GROUND. 🔴🔴🔴
#
# WHY ELEVATED: this is the first time the FULL perception-driven chain moves
# real motors (first_run_elevated.sh only proved the consumer against a
# hand-driven publisher). A wiring bug anywhere in the chain could spin the
# wheels unexpectedly. Elevated, that is a spinning wheel in the air.
#
# The chain under test (every hop live):
#   /scan (lidar, BEST_EFFORT) ──> perception_governor ──> /kirra/perception_speed_cap
#                              └─> occy_doer (+Taj corridor +Occy plan) ──> /cmd_vel_raw
#   /cmd_vel_raw ──> cmd_vel_interceptor [Taj cap → KIRRA verifier → mint]
#                       └──> /kirra/release (payload‖token, 128-byte signed frames)
#   /kirra/release ──> kirra_motor_consumer [Rust FFI verify] ──> /dev/myserial
#
# The four phases:
#   (a) NOMINAL           clear corridor + goal ahead → wheels spin (governed)
#   (b) PERCEPTION REFUSAL obstacle in front of lidar → Taj caps → STOP
#                          — the perception-driven-refusal proof
#   (c) STALE PERCEPTION  lidar driver killed → doer holds + cap stale → STILL
#   (d) VERIFIER DEATH    verifier killed → no releases → consumer decels → STOP
#
# This is a GUIDED script: you are the acceptance sensor. Answer honestly; a
# "no" fails the run.
#
# Prereqs (each in its own terminal, ROS 2 sourced, ROS_DOMAIN_ID consistent):
#   1. verifier: KIRRA_GOVERNOR_SIGNING_KEY_SOURCE configured (dev key needs
#      KIRRA_GOVERNOR_SIGNING_KEY_ALLOW_DEV=1) + KIRRA_VEHICLE_CLASS set —
#      minting MUST be on or phase (a) has no frames to relay.
#   2. sidecars + nodes: ros2 launch kirra_safety kirra_with_robot.launch.py \
#        use_perception_cap:=true use_occy_doer:=true kirra_token:=$KIRRA_ADMIN_TOKEN
#      (interceptor wheelbase_m MUST equal the active class contract's — a
#      mismatch latches every command to stop, by design.)
#   3. lidar driver publishing /scan (e.g. ydlidar_ros2_driver, TG30).
#   4. the verifying consumer: python3 robot/kirra_motor_consumer.py with all
#      KIRRA_* config exported, KIRRA_RELEASE_TOPIC=/kirra/release, and
#      KIRRA_GOVERNOR_VK_HEX pinned to the verifier's key.
#   5. NO vendor /cmd_vel motor node running (the consumer owns /dev/myserial).
set -euo pipefail

confirm() {  # confirm "question" -> exits non-zero on anything but y/Y
  read -r -p "$1 [y/N] " ans
  case "$ans" in
    y|Y) return 0 ;;
    *) echo "✗ operator reported failure — LIVE-LOOP TEST FAILED"; exit 1 ;;
  esac
}

echo "=================================================================="
echo "  KIRRA LIVE-LOOP TEST — 🔴 WHEELS MUST BE OFF THE GROUND 🔴"
echo "=================================================================="
read -r -p "Is the robot ELEVATED with all wheels free to spin? [y/N] " ans
[ "$ans" = "y" ] || [ "$ans" = "Y" ] || { echo "Elevate the robot first. Aborting."; exit 1; }

echo
echo "Pre-flight (watch these in spare terminals for the proofs below):"
echo "  ros2 topic echo /kirra/enforcement_action     # interceptor verdicts"
echo "  ros2 topic echo /kirra/perception_health      # Taj corridor health"
echo "  ros2 topic hz   /kirra/release                # signed-frame flow"
if command -v ros2 >/dev/null 2>&1; then
  echo "  active nodes:"; ros2 node list 2>/dev/null | sed 's/^/    /' || true
fi
confirm "Are the verifier (minting ON), launch stack, lidar, and consumer all up?"
confirm "Is every vendor /cmd_vel motor node ABSENT (consumer is the sole /dev/myserial writer)?"

# ---- Phase (a): nominal — perception-driven governed motion -----------------
echo
echo "── Phase (a): NOMINAL — clear corridor, goal ahead ──"
echo "Clear ~2 m in front of the lidar, then send a goal ~1.5 m ahead, e.g.:"
echo "  ros2 topic pub -1 /goal_pose geometry_msgs/msg/PoseStamped \\"
echo "    '{header: {frame_id: odom}, pose: {position: {x: 1.5}, orientation: {w: 1.0}}}'"
echo "Expected: interceptor logs 'release relay ACTIVE', /kirra/release shows"
echo "frames flowing, and the wheels SPIN under governed (clamped) commands."
confirm "Did the wheels SPIN, with 'release relay ACTIVE' in the interceptor log?"

# ---- Phase (b): perception-driven refusal (the load-bearing proof) ----------
echo
echo "── Phase (b): PERCEPTION-DRIVEN REFUSAL — obstacle in the corridor ──"
echo "With the goal still active, place a large obstacle (box/board) ~0.5 m in"
echo "front of the lidar. Expected: Taj's clear-distance collapses, the cap"
echo "drops (watch /kirra/perception_health), the enforcement log shows the"
echo "capped/stopped verdict, and the wheels STOP while the goal is still set."
confirm "Did the wheels STOP with the obstacle in place (goal still active)?"
confirm "Does /kirra/perception_health (or the interceptor log) show the collapsed clear-distance/cap as the cause?"
echo "Remove the obstacle. Expected: motion RESUMES (governed) within ~1 s."
confirm "Did motion resume after removing the obstacle?"

# ---- Phase (c): stale perception → hold --------------------------------------
echo
echo "── Phase (c): STALE PERCEPTION — kill the lidar driver ──"
echo "Stop the lidar driver process (Ctrl-C its terminal). Expected: the doer"
echo "holds ('stale-scan'), the perception cap goes stale → the interceptor"
echo "fails closed, and the wheels are STILL. Nothing may keep driving on the"
echo "last scan."
confirm "Did the wheels stop/stay STILL after the lidar died?"
echo "Restart the lidar driver before the next phase."
confirm "Is the lidar back up (/scan publishing) and motion restored?"

# ---- Phase (d): verifier death → consumer starves → decel-to-zero -----------
echo
echo "── Phase (d): VERIFIER DEATH — kill the verifier process ──"
echo "Kill the verifier (Ctrl-C its terminal). The doer keeps proposing, but no"
echo "releases are minted → the consumer's liveness deadline starves → SS-002"
echo "decel-to-zero. Expected: wheels STOP within the configured liveness"
echo "window, and the interceptor logs CONNECTION_ERROR:STOP (defense in depth:"
echo "both layers fail closed independently)."
confirm "Did the wheels STOP after the verifier was killed?"

echo
echo "=================================================================="
echo "  ✅ LIVE-LOOP TEST PASSED (elevated). All four phases confirmed:"
echo "     (a) nominal governed motion, (b) obstacle → perception-driven"
echo "     refusal + recovery, (c) dead lidar → hold, (d) dead verifier →"
echo "     consumer decel-to-zero."
echo "  Save the terminal logs (interceptor + consumer) as the acceptance"
echo "  record. Only NOW may floor testing be considered."
echo "=================================================================="
