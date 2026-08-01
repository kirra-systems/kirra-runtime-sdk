#!/usr/bin/env python3
"""Host tests for voice_route.classify_transcript — the pure deterministic
transcript router. No I/O, no network, no LLM: every case is a fixed string
in, a (RouteKind, reason-token) out.

The safety cases pinned here:
  * every REQUIRED motion phrase routes to MOTION (wake prefixes, courtesy,
    punctuation and case must not change that);
  * every REQUIRED conversation phrase — including the substring traps
    ("motor drive", "stop talking", "turn the explanation", "go over that
    again") — routes to CONVERSATION;
  * every REQUIRED ambiguous phrase fails CLOSED (never motion);
  * reasons are stable tokens that never contain the transcript.

Runs standalone (`python3 robot/voice_route_test.py`, exit 1 on failure);
importable under pytest.
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from voice_route import (  # noqa: E402
    RouteKind, classify_transcript, normalize, strip_wake_prefixes,
)

_F: list[str] = []


def check(cond, msg):
    if not cond:
        _F.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


def kind_of(text: str) -> RouteKind:
    return classify_transcript(text).kind


MOTION_REQUIRED = [
    "Drive forward one meter.",
    "Hey Parker, turn left.",
    "Rabbit, stop.",
    "Please go to the loading dock.",
    "Back up slowly.",
    "Cruise at two meters per second.",
    "Pull over.",
    # extra coverage from the design examples
    "move forward",
    "go to the loading dock",
    "turn right",
    "hold position",
    "change lanes",
    "reverse",
    "go straight",
    "take me to the dock",
    "hey parker drive forward one meter",
    "hello rabbit turn left",
    "parker stop",
    "HALT",
]

CONVERSATION_REQUIRED = [
    "How are you?",
    "Tell me a joke.",
    "Why did we stop talking?",
    "Explain how a motor drive works.",
    "Turn the explanation into a summary.",
    "Go over that again.",
    "What do you see?",
    # read-only robot questions stay conversation
    "what is your status",
    "why did you stop",
    "are your systems okay",
    "what can you do",
    "thank you",
    "good morning",
    "explain water treatment",
    "what is your name",
    # more substring traps
    "how does a motor drive work",
    "turn the summary into bullet points",
    "tell me about the loading dock",
]

AMBIGUOUS_REQUIRED = [
    "Go ahead.",
    "Do it.",
    "Proceed.",
    "Take me there.",
    "Keep going.",
    "okay move",
    "continue",
    "let's go",
    "go",
    "move",
]


def test_required_motion_phrases():
    for t in MOTION_REQUIRED:
        d = classify_transcript(t)
        check(d.kind is RouteKind.MOTION,
              f"must be MOTION, got {d.kind.value} ({d.reason}): {t!r}")


def test_required_conversation_phrases():
    for t in CONVERSATION_REQUIRED:
        d = classify_transcript(t)
        check(d.kind is RouteKind.CONVERSATION,
              f"must be CONVERSATION, got {d.kind.value} ({d.reason}): {t!r}")


def test_required_ambiguous_phrases():
    for t in AMBIGUOUS_REQUIRED:
        d = classify_transcript(t)
        check(d.kind is RouteKind.AMBIGUOUS,
              f"must be AMBIGUOUS, got {d.kind.value} ({d.reason}): {t!r}")
        check(d.kind is not RouteKind.MOTION,
              f"ambiguous must NEVER become motion: {t!r}")


def test_wake_prefixes_do_not_change_classification():
    pairs = [
        ("drive forward one meter", "hey parker drive forward one meter"),
        ("turn left", "hello rabbit, turn left"),
        ("stop", "Parker, stop!"),
        ("how are you", "hey rabbit how are you"),
        ("go ahead", "okay parker go ahead"),
    ]
    for bare, wrapped in pairs:
        check(kind_of(bare) is kind_of(wrapped),
              f"wake prefix changed the route: {bare!r} vs {wrapped!r}")


def test_punctuation_case_and_honorifics():
    variants = ["STOP", "Stop.", "stop!", "  stop  ", "Rabbit... stop?!"]
    for v in variants:
        check(kind_of(v) is RouteKind.MOTION, f"hold form missed: {v!r}")
    check(kind_of("PLEASE PULL OVER!") is RouteKind.MOTION,
          "courtesy + case must not hide 'pull over'")


def test_pronoun_destinations_stay_ambiguous():
    for t in ("go to there", "take me to it", "drive to that"):
        d = classify_transcript(t)
        check(d.kind is RouteKind.AMBIGUOUS,
              f"pronoun destination must be AMBIGUOUS: {t!r} → {d.kind.value}")


def test_hold_forms_are_exact_not_prefix():
    # "stop talking" is not a robot-motion order; only enumerated hold
    # forms qualify.
    check(kind_of("stop talking") is RouteKind.CONVERSATION,
          "'stop talking' must not create a hold intent")
    check(kind_of("stop moving") is RouteKind.MOTION,
          "'stop moving' is the hold order")


def test_reasons_are_stable_tokens_without_transcript():
    for t in MOTION_REQUIRED + CONVERSATION_REQUIRED + AMBIGUOUS_REQUIRED:
        d = classify_transcript(t)
        check(" " not in d.reason and d.reason.replace("_", "").isalnum(),
              f"reason must be a bare token: {d.reason!r}")
        # No transcript word longer than 3 chars may leak into the reason —
        # except the fixed CATEGORY vocabulary the reason tokens are built
        # from ("hold"/"motion"/"destination" name the rule, not the words).
        for w in normalize(t).split():
            if len(w) > 3 and w not in ("motion", "hold", "destination"):
                check(w not in d.reason,
                      f"transcript word {w!r} leaked into reason {d.reason!r}")


def test_normalize_and_strip_helpers():
    check(normalize("Hey, Parker!!") == "hey parker", "normalize basic")
    check(strip_wake_prefixes("hey parker turn left") == "turn left",
          "single prefix strip")
    check(strip_wake_prefixes("okay parker please go ahead") == "go ahead",
          "stacked prefix strip")
    check(strip_wake_prefixes("hey parker") == "", "pure wake → empty")
    d = classify_transcript("hey parker")
    check(d.kind is RouteKind.AMBIGUOUS and d.reason == "empty_after_normalization",
          "bare wake phrase fails closed with no endpoint")


def test_every_kind_has_disjoint_membership():
    # One transcript, one classification — sanity that the three required
    # sets do not overlap under classification.
    seen = {}
    for t in MOTION_REQUIRED + CONVERSATION_REQUIRED + AMBIGUOUS_REQUIRED:
        k = kind_of(t)
        n = normalize(t)
        check(seen.setdefault(n, k) is k, f"unstable classification: {t!r}")


# --- standalone runner (house pattern) ----------------------------------------

def main() -> int:
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"  ok  {name}")
    if _F:
        print(f"voice_route_test: {len(_F)} FAILURE(S)")
        return 1
    print("voice_route_test: ALL OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
