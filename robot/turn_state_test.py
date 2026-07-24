#!/usr/bin/env python3
"""Host tests for turn_state.py — the pure cross-process turn signal + re-arm
gate that fixes the wake listener's "only answered one question" bug (Slice R).

No audio, no subprocesses: the round-trip uses a tmp file, the re-arm decision is
pure. Runs standalone (`python3 robot/turn_state_test.py`, exit 1 on failure);
also importable under pytest.

Covers: fail-safe parse (absent/corrupt/partial/negative), the active/done
round-trip, seq monotonicity across turns, and rearm_decision across every
branch — culminating in a simulation proving FIVE consecutive wake→turn→re-arm
cycles all re-arm promptly (no dead window) AND that a fast turn's follow-up is
never dropped while a long turn is never cut short before its max ceiling.
"""
from __future__ import annotations

import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from turn_state import (  # noqa: E402
    mark_active, mark_done, parse_state, read_state, rearm_decision,
)


# --- fail-safe parse ---------------------------------------------------------

def test_parse_absent_or_corrupt_is_idle() -> None:
    assert parse_state(None) == (False, 0)
    assert parse_state("") == (False, 0)
    assert parse_state("not json {") == (False, 0)
    assert parse_state("[]") == (False, 0)            # wrong shape → idle
    assert parse_state('{"active": true}') == (True, 0)   # missing seq → 0
    assert parse_state('{"seq": 4}') == (False, 4)        # missing active → False


def test_parse_negative_seq_heals_to_zero() -> None:
    # A corrupt negative counter must never read as "advanced" — floor at 0.
    assert parse_state('{"active": false, "seq": -9}') == (False, 0)


def test_read_missing_file_is_idle() -> None:
    with tempfile.TemporaryDirectory() as d:
        assert read_state(os.path.join(d, "nope.state")) == (False, 0)


# --- active/done round-trip + seq monotonicity -------------------------------

def test_active_done_round_trip_and_seq_advances() -> None:
    with tempfile.TemporaryDirectory() as d:
        p = os.path.join(d, "turn.state")
        assert read_state(p) == (False, 0)             # fresh → idle
        mark_active(p)
        assert read_state(p) == (True, 0)              # in progress, none done yet
        seq1 = mark_done(p)
        assert seq1 == 1 and read_state(p) == (False, 1)
        # second turn: seq strictly advances, active toggles correctly
        mark_active(p)
        assert read_state(p) == (True, 1)
        seq2 = mark_done(p)
        assert seq2 == 2 and read_state(p) == (False, 2)


def test_mark_done_without_active_still_advances() -> None:
    # A turn that errored before mark_active (defensive) still advances the
    # counter on the finally's mark_done — the listener must not wedge.
    with tempfile.TemporaryDirectory() as d:
        p = os.path.join(d, "turn.state")
        assert mark_done(p) == 1
        assert read_state(p) == (False, 1)


# --- the pure re-arm gate ----------------------------------------------------

GRACE, MAX = 7.0, 45.0


def test_completed_turn_reopens_immediately() -> None:
    # seq advanced past baseline at t=2s (a very fast turn) → reopen NOW, do not
    # sit out the rest of a fixed hold-off. THIS is the "only one question" fix.
    assert rearm_decision(2.0, False, True, grace_s=GRACE, max_s=MAX) == "reopen"


def test_in_progress_turn_waits_until_done_not_cut_by_grace() -> None:
    # active turn, well past the grace window, not yet advanced → keep WAITING
    # (a long LLM+TTS reply must not be cut short and have the mic reopen on top
    # of Rabbit's own voice).
    assert rearm_decision(20.0, True, False, grace_s=GRACE, max_s=MAX) == "wait"


def test_in_progress_turn_reopens_at_max_ceiling() -> None:
    # active but never signalled done by the ceiling (writer crashed mid-turn) →
    # fail-safe reopen so the listener can't wedge shut forever.
    assert rearm_decision(MAX, True, False, grace_s=GRACE, max_s=MAX) == "reopen"


def test_garbage_clip_reopens_after_grace() -> None:
    # No turn ever started (empty/garbage clip → rabbit_converse got no line):
    # wait through the grace window, then reopen.
    assert rearm_decision(3.0, False, False, grace_s=GRACE, max_s=MAX) == "wait"
    assert rearm_decision(GRACE, False, False, grace_s=GRACE, max_s=MAX) == "reopen"


def test_advanced_beats_everything() -> None:
    # Completion is checked first: even at/over max, an advanced turn is a clean
    # reopen (not the fail-safe branch) — same outcome, correct reason.
    assert rearm_decision(100.0, True, True, grace_s=GRACE, max_s=MAX) == "reopen"


def _simulate_wait(samples, *, grace_s=GRACE, max_s=MAX):
    """Drive rearm_decision over an ordered list of (elapsed, active, advanced)
    poll samples; return the elapsed at which it FIRST says 'reopen' (or None)."""
    for elapsed, active, advanced in samples:
        if rearm_decision(elapsed, active, advanced,
                          grace_s=grace_s, max_s=max_s) == "reopen":
            return elapsed
    return None


def test_five_consecutive_cycles_all_rearm_promptly() -> None:
    """The Slice R acceptance proof: five back-to-back wake→turn→re-arm cycles,
    each a realistic fast turn (record+STT ~5s, then a short reply), re-arm the
    moment the turn completes — never sitting out a dead window. If ANY cycle
    failed to reopen, the operator's next 'hey rabbit' would be dropped."""
    reopen_times = []
    for _ in range(5):
        # poll timeline for one cycle: mic-closed grace, turn goes active at ~5s
        # (post record+STT), completes at ~8s (short reply spoken).
        samples = [
            (0.0, False, False),   # just fired; nothing started yet
            (3.0, False, False),   # still recording — inside grace, keep waiting
            (5.0, True, False),    # transcript arrived, turn active
            (7.0, True, False),    # LLM/TTS running
            (8.0, False, True),    # turn done, seq advanced → reopen
        ]
        t = _simulate_wait(samples)
        assert t == 8.0, f"cycle did not re-arm at turn completion (got {t})"
        reopen_times.append(t)
    assert reopen_times == [8.0] * 5


def test_fast_turn_followup_is_not_dropped() -> None:
    """Regression for the exact reported symptom: a turn that finishes FAST must
    reopen the mic well before the old fixed 10s+ hold-off would have, so an
    immediate follow-up wake is heard."""
    # turn active at 5s, done at 6s → reopen at 6s, not held to 12s.
    t = _simulate_wait([
        (0.0, False, False), (5.0, True, False), (6.0, False, True),
    ])
    assert t == 6.0


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items())
             if k.startswith("test_") and callable(v)]
    failures = 0
    for t in tests:
        try:
            t()
            print(f"  ok   {t.__name__}")
        except AssertionError as e:
            failures += 1
            print(f"  FAIL {t.__name__}: {e}")
    print(f"\n{len(tests) - failures}/{len(tests)} passed")
    return 1 if failures else 0


if __name__ == "__main__":
    print("turn_state host tests (pure, no audio):")
    sys.exit(_run_all())
