#!/usr/bin/env python3
"""Host tests for the barge-in core in `barge_in.py`.

The decision + signal logic (`should_interrupt`, `enabled`, `read_epoch`,
`raise_barge_in`, `make_file_cancel_check`) is pure/filesystem-only and runs on a
plain host. `speak_interruptible` is exercised with standard unix commands
(`sleep`) instead of a real TTS engine — a killable subprocess is the point.

Runs standalone (`python3 robot/barge_in_test.py`, exit 1 on failure); also
importable under pytest.

Covers: the priority arbiter truth table; the fail-closed enable gate;
epoch round-trip (absent/corrupt → 0, monotonic advance, atomic); the
baseline cancel-check (a pre-existing signal does NOT false-cut, a later one
does); and speak_interruptible completing normally vs being cut mid-playback.
"""
from __future__ import annotations

import os
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import barge_in  # noqa: E402
from barge_in import (  # noqa: E402
    P0_ESTOP, P1_INTERRUPT, P2_MISSION, P3_INFO, make_file_cancel_check,
    raise_barge_in, read_epoch, should_interrupt, speak_interruptible,
)

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


# ── priority arbiter ─────────────────────────────────────────────────────────

def test_priority_truth_table():
    check(should_interrupt(P3_INFO, P1_INTERRUPT), "P1 must cut P3 info-speech")
    check(should_interrupt(P3_INFO, P0_ESTOP), "P0 e-stop must cut P3")
    check(should_interrupt(P2_MISSION, P1_INTERRUPT), "P1 must cut P2 mission")
    check(not should_interrupt(P1_INTERRUPT, P3_INFO), "P3 must NOT cut P1")
    check(not should_interrupt(P3_INFO, P3_INFO), "equal priority must NOT interrupt")
    check(not should_interrupt(P1_INTERRUPT, P2_MISSION), "P2 must NOT cut P1")


# ── enable gate (fail-closed off) ────────────────────────────────────────────

def test_enable_gate_fail_closed():
    prev = os.environ.get("KIRRA_BARGE_IN_ENABLED")
    try:
        for off in (None, "", "0", "false", "no", "off", "enabled", "2"):
            if off is None:
                os.environ.pop("KIRRA_BARGE_IN_ENABLED", None)
            else:
                os.environ["KIRRA_BARGE_IN_ENABLED"] = off
            check(not barge_in.enabled(), f"{off!r} must NOT arm (fail-closed)")
        for on in ("1", "true", "TRUE", "yes", "on"):
            os.environ["KIRRA_BARGE_IN_ENABLED"] = on
            check(barge_in.enabled(), f"{on!r} should arm")
    finally:
        if prev is None:
            os.environ.pop("KIRRA_BARGE_IN_ENABLED", None)
        else:
            os.environ["KIRRA_BARGE_IN_ENABLED"] = prev


# ── epoch signal ─────────────────────────────────────────────────────────────

def test_epoch_absent_and_corrupt_read_zero():
    with tempfile.TemporaryDirectory() as d:
        p = os.path.join(d, "sig.epoch")
        check(read_epoch(p) == 0, "absent file → epoch 0")
        with open(p, "w") as f:
            f.write("garbage")
        check(read_epoch(p) == 0, "corrupt file → epoch 0 (fail-safe)")


def test_epoch_monotonic_advance():
    with tempfile.TemporaryDirectory() as d:
        p = os.path.join(d, "sig.epoch")
        check(raise_barge_in(p) == 1, "first raise → 1")
        check(raise_barge_in(p) == 2, "second raise → 2")
        check(read_epoch(p) == 2, "reads back the latest epoch")


def test_baseline_cancel_check():
    with tempfile.TemporaryDirectory() as d:
        p = os.path.join(d, "sig.epoch")
        raise_barge_in(p)                       # a signal from BEFORE we start
        baseline = read_epoch(p)                # baseline captures it
        cancel = make_file_cancel_check(p, baseline)
        check(not cancel(), "a pre-existing signal must NOT false-cut")
        raise_barge_in(p)                       # a NEW signal during speech
        check(cancel(), "a signal raised after baseline must cut")


# ── interruptible speak ──────────────────────────────────────────────────────

def test_speak_completes_when_not_cancelled():
    t0 = time.monotonic()
    interrupted = speak_interruptible("hi", ["sleep", "0.2"],
                                      cancel_check=lambda: False, poll_ms=20)
    check(not interrupted, "uncancelled speech should complete, not interrupt")
    check(time.monotonic() - t0 >= 0.15, "should have waited for playback to finish")


def test_speak_cut_mid_playback():
    t0 = time.monotonic()
    interrupted = speak_interruptible("a long droning reply", ["sleep", "5"],
                                      cancel_check=lambda: True, poll_ms=20)
    elapsed = time.monotonic() - t0
    check(interrupted, "a raised cancel must cut playback")
    check(elapsed < 1.0, f"cut should be prompt, took {elapsed:.2f}s")


def test_speak_no_tts_prints_only():
    check(speak_interruptible("printed", [], cancel_check=lambda: True) is False,
          "no TTS command → print only, never 'interrupted'")


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
    if _FAILURES:
        print(f"\n{len(_FAILURES)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"barge_in_test: all {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
