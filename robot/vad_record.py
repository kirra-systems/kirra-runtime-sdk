#!/usr/bin/env python3
"""vad_record.py — VAD-endpointed bounded recorder (Slice 1 of the voice-cognition
roadmap). A DROP-IN replacement for `arecord -d N …`: it records ONE utterance
that stops on trailing SILENCE instead of always waiting a fixed window, then
writes it to the WAV path passed as its last argument (the house convention —
`rabbit_voice.sh` calls `$KIRRA_RECORD_CMD "$wav"`).

    KIRRA_RECORD_CMD="python3 /opt/kirra/robot/vad_record.py"   # opt in
    #  (unset / left as `arecord -d 4 …` → byte-identical prior behaviour)

Why: the fixed `-d 4` clip is the single biggest voice-latency lever
(docs/hardware/RABBIT_AUDIO_STACK.md §1). Endpointing on silence makes a terse
command ("check yourself") return in ~1 s instead of always 4 s, while a longer
sentence still gets its time — up to a HARD cap.

🔴 SAFETY / DISCIPLINE — this is a RECORDER, not a new authority:
  * It is still a BOUNDED mic, never an open one. `KIRRA_VAD_MAX_MS` is a HARD
    ceiling (the endpointer STOPs at it no matter what), exactly like arecord's
    `-d` bound — the "no open mic" guarantee is preserved.
  * It only produces a WAV that the EXISTING pipeline transcribes. It emits no
    intent, no command, no motion; a silent/garbled clip yields an empty
    transcript → the fenced parser latches nothing → no motion (fail-closed).
  * Fail-closed on capture/backend error: no clip (or an empty one) → the turn
    dead-ends, never a hang (bounded by MAX_MS / START_TIMEOUT_MS) and never a
    fabricated utterance.

Backends (`KIRRA_VAD_BACKEND`):
  * `energy` (default) — RMS energy vs a floor. ZERO new deps (same idea as
    wake_word.py's pre-gate); robust for endpointing in a reasonable environment.
  * anything else → FATAL exit (fail-closed seam; a Silero/webrtc backend is a
    later slice — no silent fallback to an unimplemented detector).

Env (all optional):
  KIRRA_VAD_BACKEND        'energy' (default); other → refuse
  KIRRA_VAD_CAPTURE_CMD    raw-PCM capture to stdout
                           (default "arecord -f S16_LE -r 16000 -c 1 -t raw")
  KIRRA_VAD_RMS_FLOOR      speech energy threshold (default 300; 16-bit FS=32767)
  KIRRA_VAD_FRAME_MS       analysis frame (default 30 ms)
  KIRRA_VAD_MIN_SPEECH_MS  min speech before an endpoint is honored (default 300)
  KIRRA_VAD_SILENCE_MS     trailing silence that ends the utterance (default 800)
  KIRRA_VAD_MAX_MS         HARD ceiling — always stop by here (default 8000)
  KIRRA_VAD_START_TIMEOUT_MS  if speech never starts, give up (default 3000)
  KIRRA_VAD_SAMPLE_RATE    Hz (default 16000, whisper-native)

The pure core (`rms_energy`, `Endpointer`) is host-tested in vad_record_test.py;
the capture loop is the thin hardware seam.
"""
import os
import subprocess
import sys
import wave

SAMPLE_WIDTH_BYTES = 2  # S16_LE


def log(msg):
    """Human-facing output to STDERR only (stdout is left clean, though this tool
    writes its result to a file, not a pipe)."""
    print(f"vad_record: {msg}", file=sys.stderr, flush=True)


# ── pure core (host-tested; no I/O) ──────────────────────────────────────────

def rms_energy(frame_bytes):
    """RMS amplitude of a little-endian signed-16 PCM frame (0.0 for an empty
    frame). Pure integer math — no numpy dep."""
    n = len(frame_bytes) // SAMPLE_WIDTH_BYTES
    if n == 0:
        return 0.0
    total = 0
    for i in range(0, n * SAMPLE_WIDTH_BYTES, SAMPLE_WIDTH_BYTES):
        s = int.from_bytes(frame_bytes[i:i + SAMPLE_WIDTH_BYTES], "little", signed=True)
        total += s * s
    return (total / n) ** 0.5


# Endpoint decisions.
CONTINUE = "continue"
STOP_ENDPOINTED = "endpointed"   # speech, then enough trailing silence
STOP_MAX = "max"                 # hit the hard ceiling
STOP_NO_SPEECH = "no_speech"     # speech never started


class Endpointer:
    """Silence-endpoint state machine, fed one (is_speech, t_ms) per frame.

    Guarantees:
      * never ends before `min_speech_ms` of speech has been seen (a click can't
        end the clip);
      * ends after `silence_ms` of trailing silence once speech has started;
      * ALWAYS ends by `max_ms` (the hard bounded-mic ceiling);
      * if speech never starts, ends at `start_timeout_ms` (→ empty clip → the
        turn dead-ends, fail-closed).
    Time is injected (t_ms) so the logic is deterministic under test."""

    def __init__(self, min_speech_ms=300, silence_ms=800, max_ms=8000,
                 start_timeout_ms=3000):
        self.min_speech_ms = min_speech_ms
        self.silence_ms = silence_ms
        self.max_ms = max_ms
        self.start_timeout_ms = start_timeout_ms
        self.speech_started_at = None
        self.last_speech_at = None
        self.prev_t_ms = 0
        self.speech_ms = 0  # ACCUMULATED speech time, not elapsed-since-start

    def feed(self, is_speech, t_ms):
        """Return one of CONTINUE / STOP_* for the frame ending at t_ms."""
        # The hard ceiling wins over everything — the bounded-mic guarantee.
        if t_ms >= self.max_ms:
            return STOP_MAX
        dt = t_ms - self.prev_t_ms
        self.prev_t_ms = t_ms
        if self.speech_started_at is None:
            if is_speech:
                self.speech_started_at = t_ms
                self.last_speech_at = t_ms
                self.speech_ms += dt
            elif t_ms >= self.start_timeout_ms:
                return STOP_NO_SPEECH
            return CONTINUE
        # In speech.
        if is_speech:
            self.last_speech_at = t_ms
            self.speech_ms += dt
            return CONTINUE
        trailing_silence_ms = t_ms - self.last_speech_at
        # Endpoint only after ENOUGH ACTUAL SPEECH (a lone click never qualifies)
        # AND enough trailing silence.
        if self.speech_ms >= self.min_speech_ms and trailing_silence_ms >= self.silence_ms:
            return STOP_ENDPOINTED
        return CONTINUE


# ── hardware seam (thin; not unit-tested) ────────────────────────────────────

def _env_int(key, default):
    raw = (os.environ.get(key) or "").strip()
    if not raw:
        return default
    try:
        return int(raw, 0)
    except ValueError:
        log(f"{key}={raw!r} not an integer — using default {default}")
        return default


def _write_wav(path, pcm, sample_rate):
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(SAMPLE_WIDTH_BYTES)
        w.setframerate(sample_rate)
        w.writeframes(pcm)


def main(argv):
    if len(argv) < 1:
        log("FATAL: no output WAV path (it is appended as the last argument, "
            "like arecord's target).")
        return 2
    out_path = argv[-1]

    backend = (os.environ.get("KIRRA_VAD_BACKEND") or "energy").strip().lower()
    if backend != "energy":
        log(f"FATAL: KIRRA_VAD_BACKEND={backend!r} not supported (only 'energy' "
            "is wired; silero/webrtc are a later slice).")
        return 2

    sample_rate = _env_int("KIRRA_VAD_SAMPLE_RATE", 16000)
    frame_ms = _env_int("KIRRA_VAD_FRAME_MS", 30)
    floor = _env_int("KIRRA_VAD_RMS_FLOOR", 300)
    endpointer = Endpointer(
        min_speech_ms=_env_int("KIRRA_VAD_MIN_SPEECH_MS", 300),
        silence_ms=_env_int("KIRRA_VAD_SILENCE_MS", 800),
        max_ms=_env_int("KIRRA_VAD_MAX_MS", 8000),
        start_timeout_ms=_env_int("KIRRA_VAD_START_TIMEOUT_MS", 3000),
    )
    frame_bytes = max(1, (sample_rate * frame_ms) // 1000) * SAMPLE_WIDTH_BYTES

    import shlex
    capture_cmd = os.environ.get("KIRRA_VAD_CAPTURE_CMD") \
        or f"arecord -f S16_LE -r {sample_rate} -c 1 -t raw"
    capture_argv = shlex.split(capture_cmd)

    log(f"endpointing capture ({backend}, floor={floor}, silence="
        f"{endpointer.silence_ms}ms, max={endpointer.max_ms}ms) → {out_path}")

    pcm = bytearray()
    proc = None
    try:
        proc = subprocess.Popen(capture_argv, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL)
    except Exception as e:  # noqa: BLE001
        log(f"FATAL: capture command failed to start ({e})")
        return 2

    reason = STOP_MAX
    t_ms = 0
    try:
        while True:
            chunk = proc.stdout.read(frame_bytes)
            if not chunk or len(chunk) < frame_bytes:
                # Stream ended / underran — stop with whatever we have (fail-soft).
                break
            t_ms += frame_ms
            is_speech = rms_energy(chunk) >= floor
            decision = endpointer.feed(is_speech, t_ms)
            if is_speech or endpointer.speech_started_at is not None:
                pcm += chunk  # keep audio only from first speech onward (+ trailing)
            if decision != CONTINUE:
                reason = decision
                break
    finally:
        if proc:
            proc.terminate()
            try:
                proc.wait(timeout=1)
            except Exception:  # noqa: BLE001
                proc.kill()

    # Fail-closed: no speech → write an (empty) clip so the turn dead-ends cleanly
    # in the parser rather than reusing a stale file.
    try:
        _write_wav(out_path, bytes(pcm), sample_rate)
    except Exception as e:  # noqa: BLE001
        log(f"FATAL: could not write {out_path} ({e})")
        return 2

    log(f"stopped: {reason} ({t_ms} ms, {len(pcm)} bytes pcm)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
