#!/usr/bin/env bash
# entrypoint.sh — Orin Jazzy runtime dispatcher (ADR-0036, Step 3).
# Sources the Jazzy base + the curated-msgs overlay, then execs the requested
# role. ROS setup.bash references unset vars, so nounset is off during sourcing.
set -eo pipefail

# Base ROS (distro-agnostic resolver, though this image is Jazzy) + the ws overlay.
# shellcheck source=robot/ros_env.sh
source /opt/kirra/robot/ros_env.sh 2>/dev/null && kirra_source_ros || source /opt/ros/jazzy/setup.bash
# shellcheck disable=SC1090
source "${KIRRA_ROS_WS_SETUP:-/opt/kirra/ros2_ws_install/setup.bash}"

ROLE="${1:-adapter}"; shift || true
echo "[orin-jazzy] role=${ROLE}  ROS_DISTRO=${ROS_DISTRO:-?}  ROS_DOMAIN_ID=${ROS_DOMAIN_ID:-?}"

case "${ROLE}" in
  adapter)
    # The checker: bounds whatever the doer proposes on the boundary topics.
    exec /opt/kirra/bin/kirra_ros2_adapter_node --corridor-source "${KIRRA_CORRIDOR_SOURCE:-mock}" "$@"
    ;;
  consumer)
    # The ADR-0033 governed motor consumer (owns the motor board, verify-before-drive).
    exec python3 /opt/kirra/robot/kirra_motor_consumer.py "$@"
    ;;
  mint)
    exec /opt/kirra/bin/kirra_ros_release_mint "$@"
    ;;
  bash|shell)
    exec bash "$@"
    ;;
  *)
    echo "[orin-jazzy] unknown role '${ROLE}' (adapter|consumer|mint|bash)" >&2
    exit 2
    ;;
esac
