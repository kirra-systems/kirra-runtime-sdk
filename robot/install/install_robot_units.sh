#!/usr/bin/env bash
# install_robot_units.sh — stage the robot-side DOER units (ROS stack + Rabbit
# watcher) as systemd services, rendered to the invoking user. Companion to
# install_kirra.sh (which stages the consumer) and deploy/systemd/install.sh
# (the governor stack). Does NOT enable — validate each as a service, wheels-up,
# BEFORE enabling (docs/hardware/R2_AUTOSTART_CHECKLIST.md).
#
#   robot/install/install_robot_units.sh
#
# What it does (idempotent):
#   1. copies the Rabbit scripts the watcher runs to /opt/kirra/robot (install_kirra.sh
#      stages the consumer scripts; these are the Rabbit/voice ones on top),
#   2. renders __KIRRA_ROBOT_USER__ → the invoking user in both units + installs
#      them to /etc/systemd/system,
#   3. daemon-reload.
#
# Enable deliberately after validation:
#   sudo systemctl enable --now kirra-ros-stack        # occy_doer + interceptor + perception_governor
#   sudo systemctl enable --now kirra-rabbit-watch       # Channel-A narration (see the audio-in-service caveat)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${HERE}/../.." && pwd)"
UNITS="${HERE}/systemd"
OPT=/opt/kirra
# The invoking user (works whether or not the script itself is run under sudo).
ROBOT_USER="${SUDO_USER:-${USER}}"

echo "== robot-side unit install — user: ${ROBOT_USER} =="

# ---- 1. Rabbit scripts -> /opt/kirra/robot -----------------------------------
# The rabbit-watch unit runs /opt/kirra/robot/rabbit_watch.py, which imports rabbit_ask;
# ship the whole Rabbit/voice set so the interactive tools are staged too.
echo "== 1. Rabbit scripts -> ${OPT}/robot =="
sudo install -d -m 0755 "${OPT}/robot"
for f in rabbit_persona.py rabbit_watch.py rabbit_ask.py rabbit_converse.py rabbit_boot.py \
         rabbit_voice.sh ptt_button.py run_voice_ptt.sh inspect_corridor.py rabbit_ota.py \
         ros_env.sh kirra_voice_doctor.sh kirra_doctor.py rabbit_diag.py \
         wake_word.py rabbit_wake.py turn_state.py vad_record.py pulse_capture.sh \
         barge_in.py skill_registry.py world_model.py mission.py; do
  [[ -f "${REPO}/robot/${f}" ]] || { echo "  ⚠ missing ${REPO}/robot/${f} — skipped"; continue; }
  sudo install -m 0755 "${REPO}/robot/${f}" "${OPT}/robot/${f}"
  echo "  installed ${OPT}/robot/${f}"
done
# The diagnostics framework package (docs/diagnostics.md) — a directory, so cp -r.
if [[ -d "${REPO}/robot/doctor" ]]; then
  sudo rm -rf "${OPT}/robot/doctor"
  sudo cp -r "${REPO}/robot/doctor" "${OPT}/robot/doctor"
  sudo find "${OPT}/robot/doctor" -type d -exec chmod 0755 {} +
  sudo find "${OPT}/robot/doctor" -type f -exec chmod 0644 {} +
  echo "  installed ${OPT}/robot/doctor/ (diagnostics modules)"
fi

# ---- 2. render + install the units -----------------------------------------
echo "== 2. units -> /etc/systemd/system =="
for u in kirra-ros-stack.service kirra-lidar.service \
         kirra-rabbit-watch.service kirra-rabbit-greet.service \
         kirra-rabbit-voice.service \
         kirra-ota-check.service kirra-ota-check.timer \
         kirra-doctor.service kirra-doctor.timer; do
  [[ -f "${UNITS}/${u}" ]] || { echo "❌ missing ${UNITS}/${u}"; exit 1; }
  sed "s/__KIRRA_ROBOT_USER__/${ROBOT_USER}/g" "${UNITS}/${u}" \
    | sudo tee "/etc/systemd/system/${u}" >/dev/null
  echo "  installed /etc/systemd/system/${u} (User=${ROBOT_USER})"
done

# ---- 2b. the USER unit (Pulse-capable voice listener) ------------------------
# On a unit with an active user audio session the sound server owns the mic
# (direct ALSA capture fails EBUSY) — the voice listener must run INSIDE the
# user's session so pulse_capture.sh can reach the per-user Pulse socket. See
# robot/install/systemd/user/rabbit-voice.service for the full rationale.
echo "== 2b. user unit -> ~${ROBOT_USER}/.config/systemd/user =="
USER_HOME="$(getent passwd "${ROBOT_USER}" | cut -d: -f6)"
USER_UNIT_DIR="${USER_HOME}/.config/systemd/user"
sudo -u "${ROBOT_USER}" mkdir -p "${USER_UNIT_DIR}"
sudo install -m 0644 "${UNITS}/user/rabbit-voice.service" "${USER_UNIT_DIR}/rabbit-voice.service"
sudo chown "${ROBOT_USER}:" "${USER_UNIT_DIR}/rabbit-voice.service"
echo "  installed ${USER_UNIT_DIR}/rabbit-voice.service (user unit — Pulse-capable)"

# ---- 3. reload -------------------------------------------------------------
sudo systemctl daemon-reload
echo "== done — STAGED, not enabled =="
echo "  the ROS stack default ws overlay is /home/${ROBOT_USER}/kirra-runtime-sdk/ros2_ws/install/setup.bash"
echo "  (override with KIRRA_ROS_WS_SETUP in /etc/kirra/robot.env if the ws is elsewhere)."
echo
echo "  validate then enable, one at a time:"
echo "    sudo systemctl start kirra-ros-stack   && journalctl -u kirra-ros-stack -f"
echo "    sudo systemctl start kirra-rabbit-watch  && journalctl -u kirra-rabbit-watch -f"
echo "    sudo systemctl start kirra-rabbit-voice  && journalctl -u kirra-rabbit-voice -f"
echo "      (wake word is OPT-IN: also set KIRRA_WAKE_ENABLED=1 + KIRRA_WAKE_STT_CMD in robot.env)"
echo "  when the mic is owned by the user session's sound server (desktop image;"
echo "  direct ALSA capture fails EBUSY), use the USER unit instead — as ${ROBOT_USER}:"
echo "    systemctl --user daemon-reload && systemctl --user start rabbit-voice"
echo "      (needs KIRRA_PULSE_SOURCE in robot.env; loginctl enable-linger ${ROBOT_USER} for boot)"
echo
echo "  the Engineering Assistant is enabled by env.template on a FRESH install."
echo "  An EXISTING /etc/kirra/robot.env is never overwritten (it holds measured"
echo "  calibration), so upgrading robots need the line appended once:"
echo "    grep -q '^KIRRA_ASSIST_ENABLED=' /etc/kirra/robot.env \\"
echo "      || echo 'KIRRA_ASSIST_ENABLED=1' | sudo tee -a /etc/kirra/robot.env"
echo "    sudo systemctl restart kirra-rabbit-voice"
echo "  Check it with: robot/install/verify_deployment.py"
