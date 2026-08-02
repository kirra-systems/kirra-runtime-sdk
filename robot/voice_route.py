#!/usr/bin/env python3
"""voice_route.py — the PURE deterministic transcript router (no I/O, no LLM).

One question, answered conservatively: is this utterance an EXPLICIT robot
motion request (→ the governed /intent door), ordinary conversation (→ the
chat sidecar), or too ambiguous to move on (→ a fixed local clarification,
NO endpoint)?

🔴 Routing philosophy (the safety half of the design):
  * An LLM is never asked whether text is motion — the decision is a small
    explicit grammar, testable and stable.
  * Motion requires ACTION STRUCTURE: an imperative motion verb as the FIRST
    token of the (wake-prefix-stripped) utterance plus a recognized
    direction/destination complement, or an enumerated whole-utterance form
    ("stop", "hold position", "pull over", …). Unrestricted substring matches
    are banned — "explain how a motor drive works", "why did we stop
    talking", "turn the explanation into a summary" and "go over that again"
    are conversation, and the tests pin them.
  * Ambiguity fails CLOSED for motion: "go ahead", "do it", "proceed",
    "take me there" never route to /intent — the robot asks for specifics.
  * This layer only chooses an ENDPOINT. Mick's own deterministic non-motion
    fence and typed parse remain the authority behind /intent; the chat
    sidecar's motion-shape refusal remains behind /chat. Defense in depth,
    not replaced.

Reasons are STABLE TOKENS (never transcript text) so they are loggable
without widening the transcribe-and-discard privacy policy.
"""
from __future__ import annotations

import re
from dataclasses import dataclass
from enum import Enum


class RouteKind(Enum):
    CONVERSATION = "conversation"
    MOTION = "motion"
    AMBIGUOUS = "ambiguous"


@dataclass(frozen=True)
class RouteDecision:
    kind: RouteKind
    reason: str


# Wake phrases (mirrors wake_word.py's set) plus bare honorifics. Stripped
# repeatedly from the front so "hey parker rabbit stop" still classifies the
# remainder. Stripping NEVER changes the decision grammar itself.
_WAKE_PREFIXES = (
    "hey parker", "hello parker", "hey rabbit", "hello rabbit",
    "okay parker", "ok parker", "okay rabbit", "ok rabbit",
    "parker", "rabbit",
)

# Leading courtesy that must not hide the imperative underneath.
_COURTESY_PREFIXES = ("please", "could you", "can you", "would you", "now")

# Whole-utterance forms that are motion-shaped but carry NO usable direction
# or destination — never enough to move on. (Bare "move" included: direction
# unknown.)
_AMBIGUOUS_FORMS = frozenset({
    "go", "go ahead", "do it", "proceed", "continue", "keep going",
    "take me there", "lets go", "let s go", "move", "okay move", "ok move",
    "carry on", "onward",
})

# Whole-utterance STOP/HOLD forms → the governed hold intent (Mick's typed
# `hold`; Occy decelerates and holds). This is deliberately NOT a motor
# cutoff and NOT the e-stop — the physical e-stop is separate hardware.
_HOLD_FORMS = frozenset({
    "stop", "halt", "hold", "hold position", "stop now", "stop moving",
    "stop the robot", "stop driving", "stay put", "stand by",
})

# Direction complements per verb (the complement is what makes an imperative
# verb a MOTION order rather than table talk).
_DIR_WORDS = frozenset({
    "forward", "forwards", "ahead", "straight", "back", "backward",
    "backwards", "left", "right", "around",
})
_PRONOUN_DESTS = frozenset({"there", "here", "it", "that", "them", "away"})

_WS = re.compile(r"\s+")
_PUNCT = re.compile(r"[^a-z0-9 ]+")

# Bounded distance expressions — the ONLY complement (besides a direction or
# destination) that makes a bare "drive" an explicit motion order. Narrow on
# purpose: a small-number word or a plain numeric literal, followed by a
# meter unit (the router is meters-only today; no other unit is admitted and
# no conversion logic exists). "one hour", "better performance", "me crazy"
# and every prose complement fail this shape and stay conversation.
_NUMBER_WORDS = frozenset({
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    "ten",
})
_DISTANCE_UNITS = frozenset({"meter", "meters"})


def _is_bounded_distance(tokens: list[str]) -> bool:
    """Exactly `<number> <meter unit>` — nothing more, nothing less.

    Numeric literals are bounded to the SAME 1..10 range as the word list:
    "0 meters" is a no-op nobody dictates, and an out-of-range figure
    ("9999 meters") is more plausibly a mis-transcription than a real order
    — either way, admission stays narrow and the utterance falls through to
    conversation rather than the motion door."""
    if len(tokens) != 2 or tokens[1] not in _DISTANCE_UNITS:
        return False
    if tokens[0] in _NUMBER_WORDS:
        return True
    return tokens[0].isdigit() and 1 <= int(tokens[0]) <= 10


def normalize(text: str) -> str:
    """Lowercase, strip punctuation to spaces, collapse whitespace."""
    t = _PUNCT.sub(" ", text.lower())
    return _WS.sub(" ", t).strip()


def strip_wake_prefixes(norm: str) -> str:
    """Remove leading wake phrases / honorifics / courtesy, repeatedly."""
    changed = True
    while changed and norm:
        changed = False
        for p in _WAKE_PREFIXES + _COURTESY_PREFIXES:
            if norm == p:
                return ""
            if norm.startswith(p + " "):
                norm = norm[len(p) + 1:]
                changed = True
    return norm


def _dest_after_to(tokens: list[str], to_idx: int) -> str | None:
    """The destination tokens after a 'to', or None if absent/pronoun-only.

    'go to the loading dock' → 'the loading dock'; 'take me to there' → None
    (a pronoun is not a destination the robot can resolve from this
    utterance alone — ambiguous, do not move).
    """
    rest = tokens[to_idx + 1:]
    if not rest:
        return None
    content = [w for w in rest if w not in ("the", "a", "an", "my", "our")]
    if not content or all(w in _PRONOUN_DESTS for w in content):
        return None
    return " ".join(rest)


def classify_transcript(text: str) -> RouteDecision:
    """Classify ONE transcript. Pure; call exactly once per turn."""
    norm = strip_wake_prefixes(normalize(text))
    if not norm:
        return RouteDecision(RouteKind.AMBIGUOUS, "empty_after_normalization")
    tokens = norm.split(" ")

    if norm in _HOLD_FORMS:
        return RouteDecision(RouteKind.MOTION, "explicit_hold")
    if norm in _AMBIGUOUS_FORMS:
        return RouteDecision(RouteKind.AMBIGUOUS, "ambiguous_motion_phrase")

    verb, rest = tokens[0], tokens[1:]

    # Enumerated multi-word motion forms (verb-first, bounded complements).
    if verb == "drive" and rest:
        # "drive one meter" — Whisper commonly drops the direction word from
        # "drive forward one meter"; a bounded distance IS the complement.
        if _is_bounded_distance(rest):
            return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
        # "drive for one meter" — the known STT substitution of "forward" →
        # "for". Admitted ONLY when the remainder is a bounded distance:
        # "drive for one hour" / "drive for better performance" and every
        # duration/purpose/prose complement stay conversation.
        if rest[0] == "for" and _is_bounded_distance(rest[1:]):
            return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
    if verb in ("drive", "move", "head") and rest:
        if rest[0] in _DIR_WORDS:
            return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
        if rest[0] == "to":
            if _dest_after_to(tokens, 1):
                return RouteDecision(RouteKind.MOTION, "explicit_destination")
            return RouteDecision(RouteKind.AMBIGUOUS, "motion_without_destination")
    if verb == "go" and rest:
        if rest[0] in _DIR_WORDS:
            return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
        if rest[0] == "to":
            if _dest_after_to(tokens, 1):
                return RouteDecision(RouteKind.MOTION, "explicit_destination")
            return RouteDecision(RouteKind.AMBIGUOUS, "motion_without_destination")
        # "go over that again", "go on" … → conversation (default below)
    if verb == "turn" and rest and rest[0] in ("left", "right", "around"):
        return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
    if verb == "back" and rest and rest[0] == "up":
        return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
    if verb == "reverse" and (not rest or rest[0] in _DIR_WORDS | {"slowly", "a", "one", "two"}):
        return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
    if verb == "pull" and rest and rest[0] == "over":
        return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
    if verb == "cruise":
        return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
    if verb == "change" and rest and rest[0] in ("lane", "lanes"):
        return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
    if norm.startswith("lane change"):
        return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
    if verb == "take" and rest and rest[0] == "me":
        if len(rest) >= 2 and rest[1] == "to" and _dest_after_to(tokens, 2):
            return RouteDecision(RouteKind.MOTION, "explicit_destination")
        return RouteDecision(RouteKind.AMBIGUOUS, "motion_without_destination")

    return RouteDecision(RouteKind.CONVERSATION, "default_conversation")
