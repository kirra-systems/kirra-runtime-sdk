#!/usr/bin/env python3
"""wake_word.py — an opt-in wake-word TRIGGER for the Rabbit voice loop (W1).

A third trigger producer honoring the ONE trigger contract (ptt_button.py:
exactly ONE newline on stdout per trigger), so:

    python3 robot/wake_word.py | ./robot/rabbit_voice.sh

makes "hello rabbit" (or "hey rabbit" / "yo rabbit") = the Enter key. ZERO
change to rabbit_voice.sh / rabbit_converse.py / mick / Occy / the checker:
after the trigger fires, everything is the existing pipeline — the bounded
KIRRA_RECORD_CMD clip, STT, the deterministic matchers, at most the fenced
text-to-the-door POST.

🔴 SAFETY — WHAT A WAKE WORD IS (and is NOT), same banner as the button:
  * It is a MICROPHONE trigger. A false fire is exactly the phantom-PTT-press
    class (already documented for the floating Orin pin): one bounded clip,
    an empty/garbage transcript, no intent latched, NO motion. It adds no
    actuation authority and cannot bypass the checker.
  * It is NOT the e-stop (separate hardware kill — R2_UNTETHERED_BRINGUP.md §3).

DETECTION (no LLM anywhere in this file):
  arecord raw stream → in-memory ring buffer → RMS energy pre-gate (a silent
  room runs ZERO inference) → whisper.cpp tiny (KIRRA_WAKE_STT_CMD) on a ~2 s
  window in tmpfs → a PURE token matcher (ordered-adjacent tokens with a small
  edit tolerance on long tokens only — "hallo rabbits" wakes, a stray "yo"
  never does). Matched → newline trigger + ack cue + mic released for the
  hold-off window (rabbit_voice.sh needs the device) + cooldown.

PRIVACY (the RABBIT_AUDIO_STACK.md §3 objection, answered):
  transcribe-and-discard — fixed in-memory window, tmpfs wav deleted every
  cycle, NO audio persisted, NO transcript logged (only wake HITS are logged).
  Everything stays on the robot (whisper.cpp is local). Operator controls:
  "rabbit, go to sleep" / "stop listening" (rabbit_wake.py writes the state
  file this listener polls), and an optional LED lit while the mic is open.

STDOUT DISCIPLINE (ptt_button.py's rule): ONLY trigger newlines touch stdout;
every log goes to STDERR.

Env (robot/install/rabbit.env.example has the annotated set):
  KIRRA_WAKE_ENABLED      master gate; not truthy → exit 0 immediately (unit stays off)
  KIRRA_WAKE_STT_CMD      REQUIRED when enabled — whisper-cli + a TINY model;
                          wav path appended as the last arg (house convention)
  KIRRA_WAKE_PHRASES      comma-separated, default "hello rabbit,hey rabbit,yo rabbit"
  KIRRA_WAKE_RECORD_CMD   raw-stream capture, default "arecord -f S16_LE -r 16000 -c 1 -t raw"
  KIRRA_WAKE_RMS_FLOOR    energy gate (default 300; 16-bit full scale = 32767)
  KIRRA_WAKE_COOLDOWN_S   refractory period between wakes (default 2)
  KIRRA_WAKE_HOLDOFF_S    mic released this long after a trigger (default 10;
                          must cover KIRRA_RECORD_CMD's -d bound + STT + TTS)
  KIRRA_WAKE_ACK_CMD      cue command on wake (no stdin). Default: speak "Yes?"
                          via KIRRA_TTS_CMD if set, else a stderr note.
  KIRRA_WAKE_STATE_FILE   nap/mute control file (default /tmp/kirra_rabbit_wake.state)
  KIRRA_WAKE_LED_PIN      optional BOARD output pin lit while the mic is open
"""
from __future__ import annotations

import array
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import time
import wave

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import turn_state  # noqa: E402 — cross-process "turn in progress" signal (Slice R)

# ---------------------------------------------------------------------------
# Pure logic (host-tested in robot/wake_word_test.py — no audio, no GPIO)
# ---------------------------------------------------------------------------

DEFAULT_PHRASES = "hello rabbit,hey rabbit,yo rabbit"

# Whisper decorates non-speech as bracketed/parenthesized annotations
# ("[BLANK_AUDIO]", "(wind blowing)"); they are never wake tokens.
_ANNOTATION_RE = re.compile(r"\[[^\]]*\]|\([^)]*\)")
_NON_WORD_RE = re.compile(r"[^a-z']+")


def parse_phrases(spec: str | None) -> list[list[str]]:
    """KIRRA_WAKE_PHRASES → list of token lists. Empty/whitespace entries are
    dropped; an all-empty spec yields [] (the caller refuses to start on it —
    an enabled listener with nothing to listen for is a misconfiguration)."""
    out: list[list[str]] = []
    for part in (spec or "").split(","):
        toks = transcript_tokens(part)
        if toks:
            out.append(toks)
    return out


def transcript_tokens(text: str | None) -> list[str]:
    """Normalize an STT transcript to lowercase word tokens. Bracketed
    annotations are stripped BEFORE tokenizing, apostrophes kept ("what's")."""
    t = _ANNOTATION_RE.sub(" ", (text or "").lower())
    return [w for w in _NON_WORD_RE.split(t) if w]


def _edit_distance_le(a: str, b: str, bound: int) -> bool:
    """Levenshtein distance <= bound (bound is 0 or 1 here; small and exact)."""
    if a == b:
        return True
    if bound <= 0:
        return False
    la, lb = len(a), len(b)
    if abs(la - lb) > bound:
        return False
    prev = list(range(lb + 1))
    for i in range(1, la + 1):
        cur = [i] + [0] * lb
        for j in range(1, lb + 1):
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1,
                         prev[j - 1] + (a[i - 1] != b[j - 1]))
        prev = cur
    return prev[lb] <= bound


def _token_matches(heard: str, want: str) -> bool:
    """Per-token tolerance rule: long tokens (>=5 chars: hello, rabbit) absorb
    ONE edit ("hallo", "rabbits"); short tokens (hey, yo) match EXACTLY —
    "yo"'s edit-distance-1 neighborhood (to/so/no/go) is all noise words."""
    return _edit_distance_le(heard, want, 1 if len(want) >= 5 else 0)


def wake_hit(tokens: list[str], phrases: list[list[str]]) -> str | None:
    """Return the matched phrase (joined) or None. A phrase matches when its
    tokens appear ADJACENT and IN ORDER anywhere in the transcript — "hey the
    rabbit robot" does NOT wake (intervening token), "well hello rabbit" does."""
    for phrase in phrases:
        n = len(phrase)
        for i in range(len(tokens) - n + 1):
            if all(_token_matches(tokens[i + k], phrase[k]) for k in range(n)):
                return " ".join(phrase)
    return None


def rms(pcm: bytes) -> float:
    """RMS of 16-bit little-endian mono PCM. Stdlib-only (no audioop — it is
    removed in newer Pythons); an odd trailing byte is dropped."""
    if len(pcm) < 2:
        return 0.0
    samples = array.array("h")
    samples.frombytes(pcm[: len(pcm) - (len(pcm) % 2)])
    if sys.byteorder == "big":
        samples.byteswap()
    if not samples:
        return 0.0
    return (sum(s * s for s in samples) / len(samples)) ** 0.5


def wake_allowed(state_text: str | None, now_ms: int) -> bool:
    """The nap/mute gate over the control file rabbit_wake.py writes.
    FAIL-OPEN BY DESIGN and safely so: an absent/corrupt state file means
    LISTENING — the wake word carries no actuation authority (a trigger
    dead-ends at the fence), so the availability-friendly default is right,
    unlike anything safety-bearing. mode: awake | nap (until until_ms) | mute."""
    if not state_text:
        return True
    try:
        j = json.loads(state_text)
        mode = j.get("mode")
        if mode == "mute":
            return False
        if mode == "nap":
            return now_ms >= int(j.get("until_ms", 0))
        return True
    except Exception:  # noqa: BLE001 — corrupt state file → listening
        return True


def followup_decision(elapsed_s: float, onset: bool, window_s: float) -> str:
    """Follow-up mode (Slice F): after a reply, should the listener fire a
    follow-up trigger WITHOUT a wake word, keep waiting for the operator to
    start speaking, or close the window?

    Like Google's Continued Conversation / Alexa Follow-Up Mode: for a short
    window after each answer the operator can just talk. Pure so the whole
    follow-up gate is host-testable with no audio.

      onset      — the operator has STARTED speaking (sustained energy above the
                   floor) → fire a follow-up trigger, treated exactly as a wake
                   (the same fenced text-to-the-door path; zero new authority).
      window_s   — how long to hold the window open; a silent window closes and
                   the listener falls back to requiring the wake word.

    Returns 'trigger' | 'listen' | 'expire'. Onset wins even past the window —
    speech that just began is honoured, not dropped on a boundary tie."""
    if onset:
        return "trigger"
    if elapsed_s >= window_s:
        return "expire"
    return "listen"


# ---------------------------------------------------------------------------
# The listener (hardware side — not imported by tests)
# ---------------------------------------------------------------------------

SAMPLE_RATE = 16_000
BYTES_PER_SAMPLE = 2
HOP_S = 0.25          # RMS gate granularity
WINDOW_S = 2.0        # transcription window handed to whisper
STATE_POLL_S = 1.0    # nap/mute file poll cadence
REARM_POLL_S = 0.2    # turn-state poll cadence while waiting to re-arm the mic


def _wait_for_rearm(state_file, baseline_seq, grace_s, max_s):
    """After a trigger fires, hold the mic CLOSED until the turn it caused is
    settled, then return so the outer loop reopens the mic. Event-driven (polls
    turn_state) and bounded by grace_s / max_s, so an old/absent/crashed writer
    degrades to a timed reopen instead of wedging. Replaces the old blind
    `time.sleep(holdoff_s)` that dropped a fast turn's follow-up wake (Slice R)."""
    t0 = time.monotonic()
    while True:
        active, seq = turn_state.read_state(state_file)
        elapsed = time.monotonic() - t0
        decision = turn_state.rearm_decision(
            elapsed, active, seq > baseline_seq, grace_s=grace_s, max_s=max_s)
        if decision == "reopen":
            return
        time.sleep(REARM_POLL_S)


def _log(msg: str) -> None:
    print(f"wake_word: {msg}", file=sys.stderr, flush=True)


def _fire_trigger_and_wait(turn_state_file, rearm_mode, holdoff_s,
                           grace_s, max_s, cooldown_s, after_fire=None):
    """Emit ONE trigger newline (the wake/follow-up signal) and hold the mic
    closed until the turn it causes settles (Slice R), then a cooldown. Shared by
    the wake path and the follow-up path so both re-arm identically. The mic is
    already CLOSED by the caller before this runs (release-before-trigger)."""
    baseline_seq = turn_state.read_state(turn_state_file)[1]
    sys.stdout.write("\n")   # THE TRIGGER — sole stdout writer
    sys.stdout.flush()
    if after_fire is not None:
        after_fire()
    if rearm_mode == "timer":
        time.sleep(holdoff_s)   # legacy: blind fixed hold-off
    else:
        _wait_for_rearm(turn_state_file, baseline_seq, grace_s, max_s)
    time.sleep(cooldown_s)   # refractory: no immediate re-trigger


def _truthy(v: str | None) -> bool:
    return (v or "").strip().lower() in ("1", "true", "yes", "on")


class _Led:
    """Optional mic-open indicator. Degrades to a no-op without Jetson.GPIO —
    the LED is UX, never load-bearing."""

    def __init__(self, pin_env: str | None):
        self.pin = None
        self.gpio = None
        if not pin_env:
            return
        try:
            import Jetson.GPIO as GPIO  # type: ignore
            pin = int(pin_env)
            GPIO.setmode(GPIO.BOARD)
            GPIO.setup(pin, GPIO.OUT, initial=GPIO.LOW)
            self.pin, self.gpio = pin, GPIO
        except Exception as e:  # noqa: BLE001
            _log(f"LED disabled ({type(e).__name__}: {e})")

    def set(self, on: bool) -> None:
        if self.gpio is not None:
            try:
                self.gpio.output(self.pin, self.gpio.HIGH if on else self.gpio.LOW)
            except Exception:  # noqa: BLE001
                pass

    def close(self) -> None:
        if self.gpio is not None:
            try:
                self.set(False)
                self.gpio.cleanup(self.pin)
            except Exception:  # noqa: BLE001
                pass


def _transcribe(stt_cmd: list[str], window: bytes, tmp_dir: str) -> str:
    """Window → tmpfs wav → STT → transcript. The wav is deleted before this
    returns, success or not (transcribe-and-discard: nothing persists)."""
    fd, path = tempfile.mkstemp(suffix=".wav", dir=tmp_dir)
    try:
        with os.fdopen(fd, "wb") as f, wave.open(f, "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(BYTES_PER_SAMPLE)
            w.setframerate(SAMPLE_RATE)
            w.writeframes(window)
        p = subprocess.run(stt_cmd + [path], capture_output=True, text=True,
                           timeout=10.0)
        return p.stdout if p.returncode == 0 else ""
    except Exception:  # noqa: BLE001 — a failed STT cycle is just a missed window
        return ""
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass


def _ack(ack_cmd: str | None, tts_cmd: str | None) -> None:
    """The audible wake acknowledgment (voice line W1) — false fires must be
    HEARD, not silent. Precedence: KIRRA_WAKE_ACK_CMD, else "Yes?" through
    KIRRA_TTS_CMD, else a stderr note (print-not-speak parity with rabbit_ask)."""
    try:
        if ack_cmd:
            subprocess.run(shlex.split(ack_cmd), timeout=5.0,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        elif tts_cmd:
            subprocess.run(shlex.split(tts_cmd), input="Yes?", text=True,
                           timeout=10.0, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL)
        else:
            _log("wake ack (no KIRRA_WAKE_ACK_CMD / KIRRA_TTS_CMD — silent)")
    except Exception as e:  # noqa: BLE001 — a broken cue never blocks the trigger
        _log(f"ack failed ({type(e).__name__}: {e})")


def _read_state(path: str) -> str | None:
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        return None


def _listen_for_speech_onset(record_cmd, rms_floor, window_s, onset_hops,
                             hop_bytes, led, state_file) -> bool:
    """Follow-up mode (Slice F): open the mic and wait up to window_s for the
    operator to START speaking (onset_hops consecutive energy hops above the
    floor). Returns True on onset — with the mic already CLOSED so the turn
    recorder can claim the device (release-before-trigger, same as the wake
    path) — or False on a silent timeout / nap-mute / recorder fault (→ fall
    back to requiring the wake word). No STT and no phrase match: after Rabbit's
    OWN answer, the operator simply talking is the trigger, exactly like a
    Nest/Alexa follow-up. Carries no new authority — it fires the same fenced
    trigger the wake word does."""
    try:
        cap = subprocess.Popen(record_cmd, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL)
    except Exception as e:  # noqa: BLE001 — no mic → no follow-up, just fall back
        _log(f"follow-up: recorder failed to start ({type(e).__name__}) — wake-word only")
        return False
    led.set(True)
    loud = 0
    onset = False
    t0 = time.monotonic()
    try:
        while True:
            elapsed = time.monotonic() - t0
            # A nap/mute engaged during the window closes it immediately.
            if not wake_allowed(_read_state(state_file), int(time.time() * 1000)):
                break
            decision = followup_decision(elapsed, onset, window_s)
            if decision != "listen":
                break
            chunk = cap.stdout.read(hop_bytes)
            if not chunk:
                break  # recorder died → treat as no onset
            loud = loud + 1 if rms(chunk) >= rms_floor else 0
            if loud >= onset_hops:
                onset = True   # next iteration's decision → 'trigger' → break
    finally:
        led.set(False)
        cap.terminate()
        try:
            cap.wait(timeout=2.0)
        except subprocess.TimeoutExpired:
            cap.kill()
    return onset


def main() -> int:
    if not _truthy(os.environ.get("KIRRA_WAKE_ENABLED")):
        _log("KIRRA_WAKE_ENABLED not set — wake word off (exit 0; PTT/Enter still work)")
        return 0

    stt_raw = os.environ.get("KIRRA_WAKE_STT_CMD", "")
    if not stt_raw.strip():
        _log("FATAL: KIRRA_WAKE_ENABLED is on but KIRRA_WAKE_STT_CMD is unset — "
             'point it at whisper-cli with a TINY model, e.g. '
             '"whisper-cli -m models/ggml-tiny.en.bin -np -nt -f"')
        return 1
    stt_cmd = shlex.split(stt_raw)

    phrases = parse_phrases(os.environ.get("KIRRA_WAKE_PHRASES", DEFAULT_PHRASES))
    if not phrases:
        _log("FATAL: KIRRA_WAKE_PHRASES parsed to nothing — an enabled listener "
             "with no phrases is a misconfiguration")
        return 1

    record_cmd = shlex.split(os.environ.get(
        "KIRRA_WAKE_RECORD_CMD", "arecord -f S16_LE -r 16000 -c 1 -t raw"))
    rms_floor = float(os.environ.get("KIRRA_WAKE_RMS_FLOOR", "300"))
    cooldown_s = float(os.environ.get("KIRRA_WAKE_COOLDOWN_S", "2"))
    holdoff_s = float(os.environ.get("KIRRA_WAKE_HOLDOFF_S", "10"))
    state_file = os.environ.get("KIRRA_WAKE_STATE_FILE", "/tmp/kirra_rabbit_wake.state")
    # Slice R: re-arm the mic on the TURN-DONE signal, not a blind timer.
    #   default ("signal"): wait for rabbit_converse to finish the turn, bounded
    #   by grace_s (wait-for-start / garbage-clip reopen) and max_s (hung-writer
    #   ceiling). "timer" forces the legacy fixed-holdoff sleep.
    rearm_mode = (os.environ.get("KIRRA_WAKE_REARM") or "signal").strip().lower()
    turn_state_file = turn_state.state_path()
    rearm_grace_s = float(os.environ.get("KIRRA_WAKE_TURN_GRACE_S", "7"))
    rearm_max_s = float(os.environ.get("KIRRA_WAKE_TURN_MAX_S", "45"))
    # Slice F: follow-up mode (opt-in, like Nest/Alexa). After a reply, hold a
    # short window where the operator can just talk — no wake word — before the
    # listener falls back to requiring one.
    followup_enabled = _truthy(os.environ.get("KIRRA_WAKE_FOLLOWUP_ENABLED"))
    followup_window_s = float(os.environ.get("KIRRA_WAKE_FOLLOWUP_S", "6"))
    followup_onset_hops = max(1, int(os.environ.get("KIRRA_WAKE_FOLLOWUP_ONSET_HOPS", "2")))
    ack_cmd = os.environ.get("KIRRA_WAKE_ACK_CMD")
    tts_cmd = os.environ.get("KIRRA_TTS_CMD")
    tmp_dir = "/dev/shm" if os.path.isdir("/dev/shm") else tempfile.gettempdir()

    hop_bytes = int(SAMPLE_RATE * HOP_S) * BYTES_PER_SAMPLE
    window_bytes = int(SAMPLE_RATE * WINDOW_S) * BYTES_PER_SAMPLE
    led = _Led(os.environ.get("KIRRA_WAKE_LED_PIN"))
    _log(f"listening for {[' '.join(p) for p in phrases]} "
         f"(rms_floor={rms_floor:g}, holdoff={holdoff_s:g}s)")

    try:
        while True:
            # Nap/mute gate — while suspended, the mic stays CLOSED (no capture
            # process at all), polled once a second.
            if not wake_allowed(_read_state(state_file), int(time.time() * 1000)):
                led.set(False)
                time.sleep(STATE_POLL_S)
                continue

            cap = subprocess.Popen(record_cmd, stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL)
            led.set(True)
            ring = b""
            last_stt = 0.0
            last_state_poll = 0.0
            hit = None
            try:
                while True:
                    chunk = cap.stdout.read(hop_bytes)
                    if not chunk:
                        _log("capture stream ended (recorder died?) — retrying in 2 s")
                        time.sleep(2.0)
                        break
                    ring = (ring + chunk)[-window_bytes:]

                    now = time.monotonic()
                    if now - last_state_poll >= STATE_POLL_S:
                        last_state_poll = now
                        if not wake_allowed(_read_state(state_file),
                                            int(time.time() * 1000)):
                            _log("nap/mute engaged — releasing the mic")
                            break

                    # Energy pre-gate: a quiet hop runs no inference at all.
                    if rms(chunk) < rms_floor:
                        continue
                    # Rate-limit STT to at most one run per second of audio-with-
                    # energy (the window slides; adjacent runs see the phrase).
                    if now - last_stt < 1.0 or len(ring) < window_bytes:
                        continue
                    last_stt = now
                    text = _transcribe(stt_cmd, ring, tmp_dir)
                    hit = wake_hit(transcript_tokens(text), phrases)
                    if hit:
                        break
            finally:
                led.set(False)
                cap.terminate()
                try:
                    cap.wait(timeout=2.0)
                except subprocess.TimeoutExpired:
                    cap.kill()

            if hit:
                # Release-before-trigger: the mic is already closed (above), so
                # rabbit_voice.sh's bounded recorder can claim the device. The
                # ack ("Yes?") fires once, on the WAKE — not on each follow-up.
                _log(f'wake: "{hit}"')
                _fire_trigger_and_wait(
                    turn_state_file, rearm_mode, holdoff_s, rearm_grace_s,
                    rearm_max_s, cooldown_s,
                    after_fire=lambda: _ack(ack_cmd, tts_cmd))

                # Slice F: follow-up window(s). After the reply, listen briefly
                # for the operator to just start talking (no wake word). Each
                # onset fires another turn and reopens the window; a silent
                # window (or nap/mute) closes it → back to wake-word listening.
                while followup_enabled and wake_allowed(
                        _read_state(state_file), int(time.time() * 1000)):
                    if not _listen_for_speech_onset(
                            record_cmd, rms_floor, followup_window_s,
                            followup_onset_hops, hop_bytes, led, state_file):
                        break
                    _log("follow-up: speech onset — firing without a wake word")
                    _fire_trigger_and_wait(
                        turn_state_file, rearm_mode, holdoff_s, rearm_grace_s,
                        rearm_max_s, cooldown_s)
    except KeyboardInterrupt:
        return 0
    except BrokenPipeError:
        _log("stdout consumer gone (rabbit_voice.sh exited) — stopping")
        return 0
    finally:
        led.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
