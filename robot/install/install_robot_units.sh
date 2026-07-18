#!/usr/bin/env bash
# install_robot_units.sh — stage the robot-side DOER units (ROS stack + KITT
# watcher) as systemd services, rendered to the invoking user. Companion to
# install_kirra.sh (which stages the consumer) and deploy/systemd/install.sh
# (the governor stack). Does NOT enable — validate each as a service, wheels-up,
# BEFORE enabling (docs/hardware/R2_AUTOSTART_CHECKLIST.md).
#
#   robot/install/install_robot_units.sh
#
# What it does (idempotent):
#   1. copies the KITT scripts the watcher runs to /opt/kirra/robot (install_kirra.sh
#      stages the consumer scripts; these are the KITT/voice ones on top),
#   2. renders __KIRRA_ROBOT_USER__ → the invoking user in both units + installs
#      them to /etc/systemd/system,
#   3. daemon-reload.
#
# Enable deliberately after validation:
#   sudo systemctl enable --now kirra-ros-stack        # occy_doer + interceptor + perception_governor
#   sudo systemctl enable --now kirra-kitt-watch       # Channel-A narration (see the audio-in-service caveat)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${HERE}/../.." && pwd)"
UNITS="${HERE}/systemd"
OPT=/opt/kirra
# The invoking user (works whether or not the script itself is run under sudo).
ROBOT_USER="${SUDO_USER:-${USER}}"

echo "== robot-side unit install — user: ${ROBOT_USER} =="

# ---- 1. KITT scripts -> /opt/kirra/robot -----------------------------------
# The kitt-watch unit runs /opt/kirra/robot/kitt_watch.py, which imports kitt_ask;
# ship the whole KITT/voice set so the interactive tools are staged too.
echo "== 1. KITT scripts -> ${OPT}/robot =="
sudo install -d -m 0755 "${OPT}/robot"
for f in kitt_watch.py kitt_ask.py kitt_converse.py kitt_voice.sh ptt_button.py \
         run_voice_ptt.sh inspect_corridor.py; do
  [[ -f "${REPO}/robot/${f}" ]] || { echo "  ⚠ missing ${REPO}/robot/${f} — skipped"; continue; }
  sudo install -m 0755 "${REPO}/robot/${f}" "${OPT}/robot/${f}"
  echo "  installed ${OPT}/robot/${f}"
done

# ---- 2. render + install the units -----------------------------------------
echo "== 2. units -> /etc/systemd/system =="
for u in kirra-ros-stack.service kirra-kitt-watch.service; do
  [[ -f "${UNITS}/${u}" ]] || { echo "❌ missing ${UNITS}/${u}"; exit 1; }
  sed "s/__KIRRA_ROBOT_USER__/${ROBOT_USER}/g" "${UNITS}/${u}" \
    | sudo tee "/etc/systemd/system/${u}" >/dev/null
  echo "  installed /etc/systemd/system/${u} (User=${ROBOT_USER})"
done

# ---- 3. reload -------------------------------------------------------------
sudo systemctl daemon-reload
echo "== done — STAGED, not enabled =="
echo "  the ROS stack default ws overlay is /home/${ROBOT_USER}/kirra-runtime-sdk/ros2_ws/install/setup.bash"
echo "  (override with KIRRA_ROS_WS_SETUP in /etc/kirra/robot.env if the ws is elsewhere)."
echo
echo "  validate then enable, one at a time:"
echo "    sudo systemctl start kirra-ros-stack   && journalctl -u kirra-ros-stack -f"
echo "    sudo systemctl start kirra-kitt-watch  && journalctl -u kirra-kitt-watch -f"
