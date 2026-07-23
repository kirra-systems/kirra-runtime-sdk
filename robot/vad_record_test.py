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

import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vad_record import (  # noqa: E402
    CONTINUE, STOP_ENDPOINTED, STOP_MAX, STOP_NO_SPEECH, Endpointer, rms_energy,
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
