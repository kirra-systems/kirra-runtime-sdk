#!/usr/bin/env python3
"""Host tests for the opt-in Rabbit latency staging.

Every timing assertion uses an INJECTED clock — no sleeps, no wall clock, so
nothing here can flake on a loaded CI box. The one real-clock test only checks
that the default clock is monotonic in TYPE, not in value.

What matters and is pinned:
  * off by default, and inert when off (no output, no cost);
  * one concise line per turn, stage names + durations only;
  * NO transcript, reply text, or audio ever reaches the log;
  * a timing failure can never break the turn it is measuring.
"""
from __future__ import annotations

import io
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rabbit_latency as rl  # noqa: E402

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


class FakeClock:
    """A clock the test drives. `advance(ms)` moves it; `__call__` returns ns."""

    def __init__(self):
        self.ns = 1_000_000_000        # arbitrary non-zero start

    def __call__(self):
        return self.ns

    def advance(self, ms):
        self.ns += ms * 1_000_000
        return self


ON = {"KIRRA_RABBIT_LATENCY_LOG": "1"}
OFF: dict[str, str] = {}


# ── the gate ─────────────────────────────────────────────────────────────────

def test_disabled_by_default():
    check(rl.enabled({}) is False, "must be OFF with the var unset")
    for falsy in ("", "0", "no", "off", "false", "  "):
        check(rl.enabled({rl.ENV_FLAG: falsy}) is False, f"{falsy!r} must not enable")


def test_truthy_spellings_enable():
    for truthy in ("1", "true", "TRUE", "yes", "on", " On "):
        check(rl.enabled({rl.ENV_FLAG: truthy}) is True, f"{truthy!r} must enable")


def test_nothing_is_written_when_disabled():
    clock = FakeClock()
    t = rl.TurnTimer("reply", clock=clock)
    clock.advance(1234)
    t.mark(rl.LLM)
    out = io.StringIO()
    check(rl.log_summary(t, out=out, env=OFF) is None, "must return None when off")
    check(out.getvalue() == "", f"must write nothing when off, got {out.getvalue()!r}")


# ── staging arithmetic ───────────────────────────────────────────────────────

def test_stages_record_their_own_spans_in_order():
    clock = FakeClock()
    t = rl.TurnTimer("reply", clock=clock)
    check(t.mark(rl.LLM) == 0, "a zero-length span is 0 ms, not negative")
    clock.advance(4100)
    t.mark(rl.LLM)
    clock.advance(120)
    t.mark(rl.DOOR)
    clock.advance(300)
    t.mark(rl.TTS)
    check(t.stages == [(rl.LLM, 4100), (rl.DOOR, 120), (rl.TTS, 300)],
          f"stages wrong: {t.stages}")
    check(t.total_ms() == 4520, f"total wrong: {t.total_ms()}")


def test_a_repeated_stage_accumulates_rather_than_being_dropped():
    # A mission calls the door once per motion step. Overwriting would under-
    # report; appending twice would read as two different stages.
    clock = FakeClock()
    t = rl.TurnTimer("reply", clock=clock)
    clock.advance(100)
    t.mark(rl.DOOR)
    clock.advance(250)
    t.mark(rl.DOOR)
    check(t.stages == [(rl.DOOR, 350)], f"repeat must accumulate: {t.stages}")


def test_skip_discards_a_span_without_attributing_it():
    clock = FakeClock()
    t = rl.TurnTimer("reply", clock=clock)
    clock.advance(9000)      # idle, belongs to nobody
    t.skip()
    clock.advance(200)
    t.mark(rl.TTS)
    check(t.stages == [(rl.TTS, 200)], f"skip must not attribute: {t.stages}")


def test_the_slowest_stage_is_identified():
    clock = FakeClock()
    t = rl.TurnTimer("reply", clock=clock)
    clock.advance(1340)
    t.mark(rl.RECORD)
    clock.advance(4100)
    t.mark(rl.LLM)
    clock.advance(300)
    t.mark(rl.TTS)
    check(t.slowest() == (rl.LLM, 4100), f"slowest wrong: {t.slowest()}")
    check(rl.TurnTimer("x", clock=FakeClock()).slowest() is None,
          "no stages → no slowest, not a crash")


def test_a_backwards_clock_never_produces_a_negative_stage():
    """Defensive: monotonic_ns should never go backwards, but a stage rendered
    as '-3ms' would make the whole summary untrustworthy."""
    class Backwards:
        def __init__(self):
            self.ns = 5_000_000_000

        def __call__(self):
            self.ns -= 1_000_000
            return self.ns

    t = rl.TurnTimer("reply", clock=Backwards())
    check(t.mark(rl.LLM) >= 0, "a stage duration must never be negative")
    check(t.total_ms() >= 0, "a total must never be negative")


# ── the emitted line ─────────────────────────────────────────────────────────

def test_the_summary_is_one_line_total_first_then_stages():
    clock = FakeClock()
    t = rl.TurnTimer("reply", clock=clock)
    clock.advance(1340)
    t.mark(rl.RECORD)
    clock.advance(4100)
    t.mark(rl.LLM)
    out = io.StringIO()
    line = rl.log_summary(t, out=out, env=ON)
    check(line == "rabbit_latency: reply 5440ms (record 1340, llm 4100)", f"got {line!r}")
    check(out.getvalue().count("\n") == 1, "exactly one line per turn")
    check(out.getvalue().rstrip("\n") == line, "the returned line is what was written")


def test_a_turn_with_no_stages_still_reports_a_total():
    clock = FakeClock()
    t = rl.TurnTimer("reply", clock=clock)
    clock.advance(80)
    out = io.StringIO()
    line = rl.log_summary(t, out=out, env=ON)
    check(line == "rabbit_latency: reply 80ms", f"got {line!r}")


def test_the_summary_never_carries_speech():
    """The privacy rule, as an assertion. Only stage names and integers may
    appear — a stage name is a fixed constant, so nothing operator-spoken can
    reach the journal through this path."""
    clock = FakeClock()
    t = rl.TurnTimer("reply", clock=clock)
    for stage in (rl.WAKE_ACK, rl.RECORD, rl.STT, rl.LLM, rl.DOOR, rl.TTS):
        clock.advance(10)
        t.mark(stage)
    line = rl.log_summary(t, out=io.StringIO(), env=ON)
    allowed = set("abcdefghijklmnopqrstuvwxyz_0123456789 :(),m")
    check(set(line) <= allowed, f"unexpected characters in the summary: {line!r}")
    for stage, _ in t.stages:
        check(stage in (rl.WAKE_ACK, rl.RECORD, rl.STT, rl.LLM, rl.DOOR, rl.TTS),
              f"unknown stage name reached the log: {stage!r}")


def test_a_timing_failure_never_breaks_the_turn():
    """Measurement must not be able to break the thing it measures."""
    class Exploding:
        def summary(self):
            raise RuntimeError("boom")

    check(rl.log_summary(Exploding(), out=io.StringIO(), env=ON) is None,
          "a broken timer must be swallowed, not raised")

    class ExplodingClock:
        def __call__(self):
            raise RuntimeError("boom")

    try:
        rl.log_summary(rl.TurnTimer("x", clock=ExplodingClock()),
                       out=io.StringIO(), env=ON)
    except Exception as e:  # noqa: BLE001
        check(False, f"a broken clock escaped log_summary: {e!r}")


def test_the_default_clock_is_monotonic_not_wall_clock():
    """Wall clock can STEP under NTP mid-turn and yield a negative stage."""
    src = (Path(__file__).resolve().parent / "rabbit_latency.py").read_text()
    check("monotonic_ns" in src, "the default clock must be time.monotonic_ns")
    check("time.time(" not in src, "wall clock must not be used for staging")
    a = rl._default_clock()
    b = rl._default_clock()
    check(isinstance(a, int) and b >= a, "the default clock must not go backwards")


# ── wiring: opt-in, observability-only, no new authority ─────────────────────

def test_the_wiring_is_opt_in_everywhere():
    here = Path(__file__).resolve().parent
    for name in ("rabbit_voice.sh", "rabbit_converse.py", "wake_word.py"):
        text = (here / name).read_text()
        check(rl.ENV_FLAG in text or "rabbit_latency" in text,
              f"{name} should participate in staging")
    # The shell gate defaults to 0 (off) rather than on.
    voice = (here / "rabbit_voice.sh").read_text()
    check("LATENCY=0" in voice, "the shell timing gate must default OFF")


def test_timing_calls_cannot_affect_routing_or_actuation():
    """Every latency reference is a bare mark/log statement or an assignment —
    NEVER a condition, an argument, or a return value. If a timing number could
    gate a turn, observability would have quietly become control.

    Parsed with `ast`, not grepped: prose in a docstring must not read as code
    (and did, on the first attempt — "…on a blind timer. mark_active spans…").
    """
    import ast
    NAMES = ("timer", "_TURN_TIMER", "rabbit_latency")
    # Statement kinds in which a latency reference cannot influence anything:
    # a bare call, an assignment of a fresh timer, an import, a global decl.
    INERT = (ast.Expr, ast.Assign, ast.Import, ast.ImportFrom, ast.Global)

    def own_refs(stmt):
        """Names referenced by THIS statement, not by statements nested in it —
        otherwise every enclosing FunctionDef would match its own body."""
        found, stack = [], list(ast.iter_child_nodes(stmt))
        while stack:
            node = stack.pop()
            if isinstance(node, ast.stmt):
                continue                      # a nested statement is its own case
            if isinstance(node, ast.Name) and node.id in NAMES:
                found.append(node)
            stack.extend(ast.iter_child_nodes(node))
        return found

    here = Path(__file__).resolve().parent
    for name in ("rabbit_converse.py", "wake_word.py"):
        tree = ast.parse((here / name).read_text())
        seen = 0
        for node in ast.walk(tree):
            if not isinstance(node, ast.stmt) or not own_refs(node):
                continue
            seen += 1
            check(isinstance(node, INERT),
                  f"{name}:{node.lineno} uses timing data in a "
                  f"{type(node).__name__} — it must never gate a turn")
        check(seen > 0, f"{name} has no latency wiring at all (did it get dropped?)")


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items())
             if k.startswith("test_") and callable(v)]
    print("rabbit_latency_test:")
    for t in tests:
        before = len(_FAILURES)
        t()
        print(f"  {'ok  ' if len(_FAILURES) == before else 'FAIL'} {t.__name__}")
    if _FAILURES:
        print(f"\n{len(_FAILURES)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"\nall {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(_run_all())
