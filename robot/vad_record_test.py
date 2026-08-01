#!/usr/bin/env python3
"""Host tests for the pure VAD-endpointing core in `vad_record.py`.

Pure and dependency-light: `rms_energy` and the `Endpointer` state machine do no
I/O (no mic, no arecord, no wave file), so they run on a plain host. The capture
loop is the thin hardware seam, exercised on the robot.

Runs standalone (`python3 robot/vad_record_test.py`, exit 1 on any failure);
also importable under pytest.

Covers: RMS on silence vs a loud frame; the endpoint honored only after
min-speech + trailing-silence; a short click NOT ending the clip; continuous
speech capped by the HARD ceiling (bounded-mic guarantee); no-speech timeout;
and speech→silence→speech re-arming the trailing-silence window.
"""
from __future__ import annotations

import re
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vad_record import (  # noqa: E402
    CONTINUE, STOP_ENDPOINTED, STOP_MAX, STOP_NO_SPEECH, VAD_ABSOLUTE_MAX_MS,
    Endpointer, capture_device, resolve_capture_argv, rms_energy,
    validate_bounds, validate_capture_cmd,
)

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


def _frame(amplitude, n=480):
    """A PCM frame of `n` constant-amplitude int16 samples (little-endian)."""
    return struct.pack("<%dh" % n, *([amplitude] * n))


# ── rms_energy ───────────────────────────────────────────────────────────────

def test_rms_silence_vs_loud():
    check(rms_energy(b"") == 0.0, "empty frame → 0")
    check(rms_energy(_frame(0)) == 0.0, "silent frame → 0")
    loud = rms_energy(_frame(5000))
    check(abs(loud - 5000.0) < 1.0, f"constant 5000 → rms ~5000, got {loud}")
    check(rms_energy(_frame(100)) < rms_energy(_frame(3000)), "louder → higher rms")


# ── endpoint: the normal case ────────────────────────────────────────────────

def _run(endpointer, speech_flags, frame_ms=30):
    """Feed a list of per-frame is_speech bools; return (stop_reason, t_ms)."""
    t = 0
    for is_speech in speech_flags:
        t += frame_ms
        d = endpointer.feed(is_speech, t)
        if d != CONTINUE:
            return d, t
    return CONTINUE, t


def test_endpoint_after_speech_then_silence():
    ep = Endpointer(min_speech_ms=300, silence_ms=300, max_ms=8000, start_timeout_ms=3000)
    # 400 ms speech (14 frames) then silence; endpoint ~300 ms into the silence.
    flags = [True] * 14 + [False] * 20
    reason, t = _run(ep, flags)
    check(reason == STOP_ENDPOINTED, f"expected endpointed, got {reason}")
    # speech ended at 420 ms; +300 ms silence → ~720 ms.
    check(680 <= t <= 760, f"endpoint timing off: {t} ms")


def test_short_click_does_not_end():
    # A single 30 ms blip (< min_speech 300) then silence must NOT endpoint;
    # it should run to the no-speech/other stop, never STOP_ENDPOINTED early.
    ep = Endpointer(min_speech_ms=300, silence_ms=300, max_ms=8000, start_timeout_ms=3000)
    flags = [True] + [False] * 40
    reason, t = _run(ep, flags)
    check(reason != STOP_ENDPOINTED,
          f"a sub-min click must not endpoint the clip (got {reason} at {t})")


def test_continuous_speech_hits_hard_cap():
    ep = Endpointer(min_speech_ms=300, silence_ms=800, max_ms=1000, start_timeout_ms=3000)
    flags = [True] * 200  # never any silence
    reason, t = _run(ep, flags)
    check(reason == STOP_MAX, f"continuous speech must hit the hard cap, got {reason}")
    check(t >= 1000, f"cap should fire at/after max_ms, got {t}")


def test_no_speech_times_out():
    ep = Endpointer(min_speech_ms=300, silence_ms=800, max_ms=8000, start_timeout_ms=1000)
    flags = [False] * 200
    reason, t = _run(ep, flags)
    check(reason == STOP_NO_SPEECH, f"silence-only must time out, got {reason}")
    check(t >= 1000, f"no-speech timeout at start_timeout_ms, got {t}")


def test_pause_within_speech_rearms_silence():
    # speech, a SHORT pause (< silence_ms), then speech again → must NOT endpoint
    # at the short pause; only the final trailing silence ends it.
    ep = Endpointer(min_speech_ms=300, silence_ms=500, max_ms=8000, start_timeout_ms=3000)
    flags = ([True] * 14) + ([False] * 10) + ([True] * 10) + ([False] * 30)
    reason, t = _run(ep, flags)
    check(reason == STOP_ENDPOINTED, f"expected final endpoint, got {reason}")
    # It must have survived the 300 ms mid-pause (10 frames) and ended in the
    # final silence run, i.e. well after the second speech burst.
    check(t > (14 + 10 + 10) * 30, f"ended too early (mid-pause?): {t} ms")


def test_hard_cap_wins_even_during_speech():
    # At exactly max_ms the endpointer stops regardless of speech state.
    ep = Endpointer(min_speech_ms=0, silence_ms=800, max_ms=300, start_timeout_ms=3000)
    check(ep.feed(True, 300) == STOP_MAX, "max_ms boundary must stop even mid-speech")


# ── exact threshold boundaries ───────────────────────────────────────────────

def test_min_speech_boundary_is_inclusive():
    # Exactly min_speech_ms of accumulated speech DOES qualify; one frame less
    # does not. The endpoint rule is `>=`, and which side of it a real utterance
    # lands on decides whether the operator's short command is kept or dropped.
    for frames, want_endpoint in ((10, True), (9, False)):
        ep = Endpointer(min_speech_ms=300, silence_ms=300, max_ms=8000,
                        start_timeout_ms=5000)
        reason, _ = _run(ep, [True] * frames + [False] * 20)
        got = reason == STOP_ENDPOINTED
        check(got == want_endpoint,
              f"{frames} speech frames (={frames * 30} ms) vs min 300: "
              f"expected endpoint={want_endpoint}, got {reason}")


def test_trailing_silence_boundary_is_inclusive():
    # Endpoint fires on the frame where trailing silence REACHES silence_ms,
    # not one frame later — that difference is pure added latency per turn.
    ep = Endpointer(min_speech_ms=60, silence_ms=300, max_ms=8000,
                    start_timeout_ms=5000)
    for _ in range(4):                       # 120 ms speech, ends at t=120
        ep.feed(True, ep.prev_t_ms + 30)
    t = 120
    for _i in range(1, 20):                  # silence frames
        t += 30
        d = ep.feed(False, t)
        if d == STOP_ENDPOINTED:
            check(t - 120 == 300, f"endpoint at {t - 120} ms of silence, want exactly 300")
            return
    check(False, "trailing silence never endpointed")


def test_start_timeout_boundary():
    # At exactly start_timeout_ms with no speech, give up.
    ep = Endpointer(min_speech_ms=300, silence_ms=800, max_ms=8000,
                    start_timeout_ms=1000)
    check(ep.feed(False, 970) == CONTINUE, "before the timeout must continue")
    check(ep.feed(False, 1000) == STOP_NO_SPEECH, "at the timeout must give up")


# ── the latency claim, in the simulated timing model ─────────────────────────

def test_a_short_command_endpoints_well_before_the_old_fixed_window():
    """The whole point of the change.

    The deployed recorder was `arecord -d 4` — EVERY turn cost 4000 ms of
    capture, however short. With the deployed VAD settings a ~700 ms command
    ("how are you?") must finish far sooner. Same frame clock as the real loop
    (30 ms), so this is the recorder's own timing model, not wall clock.
    """
    OLD_FIXED_MS = 4000
    ep = Endpointer(min_speech_ms=250, silence_ms=600, max_ms=6000,
                    start_timeout_ms=3000)     # the deployed settings
    speech_frames = 23                          # ~690 ms of speech
    reason, t = _run(ep, [True] * speech_frames + [False] * 100)
    check(reason == STOP_ENDPOINTED, f"a normal short command must endpoint, got {reason}")
    check(t < OLD_FIXED_MS,
          f"VAD took {t} ms — no better than the fixed {OLD_FIXED_MS} ms window")
    # Speech ends at 690 ms; +600 ms silence ⇒ ~1290 ms, a ~2.7 s saving.
    check(1250 <= t <= 1350, f"expected ~1290 ms, got {t}")
    check(OLD_FIXED_MS - t > 2000, f"saving only {OLD_FIXED_MS - t} ms; expected > 2 s")


def test_a_long_utterance_still_gets_its_time_up_to_the_cap():
    # The flip side: VAD must not truncate someone speaking a full sentence.
    ep = Endpointer(min_speech_ms=250, silence_ms=600, max_ms=6000,
                    start_timeout_ms=3000)
    reason, t = _run(ep, [True] * 150 + [False] * 40)   # 4.5 s of speech
    check(reason == STOP_ENDPOINTED, f"expected endpoint, got {reason}")
    check(t > 4500, f"a 4.5 s utterance was cut at {t} ms")
    check(t <= 6000, f"must never exceed the hard cap, got {t} ms")


def test_shipped_endpoint_defaults_are_the_jetson_calibrated_values():
    """The 2026-08 Orin calibration: RMS floor 350 (300 let ambient noise
    hold the clip open to the max_ms cap — 'stopped: max (8000 ms)') and
    trailing silence 600 ms (endpointed reliably, no clipping of final
    words, STT accuracy preserved across three consecutive captures). A
    silent revert of either default reintroduces the max-duration failure,
    so the shipped literals are pinned here."""
    import pathlib
    src = (pathlib.Path(__file__).resolve().parent / "vad_record.py").read_text()
    check('_env_int("KIRRA_VAD_RMS_FLOOR", 350)' in src,
          "shipped RMS floor default must be the calibrated 350")
    check('_env_int("KIRRA_VAD_SILENCE_MS", 600)' in src,
          "shipped trailing-silence default must be the calibrated 600 ms")


# ── explicit capture device required (fail-closed, before any mic opens) ─────

_GOOD_CMD = "arecord -D plughw:CARD=Device,DEV=0 -f S16_LE -r 16000 -c 1 -t raw"


def test_bare_arecord_capture_cmd_is_refused():
    argv, why = validate_capture_cmd("arecord -f S16_LE -r 16000 -c 1 -t raw")
    check(argv is None, "bare arecord must be refused (it takes ALSA's default)")
    check("default" in why.lower(), f"the reason must name the failure: {why}")


def test_arecord_with_an_explicit_device_is_accepted():
    for cmd in (_GOOD_CMD, "arecord --device plughw:1,0 -t raw",
                "/usr/bin/arecord -D hw:2,0 -t raw"):
        argv, why = validate_capture_cmd(cmd)
        check(argv is not None, f"must be accepted: {cmd} ({why})")


def test_a_custom_capture_backend_is_left_alone():
    # Not arecord → no ALSA-specific rule. A custom backend picks its device
    # however it likes and this module has no business guessing the convention.
    argv, why = validate_capture_cmd("my-capture --raw --rate 16000")
    check(argv is not None, f"a custom backend must not get the arecord rule: {why}")
    check(argv[0] == "my-capture", f"argv preserved verbatim: {argv}")


def test_blank_and_unparseable_capture_cmds_are_refused():
    for raw in ("", "   ", None, 'arecord -D "unterminated'):
        argv, _ = validate_capture_cmd(raw)
        check(argv is None, f"must be refused: {raw!r}")


def test_device_is_required_when_no_capture_cmd_is_given():
    argv, why = resolve_capture_argv(None, None, 16000)
    check(argv is None, "no device and no capture command must refuse, not default")
    check("KIRRA_VAD_DEVICE" in why, f"the reason must name the fix: {why}")
    argv, why = resolve_capture_argv("   ", "", 16000)
    check(argv is None, "a blank device is still no device")


def test_device_builds_the_arecord_line_with_the_right_rate():
    argv, why = resolve_capture_argv("plughw:CARD=Device,DEV=0", None, 16000)
    check(argv is not None, f"a named device must resolve: {why}")
    check(capture_device(argv) == "plughw:CARD=Device,DEV=0", f"device not carried: {argv}")
    check("-t" in argv and argv[argv.index("-t") + 1] == "raw", f"must be a RAW stream: {argv}")
    check(argv[argv.index("-r") + 1] == "16000", f"sample rate not honored: {argv}")
    argv, _ = resolve_capture_argv("hw:1,0", None, 8000)
    check(argv[argv.index("-r") + 1] == "8000", f"sample rate not honored: {argv}")


def test_an_explicit_capture_cmd_wins_over_the_device():
    argv, _ = resolve_capture_argv("plughw:CARD=Ignored,DEV=0", _GOOD_CMD, 16000)
    check(capture_device(argv) == "plughw:CARD=Device,DEV=0",
          f"KIRRA_VAD_CAPTURE_CMD must take precedence: {argv}")


def test_capture_device_reports_none_for_a_custom_backend():
    check(capture_device(["my-capture", "--raw"]) == "", "no -D → no device reported")


def test_validation_never_spawns_anything():
    # The point of validating first: no microphone is opened and no process is
    # started while deciding whether the configuration is usable.
    import subprocess
    spawned = []
    saved = {}
    for name in ("Popen", "run", "call", "check_call", "check_output"):
        saved[name] = getattr(subprocess, name)
        setattr(subprocess, name,
                lambda *a, _n=name, **k: spawned.append(_n))
    try:
        for cmd in ("", "   ", "arecord -f S16_LE", _GOOD_CMD, "my-capture --raw"):
            validate_capture_cmd(cmd)
            resolve_capture_argv("plughw:CARD=Device,DEV=0", cmd, 16000)
        validate_bounds(6000, 3000, 650, 250, 30)
    finally:
        for name, fn in saved.items():
            setattr(subprocess, name, fn)
    check(spawned == [], f"validation spawned: {spawned}")


# ── bounds are validated, so the mic stays bounded ───────────────────────────

def test_sane_bounds_are_accepted():
    check(validate_bounds(6000, 3000, 650, 250, 30) == "", "the deployed settings must pass")


def test_non_positive_and_absurd_bounds_are_refused():
    check(validate_bounds(0, 3000, 650, 250, 30) != "", "max_ms=0 must be refused")
    check(validate_bounds(-1, 3000, 650, 250, 30) != "", "negative max_ms must be refused")
    check(validate_bounds(6000, 0, 650, 250, 30) != "", "start_timeout=0 must be refused")
    check(validate_bounds(6000, 3000, 0, 250, 30) != "", "silence_ms=0 must be refused")
    check(validate_bounds(6000, 3000, 650, -1, 30) != "", "negative min_speech must be refused")
    check(validate_bounds(6000, 3000, 650, 250, 0) != "", "frame_ms=0 must be refused")


def test_an_absurd_ceiling_cannot_turn_this_into_an_open_mic():
    check(validate_bounds(VAD_ABSOLUTE_MAX_MS, 3000, 650, 250, 30) == "",
          "the absolute ceiling itself must be allowed")
    why = validate_bounds(VAD_ABSOLUTE_MAX_MS + 1, 3000, 650, 250, 30)
    check(why != "", "past the absolute ceiling must be refused")
    check("open microphone" in why, f"the reason must say why: {why}")
    check(validate_bounds(600_000, 3000, 650, 250, 30) != "",
          "a 10-minute ceiling (a plausible typo) must be refused")


def test_a_start_timeout_past_the_cap_is_refused():
    # It could never fire — the ceiling would always win, so the "speech never
    # started" path would silently become "record the full window".
    check(validate_bounds(3000, 6000, 650, 250, 30) != "",
          "start_timeout > max_ms must be refused")


# ── the two recorder contracts stay independent ──────────────────────────────

def test_wake_and_turn_recorder_contracts_are_separate():
    """`KIRRA_WAKE_RECORD_CMD` is the always-on RAW stream; `KIRRA_RECORD_CMD` is
    the one-turn WAV recorder. Swapping them is a real deployment mistake — the
    turn recorder would never terminate, or the wake listener would stop after
    one bounded window — so the two live in different modules with different
    validators and neither reads the other's variable."""
    import wake_word
    src = Path(__file__).resolve().parent

    def env_reads(module_name):
        """Env vars a module actually READS (mentions in prose don't count —
        the two files reference each other's variables on purpose, to explain
        why they are different contracts)."""
        text = (src / module_name).read_text()
        return set(re.findall(r'environ\.get\(\s*"([A-Z0-9_]+)"', text)) | \
            set(re.findall(r'environ\[\s*"([A-Z0-9_]+)"', text))

    vad_reads, wake_reads = env_reads("vad_record.py"), env_reads("wake_word.py")
    check("KIRRA_WAKE_RECORD_CMD" not in vad_reads,
          f"vad_record must not READ the wake recorder's variable: {vad_reads}")
    check("KIRRA_RECORD_CMD" not in wake_reads,
          f"wake_word must not READ the turn recorder's variable: {wake_reads}")
    check("KIRRA_VAD_DEVICE" not in wake_reads,
          "wake_word must not read the turn recorder's device")
    check("KIRRA_VAD_DEVICE" in vad_reads, f"sanity: vad_record reads its own device: {vad_reads}")
    # Same rule, independently implemented — a bare arecord is refused by both.
    check(wake_word.validate_record_cmd("arecord -t raw")[0] is None,
          "wake validator must still refuse a bare arecord")
    check(validate_capture_cmd("arecord -t raw")[0] is None,
          "vad validator must refuse a bare arecord")


# ── the capture seam, end to end (a scripted backend — no mic, no arecord) ───
#
# These drive the REAL main() with KIRRA_VAD_CAPTURE_CMD pointed at a scripted
# PCM producer, so the WAV shape, the exit-code contract and the bounded-mic
# guarantee are checked against the actual read loop rather than argued about.
# Deterministic: every stop is driven by the scripted audio or a bound.

_BACKEND_SPEECH = (
    "import sys,struct,time\n"
    "f=480\n"
    "loud=struct.pack('<%dh'%f,*([6000]*f))\n"
    "quiet=struct.pack('<%dh'%f,*([0]*f))\n"
    "n=0\n"
    "while True:\n"
    "    sys.stdout.buffer.write(loud if n<{speech_frames} else quiet)\n"
    "    sys.stdout.buffer.flush()\n"
    "    n+=1\n"
    "    time.sleep(0.005)\n"
)
_BACKEND_STALL = "import time\ntime.sleep(600)\n"
_BACKEND_FAIL = "import sys\nsys.exit(3)\n"

_VAD = None


def _vad_path():
    global _VAD
    if _VAD is None:
        _VAD = str(Path(__file__).resolve().parent / "vad_record.py")
    return _VAD


def _record(backend_src, out_path, **env_extra):
    """Run vad_record.py in a child with a scripted capture backend.
    Returns (exit_code, stderr)."""
    import os
    import subprocess
    import tempfile
    with tempfile.NamedTemporaryFile("w", suffix=".py", delete=False) as f:
        f.write(backend_src)
        backend = f.name
    env = {k: v for k, v in os.environ.items() if not k.startswith("KIRRA_VAD_")}
    env["KIRRA_VAD_CAPTURE_CMD"] = f"{sys.executable} {backend}"
    env.update({k: str(v) for k, v in env_extra.items()})
    try:
        done = subprocess.run([sys.executable, _vad_path(), out_path],
                              env=env, capture_output=True, text=True, timeout=60)
        return done.returncode, done.stderr
    finally:
        os.unlink(backend)


def _wav_ms(path):
    import wave
    with wave.open(path) as w:
        check(w.getnchannels() == 1, f"must be mono, got {w.getnchannels()}")
        check(w.getsampwidth() == 2, f"must be S16, got {w.getsampwidth()}")
        check(w.getframerate() == 16000, f"must be 16 kHz, got {w.getframerate()}")
        return round(1000 * w.getnframes() / w.getframerate())


def test_seam_writes_a_valid_bounded_wav_at_the_given_path():
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        out = str(Path(d) / "turn.wav")
        rc, err = _record(_BACKEND_SPEECH.format(speech_frames=10), out,
                          KIRRA_VAD_SILENCE_MS=300, KIRRA_VAD_MIN_SPEECH_MS=250,
                          KIRRA_VAD_MAX_MS=6000)
        check(rc == 0, f"a normal turn must succeed, got {rc}: {err}")
        check(Path(out).exists(), "the WAV must be written to the path we passed")
        ms = _wav_ms(out)
        # 300 ms speech + 300 ms trailing silence — well under the old fixed 4 s.
        check(500 <= ms <= 800, f"expected ~600 ms of audio, got {ms} ms")
        check("endpointed" in err, f"must stop by ENDPOINT, not a bound: {err}")


def test_seam_never_exceeds_the_hard_cap():
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        out = str(Path(d) / "turn.wav")
        rc, err = _record(_BACKEND_SPEECH.format(speech_frames=10000), out,
                          KIRRA_VAD_SILENCE_MS=300, KIRRA_VAD_MIN_SPEECH_MS=250,
                          KIRRA_VAD_MAX_MS=900, KIRRA_VAD_START_TIMEOUT_MS=800)
        check(rc == 0, f"hitting the cap is not an error, got {rc}: {err}")
        ms = _wav_ms(out)
        check(ms <= 960, f"the bounded-mic ceiling leaked: {ms} ms for a 900 ms cap")
        check("stopped: max" in err, f"must stop on the cap: {err}")


def test_seam_bounds_a_stalled_device_instead_of_hanging():
    """The no-open-mic guarantee under the failure that actually threatens it.

    The endpointer counts SAMPLES; a wedged capture device delivers none, so
    sample-time never advances and a plain blocking read would hold the mic open
    forever. The wall-clock deadline is what bounds this."""
    import tempfile
    import time as _time
    with tempfile.TemporaryDirectory() as d:
        out = str(Path(d) / "turn.wav")
        t0 = _time.monotonic()
        rc, err = _record(_BACKEND_STALL, out, KIRRA_VAD_MAX_MS=500,
                          KIRRA_VAD_START_TIMEOUT_MS=400)
        elapsed = _time.monotonic() - t0
        check(rc != 0, "a stalled capture must report failure, not a silent empty clip")
        check("stalled" in err, f"the reason must name the stall: {err}")
        # max_ms + the stall grace, with generous room for a slow CI box.
        check(elapsed < 20, f"a stalled device was not bounded: took {elapsed:.1f} s")


def test_seam_returns_nonzero_when_the_recorder_fails():
    """rabbit_voice.sh keys on the exit status: nonzero -> "recorder failed --
    nothing captured" -> no transcript, no intent, no motion. A dead device must
    NOT look like a successful silent turn."""
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        out = str(Path(d) / "turn.wav")
        rc, err = _record(_BACKEND_FAIL, out)
        check(rc != 0, f"a capture command that exits nonzero must fail the run (got {rc})")
        check("exited 3" in err, f"the reason must carry the child status: {err}")


def test_seam_requires_an_output_path():
    import os
    import subprocess
    env = dict(os.environ)
    env["KIRRA_VAD_DEVICE"] = "plughw:CARD=Device,DEV=0"
    done = subprocess.run([sys.executable, _vad_path()],
                          env=env, capture_output=True, text=True, timeout=30)
    check(done.returncode != 0, "no output path must be refused")
    check("output WAV path" in done.stderr, f"reason: {done.stderr}")


def test_seam_refuses_to_open_a_mic_without_an_explicit_device():
    import os
    import subprocess
    import tempfile
    env = {k: v for k, v in os.environ.items() if not k.startswith("KIRRA_VAD_")}
    with tempfile.TemporaryDirectory() as d:
        out = str(Path(d) / "turn.wav")
        done = subprocess.run([sys.executable, _vad_path(), out],
                              env=env, capture_output=True, text=True, timeout=30)
        check(done.returncode != 0, "an unconfigured device must fail closed")
        check("KIRRA_VAD_DEVICE" in done.stderr, f"reason must name the fix: {done.stderr}")
        check(not Path(out).exists(),
              "nothing may be written when the configuration is refused")


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
    if _FAILURES:
        print(f"\n{len(_FAILURES)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"vad_record_test: all {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
