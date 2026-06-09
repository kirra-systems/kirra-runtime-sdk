#!/usr/bin/env bash
# #44 ort-cpu PRODUCTION entrypoint.
#
# Sources the ROS 2 Jazzy environment + the curated msgs overlay, runs the
# FAIL-CLOSED backend-load probe, and only then execs the node. If the ORT CPU
# runtime does not actually load the model, the probe exits non-zero and this
# script REFUSES to start the node — there is NO MockBackend fallback in the
# production image.
set -euo pipefail

# ROS setup scripts reference unbound vars under `set -u`; relax around sourcing.
set +u
source /opt/ros/jazzy/setup.bash
source /opt/parko/ros2_ws/install/setup.bash
set -u

# PARKO_BACKEND_PROBE gate: exits 0 ONLY if ORT CPU actually loaded the model.
echo "PARKO_BACKEND_PROBE: validating ORT CPU backend load (${PARKO_MODEL_PATH})..."
/opt/parko/bin/backend_load_probe "${PARKO_MODEL_PATH}"

exec /opt/parko/bin/parko_ros2_node "$@"
