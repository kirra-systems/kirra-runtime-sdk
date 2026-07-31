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
# OVERLAY: run this with the workspace that provides ydlidar_ros2_driver already
# sourced. sudo does not inherit it, and the vendor driver lives OUTSIDE this
# repo (the validated unit had it under ~/yahboomcar_ros2_ws), so a plain
# `sudo install_kirra.sh` reports the driver missing even when it is installed:
#
#   sudo env "PATH=$HOME/.cargo/bin:$PATH" bash -c '
#     source /opt/ros/humble/setup.bash
#     source "$HOME/yahboomcar_ros2_ws/software/library_ws/install/setup.bash"
#     exec robot/install/install_kirra.sh'
#
# The lidar check is non-fatal in place and reported at the end, so a missing
# driver no longer prevents the unit, manifest and identity steps from running.
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
# Distro-agnostic ROS 2 (Jazzy 24.04 or Humble 22.04, per ADR-0036;
# override with KIRRA_ROS_SETUP / KIRRA_ROS_DISTRO_PREF).
# shellcheck source=robot/ros_env.sh
source "${REPO}/robot/ros_env.sh"
ROS_SETUP="$(kirra_ros_setup_path || true)"

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

if [[ -n "${ROS_SETUP}" && -f "${ROS_SETUP}" ]]; then
  ok "ROS 2 $(kirra_ros_distro) found (${ROS_SETUP})"
else
  fail "ROS 2 not found under /opt/ros (looked for: ${KIRRA_ROS_DISTRO_PREF:-jazzy humble}) — the vendor Yahboom image ships Humble; on a bare 22.04/24.04 install it manually first (manual step, see README.md). Override the path with KIRRA_ROS_SETUP=/opt/ros/<distro>/setup.bash"
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

# ADR-0033 Tier-3 (#887): tighten the motor port to the consumer user, 0600.
# The vendor rule ships it 0777 (world-writable actuation below the checker);
# this 99- rule overrides owner/mode while keeping the /dev/myserial symlink.
# The consumer's boot sentinel refuses to start until this holds (or
# KIRRA_ALLOW_SHARED_SERIAL=1 acknowledges a bring-up run).
KIRRA_USER="${SUDO_USER:-${USER}}"
RULE_SRC="${HERE}/99-kirra-serial-exclusivity.rules"
RULE_DST="/etc/udev/rules.d/99-kirra-serial-exclusivity.rules"
if [[ -f "${RULE_SRC}" ]]; then
  sed "s/__KIRRA_ROBOT_USER__/${KIRRA_USER}/g" "${RULE_SRC}" | sudo tee "${RULE_DST}" >/dev/null
  sudo udevadm control --reload 2>/dev/null || true
  sudo udevadm trigger 2>/dev/null || true
  ok "serial-exclusivity udev rule installed (${RULE_DST}, OWNER=${KIRRA_USER}, MODE=0600) — replug the board if perms don't update"
else
  warn "99-kirra-serial-exclusivity.rules not found beside the installer — the motor port stays on the vendor 0777 rule and the consumer sentinel will refuse to start"
fi

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
# The consumer is what kirra-consumer.service execs, so it must be EXECUTABLE —
# installing it 0644 with the rest left the unit pointing at a non-executable
# file and verify_deployment.py reporting it as such.
sudo install -m 0755 "${REPO}/robot/kirra_motor_consumer.py" \
                     "${OPT}/robot/kirra_motor_consumer.py"
# Imported modules, never exec'd, so 0644 is correct for these.
# motor_authority.py was missing entirely: robot/doctor/modules/devices.py
# imports it at runtime, so a deployed robot needs it beside the consumer.
# EVERY module the consumer imports, at any depth. Three were missing and a
# fresh install could not start:
#
#   clock_step_guard.py   imported at MODULE SCOPE (line 99) — this one fails
#                         FIRST, with ModuleNotFoundError, before the process
#                         reaches anything else
#   serial_exclusivity.py the #887 Tier-3 boot sentinel, imported in main()
#   kirra_r2cp.py         the R2CP last hop, imported under that drive mode
#
# Every deployed robot survived only because it already carried untracked copies
# from some earlier manual step, which is why this never surfaced: the machines
# that would have failed were the ones nobody had built yet.
#
# The closure is asserted by test_a_fresh_install_can_import_the_consumer, which
# copies exactly what these lines install into an EMPTY directory and imports the
# consumer there — an untracked leftover in /opt/kirra cannot satisfy it.
for f in kirra_ffi.py kirra_release_publisher.py r2_drive.py motor_authority.py \
         serial_exclusivity.py clock_step_guard.py kirra_r2cp.py; do
  sudo install -m 0644 "${REPO}/robot/${f}" "${OPT}/robot/${f}"
done

# The VERIFIER itself, plus the two modules it imports. Without this it runs
# only from a checkout, so its verdict describes whatever that checkout
# believes rather than the robot: on 2026-07-31 a stale checkout reported a
# stale expectation against a correctly-deployed machine, and the FAIL was an
# artefact of where the tool was read from. Installed here it lands beside its
# imports, is digested by the manifest below like every other artifact, and can
# be run on a robot with no repo at all.
sudo install -m 0755 "${REPO}/robot/install/verify_deployment.py" \
                     "${OPT}/robot/verify_deployment.py"
sudo install -m 0644 "${REPO}/robot/consumer_config_contract.py" \
                     "${OPT}/robot/consumer_config_contract.py"
sudo install -m 0644 "${REPO}/robot/install/preflight_consumer_env.py" \
                     "${OPT}/robot/preflight_consumer_env.py"
for f in first_run_elevated.sh live_loop_elevated.sh steering_bench_elevated.sh; do
  sudo install -m 0755 "${REPO}/robot/${f}" "${OPT}/robot/${f}"
done
# The distro-agnostic ROS resolver the consumer unit sources (ADR-0036).
sudo install -m 0644 "${REPO}/robot/ros_env.sh" "${OPT}/robot/ros_env.sh"
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
  # The evidence-bound V2 profile digest can NEVER be auto-filled: unlike the
  # dev VK it is not derivable from a key, it is a property of THIS deployment
  # (Taj computes it from the perception request for this robot). It stays a
  # placeholder under --dev-key too, so say so explicitly.
  warn "KIRRA_PROFILE_DIGEST left as a placeholder — the consumer will refuse to start until you pin this robot's 64-lowercase-hex platform profile digest. Obtain it with: python3 ${OPT}/robot/kirra_doctor.py --module profile_digest"
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
# DEFERRED failure. A missing lidar driver still fails the install, but the
# verdict is reported at the END rather than aborting here — see the exit at the
# bottom. Aborting at this point skipped the systemd unit render, the installed-
# artifact manifest and the deployment-identity block, all of which come later
# and none of which have anything to do with the lidar. On 2026-07-31 that
# ordering meant a robot whose unit carried the wrong User= could not have it
# corrected by re-running the installer: every attempt died here first. A robot
# that cannot render its unit is a worse failure than one that cannot find its
# lidar, so the deployment work now completes either way.
LIDAR_MISSING=0
if ros2 pkg prefix ydlidar_ros2_driver >/dev/null 2>&1; then
  ok "ydlidar_ros2_driver present ($(ros2 pkg prefix ydlidar_ros2_driver))"
else
  LIDAR_MISSING=1
  warn "ydlidar_ros2_driver NOT found — the install will CONTINUE and then fail at the end. The validated unit had it vendor-preinstalled; a from-source build is documented (but NOT hardware-validated) in README.md. The lidar is required for the live loop. If the driver lives in an overlay workspace (the validated unit had it under ~/yahboomcar_ros2_ws), source that overlay before running this installer — see the usage note at the top of this script."
fi
echo "  validated launch (installer/platform_map.toml:31-37 — TG30, 512000 baud):"
echo "    ros2 launch ydlidar_ros2_driver ydlidar_launch.py"
echo "  with the CAPTURED params (port /dev/ydlidar, baud 512000) — see"
echo "  capture_from_robot.sh + README.md 'Config capture'."

# ---- 6. systemd (staged, NOT enabled) ---------------------------------------
echo "== 6. systemd unit (optional; staged, not enabled) =="
# Render the unit's User= to the invoking user (review #906: never hard-code
# an image-specific username). Serial access needs dialout membership.
# Must match KIRRA_USER above (the udev rule's OWNER). `id -un` is WRONG here:
# this installer is documented to run under sudo, so it returns root, and the
# unit was rendered User=root. Root then bypasses the 0600 owner check on
# /dev/myserial, so serial kept working and nothing surfaced — but Fast DDS
# shared memory does NOT bypass: the reader's /dev/shm port segment is created
# root:root 0644, the robot-user publisher cannot write into it, and release
# frames are never delivered. Discovery still matches (UDP multicast) and timers
# still fire (local), which is exactly the "timer runs, subscription never does"
# fault. Derive it the same way as line 97 or the two drift apart again.
ROBOT_USER="${SUDO_USER:-${USER}}"
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

# ---- 6a. record what was installed ------------------------------------------
# Digest every artifact this installer places, so a later edit to a DEPLOYED
# file is detectable. On 2026-07-31 a consumer was hand-patched in place while
# diagnosing; the deployed file silently stopped matching its source, and hours
# of later reasoning were done against a file that was no longer the one
# running. Nothing reported it because nothing had recorded what was installed.
#
# Observability only — verify_deployment.py WARNs on a mismatch, nothing is
# gated on this file, and a bring-up edit stays legitimate. What it stops being
# is invisible.
MANIFEST="${OPT}/installed.sha256"
{
  for _m in "${OPT}"/robot/*.py "${OPT}"/robot/*.sh \
            "${OPT}/lib/libkirra_consumer_ffi.so" \
            "${OPT}/bin/kirra_ros_release_mint"; do
    [[ -f "${_m}" ]] && sha256sum "${_m}"
  done
} | sudo tee "${MANIFEST}" >/dev/null
sudo chmod 0644 "${MANIFEST}"
ok "recorded $(wc -l < "${MANIFEST}") installed artifact digests -> ${MANIFEST}"

# ---- 6b. deployment identity ------------------------------------------------
# Print the four facts that must agree, and FAIL LOUDLY when they do not.
#
# Both faults in the 2026-07-31 incident were invisible mismatches between these
# lines. The unit ran as root while the udev rule owned the port to the robot
# user, so Fast DDS shared-memory segments were unwritable by the publisher and
# release frames were never delivered. Separately, KIRRA_CONSUMER_LIB pointed at
# a path the installer does not write, so the consumer loaded an old verify core
# and every rebuild changed nothing. Every individual component reported healthy
# throughout; nothing printed them side by side.
echo
echo "== deployment identity (these four must agree) =="
_id_unit="$(sed -n 's/^User=//p' /etc/systemd/system/kirra-consumer.service 2>/dev/null || true)"
_id_port="$(stat -c '%U' -L /dev/myserial 2>/dev/null || echo '<absent>')"
_id_lib="$(sed -n 's/^KIRRA_CONSUMER_LIB=//p' /etc/kirra/robot.env 2>/dev/null || true)"
_id_lib_installed="${OPT}/lib/libkirra_consumer_ffi.so"
[[ -n "${_id_lib}" ]] || _id_lib="<unset — defaults to ${_id_lib_installed}>"

printf '  configured robot user : %s\n' "${ROBOT_USER}"
printf '  systemd User=         : %s\n' "${_id_unit:-<unset>}"
printf '  /dev/myserial owner   : %s\n' "${_id_port}"
printf '  KIRRA_CONSUMER_LIB    : %s\n' "${_id_lib}"
printf '  library exists        : %s\n' \
  "$([[ -f "${_id_lib_installed}" ]] && echo yes || echo NO)"

_id_bad=0
[[ "${_id_unit}" == "${ROBOT_USER}" ]] || {
  warn "systemd User='${_id_unit:-<unset>}' != robot user '${ROBOT_USER}' — a consumer running as another user cannot exchange DDS shared-memory data with same-host publishers, while discovery still matches and timers still fire"
  _id_bad=1
}
# The port is absent until the board is plugged in; that is not a mismatch.
if [[ "${_id_port}" != "<absent>" && "${_id_port}" != "${ROBOT_USER}" ]]; then
  warn "/dev/myserial is owned by '${_id_port}', not '${ROBOT_USER}' — replug the board or re-trigger udev; the consumer's boot sentinel will refuse to start"
  _id_bad=1
fi
if [[ "${_id_lib}" != "<unset"* && "${_id_lib}" != "${_id_lib_installed}" ]]; then
  warn "KIRRA_CONSUMER_LIB='${_id_lib}' is not where this installer writes the cdylib (${_id_lib_installed}) — the consumer would load a library no rebuild replaces"
  _id_bad=1
fi
[[ "${_id_bad}" -eq 0 ]] && ok "deployment identity consistent"

# ---- 7. verification checklist ----------------------------------------------
echo
echo "== done — verification (see README.md §Verification for the full text) =="
cat <<'EOF'
  1. Consumer starts and OWNS the motor board (terminal, validated mode):
       set -a; source /etc/kirra/robot.env; set +a
       source /opt/ros/humble/setup.bash    # or /opt/ros/jazzy on 24.04
       python3 /opt/kirra/robot/kirra_motor_consumer.py
     Expect: "KIRRA consumer OWNS /dev/myserial (sole writer)..." and NO
     car-type FATAL. 🔴 The vendor base node must NOT be running.
  2. Lidar publishes:
       ros2 topic hz /scan        # steady ~10 Hz (TG30)
       ros2 topic echo /scan --once   # finite room-plausible ranges
  3. First governed motion: robot/first_run_elevated.sh — 🔴 WHEELS ELEVATED.
EOF

# ---- 8. deferred verdict ----------------------------------------------------
# Everything above ran. If the lidar driver was missing, the install is still a
# FAILURE and exits non-zero — the exit-code contract is unchanged, only its
# position moved, so the deployment steps that come after step 5 are no longer
# hostage to an unrelated driver check.
if [[ "${LIDAR_MISSING}" -eq 1 ]]; then
  fail "ydlidar_ros2_driver NOT found (reported at step 5). The deployment steps above COMPLETED — unit, manifest and identity are installed — but the live loop cannot run without the lidar. Install the driver, or source the overlay that provides it, and re-run."
fi
