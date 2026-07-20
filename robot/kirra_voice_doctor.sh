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
# device fails SILENTLY mid-turn otherwise.
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
# ALSA card number from a plughw:N,0 / hw:N,0 spec
card_of() { local d="${1#plughw:}"; d="${d#hw:}"; printf '%s' "${d%%,*}"; }

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
mc="$(card_of "$(opt_val -D "${KIRRA_RECORD_CMD:-}")")"
if [ -n "$mc" ]; then
  if command -v arecord >/dev/null 2>&1 && arecord -l 2>/dev/null | grep -qE "^card ${mc}:"; then
    ok "mic device present: plughw:${mc},0"
  else
    bad "mic device plughw:${mc},0 NOT in arecord -l (card drifted?)"; fix "arecord -l → update KIRRA_RECORD_CMD -D plughw:<N>,0 — §0"
  fi
else
  warn "KIRRA_RECORD_CMD has no -D plughw:N,0 (mic device not pinned)"
fi

# 5. SPEAKER device present in aplay -l (parse the plughw from the TTS wrapper file)
tts_src="${KIRRA_TTS_CMD:-}"; tf="$(first_tok "$tts_src")"
[ -f "$tf" ] && tts_src="$(cat "$tf" 2>/dev/null || true)"
sc="$(card_of "$(opt_val -D "$tts_src")")"
if [ -n "$sc" ]; then
  if command -v aplay >/dev/null 2>&1 && aplay -l 2>/dev/null | grep -qE "^card ${sc}:"; then
    ok "speaker device present: plughw:${sc},0"
  else
    bad "speaker device plughw:${sc},0 NOT in aplay -l (card drifted?)"; fix "aplay -l → update speak.sh aplay -D plughw:<N>,0 — §0"
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

# summary + exit code
if [ "$QUIET" = 1 ]; then
  [ "$FAIL" -eq 0 ] && echo "OK" || echo "FAIL: $FIRST_FAIL"
else
  echo "== ${PASS} ok / ${WARN} warn / ${FAIL} fail =="
fi
[ "$FAIL" -eq 0 ]
