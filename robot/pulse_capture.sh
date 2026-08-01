#!/usr/bin/env bash
# pulse_capture.sh — PulseAudio raw-capture backend for the Rabbit microphone.
#
# Emits the EXACT raw-stream contract wake_word.py / vad_record.py expect on
# stdout — signed 16-bit little-endian PCM, mono, 16 kHz, long-running until
# terminated — but through the SESSION SOUND SERVER (parec) instead of a direct
# ALSA open. Why this exists: on a unit with an active user audio session the
# sound server (PulseAudio/PipeWire) OWNS the USB mic, and a direct
# `arecord -D plughw:…` open fails "Device or resource busy" even while
# /proc/asound shows the PCM closed (device reservation) — the recorder dies
# instantly and the wake listener sits deaf in its retry loop. Capturing
# through the server is the reliable path in that topology. (A headless /
# no-session system service keeps ALSA-direct — RABBIT_AUDIO_STACK.md §4.)
#
# 🔴 FAIL-CLOSED, NO MIC GUESSING: the source must be named EXPLICITLY in
# KIRRA_PULSE_SOURCE (`pactl list short sources` → the name column). There is
# deliberately NO fallback to the Pulse default source — a wrong microphone is
# worse than a visible failure. Missing parec / missing pactl / unset or
# absent source each exit nonzero with an actionable stderr message; stdout
# stays clean (it is the PCM pipe).
#
# This is a MICROPHONE backend only: it feeds the same transcribe-and-discard
# wake listener / bounded turn recorder and carries zero actuation authority.
#
# Usage:
#   pulse_capture.sh              # validate, then exec parec (raw PCM → stdout)
#   pulse_capture.sh --check      # validate only, no capture (exit 0/1)
#   pulse_capture.sh --print-cmd  # print the parec argv, one arg per line
#                                 # (host tests / debugging; no server needed)
#
# Env:
#   KIRRA_PULSE_SOURCE  REQUIRED — exact PulseAudio source name, e.g.
#     alsa_input.usb-C-Media_Electronics_Inc._USB_PnP_Sound_Device-00.analog-mono
#   KIRRA_PULSE_PAREC   override the parec binary (host tests / diagnostics)
#   KIRRA_PULSE_PACTL   override the pactl binary (host tests / diagnostics)
set -euo pipefail

# The wake_word.py raw-stream contract (SAMPLE_RATE / BYTES_PER_SAMPLE there).
RATE=16000
CHANNELS=1
FORMAT=s16le

PAREC="${KIRRA_PULSE_PAREC:-parec}"
PACTL="${KIRRA_PULSE_PACTL:-pactl}"
SRC="${KIRRA_PULSE_SOURCE:-}"

MODE=run
case "${1:-}" in
  --check) MODE=check ;;
  --print-cmd) MODE=print ;;
  "") ;;
  *) echo "pulse_capture: unknown argument '${1}' (known: --check, --print-cmd)" >&2; exit 2 ;;
esac

die() { echo "pulse_capture: ERROR: $*" >&2; exit 1; }

[ -n "$SRC" ] || die "KIRRA_PULSE_SOURCE is unset/empty — set it to the exact PulseAudio source name (\`pactl list short sources\`, name column). Refusing to guess a microphone: there is no default-source fallback (a wrong mic is worse than a visible failure)."

CMD=("$PAREC" "--device=$SRC" "--format=$FORMAT" "--rate=$RATE" "--channels=$CHANNELS")

if [ "$MODE" = print ]; then
  printf '%s\n' "${CMD[@]}"
  exit 0
fi

command -v "$PAREC" >/dev/null 2>&1 \
  || die "parec not found ('$PAREC') — install it (sudo apt install pulseaudio-utils) or point KIRRA_PULSE_PAREC at it."

# Validate the configured source EXISTS before capturing — parec's own failure
# would be silent to the caller (the listener DEVNULLs recorder stderr), and a
# renamed/unplugged mic must be a visible refusal, never a wrong-mic capture.
command -v "$PACTL" >/dev/null 2>&1 \
  || die "pactl not found ('$PACTL') — install pulseaudio-utils; the configured source cannot be validated without it (refusing to start an unvalidated capture)."

if ! sources="$("$PACTL" list short sources 2>&1)"; then
  die "pactl cannot reach the sound server: ${sources:-no output}. Is this running OUTSIDE the user session (no XDG_RUNTIME_DIR / per-user pulse socket)? The Pulse backend needs the user session — run the voice listener as a USER unit; a no-session system service must use ALSA-direct instead (RABBIT_AUDIO_STACK.md §4)."
fi

if ! printf '%s\n' "$sources" | awk '{print $2}' | grep -Fxq -- "$SRC"; then
  {
    echo "pulse_capture: ERROR: configured source not present: $SRC"
    echo "pulse_capture: available sources (pactl list short sources):"
    printf '%s\n' "$sources" | awk '{print "pulse_capture:   " $2}'
    echo "pulse_capture: fix KIRRA_PULSE_SOURCE (mic unplugged / renamed?). NOT falling back to another microphone."
  } >&2
  exit 1
fi

if [ "$MODE" = check ]; then
  echo "pulse_capture: ok — parec + source '$SRC' present (${FORMAT}/${RATE}Hz/${CHANNELS}ch)" >&2
  exit 0
fi

# exec so the caller's SIGTERM lands on parec itself (clean teardown when the
# parent service stops) and stdout carries parec's raw PCM alone.
exec "${CMD[@]}"
