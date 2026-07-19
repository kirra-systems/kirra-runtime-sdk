#!/usr/bin/env bash
# run_voice_ptt.sh — voice push-to-talk in ONE command: a GPIO button drives the
# governed voice shell. Wires robot/ptt_button.py (the hardware trigger) into
# speech_shell (STT -> the fail-closed POST /intent door -> Occy -> KIRRA -> TTS).
#
#   ./robot/run_voice_ptt.sh
#
# The loop it talks to (mick_service + Ollama, planner_service, taj_service,
# occy_doer, interceptor) must already be up — see docs/hardware/
# R2_LIVE_LOOP_BRINGUP.md. This script only stands up the VOICE FRONT-END.
#
# 🔴 The button is a MIC trigger, not the e-stop (a separate hardware kill —
# R2_UNTETHERED_BRINGUP.md §3). A misheard press dead-ends in the intent parser:
# no intent, no motion.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
cd "$REPO"

# Optional shared env (put KIRRA_STT_CMD / KIRRA_TTS_CMD / KIRRA_PTT_* here):
if [ -f /etc/kirra/robot.env ]; then
  set -a; . /etc/kirra/robot.env; set +a
fi

# STT is REQUIRED (speech_shell refuses to start without an ear). No default path
# is invented — point it at your built whisper-cli + model.
if [ -z "${KIRRA_STT_CMD:-}" ]; then
  echo "FATAL: KIRRA_STT_CMD is required, e.g.:" >&2
  echo '  export KIRRA_STT_CMD="whisper-cli -m models/ggml-base.en.bin -np -nt -f"' >&2
  echo "(build whisper.cpp; see docs/testing/SPEECH_RABBIT_DEMO.md)" >&2
  exit 1
fi
# Bounded push-to-talk recorder (the -d bound = no open mic). Overridable.
: "${KIRRA_RECORD_CMD:=arecord -d 4 -f S16_LE -r 16000 -c 1}"; export KIRRA_RECORD_CMD
# TTS optional: unset -> narration prints instead of speaks.
: "${KIRRA_MICK_URL:=http://127.0.0.1:8102}"; export KIRRA_MICK_URL

# Resolve the speech_shell binary (release then debug, like run_consumer_r2.sh).
SHELL_BIN="${KIRRA_SPEECH_SHELL_BIN:-}"
if [ -z "$SHELL_BIN" ]; then
  for prof in release debug; do
    [ -x "$REPO/target/$prof/speech_shell" ] && { SHELL_BIN="$REPO/target/$prof/speech_shell"; break; }
  done
fi
[ -n "$SHELL_BIN" ] && [ -x "$SHELL_BIN" ] || {
  echo "FATAL: speech_shell not built (cargo build -p kirra-sidecars --bin speech_shell --release)" >&2
  exit 1
}

echo "voice PTT: GPIO ${KIRRA_PTT_PIN_MODE:-BOARD} pin ${KIRRA_PTT_GPIO_PIN:-18} -> $SHELL_BIN" >&2
# The button's newline-per-press IS the interactive trigger. Ctrl-C stops both.
python3 "$HERE/ptt_button.py" | "$SHELL_BIN"
