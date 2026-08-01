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

Env:
  KIRRA_VAD_DEVICE         REQUIRED for the built-in arecord backend — the ALSA
                           capture device, e.g. "plughw:CARD=Device,DEV=0".
                           There is deliberately NO default: ALSA's default is
                           whichever card the kernel enumerated first, which on
                           the R2 is not the microphone, and the failure is
                           SILENT (the stream opens and delivers near-silence, so
                           every turn endpoints as "no speech"). Same rule the
                           wake listener already enforces on
                           KIRRA_WAKE_RECORD_CMD.
  KIRRA_VAD_CAPTURE_CMD    full raw-PCM-to-stdout capture command, overriding the
                           built-in arecord line. A custom (non-arecord) backend
                           is left alone — it has its own device convention and
                           this module has no business guessing it.
  KIRRA_VAD_BACKEND        'energy' (default); other → refuse
  KIRRA_VAD_RMS_FLOOR      speech energy threshold (default 350; 16-bit FS=32767;
                           Jetson-measured 2026-08: 300 let ambient noise hold
                           the clip open to the max_ms cap)
  KIRRA_VAD_FRAME_MS       analysis frame (default 30 ms)
  KIRRA_VAD_MIN_SPEECH_MS  min speech before an endpoint is honored (default 300)
  KIRRA_VAD_SILENCE_MS     trailing silence that ends the utterance (default 600;
                           Jetson-measured 2026-08: 600 endpoints reliably with
                           no clipping of final words)
  KIRRA_VAD_MAX_MS         HARD ceiling — always stop by here (default 8000;
                           must be positive and <= VAD_ABSOLUTE_MAX_MS)
  KIRRA_VAD_START_TIMEOUT_MS  if speech never starts, give up (default 3000)
  KIRRA_VAD_SAMPLE_RATE    Hz (default 16000, whisper-native)

Contract with the caller (`rabbit_voice.sh`): the WAV path is APPENDED as the
last argument, and a NONZERO exit means "no usable clip" — the caller logs
"recorder failed — nothing captured" and the turn dead-ends with no transcript,
no intent and no motion.

The pure core (`rms_energy`, `Endpointer`, `validate_capture_cmd`,
`resolve_capture_argv`, `validate_bounds`) is host-tested in vad_record_test.py;
only the read loop is the hardware seam.
"""
import os
import selectors
import shlex
import subprocess
import sys
import time
import wave

SAMPLE_WIDTH_BYTES = 2  # S16_LE

# An absolute ceiling on KIRRA_VAD_MAX_MS. The per-turn bound is operator
# tunable, but not without limit: this module's whole safety claim is "a BOUNDED
# mic, never an open one", and a config typo (`KIRRA_VAD_MAX_MS=600000`) must not
# be able to quietly turn a 6-second recorder into a ten-minute one.
VAD_ABSOLUTE_MAX_MS = 30_000

# How long the read loop waits past the hard ceiling before declaring the capture
# STALLED. The endpointer counts SAMPLES; if the device wedges, samples stop
# arriving and sample-time stops advancing, so a wall clock is what actually
# bounds the mic. Generous enough not to trip on ordinary scheduling jitter.
CAPTURE_STALL_GRACE_MS = 2_000


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


# ── pure configuration resolution (host-tested; no spawning, no probing) ─────

def validate_capture_cmd(raw):
    """Parse and check an explicit `KIRRA_VAD_CAPTURE_CMD`.

    Returns `(argv, "")` or `(None, reason)`. Pure: nothing is spawned and no
    device is probed, so a misconfigured turn recorder fails BEFORE a microphone
    is opened.

    Same two rules the wake listener applies to `KIRRA_WAKE_RECORD_CMD`
    (`wake_word.validate_record_cmd`) — deliberately identical, because the
    failure mode is identical:

      - the command must parse and name a program;
      - if that program is `arecord`, it must select a device explicitly
        (`-D`/`--device`). A bare `arecord` takes ALSA's default — the first card
        the kernel enumerated, which on the R2 is not the mic — and the failure
        is SILENT: near-silence arrives, every turn endpoints as "no speech",
        and the operator sees a robot that never hears them.

    A non-`arecord` program is LEFT ALONE. A custom capture backend has its own
    device-selection convention and this module has no business guessing it.
    """
    if not raw or not raw.strip():
        return None, "KIRRA_VAD_CAPTURE_CMD is blank"
    try:
        argv = shlex.split(raw)
    except ValueError as exc:
        return None, f"KIRRA_VAD_CAPTURE_CMD is not parseable as a command: {exc}"
    if not argv:
        return None, "KIRRA_VAD_CAPTURE_CMD parsed to an empty command"
    if os.path.basename(argv[0]) == "arecord" and \
            not any(a in ("-D", "--device") for a in argv[1:]):
        return None, ("KIRRA_VAD_CAPTURE_CMD uses arecord without -D/--device, so it "
                      "would capture from ALSA's DEFAULT device — on this platform "
                      "that is not the microphone, and every turn would record "
                      "near-silence without erroring. Name the device, e.g. "
                      '"arecord -D plughw:CARD=Device,DEV=0 -f S16_LE -r 16000 '
                      '-c 1 -t raw"')
    return argv, ""


def resolve_capture_argv(device, capture_cmd, sample_rate):
    """Decide what to run for capture. Returns `(argv, "")` or `(None, reason)`.

    Precedence, both fail-closed:
      1. an explicit `KIRRA_VAD_CAPTURE_CMD` wins (validated as above) — this is
         the escape hatch for a non-arecord backend;
      2. otherwise the built-in arecord line, which REQUIRES `KIRRA_VAD_DEVICE`.

    There is no third case. With neither set we refuse rather than fall back to
    ALSA's default device, for exactly the reason above.
    """
    if capture_cmd and capture_cmd.strip():
        return validate_capture_cmd(capture_cmd)
    if not device or not device.strip():
        return None, ("KIRRA_VAD_DEVICE is unset and no KIRRA_VAD_CAPTURE_CMD was "
                      "given, so there is no microphone to open. Set an EXPLICIT "
                      'ALSA device, e.g. KIRRA_VAD_DEVICE="plughw:CARD=Device,DEV=0" '
                      "(`arecord -l` lists the cards). There is no default: ALSA's "
                      "default device is not the microphone on this platform.")
    return ["arecord", "-D", device.strip(), "-f", "S16_LE",
            "-r", str(sample_rate), "-c", "1", "-t", "raw"], ""


def capture_device(argv):
    """The ALSA device `argv` selects, or "" if it names none. Diagnostics only —
    reporting WHICH mic was opened is the difference between "VAD is on" and
    "VAD is on and listening to the right thing"."""
    for flag, value in zip(argv, argv[1:]):
        if flag in ("-D", "--device"):
            return value
    return ""


def validate_bounds(max_ms, start_timeout_ms, silence_ms, min_speech_ms, frame_ms):
    """Check the timing configuration. Returns "" if usable, else the reason.

    The bounded-mic guarantee is only as good as these numbers, so a nonsense
    value is REFUSED rather than silently defaulted: a non-positive ceiling would
    stop the clip before a single frame (a recorder that records nothing), and an
    absurdly large one would defeat the bound this module exists to provide.
    """
    if max_ms <= 0:
        return f"KIRRA_VAD_MAX_MS must be positive (got {max_ms})"
    if max_ms > VAD_ABSOLUTE_MAX_MS:
        return (f"KIRRA_VAD_MAX_MS={max_ms} exceeds the absolute ceiling "
                f"{VAD_ABSOLUTE_MAX_MS} ms — this is a bounded recorder, not an "
                "open microphone")
    if start_timeout_ms <= 0:
        return f"KIRRA_VAD_START_TIMEOUT_MS must be positive (got {start_timeout_ms})"
    if start_timeout_ms > max_ms:
        return (f"KIRRA_VAD_START_TIMEOUT_MS={start_timeout_ms} exceeds "
                f"KIRRA_VAD_MAX_MS={max_ms} — the start timeout could never fire")
    if silence_ms <= 0:
        return f"KIRRA_VAD_SILENCE_MS must be positive (got {silence_ms})"
    if min_speech_ms < 0:
        return f"KIRRA_VAD_MIN_SPEECH_MS must not be negative (got {min_speech_ms})"
    if frame_ms <= 0:
        return f"KIRRA_VAD_FRAME_MS must be positive (got {frame_ms})"
    return ""


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
    floor = _env_int("KIRRA_VAD_RMS_FLOOR", 350)
    min_speech_ms = _env_int("KIRRA_VAD_MIN_SPEECH_MS", 300)
    silence_ms = _env_int("KIRRA_VAD_SILENCE_MS", 600)
    max_ms = _env_int("KIRRA_VAD_MAX_MS", 8000)
    start_timeout_ms = _env_int("KIRRA_VAD_START_TIMEOUT_MS", 3000)

    # Every check below runs BEFORE a microphone is opened (nothing is spawned
    # until the Popen further down), so a misconfigured recorder fails without
    # ever touching the device.
    bad_bounds = validate_bounds(max_ms, start_timeout_ms, silence_ms,
                                 min_speech_ms, frame_ms)
    if bad_bounds:
        log(f"FATAL: {bad_bounds}")
        return 2

    capture_argv, why = resolve_capture_argv(
        os.environ.get("KIRRA_VAD_DEVICE"),
        os.environ.get("KIRRA_VAD_CAPTURE_CMD"),
        sample_rate)
    if capture_argv is None:
        log(f"FATAL: {why}")
        return 2

    endpointer = Endpointer(min_speech_ms=min_speech_ms, silence_ms=silence_ms,
                            max_ms=max_ms, start_timeout_ms=start_timeout_ms)
    frame_bytes = max(1, (sample_rate * frame_ms) // 1000) * SAMPLE_WIDTH_BYTES

    device = capture_device(capture_argv) or f"{os.path.basename(capture_argv[0])} (custom backend)"
    log(f"endpointing capture ({backend}, device={device}, floor={floor}, "
        f"silence={silence_ms}ms, max={max_ms}ms) → {out_path}")

    pcm = bytearray()
    proc = None
    try:
        proc = subprocess.Popen(capture_argv, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL)
    except Exception as e:  # noqa: BLE001
        log(f"FATAL: capture command failed to start ({e})")
        return 2

    # The WALL-CLOCK bound. The endpointer counts SAMPLES, which only bounds the
    # mic while samples keep arriving at the expected rate — a wedged device
    # delivers none, sample-time stops advancing, and a plain blocking read would
    # wait forever with the mic held open. So the loop is driven by a selector
    # with a deadline, and a stall is a hard stop like any other.
    deadline = time.monotonic() + (max_ms + CAPTURE_STALL_GRACE_MS) / 1000.0
    stalled = False
    reason = STOP_MAX
    t_ms = 0
    pending = bytearray()
    sel = selectors.DefaultSelector()
    sel.register(proc.stdout, selectors.EVENT_READ)
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                stalled = True
                break
            if not sel.select(timeout=remaining):
                stalled = True     # nothing readable before the deadline
                break
            data = os.read(proc.stdout.fileno(), frame_bytes)
            if not data:
                break              # EOF — the capture process ended on its own
            pending += data
            if len(pending) < frame_bytes:
                continue           # partial frame; wait for the rest
            chunk = bytes(pending[:frame_bytes])
            del pending[:frame_bytes]
            t_ms += frame_ms
            is_speech = rms_energy(chunk) >= floor
            decision = endpointer.feed(is_speech, t_ms)
            if is_speech or endpointer.speech_started_at is not None:
                pcm += chunk  # keep audio only from first speech onward (+ trailing)
            if decision != CONTINUE:
                reason = decision
                break
    finally:
        sel.close()
        if proc:
            proc.terminate()
            try:
                proc.wait(timeout=1)
            except Exception:  # noqa: BLE001
                proc.kill()

    if stalled:
        # A held-open, silent device. Never a hang and never a stale clip.
        log(f"FATAL: capture stalled — no audio for {max_ms}+{CAPTURE_STALL_GRACE_MS} ms "
            f"from {device}. Is the mic in use by another process?")
        return 2

    # A capture process that exited on its own with a nonzero status never gave
    # us a usable clip (a bad device, a busy card, wrong format). Report it: the
    # caller then logs "recorder failed" and the turn dead-ends, instead of
    # transcribing a silent stub and looking like a mishearing.
    # rc is negative when WE terminated it (the normal endpointed path), so only
    # a genuine self-exit with a positive status counts.
    rc = proc.returncode if proc else None
    if t_ms == 0 and rc is not None and rc > 0:
        log(f"FATAL: capture command exited {rc} without producing audio "
            f"(device={device}).")
        return 2

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
