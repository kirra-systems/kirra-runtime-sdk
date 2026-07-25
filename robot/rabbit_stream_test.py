#!/usr/bin/env python3
"""rabbit_stream_test — host tests for the streaming say-parser. CI-safe: pure,
no hardware/HTTP/LLM. Feeds synthetic router-JSON streams at various chunk
boundaries and asserts: chat turns voice their say as clean unescaped clauses,
drive turns voice NOTHING before routing, and any non-directive-first / malformed
stream gives up so the caller falls back to the current behaviour.
Runner style matches rabbit_voice_test.py (assert-based, stdlib only).
"""
from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rabbit_stream as rs  # noqa: E402


def _drive(payload: str, chunk: int):
    """Feed `payload` through the parser in fixed-size chunks; return
    (spoken_clauses, finish_plan)."""
    p = rs.StreamingSayParser()
    spoken = []
    for i in range(0, len(payload), chunk):
        spoken += p.feed(payload[i:i + chunk])
    plan = p.finish()
    spoken += plan["trailing"]
    return spoken, plan


# ---- split_ready_clauses -----------------------------------------------------

def test_split_ready_clauses() -> None:
    clauses, rem = rs.split_ready_clauses("Hello there. How are you? I am fine")
    assert clauses == ["Hello there.", "How are you?"], clauses
    assert rem == "I am fine", rem


def test_split_does_not_break_on_decimal() -> None:
    # No whitespace after the '.', so "2.5" is not a boundary.
    clauses, rem = rs.split_ready_clauses("The gap is 2.5 meters")
    assert clauses == []
    assert rem == "The gap is 2.5 meters"


def test_safe_unescape_handles_truncated_escape() -> None:
    assert rs._safe_unescape('hello') == "hello"
    assert rs._safe_unescape('a\\"b') == 'a"b'
    assert rs._safe_unescape('trailing\\') == "trailing"          # dangling backslash trimmed
    assert rs._safe_unescape('half\\u12') == "half"               # incomplete \u trimmed


# ---- chat turns stream their say --------------------------------------------

def test_chat_turn_streams_clauses_across_chunks() -> None:
    payload = '{"directive": null, "say": "We are nominal. Holding position for now."}'
    for chunk in (1, 3, 7, 200):
        spoken, plan = _drive(payload, chunk)
        assert plan["route"] == "chat", (chunk, plan)
        assert plan["streamed"] is True, (chunk, plan)
        assert plan["directive"] is None
        # Every clause is clean prose — no JSON punctuation ever leaks.
        joined = " ".join(spoken)
        assert "{" not in joined and '"' not in joined and "directive" not in joined, spoken
        assert spoken == ["We are nominal.", "Holding position for now."], (chunk, spoken)


def test_chat_first_clause_available_before_stream_ends() -> None:
    # The whole point: the first sentence is emitted mid-stream, not at finish.
    p = rs.StreamingSayParser()
    early = []
    early += p.feed('{"directive": null, "say": "On it. ')   # first sentence + a space
    assert early == ["On it."], early                          # spoken already, before the rest
    early += p.feed('Standing by."}')
    plan = p.finish()
    assert (early + plan["trailing"]) == ["On it.", "Standing by."]


def test_chat_unescapes_quotes_and_newlines() -> None:
    payload = '{"directive": null, "say": "I heard \\"go\\" clearly. All set.\\n"}'
    spoken, plan = _drive(payload, 5)
    assert plan["route"] == "chat"
    assert spoken[0] == 'I heard "go" clearly.'
    assert "All set." in spoken[1]


def test_short_chat_reply_with_no_terminator_flushes_at_finish() -> None:
    payload = '{"directive": null, "say": "Understood"}'
    spoken, plan = _drive(payload, 4)
    assert spoken == ["Understood"], spoken
    assert plan["streamed"] is True


# ---- drive turns voice NOTHING before routing -------------------------------

def test_drive_turn_emits_no_speech_and_reports_directive() -> None:
    payload = '{"directive": "take us to the door", "say": "Heading for the door."}'
    spoken, plan = _drive(payload, 6)
    assert spoken == [], spoken                     # nothing pre-spoken on a drive turn
    assert plan["route"] == "drive"
    assert plan["streamed"] is False
    assert plan["directive"] == "take us to the door"


def test_drive_directive_resolved_before_say_is_seen() -> None:
    # Directive is complete well before the say value starts → drive locked in early.
    p = rs.StreamingSayParser()
    out = p.feed('{"directive": "creep forward", ')
    assert out == []
    assert p.phase == "drive" and p.directive == "creep forward"


# ---- fail-safe: non-directive-first / malformed → give up (caller falls back)-

def test_say_first_stream_gives_up_and_speaks_nothing() -> None:
    # Legacy / non-compliant ordering: say before directive → cannot stream safely.
    payload = '{"say": "Heading out.", "directive": "go forward"}'
    spoken, plan = _drive(payload, 5)
    assert spoken == [], spoken
    assert plan["streamed"] is False
    # raw is preserved verbatim for the caller's parse_reply fallback.
    assert plan["raw"] == payload


def test_garbage_stream_gives_up() -> None:
    payload = "sorry I cannot comply as JSON"
    spoken, plan = _drive(payload, 4)
    assert spoken == []
    assert plan["streamed"] is False
    assert plan["route"] == "unknown"
    assert plan["raw"] == payload


def test_raw_is_always_the_full_payload() -> None:
    payload = '{"directive": null, "say": "Anything at all here."}'
    _, plan = _drive(payload, 9)
    assert plan["raw"] == payload


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    print("rabbit_stream host tests (no hardware):")
    failed = 0
    for t in tests:
        try:
            t()
            print(f"  ok   {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"  FAIL {t.__name__}: {e}")
    print(f"\n{len(tests) - failed}/{len(tests)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(_run_all())
