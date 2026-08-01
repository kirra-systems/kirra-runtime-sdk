#!/usr/bin/env bash
# voice_transcribe.sh — the ONE shared trigger → record → STT producer.
#
# Extracted VERBATIM from rabbit_voice.sh so both voice frontends share a
# single recorder/STT implementation instead of drifting copies:
#
#   rabbit_voice.sh      → voice_transcribe.sh | rabbit_converse.py   (legacy)
#   mick_voice_router.sh → voice_transcribe.sh | voice_turn_router.py (hybrid)
#
# Contract: each line on stdin is a TRIGGER (wake_word.py / ptt_button.py /
# Enter); each trigger records ONE bounded clip ($KIRRA_RECORD_CMD, WAV path
# appended), transcribes it ($KIRRA_STT_CMD, WAV path appended) and emits ONE
# transcript line on stdout. Empty/failed captures emit nothing. The WAV is
# deleted after STT; the transcript is never written to disk and never logged
# (opt-in latency lines carry stage names + durations ONLY).
#
# The caller sources /etc/kirra/robot.env (and ROS, if its consumer needs
# it) BEFORE piping in — this producer reads only its two commands from the
# environment and stays session-agnostic.
set -euo pipefail

if [ -z "${KIRRA_STT_CMD:-}" ]; then
  echo "FATAL: KIRRA_STT_CMD required (e.g. \"whisper-cli -m models/ggml-base.en.bin -np -nt -f\")" >&2
  exit 1
fi
: "${KIRRA_RECORD_CMD:=arecord -d 4 -f S16_LE -r 16000 -c 1}"

LATENCY=0
case "${KIRRA_RABBIT_LATENCY_LOG:-}" in 1|true|yes|on|TRUE|YES|ON) LATENCY=1 ;; esac
now_ms() { date +%s%3N 2>/dev/null || echo 0; }

while IFS= read -r _; do
  wav="$(mktemp --suffix=.wav)"
  [ "$LATENCY" = 1 ] && t0="$(now_ms)"
  # shellcheck disable=SC2086
  if $KIRRA_RECORD_CMD "$wav" >/dev/null 2>&1; then
    [ "$LATENCY" = 1 ] && t1="$(now_ms)"
    # shellcheck disable=SC2086
    text="$($KIRRA_STT_CMD "$wav" 2>/dev/null | tr '\r\n' '  ' | sed 's/  */ /g;s/^ //;s/ $//')"
    if [ "$LATENCY" = 1 ]; then
      t2="$(now_ms)"
      # Stage NAMES and durations only — never the transcript.
      echo "rabbit_latency: capture $((t2 - t0))ms (record $((t1 - t0)), stt $((t2 - t1)))" >&2
    fi
    [ -n "${text// /}" ] && printf '%s\n' "$text"
  else
    echo "voice_transcribe: recorder failed — nothing captured" >&2
  fi
  rm -f "$wav"
done
