#!/usr/bin/env bash
# kirra_voice_doctor.sh — read-only voice/audio config doctor for the R2.
#
# Checks the voice layer configured in /etc/kirra/robot.env against the actual
# machine state and names the fix for each gap. Changes NOTHING (safe to run
# anytime, incl. over SSH). Companion to robot/install/preflight_autostart.sh
# (autostart readiness) — this one covers the STT/TTS/mic/speaker path that the
# autostart preflight does not. Concrete setup + fixes: docs/hardware/
# R2_VOICE_AUDIO_SETUP.md.
#
#   robot/kirra_voice_doctor.sh            # human report (✔/❌/⚠ + fix hints)
#   robot/kirra_voice_doctor.sh --quiet    # one line: "OK" | "FAIL: <first issue>"
#
# Exit 0 iff there is no ❌ (a ⚠ still exits 0 — warnings are non-fatal).
# The killer check: the `-D plughw:N,0` mic/speaker cards actually appear in
# arecord -l / aplay -l — ALSA card numbers drift across reboots, and a drifted
# device fails SILENTLY mid-turn otherwise. Both addressing forms are checked:
# a bare numeric index (`plughw:N,0` — drifts across reboots on generic USB
# audio dongles with no persistent udev naming) AND the more robust
# `plughw:CARD=<name>,DEV=n` (survives USB re-enumeration order changing) — a
# correctly-pinned CARD=name must never read as "drifted".
set -uo pipefail

QUIET=0
[ "${1:-}" = "--quiet" ] && QUIET=1

PASS=0; FAIL=0; WARN=0; FIRST_FAIL=""
ok()   { [ "$QUIET" = 1 ] || echo "  ✔ $*"; PASS=$((PASS + 1)); }
bad()  { [ "$QUIET" = 1 ] || echo "  ❌ $*"; FAIL=$((FAIL + 1)); [ -n "$FIRST_FAIL" ] || FIRST_FAIL="$*"; }
warn() { [ "$QUIET" = 1 ] || echo "  ⚠ $*"; WARN=$((WARN + 1)); }
fix()  { [ "$QUIET" = 1 ] || echo "       ↳ fix: $*"; }

RENV="${KIRRA_ROBOT_ENV:-/etc/kirra/robot.env}"

# first token of a command string (the binary/script it invokes)
first_tok() { set -- $1; printf '%s' "${1:-}"; }
# value following <flag> in a command string, or "" (e.g. opt_val -m "$KIRRA_STT_CMD")
opt_val() { local f="$1"; shift; set -- $1; while [ "$#" -gt 0 ]; do [ "$1" = "$f" ] && { printf '%s' "${2:-}"; return; }; shift; done; }
# ALSA card reference from a device spec: plughw:N,0 / hw:N,0 (numeric) or
# plughw:CARD=<name>,DEV=n (by-name — the robust form). Prints "num:<N>",
# "name:<NAME>", or "num:" (empty value) if no -D spec was given at all.
card_ref() {
  local d="${1#plughw:}"; d="${d#hw:}"; d="${d%%,*}"
  case "$d" in
    CARD=*) printf 'name:%s' "${d#CARD=}" ;;
    *)      printf 'num:%s' "$d" ;;
  esac
}
# Is a card_ref() result present in `arecord -l` / `aplay -l` (arg1: the tool)?
card_present() {
  local tool="$1" kind="${2%%:*}" val="${2#*:}"
  case "$kind" in
    num)  "$tool" -l 2>/dev/null | grep -qE "^card ${val}:" ;;
    name) "$tool" -l 2>/dev/null | grep -qE "^card [0-9]+: ${val} " ;;
  esac
}

[ "$QUIET" = 1 ] || echo "== R2 voice/audio doctor =="

# 1. env file present + loadable
if [ -r "$RENV" ]; then
  ok "robot.env readable ($RENV)"
  # shellcheck disable=SC1090
  set -a; . "$RENV"; set +a
else
  bad "robot.env not readable ($RENV)"; fix "create it / check perms — R2_VOICE_AUDIO_SETUP.md §4"
fi

# 2. STT engine + model
if [ -n "${KIRRA_STT_CMD:-}" ]; then
  b="$(first_tok "$KIRRA_STT_CMD")"
  command -v "$b" >/dev/null 2>&1 && ok "STT binary: $b" || { bad "STT binary not found: $b"; fix "build whisper.cpp + symlink whisper-cli — §1"; }
  m="$(opt_val -m "$KIRRA_STT_CMD")"
  [ -z "$m" ] || { [ -f "$m" ] && ok "STT model: $m" || { bad "STT model missing: $m"; fix "download-ggml-model.sh base.en — §1"; }; }
else
  bad "KIRRA_STT_CMD unset"; fix "set it in $RENV — §4"
fi

# 3. TTS wrapper
if [ -n "${KIRRA_TTS_CMD:-}" ]; then
  b="$(first_tok "$KIRRA_TTS_CMD")"
  { [ -x "$b" ] || command -v "$b" >/dev/null 2>&1; } && ok "TTS command: $b" || { bad "TTS command not found/executable: $b"; fix "create speak.sh + chmod +x — §3"; }
else
  warn "KIRRA_TTS_CMD unset — Rabbit will PRINT, not speak"
fi

# 4. MIC device present in arecord -l  (the drift check)
mic_spec="$(opt_val -D "${KIRRA_RECORD_CMD:-}")"
mc="$(card_ref "$mic_spec")"
if [ -n "${mc#*:}" ]; then
  if command -v arecord >/dev/null 2>&1 && card_present arecord "$mc"; then
    ok "mic device present: -D $mic_spec"
  else
    bad "mic device NOT in arecord -l (card drifted?): -D $mic_spec"; fix "arecord -l → update KIRRA_RECORD_CMD -D plughw:<N>,0 (or the more robust plughw:CARD=<name>,DEV=0) — §0"
  fi
else
  warn "KIRRA_RECORD_CMD has no -D plughw:N,0 (mic device not pinned)"
fi

# 5. SPEAKER device present in aplay -l (parse the plughw from the TTS wrapper file)
tts_src="${KIRRA_TTS_CMD:-}"; tf="$(first_tok "$tts_src")"
[ -f "$tf" ] && tts_src="$(cat "$tf" 2>/dev/null || true)"
spk_spec="$(opt_val -D "$tts_src")"
sc="$(card_ref "$spk_spec")"
if [ -n "${sc#*:}" ]; then
  if command -v aplay >/dev/null 2>&1 && card_present aplay "$sc"; then
    ok "speaker device present: -D $spk_spec"
  else
    bad "speaker device NOT in aplay -l (card drifted?): -D $spk_spec"; fix "aplay -l → update speak.sh aplay -D plughw:<N>,0 (or the more robust plughw:CARD=<name>,DEV=0) — §0"
  fi
else
  warn "no speaker plughw:N,0 found in the TTS path (not pinned)"
fi

# 6. loop services listening (WARN — may legitimately be off)
for pair in "8090:verifier" "8102:mick" "11434:ollama"; do
  p="${pair%%:*}"; n="${pair##*:}"
  if ss -tlnH 2>/dev/null | grep -qE ":${p}([[:space:]]|$)"; then ok "$n listening (:$p)"; else warn "$n not listening (:$p)"; fix "bring up the loop — R2_LIVE_LOOP_BRINGUP.md"; fi
done

# 7. Jetson.GPIO (PTT button) — WARN (Enter-key path works without it)
if python3 -c "import Jetson.GPIO" >/dev/null 2>&1; then
  ok "Jetson.GPIO importable (PTT ready)"
else
  warn "Jetson.GPIO not importable (PTT button off; Enter-key still works)"; fix "pip install >=2.1.12 from GitHub on Super boards — §5"
fi

# 8. wake word (W1, opt-in) — only checked when enabled; off is a clean ok.
case "$(printf '%s' "${KIRRA_WAKE_ENABLED:-}" | tr '[:upper:]' '[:lower:]')" in
  1|true|yes|on)
    ok "wake word ENABLED (KIRRA_WAKE_ENABLED)"
    # 8a. the wake STT engine (a SECOND, tiny model — not the turn STT).
    if [ -n "${KIRRA_WAKE_STT_CMD:-}" ]; then
      wb="$(first_tok "$KIRRA_WAKE_STT_CMD")"
      command -v "$wb" >/dev/null 2>&1 && ok "wake STT binary: $wb" || { bad "wake STT binary not found: $wb"; fix "build whisper.cpp; use the TINY model for the listener"; }
      wm="$(opt_val -m "$KIRRA_WAKE_STT_CMD")"
      [ -z "$wm" ] || { [ -f "$wm" ] && ok "wake STT model: $wm" || { bad "wake STT model missing: $wm"; fix "download-ggml-model.sh tiny.en"; }; }
    else
      bad "KIRRA_WAKE_ENABLED is on but KIRRA_WAKE_STT_CMD unset"; fix 'set it, e.g. "whisper-cli -m models/ggml-tiny.en.bin -np -nt -f"'
    fi
    # 8b. phrases parse to something.
    if python3 -c "
import sys; sys.path.insert(0, '$(dirname "$0")')
from wake_word import parse_phrases, DEFAULT_PHRASES
import os
sys.exit(0 if parse_phrases(os.environ.get('KIRRA_WAKE_PHRASES', DEFAULT_PHRASES)) else 1)" 2>/dev/null; then
      ok "wake phrases parse (${KIRRA_WAKE_PHRASES:-hello rabbit,hey rabbit,yo rabbit})"
    else
      bad "KIRRA_WAKE_PHRASES parses to nothing"; fix "comma-separated phrases, e.g. \"hello rabbit,hey rabbit\""
    fi
    # 8c. hold-off must cover the turn recorder's -d bound (mic contention:
    # the listener releases the device for holdoff; a short holdoff steals it
    # back mid-turn and the turn recorder fails SILENTLY).
    rd="$(opt_val -d "${KIRRA_RECORD_CMD:-arecord -d 4}")"
    ho="${KIRRA_WAKE_HOLDOFF_S:-10}"
    if [ -n "$rd" ] && awk "BEGIN{exit !($ho >= $rd + 3)}" 2>/dev/null; then
      ok "wake holdoff ${ho}s covers the ${rd}s turn recording (+STT/TTS)"
    else
      warn "KIRRA_WAKE_HOLDOFF_S=${ho} may not cover the ${rd:-?}s turn recording + STT + TTS"; fix "set KIRRA_WAKE_HOLDOFF_S >= record -d + ~6"
    fi
    # 8d. ack cue: false fires must be AUDIBLE, not silent.
    if [ -n "${KIRRA_WAKE_ACK_CMD:-}" ] || [ -n "${KIRRA_TTS_CMD:-}" ]; then
      ok "wake ack cue available (KIRRA_WAKE_ACK_CMD or TTS \"Yes?\")"
    else
      warn "no KIRRA_WAKE_ACK_CMD and no KIRRA_TTS_CMD — wakes (incl. FALSE fires) will be silent"
    fi
    # 8e. state-file directory writable (nap/mute controls).
    wsf="${KIRRA_WAKE_STATE_FILE:-/tmp/kirra_rabbit_wake.state}"
    wsd="$(dirname "$wsf")"
    [ -d "$wsd" ] && [ -w "$wsd" ] && ok "wake state dir writable: $wsd" || { warn "wake state dir not writable: $wsd (nap/mute controls will no-op)"; fix "set KIRRA_WAKE_STATE_FILE to a writable path"; }
    ;;
  *)
    ok "wake word off (KIRRA_WAKE_ENABLED unset — PTT/Enter are the triggers)"
    ;;
esac

# summary + exit code
if [ "$QUIET" = 1 ]; then
  [ "$FAIL" -eq 0 ] && echo "OK" || echo "FAIL: $FIRST_FAIL"
else
  echo "== ${PASS} ok / ${WARN} warn / ${FAIL} fail =="
fi
[ "$FAIL" -eq 0 ]
