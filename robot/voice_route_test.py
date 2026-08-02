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
    "Back up slowly.",
    "Cruise at two meters per second.",
    "Pull over.",
    # extra coverage from the design examples
    "move forward",
    "turn right",
    "hold position",
    "change lanes",
    "reverse",
    "go straight",
    "hey parker drive forward one meter",
    "hello rabbit turn left",
    "parker stop",
    "HALT",
]

# SEMANTIC destinations. These name a PLACE rather than a maneuver, so they
# route to the typed destination door where a TRUSTED resolver supplies the
# coordinates — they were MOTION before the typed-destination integration,
# when the only door was /intent and an LLM had to invent the goal point.
DESTINATION_REQUIRED = [
    "Please go to the loading dock.",
    "go to the loading dock",
    "take me to the dock",
    "drive to the kitchen",
    "go to the charging dock",
    "take me to the workshop",
    "navigate to the front door",
    "head to the garage",
    "run route A",
    "follow route A",
    "take route A",
    "start patrol route",
    "follow the perimeter route",
    "drive to the red cup",
    "go to the blue chair",
    "move near the person",
    "navigate to the box",
    "drive to 125 Main Street",
    "take me to 10 Oak Avenue",
    "navigate to 42 West Elm Road",
    "hey parker, drive to the kitchen",
    "please go to the charging dock",
    "hello rabbit take me to the workshop",
    "please run route A",
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
    # Destination NOUNS in ordinary prose must never become navigation.
    "what is the kitchen used for",
    "tell me about Route 66",
    "where is the red cup",
    "what do you see near the chair",
    "explain how address routing works",
    "tell me about the kitchen",
    "what is route planning",
    "describe a red cup",
    "describe the blue chair",
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
    # A destination verb aimed at a PRONOUN names nothing resolvable.
    "go there",
    "drive to it",
    "run it",
    "follow that",
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


def test_required_destination_phrases():
    for t in DESTINATION_REQUIRED:
        d = classify_transcript(t)
        check(d.kind is RouteKind.DESTINATION,
              f"must be DESTINATION, got {d.kind.value} ({d.reason}): {t!r}")
        check(d.destination is not None,
              f"a DESTINATION decision must carry a typed request: {t!r}")


def test_typed_request_wire_shapes_are_exact():
    cases = [
        ("drive to the kitchen", {"kind": "named_place", "query": "kitchen"}),
        ("go to the charging dock",
         {"kind": "named_place", "query": "charging dock"}),
        ("take me to the workshop",
         {"kind": "named_place", "query": "workshop"}),
        ("run route A", {"kind": "saved_route", "query": "route a"}),
        ("follow the perimeter route",
         {"kind": "saved_route", "query": "perimeter route"}),
        ("drive to the red cup",
         {"kind": "tracked_object", "class": "cup", "color": "red"}),
        ("go to the blue chair",
         {"kind": "tracked_object", "class": "chair", "color": "blue"}),
        ("move near the person", {"kind": "tracked_object", "class": "person"}),
        ("navigate to the box", {"kind": "tracked_object", "class": "box"}),
        # An address keeps the operator's capitalization; routing itself is
        # always decided on the lowercase form.
        ("drive to 125 Main Street",
         {"kind": "address", "query": "125 Main Street"}),
        # Wake + courtesy prefixes must not reach the query.
        ("hey parker, please drive to the kitchen",
         {"kind": "named_place", "query": "kitchen"}),
    ]
    for text, wire in cases:
        d = classify_transcript(text)
        check(d.kind is RouteKind.DESTINATION, f"{text!r} must be DESTINATION")
        got = d.destination.to_wire() if d.destination else None
        check(got == wire, f"{text!r} → {got!r}, expected {wire!r}")


def test_the_python_parser_can_never_produce_a_coordinate():
    """The core rule, made structural on the language side: no code path in
    the router's grammar emits a numeric field. Every wire object carries
    only the kind and the operator's own words."""
    allowed = {"kind", "query", "class", "color"}
    for t in DESTINATION_REQUIRED:
        d = classify_transcript(t)
        wire = d.destination.to_wire()
        check(set(wire) <= allowed,
              f"{t!r} produced non-vocabulary keys: {sorted(set(wire) - allowed)}")
        for k, v in wire.items():
            check(isinstance(v, str),
                  f"{t!r}: field {k} is {type(v).__name__}, must be a string")
    # And the dataclass itself has no coordinate field to fill in.
    import dataclasses
    from voice_route import DestinationRequest
    fields = {f.name for f in dataclasses.fields(DestinationRequest)}
    for banned in ("x", "y", "x_m", "y_m", "lat", "lon", "pose", "heading_rad"):
        check(banned not in fields,
              f"DestinationRequest must have no {banned!r} field")


def test_destination_grammar_stays_closed():
    # An unrecognised "object" is NOT invented as a tracked object — it falls
    # through to a named place, which the registry then refuses if unknown.
    d = classify_transcript("drive to the flux capacitor")
    check(d.destination.to_wire()["kind"] == "named_place",
          "free-form scene description must not become a tracked object")
    # A colour with no class names no thing at all.
    d = classify_transcript("drive to the red")
    check(d.destination is None or d.destination.to_wire()["kind"] == "named_place",
          "a bare colour must not become a tracked object")
    # A number without a street suffix is not an address.
    d = classify_transcript("drive to bay 12")
    check(d.destination.to_wire()["kind"] == "named_place",
          "a bare number must not become an address")


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


def test_whisper_drive_distance_variants_route_to_motion():
    # Whisper renders "drive forward one meter" as "drive one meter" or
    # "drive for one meter" (the known "forward"→"for" substitution). Both
    # are explicit motion orders; the grammar admits drive + bounded
    # distance and drive + "for" + bounded distance, nothing wider.
    positives = [
        "drive one meter",
        "drive for one meter",
        "drive two meters",
        "drive for two meters",
        "drive 1 meter",
        "drive for 1 meter",
        "please drive one meter",
        "please drive for two meters",
        "hey parker drive one meter",
        "hello rabbit drive for one meter",
        # normalization: punctuation/case must not change the route
        "Drive one meter.",
        "DRIVE FOR ONE METER",
        "drive, one meter",
        "hey parker, drive for one meter",
    ]
    for t in positives:
        d = classify_transcript(t)
        check(d.kind is RouteKind.MOTION,
              f"drive-distance variant must be MOTION, got {d.kind.value} "
              f"({d.reason}): {t!r}")
        check(d.reason == "explicit_motion_verb",
              f"variant keeps the stable reason token: {t!r} → {d.reason}")


def test_broad_drive_phrases_stay_conversation():
    # The "for" substitution is admitted ONLY before a bounded distance —
    # durations, purposes, idioms and prose about drives never move a robot.
    negatives = [
        "how does a motor drive work",
        "explain a hard drive",
        "drive me crazy",
        "drive for one hour",
        "drive for better performance",
        "drive the discussion forward",
        "drive innovation",
        "the drive is one meter long",
        "what is a drive",
        "can you explain drive-by-wire",
        # numeric literals are bounded to the word list's 1..10 range: a
        # zero is a no-op and an out-of-range figure is more plausibly a
        # mis-transcription than a real order (review: Copilot on #1292)
        "drive 0 meters",
        "drive 11 meters",
        "drive 9999 meters",
        "drive for 0 meters",
        "drive for 9999 meters",
    ]
    for t in negatives:
        d = classify_transcript(t)
        check(d.kind is RouteKind.CONVERSATION,
              f"must stay CONVERSATION, got {d.kind.value} ({d.reason}): {t!r}")
    d = classify_transcript("drive 10 meters")
    check(d.kind is RouteKind.MOTION,
          "the numeric bound is inclusive: 'drive 10 meters' is motion")


def test_hold_forms_are_exact_not_prefix():
    # "stop talking" is not a robot-motion order; only enumerated hold
    # forms qualify.
    check(kind_of("stop talking") is RouteKind.CONVERSATION,
          "'stop talking' must not create a hold intent")
    check(kind_of("stop moving") is RouteKind.MOTION,
          "'stop moving' is the hold order")


# The CLOSED set of reason tokens this module may emit. A reason names the
# RULE that fired, never the transcript — so the leak check below can allow
# exactly this vocabulary and nothing else.
ALL_REASONS = {
    "empty_after_normalization", "explicit_hold", "ambiguous_motion_phrase",
    "explicit_motion_verb", "explicit_destination", "motion_without_destination",
    "default_conversation", "pronoun_destination", "destination_named_place",
    "destination_saved_route", "destination_tracked_object",
    "destination_address",
}
REASON_VOCABULARY = {w for r in ALL_REASONS for w in r.split("_")}


def test_reasons_are_stable_tokens_without_transcript():
    for t in (MOTION_REQUIRED + CONVERSATION_REQUIRED + AMBIGUOUS_REQUIRED
              + DESTINATION_REQUIRED):
        d = classify_transcript(t)
        check(" " not in d.reason and d.reason.replace("_", "").isalnum(),
              f"reason must be a bare token: {d.reason!r}")
        check(d.reason in ALL_REASONS,
              f"reason {d.reason!r} is outside the closed set")
        # No transcript word longer than 3 chars may leak into the reason —
        # except the fixed CATEGORY vocabulary the reason tokens are built
        # from ("hold"/"route"/"address" name the RULE, not the words).
        for w in normalize(t).split():
            if len(w) > 3 and w not in REASON_VOCABULARY:
                check(w not in d.reason,
                      f"transcript word {w!r} leaked into reason {d.reason!r}")


def test_a_destination_query_carries_only_the_operators_words():
    # The query is the operator's own naming of the place — never a coordinate
    # and never router-invented text. Pin that it is a SUBSTRING of the
    # normalized transcript (modulo case), so nothing can be added.
    for t in DESTINATION_REQUIRED:
        d = classify_transcript(t)
        wire = d.destination.to_wire()
        spoken = normalize(t)
        for key in ("query", "class", "color"):
            value = wire.get(key)
            if value:
                check(value.lower() in spoken,
                      f"{t!r}: {key}={value!r} is not the operator's own words")


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
    for t in (MOTION_REQUIRED + CONVERSATION_REQUIRED + AMBIGUOUS_REQUIRED
              + DESTINATION_REQUIRED):
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
