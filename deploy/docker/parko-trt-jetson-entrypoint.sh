#!/usr/bin/env bash
# PARK-021 / ADR-0036 GPU-doer entrypoint (Jetson / l4t-jetpack).
#
# Sources ROS 2 Humble + the curated msgs overlay, resolves the TensorRT-enabled
# ONNX Runtime .so, runs the FAIL-CLOSED TensorRT probe, and only then execs the
# node. With PARKO_TRT_REQUIRE_EP=1 (the image default) a missing TensorRT
# execution provider is a HARD failure — the node REFUSES to start rather than
# silently degrading to CPU. No MockBackend fallback in this image.
set -euo pipefail

# ROS setup scripts reference unbound vars under `set -u`; relax around sourcing.
set +u
source /opt/ros/humble/setup.bash
source /opt/parko/ros2_ws/install/setup.bash
set -u

# Resolve the TRT-enabled ORT dylib (the JetPack-6 onnxruntime-gpu wheel .so) if
# the operator did not pin ORT_DYLIB_PATH explicitly.
if [ -z "${ORT_DYLIB_PATH:-}" ]; then
  ORT_DYLIB_PATH="$(find /opt/ort-gpu -path '*/onnxruntime/capi/libonnxruntime.so*' -type f 2>/dev/null | head -n1)"
  export ORT_DYLIB_PATH
fi
if [ -z "${ORT_DYLIB_PATH:-}" ] || [ ! -f "${ORT_DYLIB_PATH}" ]; then
  echo "FATAL: no TensorRT-enabled ONNX Runtime .so found (ORT_DYLIB_PATH). See deploy/docker/PARKO_TRT_JETSON.md." >&2
  exit 1
fi
echo "parko-trt: ORT_DYLIB_PATH=${ORT_DYLIB_PATH}"

# Fail-closed TensorRT probe (Q-1b int8_qdq gate). PARKO_TRT_REQUIRE_EP=1 makes a
# missing EP a hard failure; the probe builds a real TRT engine to prove it works.
echo "PARKO_TRT_PROBE: validating the TensorRT execution provider (require_ep=${PARKO_TRT_REQUIRE_EP:-0})..."
if ! /opt/parko/bin/parko_trt_probe --nocapture; then
  echo "FATAL: TensorRT fail-closed probe did not pass — refusing to start the node." >&2
  echo "       Confirm --runtime nvidia, the l4t CUDA/TRT libs, and the onnxruntime-gpu wheel." >&2
  exit 1
fi

exec /opt/parko/bin/parko_ros2_node "$@"
