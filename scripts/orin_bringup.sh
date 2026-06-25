#!/usr/bin/env bash
#
# Bring up the full Kirra System-2 stack — Taj · Mick · Occy · KIRRA — on a Jetson
# Orin NX 16GB (ADR-0014), built NATIVELY for aarch64. No GPU, no CARLA, no ROS are
# required: the doer-checker stack is pure Rust compute, so it builds and runs on the
# Orin exactly as it would on the robot's System-2 board.
#
# What it does:
#   1. sanity-checks the toolchain (and the Orin, if that's where you're running it),
#   2. builds the four components + the governance plane (the verifier service),
#   3. runs the headless four-component stack demo (Taj→Mick→Occy→KIRRA),
#   4. optionally starts the KIRRA verifier service (the governance/console plane) and
#      the Occy planner HTTP endpoint, so a ROS 2 / Python client can drive real egos.
#
# Usage:
#   scripts/orin_bringup.sh            # build + run the headless stack demo
#   scripts/orin_bringup.sh --serve    # also start the verifier (:8090) + planner (:8100)
#
# This is the SINGLE-BOX bring-up (Phase 1 of ADR-0014). The stronger two-box topology
# (governor on a separate Pi over the kirra-governor-service UDP wire) is documented in
# docs/testing/ORIN_NX_STACK_BRINGUP.md.
set -euo pipefail
cd "$(dirname "$0")/.."

SERVE=0
[[ "${1:-}" == "--serve" ]] && SERVE=1

echo "== 1. toolchain + platform =="
if ! command -v cargo >/dev/null 2>&1; then
  echo "  cargo not found. Install Rust (rustup) first:"
  echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  exit 1
fi
echo "  rustc: $(rustc --version)"
ARCH="$(uname -m)"
echo "  arch:  $ARCH"
if [[ "$ARCH" == "aarch64" ]]; then
  echo "  → aarch64: native Orin/Jetson build."
  if [[ -r /etc/nv_tegra_release ]]; then
    echo "  → Jetson detected: $(head -1 /etc/nv_tegra_release)"
    echo "    tip: run 'sudo nvpmodel -q' / set a power mode; the governor is ~0 W, System-2 is the budget."
  fi
else
  echo "  → $ARCH: not a Jetson; this still builds the SAME stack (the demo is hardware-agnostic)."
fi

echo "== 2. build the four components + governance plane (release) =="
# The stack demo (Taj→Mick→Occy→KIRRA), the HTTP sidecars (Occy planner + Taj
# perception), and the verifier service. Release on the Orin.
cargo build --release -p kirra-mick --example taj_occy_kirra_stack
cargo build --release -p kirra-mick --example planner_service
cargo build --release -p kirra-mick --example taj_service
cargo build --release --bin kirra_verifier_service

echo "== 3. run the headless stack demo (Taj · Mick · Occy · KIRRA) =="
cargo run --release -p kirra-mick --example taj_occy_kirra_stack

if [[ "$SERVE" == "1" ]]; then
  echo "== 4. start the governance plane + Occy planner + Taj perception sidecars =="
  : "${KIRRA_ADMIN_TOKEN:?set KIRRA_ADMIN_TOKEN (admin/mutation routes fail-closed without it)}"
  : "${KIRRA_SUPERVISOR_RESET_KEY:?set KIRRA_SUPERVISOR_RESET_KEY (non-empty, <=64 bytes)}"
  export KIRRA_DB_PATH="${KIRRA_DB_PATH:-/tmp/kirra_orin.sqlite}"
  export KIRRA_VERIFIER_ADDR="${KIRRA_VERIFIER_ADDR:-127.0.0.1:8090}"
  export KIRRA_TAJ_ADDR="${KIRRA_TAJ_ADDR:-127.0.0.1:8101}"

  echo "  starting KIRRA verifier service on $KIRRA_VERIFIER_ADDR (governance/console plane)…"
  ./target/release/kirra_verifier_service &
  VER=$!
  echo "  starting Occy planner endpoint on 127.0.0.1:8100 (POST /plan)…"
  ./target/release/examples/planner_service &
  PLAN=$!
  echo "  starting Taj perception sidecar on $KIRRA_TAJ_ADDR (POST /perception — the cmd_vel speed cap)…"
  ./target/release/examples/taj_service &
  TAJ=$!
  trap 'kill $VER $PLAN $TAJ 2>/dev/null || true' EXIT INT TERM

  # Wait for the three sidecars to answer their health checks before declaring up.
  for svc in "verifier|http://$KIRRA_VERIFIER_ADDR/health" "planner|http://127.0.0.1:8100/health" "taj|http://$KIRRA_TAJ_ADDR/health"; do
    name="${svc%%|*}"; url="${svc##*|}"
    for _ in $(seq 1 20); do
      curl -sf "$url" >/dev/null 2>&1 && { echo "  ✓ $name up ($url)"; break; }
      sleep 0.5
    done
  done

  echo
  echo "  Stack is up. Drive real egos against it with a ROS 2 node or the Python harness:"
  echo "    KIRRA_VERIFIER_URL=http://$KIRRA_VERIFIER_ADDR KIRRA_ADMIN_TOKEN=\$KIRRA_ADMIN_TOKEN \\"
  echo "      python3 scripts/carla_drive_session.py --occy http://127.0.0.1:8100   # Occy as the doer"
  echo "  On the robot, the ros2_ws/src/kirra_safety launch wires cmd_vel through the same governor,"
  echo "  and Taj's corridor derates it (the perception_governor node POSTs /scan to the sidecar above):"
  echo "    ros2 launch kirra_safety kirra_with_robot.launch.py \\"
  echo "      kirra_token:=\$KIRRA_ADMIN_TOKEN taj_url:=http://$KIRRA_TAJ_ADDR use_perception_cap:=true"
  echo "  Ctrl-C to stop."
  wait
fi
