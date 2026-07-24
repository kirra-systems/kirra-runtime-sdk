#!/usr/bin/env bash
# rabbit_voice.sh — TALK to Rabbit by voice: trigger → record → STT → rabbit_converse.
#
# The missing glue between the mic and the conversation. Each TRIGGER (a line on
# this script's stdin) records one bounded clip, transcribes it, and feeds the
# TEXT to robot/rabbit_converse.py — which answers in persona (SPEAK) or hands a
# driving directive to the ONE fail-closed door (mick POST /intent → Occy → the
# KIRRA checker). speech_shell goes straight to /intent (command-only, Stage 0);
# THIS routes voice through the full Rabbit dialogue (Stages 2-3).
#
# The trigger source is whatever feeds stdin — pick one:
#   with a GPIO button:   python3 robot/ptt_button.py | ./robot/rabbit_voice.sh
#   with the keyboard:     ./robot/rabbit_voice.sh        # press Enter to talk
#
# 🔴 Voice cannot bypass the fence: a transcript is TEXT handed to the door, the
# same as typing. A misheard clip dead-ends in MickIntent::parse_llm_json (no
# intent, no motion). This is the mic, NOT the e-stop (a separate hardware kill).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Optional shared env (STT/TTS/RECORD live here — see robot/install/rabbit.env.example)
if [ -f /etc/kirra/robot.env ]; then set -a; . /etc/kirra/robot.env; set +a; fi

# Perception grounding ("what do you see") needs ROS in the process env so
# rabbit_converse's rclpy can subscribe to /scan. A systemd service does NOT
# inherit a login shell's ROS setup, so under the service that subscribe fails and
# perception reads "unavailable" even though the lidar is publishing — while it
# works fine from a sourced terminal. Source ROS best-effort here so the service is
# self-sufficient; fail-soft (a no-op if ROS isn't installed — the perception grab
# already degrades gracefully). ROS_DOMAIN_ID must match the lidar's — set it in
# robot.env (the sourced env above exports it), else rclpy defaults to domain 0 and
# never sees /scan.
if [ -f "${HERE}/ros_env.sh" ]; then
  # shellcheck source=/dev/null
  . "${HERE}/ros_env.sh"
  kirra_source_ros || true
  if [ -n "${KIRRA_ROS_WS_SETUP:-}" ] && [ -f "${KIRRA_ROS_WS_SETUP}" ]; then
    set +u; . "${KIRRA_ROS_WS_SETUP}" || true; set -u
  fi
fi

if [ -z "${KIRRA_STT_CMD:-}" ]; then
  echo "FATAL: KIRRA_STT_CMD required (e.g. \"whisper-cli -m models/ggml-base.en.bin -np -nt -f\")" >&2
  exit 1
fi
: "${KIRRA_RECORD_CMD:=arecord -d 4 -f S16_LE -r 16000 -c 1}"

# Producer: one transcript line per trigger. STT/RECORD are word-split command
# lists (the house convention: the WAV path is APPENDED as the last argument).
transcribe() {
  local wav text
  while IFS= read -r _; do
    wav="$(mktemp --suffix=.wav)"
    # shellcheck disable=SC2086
    if $KIRRA_RECORD_CMD "$wav" >/dev/null 2>&1; then
      # shellcheck disable=SC2086
      text="$($KIRRA_STT_CMD "$wav" 2>/dev/null | tr '\r\n' '  ' | sed 's/  */ /g;s/^ //;s/ $//')"
      [ -n "${text// /}" ] && printf '%s\n' "$text"
    else
      echo "rabbit_voice: recorder failed — nothing captured" >&2
    fi
    rm -f "$wav"
  done
}

echo "rabbit_voice: trigger to talk (button press or Enter). Ctrl-C / Ctrl-D quits." >&2
transcribe | python3 "$HERE/rabbit_converse.py"
