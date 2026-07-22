#!/usr/bin/env python3
"""Host tests for the PURE wake-word logic (`wake_word.py` matcher/gates and
`rabbit_wake.py` controls). No audio, no GPIO, no subprocesses — the impure
listener loop is deliberately not imported here (only the pure functions are).

Runs standalone (`python3 robot/wake_word_test.py`, exit 1 on any failure);
also importable under pytest. Covers: phrase parsing, transcript
normalization (whisper's bracketed annotations), the ordered-adjacent matcher
with the long-token-only edit tolerance (hallo/rabbits wake; a stray "yo" or
"yo… to/go/no" lookalikes never do), hostile near-misses, the RMS gate math,
the nap/mute fail-OPEN state gate, the rabbit_wake classifier (including
non-overlap with the OTA / diagnostics / drive matchers), and its state/reply
builders.
"""
from __future__ import annotations

import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rabbit_diag  # noqa: E402
import rabbit_wake  # noqa: E402
from rabbit_ota import match_command  # noqa: E402
from wake_word import (  # noqa: E402
    DEFAULT_PHRASES, parse_phrases, rms, transcript_tokens, wake_allowed,
    wake_hit,
)

PHRASES = parse_phrases(DEFAULT_PHRASES)


def _hit(text):
    return wake_hit(transcript_tokens(text), PHRASES)


# --- phrase parsing ----------------------------------------------------------

def test_default_phrases_parse() -> None:
    assert PHRASES == [["hello", "rabbit"], ["hey", "rabbit"], ["yo", "rabbit"]]


def test_phrase_parsing_drops_empty_entries() -> None:
    assert parse_phrases(" hello rabbit ,, ,hey rabbit") == [
        ["hello", "rabbit"], ["hey", "rabbit"]]
    assert parse_phrases("") == []
    assert parse_phrases(None) == []


# --- transcript normalization ------------------------------------------------

def test_tokens_strip_whisper_annotations_and_punctuation() -> None:
    assert transcript_tokens(" Hello, Rabbit!  ") == ["hello", "rabbit"]
    assert transcript_tokens("[BLANK_AUDIO]") == []
    assert transcript_tokens("(wind blowing) hey rabbit.") == ["hey", "rabbit"]
    assert transcript_tokens(None) == []


# --- the matcher -------------------------------------------------------------

def test_all_three_phrases_wake() -> None:
    assert _hit("hello rabbit") == "hello rabbit"
    assert _hit("Hey Rabbit") == "hey rabbit"
    assert _hit("yo rabbit") == "yo rabbit"


def test_wake_phrase_embedded_mid_sentence_wakes() -> None:
    assert _hit("well hello rabbit how are you") == "hello rabbit"


def test_long_token_tolerance_absorbs_whisper_mishears() -> None:
    # One edit on tokens >= 5 chars: hello->hallo, rabbit->rabbits/rabbi.
    assert _hit("hallo rabbit") == "hello rabbit"
    assert _hit("hello rabbits") == "hello rabbit"
    assert _hit("hey rabbi") == "hey rabbit"


def test_short_greetings_match_exactly() -> None:
    # "yo"/"hey" get NO tolerance: their edit-1 neighborhoods are noise words.
    for near in ("to rabbit", "go rabbit", "no rabbit", "so rabbit",
                 "hay rabbit", "they rabbit"):
        assert _hit(near) is None, f"{near!r} must not wake"


def test_adjacency_is_required() -> None:
    # An intervening token breaks the phrase — conversational mentions of the
    # robot must not wake it.
    assert _hit("hey the rabbit robot is over there") is None
    assert _hit("hello there rabbit") is None


def test_order_is_required_and_anchor_alone_is_insufficient() -> None:
    assert _hit("rabbit hello") is None
    assert _hit("rabbit") is None
    assert _hit("yo") is None
    assert _hit("") is None


def test_two_edits_on_the_anchor_do_not_wake() -> None:
    assert _hit("hello rabbitts") is None  # distance 2 from "rabbit"


# --- RMS gate ----------------------------------------------------------------

def test_rms_silence_is_zero_and_signal_scales() -> None:
    silence = struct.pack("<8h", *([0] * 8))
    loud = struct.pack("<8h", *([10_000, -10_000] * 4))
    assert rms(silence) == 0.0
    assert abs(rms(loud) - 10_000.0) < 1e-6
    assert rms(b"") == 0.0
    assert rms(b"\x01") == 0.0  # odd fragment: dropped, not a crash


# --- nap/mute gate (fail-OPEN by design: no actuation authority) --------------

def test_wake_allowed_states() -> None:
    now = 1_000_000
    assert wake_allowed(None, now), "absent file -> listening"
    assert wake_allowed("not json {", now), "corrupt file -> listening"
    assert wake_allowed('{"mode": "awake"}', now)
    assert not wake_allowed('{"mode": "mute"}', now)
    nap = '{"mode": "nap", "until_ms": 1000500}'
    assert not wake_allowed(nap, now), "napping until the deadline"
    assert wake_allowed(nap, 1_000_500), "nap expires exactly at until_ms"


# --- rabbit_wake controls ------------------------------------------------------

def test_wake_control_classifier() -> None:
    assert rabbit_wake.classify("rabbit, go to sleep") == "nap"
    assert rabbit_wake.classify("take a nap") == "nap"
    assert rabbit_wake.classify("stop listening") == "mute"
    assert rabbit_wake.classify("mute your ears") == "mute"
    assert rabbit_wake.classify("turn off the wake word") == "mute"
    assert rabbit_wake.classify("start listening") == "resume"
    assert rabbit_wake.classify("unmute your microphone") == "resume"
    assert rabbit_wake.classify(None) is None


def test_wake_controls_do_not_swallow_drive_or_status_turns() -> None:
    # "stop" alone is a DRIVE word — it must fall through to the router.
    for utter in ("stop", "stop right there", "creep forward one meter",
                  "what do you see", "how healthy are you",
                  "check for updates", "check yourself"):
        assert rabbit_wake.classify(utter) is None, f"{utter!r} must fall through"


def test_matcher_precedence_no_cross_matches() -> None:
    # The three deterministic matchers stay disjoint on each other's canon.
    wake_canon = ["go to sleep", "stop listening", "start listening"]
    for utter in wake_canon:
        assert match_command(utter) is None, f"OTA must not match {utter!r}"
        assert not rabbit_diag.matches(utter), f"diag must not match {utter!r}"
    assert rabbit_wake.classify("check for updates") is None
    assert rabbit_wake.classify("run diagnostics") is None


def test_mute_wins_when_both_appear() -> None:
    assert rabbit_wake.classify("go to sleep and stop listening") == "mute"


def test_state_builder_shapes() -> None:
    now = 42_000
    nap = rabbit_wake.state_for("nap", now, nap_min=30)
    assert nap == {"mode": "nap", "until_ms": now + 30 * 60_000, "set_at_ms": now}
    assert rabbit_wake.state_for("mute", now)["mode"] == "mute"
    assert rabbit_wake.state_for("resume", now)["mode"] == "awake"


def test_replies_state_the_resume_asymmetry() -> None:
    # A muted wake listener cannot hear "start listening" — the confirmations
    # must tell the operator the button is the way back (voice lines W2/W3).
    assert "button" in rabbit_wake.reply_for("nap")
    assert "button" in rabbit_wake.reply_for("mute")
    assert rabbit_wake.reply_for("resume")


# --- standalone runner (house pattern) ----------------------------------------

def _run_all() -> int:
    failures = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"  ok  {name}")
            except AssertionError as e:
                failures += 1
                print(f"FAIL  {name}: {e}")
    print("wake_word_test:", "ALL OK" if failures == 0 else f"{failures} FAILURE(S)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(_run_all())
