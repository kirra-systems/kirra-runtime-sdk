#!/usr/bin/env bash
# install_kirra.sh — Layer A: the base-agnostic KIRRA robot install.
#
# Turns a fresh Yahboom-flashed Orin NX (Ubuntu 22.04 / JetPack, ROS 2 Humble,
# vendor Rosmaster_Lib + ydlidar driver preinstalled) into a robot running the
# validated KIRRA stack: the fenced verifying motor consumer + the TG30 lidar,
# in the CURRENT validated mode (car-type 1, straight-line capable).
#
# 🔴 LAYER A ONLY. This deliberately contains NO steering config and NO
# set_car_type — the steering/R2 platform layer is BLOCKED on the vendor R2
# base image and is documented (not implemented) in PLATFORM_R2_PENDING.md.
#
# What it does (idempotent; loud on missing prereqs):
#   1. preflight checks (ROS 2 Humble, Rosmaster_Lib, cargo, device symlinks)
#   2. builds the verify-core cdylib + the dev mint binary (--skip-build to reuse)
#   3. installs artifacts to /opt/kirra/{lib,bin,robot}
#   4. renders /etc/kirra/robot.env from env.template IF ABSENT (never overwrites);
#      --dev-key fills the governor VK from the well-known DEV seed (bench only)
#   5. verifies the ydlidar driver is present + prints the validated launch line
#   6. stages (does NOT enable) the optional consumer systemd unit
#   7. prints the verification checklist
#
# Usage:
#   robot/install/install_kirra.sh [--dev-key] [--skip-build]
#
# Run as the normal robot user (e.g. jetson); privileged steps use sudo.
set -euo pipefail

DEV_SEED=2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${HERE}/../.." && pwd)"
OPT=/opt/kirra
ENVDIR=/etc/kirra
ENVFILE="${ENVDIR}/robot.env"
ROS_SETUP=/opt/ros/humble/setup.bash

USE_DEV_KEY=0
SKIP_BUILD=0
for arg in "$@"; do
  case "${arg}" in
    --dev-key)    USE_DEV_KEY=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    *) echo "error: unknown argument '${arg}' (known: --dev-key --skip-build)"; exit 2 ;;
  esac
done

fail() { echo "❌ $*" >&2; exit 1; }
warn() { echo "⚠  $*" >&2; }
ok()   { echo "✔ $*"; }

echo "== KIRRA robot install (Layer A) — repo: ${REPO} =="

# ---- 1. preflight ----------------------------------------------------------
echo "== 1. preflight =="
command -v sudo >/dev/null || fail "sudo is required (privileged install steps)"
command -v python3 >/dev/null || fail "python3 not found"

if [[ -f "${ROS_SETUP}" ]]; then
  ok "ROS 2 Humble found (${ROS_SETUP})"
else
  fail "ROS 2 Humble not found at ${ROS_SETUP} — the vendor Yahboom image ships it; on a bare 22.04 install it manually first (manual step, see README.md)"
fi

# Vendor motor-board library (runtime dep of the consumer; ships on the
# Yahboom image, pip-installed — NOT in this repo).
if python3 -c 'import Rosmaster_Lib' 2>/dev/null; then
  ok "Rosmaster_Lib importable"
else
  fail "Rosmaster_Lib not importable — install the vendor library from the Yahboom image/SDK (manual step, see README.md). The consumer cannot run without it."
fi

if [[ ${SKIP_BUILD} -eq 0 ]]; then
  command -v cargo >/dev/null || fail "cargo not found — install rustup (https://rustup.rs); the repo's rust-toolchain.toml pins the toolchain automatically. Or build elsewhere for aarch64 and re-run with --skip-build."
fi

# Device symlinks (vendor udev rules; validated unit: myserial->ttyUSB1,
# ydlidar->ttyUSB0). Missing devices don't block the BUILD, but the robot
# cannot run without them — warn loudly.
for dev in /dev/myserial /dev/ydlidar; do
  if [[ -e "${dev}" ]]; then
    ok "device ${dev} -> $(readlink -f "${dev}")"
  else
    warn "device ${dev} ABSENT — vendor udev rule missing or hardware unplugged. Restore the captured udev rules (see capture_from_robot.sh / REFLASH.md) before running the robot."
  fi
done

# ---- 2. build ---------------------------------------------------------------
echo "== 2. build (verify-core cdylib + dev mint binary) =="
SO="${REPO}/target/release/libkirra_consumer_ffi.so"
MINT="${REPO}/target/release/kirra_ros_release_mint"
if [[ ${SKIP_BUILD} -eq 1 ]]; then
  echo "  --skip-build: using existing artifacts"
else
  (cd "${REPO}" \
    && cargo build --locked --release -p kirra-consumer-ffi \
    && cargo build --locked --release -p kirra-release-token --bin kirra_ros_release_mint)
fi
[[ -f "${SO}" ]]   || fail "missing ${SO} — build failed or --skip-build without prior build"
[[ -x "${MINT}" ]] || fail "missing ${MINT} — build failed or --skip-build without prior build"
ok "artifacts present"

# ---- 3. install artifacts ---------------------------------------------------
echo "== 3. install -> ${OPT} =="
sudo install -d -m 0755 "${OPT}/lib" "${OPT}/bin" "${OPT}/robot"
sudo install -m 0644 "${SO}"   "${OPT}/lib/libkirra_consumer_ffi.so"
sudo install -m 0755 "${MINT}" "${OPT}/bin/kirra_ros_release_mint"
for f in kirra_motor_consumer.py kirra_ffi.py kirra_release_publisher.py r2_drive.py; do
  sudo install -m 0644 "${REPO}/robot/${f}" "${OPT}/robot/${f}"
done
for f in first_run_elevated.sh live_loop_elevated.sh steering_bench_elevated.sh; do
  sudo install -m 0755 "${REPO}/robot/${f}" "${OPT}/robot/${f}"
done
ok "installed cdylib, mint binary, consumer + scripts"

# ---- 4. environment ---------------------------------------------------------
echo "== 4. environment -> ${ENVFILE} =="
sudo install -d -m 0755 "${ENVDIR}"
if [[ -f "${ENVFILE}" ]]; then
  ok "${ENVFILE} exists — leaving it untouched (your pinned key/config preserved)"
else
  TMP_ENV="$(mktemp)"
  cp "${HERE}/env.template" "${TMP_ENV}"
  if [[ ${USE_DEV_KEY} -eq 1 ]]; then
    # 🔴 DEV/DEMO ONLY (bench). Production units enroll a real governor key
    # instead — see docs/safety/GOVERNOR_KEY_PROVISIONING.md.
    VK="$("${MINT}" --seed "${DEV_SEED}" pubkey)"
    [[ ${#VK} -eq 64 ]] || fail "mint pubkey returned unexpected output: '${VK}'"
    sed -i "s/^KIRRA_GOVERNOR_VK_HEX=.*/KIRRA_GOVERNOR_VK_HEX=${VK}/" "${TMP_ENV}"
    warn "governor VK pinned to the WELL-KNOWN DEV KEY (--dev-key). Bench only — never a production/golden unit."
  else
    warn "KIRRA_GOVERNOR_VK_HEX left as a placeholder — the consumer will refuse to start until you pin a key (re-run with --dev-key for the bench, or enroll a real key)."
  fi
  sudo install -m 0644 "${TMP_ENV}" "${ENVFILE}"
  rm -f "${TMP_ENV}"
  ok "rendered ${ENVFILE} from env.template"
fi

# ---- 5. lidar (validated: ydlidar TG30 @ 512000 on /dev/ydlidar) ------------
echo "== 5. lidar driver check =="
# ROS setup scripts reference unset vars — relax nounset around the source.
set +u
# shellcheck disable=SC1091
source "${ROS_SETUP}"
set -u
if ros2 pkg prefix ydlidar_ros2_driver >/dev/null 2>&1; then
  ok "ydlidar_ros2_driver present ($(ros2 pkg prefix ydlidar_ros2_driver))"
else
  fail "ydlidar_ros2_driver NOT found. The validated unit had it vendor-preinstalled; a from-source build is documented (but NOT hardware-validated) in README.md. The lidar is required for the live loop."
fi
echo "  validated launch (installer/platform_map.toml:31-37 — TG30, 512000 baud):"
echo "    ros2 launch ydlidar_ros2_driver ydlidar_launch.py"
echo "  with the CAPTURED params (port /dev/ydlidar, baud 512000) — see"
echo "  capture_from_robot.sh + README.md 'Config capture'."

# ---- 6. systemd (staged, NOT enabled) ---------------------------------------
echo "== 6. systemd unit (optional; staged, not enabled) =="
# Render the unit's User= to the invoking user (review #906: never hard-code
# an image-specific username). Serial access needs dialout membership.
ROBOT_USER="$(id -un)"
if id -nG "${ROBOT_USER}" | tr ' ' '\n' | grep -qx dialout; then
  ok "user '${ROBOT_USER}' is in dialout (serial access)"
else
  warn "user '${ROBOT_USER}' is NOT in dialout — the consumer (and this unit) cannot open /dev/myserial. Fix: sudo usermod -aG dialout ${ROBOT_USER} (then re-login)."
fi
TMP_UNIT="$(mktemp)"
sed "s/^User=__KIRRA_ROBOT_USER__$/User=${ROBOT_USER}/" \
  "${HERE}/systemd/kirra-consumer.service" > "${TMP_UNIT}"
grep -q "^User=${ROBOT_USER}$" "${TMP_UNIT}" || fail "failed to render User= into the systemd unit"
sudo install -m 0644 "${TMP_UNIT}" /etc/systemd/system/kirra-consumer.service
rm -f "${TMP_UNIT}"
sudo systemctl daemon-reload
warn "kirra-consumer.service staged but NOT enabled: the consumer-as-a-service path has NOT been hardware-validated (the validated mode is a terminal run). Enable deliberately after an elevated re-test: sudo systemctl enable --now kirra-consumer"

# ---- 7. verification checklist ----------------------------------------------
echo
echo "== done — verification (see README.md §Verification for the full text) =="
cat <<'EOF'
  1. Consumer starts and OWNS the motor board (terminal, validated mode):
       set -a; source /etc/kirra/robot.env; set +a
       source /opt/ros/humble/setup.bash
       python3 /opt/kirra/robot/kirra_motor_consumer.py
     Expect: "KIRRA consumer OWNS /dev/myserial (sole writer)..." and NO
     car-type FATAL. 🔴 The vendor base node must NOT be running.
  2. Lidar publishes:
       ros2 topic hz /scan        # steady ~10 Hz (TG30)
       ros2 topic echo /scan --once   # finite room-plausible ranges
  3. First governed motion: robot/first_run_elevated.sh — 🔴 WHEELS ELEVATED.
EOF
