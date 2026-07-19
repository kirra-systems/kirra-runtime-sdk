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
