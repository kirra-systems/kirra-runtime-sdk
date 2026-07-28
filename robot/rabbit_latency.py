#!/usr/bin/env python3
"""rabbit_latency.py — opt-in per-turn latency staging for the Rabbit voice loop.

"Rabbit feels slow" is not actionable; "the LLM took 4.1 s of a 6.3 s turn" is.
This records the stage boundaries the loop ALREADY passes through and prints one
concise line per completed turn, so a tuning change (VAD endpointing, model
residency, streaming TTS) can be MEASURED instead of assumed.

    KIRRA_RABBIT_LATENCY_LOG=1      # off by default; unset → completely inert

🔴 It observes, it never decides. Nothing here gates a turn, changes routing,
touches the fenced /intent door, or affects actuation. A timing failure is
swallowed — measurement must never be able to break the thing it measures.

PRIVACY — the existing policy is unchanged and this does not widen it: no audio,
no transcript, no reply text. Only stage NAMES and durations. Rabbit's speech
already goes through `speak()`, which prints the line; nothing new is logged here.

CLOCK: `time.monotonic_ns` by default — never wall clock, which can step under
NTP mid-turn and produce a negative stage. Injectable, so tests are deterministic
and never sleep.

CROSS-PROCESS HONESTY. A voice turn spans three processes:

    wake_word.py  ──trigger──▶  rabbit_voice.sh  ──transcript──▶  rabbit_converse.py
    (wake→ack)                  (record, stt)                     (llm, door, tts)

The trigger carries no payload (one bare newline — that is load-bearing for the
wake fence), so there is no timestamp to thread through, and this module does NOT
invent an end-to-end total by guessing. Each process reports the span it actually
owns; the journal shows them adjacent, and the operator adds them. A fabricated
"total" would be the one number here worth distrusting.

Usage:

    t = TurnTimer("turn")
    t.mark("record")     # ends the 'record' stage
    t.mark("stt")
    log_summary(t)       # "rabbit_latency: turn 1520ms (record 1340, stt 180)"
"""
from __future__ import annotations

import os
import sys
import time

ENV_FLAG = "KIRRA_RABBIT_LATENCY_LOG"

# Stage names, so the three processes emit a consistent vocabulary.
WAKE_ACK = "wake_ack"     # wake detected → acknowledgement started
RECORD = "record"         # capture started → capture complete
STT = "stt"               # capture complete → transcript available
LLM = "llm"               # request sent → model response available
DOOR = "door"             # directive offered → mick answered
TTS = "tts"               # reply available → speech started


def enabled(env=None) -> bool:
    """Is latency logging on? Off unless explicitly enabled, so a normal
    deployment is byte-identical and the journal stays quiet."""
    env = os.environ if env is None else env
    return (env.get(ENV_FLAG) or "").strip().lower() in ("1", "true", "yes", "on")


def _default_clock() -> int:
    return time.monotonic_ns()


class TurnTimer:
    """Accumulates stage durations for ONE turn.

    `mark(stage)` closes the current span and attributes it to `stage`; the next
    span starts immediately. Marking the same stage twice ADDS to it (the door
    can be called once per mission step), so a summary never silently drops a
    repeat.

    Pure and clock-injected: `clock()` returns nanoseconds, monotonic.
    """

    def __init__(self, name: str = "turn", clock=None):
        self.name = name
        self._clock = clock or _default_clock
        # Defensive from the very first read: constructing a timer must never be
        # able to raise into the turn. A clock that misbehaves degrades to a
        # frozen one (all stages 0 ms) — useless numbers, but a live robot.
        self._start = self._now()
        self._last = self._start
        self.stages: list[tuple[str, int]] = []   # ordered (stage, ms)

    def _now(self) -> int:
        try:
            return int(self._clock())
        except Exception:  # noqa: BLE001 — measurement is never load-bearing
            return getattr(self, "_last", 0)

    def mark(self, stage: str) -> int:
        """Close the current span as `stage`; return its duration in ms."""
        now = self._now()
        ms = max(0, (now - self._last) // 1_000_000)   # never negative
        self._last = now
        for i, (name, prev) in enumerate(self.stages):
            if name == stage:
                self.stages[i] = (name, prev + ms)
                return ms
        self.stages.append((stage, ms))
        return ms

    def skip(self) -> None:
        """Discard the current span without attributing it — for idle time
        between stages that belongs to nobody (e.g. waiting on the next
        trigger)."""
        self._last = self._now()

    def total_ms(self) -> int:
        return max(0, (self._now() - self._start) // 1_000_000)

    def summary(self) -> str:
        """One line. Total first (the number an operator actually feels), then
        the stages in the order they happened."""
        parts = ", ".join(f"{name} {ms}" for name, ms in self.stages)
        body = f"{self.name} {self.total_ms()}ms"
        return f"{body} ({parts})" if parts else body

    def slowest(self) -> tuple[str, int] | None:
        """The stage that dominated the turn — the one worth tuning."""
        return max(self.stages, key=lambda s: s[1]) if self.stages else None


def log_summary(timer: TurnTimer, out=None, env=None) -> str | None:
    """Emit ONE summary line to stderr if enabled; return what was written (or
    None). Never raises: observability must not be able to break a turn."""
    try:
        if not enabled(env):
            return None
        line = f"rabbit_latency: {timer.summary()}"
        print(line, file=out or sys.stderr, flush=True)
        return line
    except Exception:  # noqa: BLE001 — measurement is never load-bearing
        return None
