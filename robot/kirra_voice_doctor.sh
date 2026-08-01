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
HERE="$(cd "$(dirname "$0")" && pwd)"

# first token of a command string (the binary/script it invokes)
first_tok() { set -- $1; printf '%s' "${1:-}"; }
# nth (1-based) token of a command string, or ""
nth_tok() { local n="$1"; shift; set -- $1; shift $((n - 1)) 2>/dev/null || return 0; printf '%s' "${1:-}"; }
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
# PulseAudio mic-backend probe: delegates to pulse_capture.sh --check (the ONE
# validation implementation — parec present, KIRRA_PULSE_SOURCE set, source in
# `pactl list short sources`; explicit source, no default-mic fallback).
pulse_backend_check() {  # $1 = label
  local out
  if out="$("$HERE/pulse_capture.sh" --check 2>&1)"; then
    ok "$1 (PulseAudio): source present — ${KIRRA_PULSE_SOURCE:-?}"
  else
    bad "$1 (PulseAudio) check failed: $(printf '%s\n' "$out" | head -1)"
    fix "robot/pulse_capture.sh --check for the full reason — set KIRRA_PULSE_SOURCE from \`pactl list short sources\` (needs the user session)"
  fi
}

[ "$QUIET" = 1 ] || echo "== R2 voice/audio doctor =="

# 1. env file present + loadable. Distinguish the three failure shapes — they
# have different fixes, and the parent-traverse one is exactly how the user
# voice unit (rabbit-voice.service) dies with Result=resources and no log:
# /etc/kirra is deliberately 0750 (governor secrets), so THIS account needs
# the narrow ACL from ensure_voice_env_access.sh, not a chmod. Read-only:
# this doctor never modifies permissions.
if [ -r "$RENV" ]; then
  ok "robot.env readable ($RENV) — the user voice unit can load its EnvironmentFile"
  # shellcheck disable=SC1090
  set -a; . "$RENV"; set +a
else
  RDIR="$(dirname "$RENV")"
  if [ ! -x "$RDIR" ]; then
    bad "$(id -un) cannot traverse $RDIR — the user voice unit fails before ExecStart (Result=resources)"
    fix "sudo robot/install/ensure_voice_env_access.sh --user $(id -un)   # traverse-only ACL, no chmod"
  elif [ ! -e "$RENV" ]; then
    bad "robot.env missing ($RENV)"
    fix "robot/install/install_kirra.sh renders it if absent — R2_VOICE_AUDIO_SETUP.md §4"
  else
    bad "robot.env exists but $(id -un) cannot READ it ($RENV)"
    fix "sudo robot/install/ensure_voice_env_access.sh --user $(id -un)   # read-only file ACL"
  fi
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

# 4. TURN recorder: which mode, and is its mic pinned + present?
#    Two shapes are valid and their device lives in DIFFERENT places:
#      arecord -d N …                 → the device is this command's -D
#      python3 …/vad_record.py        → the device is KIRRA_VAD_DEVICE
#    Reading -D off the vad_record.py line would find nothing and wrongly report
#    "not pinned" on a correctly configured VAD robot.
# The VAD form is "python3 <path>/vad_record.py", so the FIRST token is the
# interpreter — classify on the script it runs, not on argv[0].
turn_prog="$(basename "$(first_tok "${KIRRA_RECORD_CMD:-}")")"
case "${KIRRA_RECORD_CMD:-}" in
  *vad_record.py*) turn_prog="vad_record.py" ;;
esac
case "$turn_prog" in
  vad_record.py)
    # 4a. the recorder must actually be installed and runnable.
    # the command is "python3 <script>" — the script is the 2nd token.
    # nth_tok, not `set --`: this runs at top level, where `set --` would
    # clobber the doctor's own arguments.
    turn_script="$(nth_tok 2 "${KIRRA_RECORD_CMD}")"
    case "$turn_script" in *vad_record.py) : ;; *) turn_script="$(first_tok "${KIRRA_RECORD_CMD}")" ;; esac
    if [ -z "$turn_script" ] || [ ! -f "$turn_script" ]; then
      bad "KIRRA_RECORD_CMD points at vad_record.py but ${turn_script:-<none>} is missing"
      fix "re-run robot/install/install_robot_units.sh (it stages /opt/kirra/robot/vad_record.py)"
    elif [ ! -r "$turn_script" ]; then
      bad "turn VAD recorder not readable: $turn_script"; fix "sudo chmod 0755 $turn_script"
    else
      ok "turn recorder: VAD endpointing ($turn_script)"
    fi
    # 4b. the VAD mic — REQUIRED, and it must not have drifted.
    if [ -n "${KIRRA_VAD_CAPTURE_CMD:-}" ]; then
      vcp="$(basename "$(first_tok "$KIRRA_VAD_CAPTURE_CMD")")"
      if [ "$vcp" = "arecord" ]; then
        vdev="$(opt_val -D "$KIRRA_VAD_CAPTURE_CMD")"
        [ -n "$vdev" ] || vdev="$(opt_val --device "$KIRRA_VAD_CAPTURE_CMD")"
        if [ -z "$vdev" ]; then
          bad "KIRRA_VAD_CAPTURE_CMD uses arecord with no -D/--device (ALSA DEFAULT — every turn would record near-silence)"
          fix 'arecord -l → add -D plughw:CARD=<name>,DEV=0'
        else
          ok "VAD capture command pins its device: -D $vdev"
        fi
      elif [ "$vcp" = "pulse_capture.sh" ] || [ "$vcp" = "parec" ]; then
        # The session-sound-server backend (the desktop image, where PulseAudio
        # OWNS the mic and direct plughw: opens fail EBUSY): validate the
        # explicit source instead of applying any ALSA rule.
        pulse_backend_check "VAD mic capture"
      else
        ok "VAD uses a custom capture backend ($vcp) — no ALSA rule applied"
      fi
    elif [ -z "${KIRRA_VAD_DEVICE:-}" ]; then
      bad "KIRRA_RECORD_CMD is vad_record.py but KIRRA_VAD_DEVICE is unset (it will REFUSE to start — there is no ALSA default)"
      fix 'arecord -l → KIRRA_VAD_DEVICE="plughw:CARD=<name>,DEV=0" — §0'
    else
      vc="$(card_ref "$KIRRA_VAD_DEVICE")"
      if command -v arecord >/dev/null 2>&1 && card_present arecord "$vc"; then
        ok "VAD mic device present: $KIRRA_VAD_DEVICE"
      else
        bad "VAD mic device NOT in arecord -l (card drifted?): $KIRRA_VAD_DEVICE"
        fix "arecord -l → update KIRRA_VAD_DEVICE to plughw:CARD=<name>,DEV=0 — §0"
      fi
    fi
    # 4c. the bounds must be positive and actually bounded (the no-open-mic claim).
    vmax="${KIRRA_VAD_MAX_MS:-8000}"; vsto="${KIRRA_VAD_START_TIMEOUT_MS:-3000}"
    vabs=30000
    if ! printf '%s' "$vmax" | grep -qE '^[0-9]+$' || [ "$vmax" -le 0 ]; then
      bad "KIRRA_VAD_MAX_MS='$vmax' is not a positive integer"; fix "set e.g. KIRRA_VAD_MAX_MS=6000"
    elif [ "$vmax" -gt "$vabs" ]; then
      bad "KIRRA_VAD_MAX_MS=$vmax exceeds the ${vabs}ms absolute ceiling (vad_record.py will refuse)"; fix "set KIRRA_VAD_MAX_MS <= $vabs"
    elif ! printf '%s' "$vsto" | grep -qE '^[0-9]+$' || [ "$vsto" -le 0 ]; then
      bad "KIRRA_VAD_START_TIMEOUT_MS='$vsto' is not a positive integer"; fix "set e.g. KIRRA_VAD_START_TIMEOUT_MS=3000"
    elif [ "$vsto" -gt "$vmax" ]; then
      bad "KIRRA_VAD_START_TIMEOUT_MS=$vsto exceeds KIRRA_VAD_MAX_MS=$vmax (it could never fire)"; fix "set the start timeout below the ceiling"
    else
      ok "VAD bounds sane: max ${vmax}ms, start timeout ${vsto}ms, silence ${KIRRA_VAD_SILENCE_MS:-800}ms"
    fi
    ;;
  arecord)
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
    rdb="$(opt_val -d "${KIRRA_RECORD_CMD}")"
    if [ -n "$rdb" ]; then
      warn "turn recorder is the FIXED ${rdb}s window — every turn costs ${rdb}s however short"
      fix "switch to VAD endpointing: KIRRA_RECORD_CMD=\"python3 /opt/kirra/robot/vad_record.py\" + KIRRA_VAD_DEVICE — see rabbit.env.example"
    else
      bad "KIRRA_RECORD_CMD uses arecord with no -d bound — that is an UNBOUNDED microphone"; fix "add -d 4, or switch to vad_record.py"
    fi
    ;;
  "")
    warn "KIRRA_RECORD_CMD unset — the turn recorder defaults to a fixed arecord window"
    ;;
  *)
    ok "turn recorder: custom backend ($turn_prog) — no ALSA rule applied"
    ;;
esac

# 4d. the two recorder contracts must not be swapped. KIRRA_WAKE_RECORD_CMD is
# the always-on RAW stream (-t raw, no -d); KIRRA_RECORD_CMD is the one-turn WAV
# recorder. Swap them and the turn recorder never terminates, or the listener
# stops after one window — both fail in confusing, intermittent ways.
if [ -n "${KIRRA_RECORD_CMD:-}" ] && [ "$turn_prog" = "arecord" ]; then
  case " ${KIRRA_RECORD_CMD} " in
    *" -t raw "*|*" -t raw") bad "KIRRA_RECORD_CMD is a RAW stream (-t raw) — that is the WAKE recorder's contract, not the turn recorder's"; fix "the turn recorder must write a bounded WAV: \"arecord -D <dev> -d 4 -f S16_LE -r 16000 -c 1\" (or vad_record.py)" ;;
  esac
fi
if [ -n "${KIRRA_WAKE_RECORD_CMD:-}" ]; then
  case " ${KIRRA_WAKE_RECORD_CMD} " in
    *" -d "*) bad "KIRRA_WAKE_RECORD_CMD has a -d bound — that is the TURN recorder's contract; the wake listener needs an UNBOUNDED raw stream"; fix "drop -d and add -t raw" ;;
  esac
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

# 6b. Rabbit model residency. THREE DISTINCT STATES, deliberately not collapsed:
#       configured residency — what robot.env asks for
#       currently loaded     — what Ollama reports right now
#       server unavailable   — we cannot tell, and must not guess
#     A model that has legitimately unloaded on a HEALTHY server with a finite
#     keep-alive is NORMAL, not a fault, so it is never a ❌ on its own.
ka="${KIRRA_RABBIT_KEEP_ALIVE:-30m}"
rmodel="${KIRRA_RABBIT_MODEL:-gemma3:4b}"
case "$ka" in
  -1)   ok "Rabbit keep-alive: -1 (PINNED RESIDENT — the dedicated-robot setting)" ;;
  0)    warn "Rabbit keep-alive: 0 (unload immediately — every turn pays a cold reload)"; fix "set KIRRA_RABBIT_KEEP_ALIVE=-1 on a dedicated robot" ;;
  *)    # A duration string ("30m", "1h") or a positive number of seconds. Empty
        # already resolved to the 30m default above, exactly as rabbit_ask does.
        ok "Rabbit keep-alive: $ka (finite hold)" ;;
esac
ourl="${KIRRA_OLLAMA_URL:-http://localhost:11434}"
if ! command -v curl >/dev/null 2>&1; then
  warn "curl not installed — cannot check whether $rmodel is loaded"
else
  ps_json="$(curl -sf --max-time 3 "${ourl}/api/ps" 2>/dev/null)"
  if [ -z "$ps_json" ]; then
    # Server unavailable is its own state: it says nothing about residency.
    warn "Ollama not reachable at $ourl — residency unknown (this is a SERVER state, not a config fault)"
    fix "systemctl status ollama; then re-run"
  elif printf '%s' "$ps_json" | grep -q "$rmodel"; then
    ok "$rmodel is loaded now (resident)"
  elif [ "$ka" = "-1" ]; then
    # Pinned but absent = nothing has warmed it since the server started.
    warn "$rmodel is NOT loaded although keep-alive is -1 — nothing has warmed it since Ollama started"
    fix "run one Rabbit turn (or: ollama run $rmodel hi >/dev/null) — it then stays pinned"
  else
    ok "$rmodel not loaded right now — expected with a finite keep-alive ($ka); it loads on the next turn"
  fi
fi

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
    # 8b2. the WAKE microphone. Distinct from KIRRA_RECORD_CMD: that one is a
    # BOUNDED wav recorder (-d N, writes a file), this is an UNBOUNDED raw
    # stream. A bare `arecord` takes ALSA's DEFAULT device — the first card the
    # kernel enumerated, which on the R2 is not the mic — and the failure is
    # SILENT: the stream opens, delivers near-silence, and nobody is heard.
    # So this is a FAIL, not a warning.
    if [ -z "${KIRRA_WAKE_RECORD_CMD:-}" ]; then
      # Unset = the DEFAULT backend: pulse_capture.sh through the session sound
      # server, which needs an explicit KIRRA_PULSE_SOURCE (wake_word.py's
      # resolver refuses to guess a microphone — mirror that here).
      pulse_backend_check "wake mic"
    else
      wrb="$(first_tok "$KIRRA_WAKE_RECORD_CMD")"
      if [ -z "$wrb" ]; then
        bad "KIRRA_WAKE_RECORD_CMD has no program"; fix 'e.g. "arecord -D plughw:CARD=Device,DEV=0 -f S16_LE -r 16000 -c 1 -t raw"'
      else
        wmic="$(opt_val -D "$KIRRA_WAKE_RECORD_CMD")"
        [ -n "$wmic" ] || wmic="$(opt_val --device "$KIRRA_WAKE_RECORD_CMD")"
        case "$(basename "$wrb")" in
          arecord)
            if [ -z "$wmic" ]; then
              bad "KIRRA_WAKE_RECORD_CMD uses arecord with no -D/--device (ALSA DEFAULT device — the listener would run deaf)"; fix 'arecord -l → "arecord -D plughw:CARD=<name>,DEV=0 -f S16_LE -r 16000 -c 1 -t raw" — §0'
            else
              # Same drift check the full-turn mic gets: a pinned card that is
              # no longer enumerated is exactly as deaf as no pin at all.
              wmc="$(card_ref "$wmic")"
              if command -v arecord >/dev/null 2>&1 && card_present arecord "$wmc"; then
                ok "wake mic device present: -D $wmic"
              else
                bad "wake mic device NOT in arecord -l (card drifted?): -D $wmic"; fix "arecord -l → update KIRRA_WAKE_RECORD_CMD -D plughw:CARD=<name>,DEV=0 — §0"
              fi
            fi
            ;;
          pulse_capture.sh|parec)
            # The session-sound-server backend: validate the explicit source.
            pulse_backend_check "wake mic"
            ;;
          *)
            # A custom capture backend has its own device convention; imposing
            # ALSA's would refuse a working configuration.
            ok "wake capture backend: $wrb (custom — no ALSA device rule applied)"
            ;;
        esac
      fi
    fi
    # 8c. hold-off must cover the turn recorder's -d bound (mic contention:
    # the listener releases the device for holdoff; a short holdoff steals it
    # back mid-turn and the turn recorder fails SILENTLY).
    # The turn recorder's WORST-CASE seconds: -d for the fixed window, the VAD
    # hard ceiling for the endpointing recorder (that is what the mic can hold).
    if [ "$turn_prog" = "vad_record.py" ]; then
      rd=$(( ${KIRRA_VAD_MAX_MS:-8000} / 1000 ))
    else
      rd="$(opt_val -d "${KIRRA_RECORD_CMD:-arecord -d 4}")"
    fi
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
