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
    DESTINATION = "destination"
    AMBIGUOUS = "ambiguous"


@dataclass(frozen=True)
class DestinationRequest:
    """A TYPED destination request — a kind plus the operator's words.

    🔴 There is no coordinate field, and there is no code path in this module
    that could produce one: naming a destination is all language is allowed to
    do. The trusted resolver (`kirra-sidecars/src/destination.rs`, behind
    `POST /destination`) is the ONLY thing that turns a name into a pose.

    `class_`/`color` are set ONLY for `tracked_object`, from a deliberately
    small closed vocabulary; every other kind carries `query` alone.
    """
    kind: str                      # named_place | saved_route | tracked_object | address
    query: str | None = None
    class_: str | None = None
    color: str | None = None

    def to_wire(self) -> dict:
        """The exact `destination` object POSTed to the resolver."""
        if self.kind == "tracked_object" and self.class_:
            wire = {"kind": self.kind, "class": self.class_}
            if self.color:
                wire["color"] = self.color
            return wire
        return {"kind": self.kind, "query": self.query or ""}


@dataclass(frozen=True)
class RouteDecision:
    kind: RouteKind
    reason: str
    destination: DestinationRequest | None = None


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
# The same punctuation strip WITHOUT lowercasing, so a destination query can
# keep the operator's capitalization ("125 Main Street") while every routing
# decision is still made on the lowercase form. Token counts match exactly.
_PUNCT_CASED = re.compile(r"[^a-zA-Z0-9 ]+")

# --- typed destination grammar (deliberately small and closed) ---------------
#
# 🔴 This grammar chooses a destination KIND and repeats the operator's words.
# It never produces a coordinate — see DestinationRequest. Anything it does
# not recognise stays conversation or fails closed as ambiguous; the resolver
# then refuses anything it cannot ground.

# Verbs that can carry a destination complement.
_DEST_VERBS = frozenset({"drive", "go", "head", "navigate", "move", "proceed"})
# Prepositions that introduce the destination itself.
_DEST_PREPOSITIONS = frozenset({"to", "near", "toward", "towards"})
# Verbs that can start a SAVED ROUTE request ("run route A").
_ROUTE_VERBS = frozenset({"run", "follow", "start", "take", "resume"})
_ARTICLES = frozenset({"the", "a", "an", "my", "our"})

# The closed tracked-object vocabulary. Small on purpose: a bigger list would
# start swallowing named places ("front door", "garage"), and free-form scene
# description is explicitly out of scope — an unrecognised thing simply routes
# as a named place and the registry refuses it.
_OBJECT_CLASSES = frozenset({
    "cup", "chair", "person", "box", "bottle", "ball", "bag", "book", "cone",
    "mug", "can",
})
_OBJECT_COLORS = frozenset({
    "red", "blue", "green", "yellow", "black", "white", "orange", "purple",
    "grey", "gray", "brown", "pink",
})

# Street suffixes that, together with a leading house number, mark an address.
_STREET_SUFFIXES = frozenset({
    "street", "st", "avenue", "ave", "road", "rd", "lane", "ln", "drive",
    "boulevard", "blvd", "way", "court", "ct", "place", "pl", "terrace",
    "circle", "crescent", "parade", "highway", "hwy",
})

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


def _split_cased(text: str) -> list[str]:
    """Tokens with case PRESERVED, through the same punctuation/whitespace
    pipeline as `normalize` — so the two token lists align index-for-index."""
    cased = _WS.sub(" ", _PUNCT_CASED.sub(" ", text)).strip()
    return cased.split(" ") if cased else []


def _strip_articles(lower: list[str], cased: list[str]) -> tuple[list[str], list[str]]:
    """Drop leading articles from an aligned (lowercase, cased) token pair."""
    i = 0
    while i < len(lower) and lower[i] in _ARTICLES:
        i += 1
    return lower[i:], cased[i:]


def _is_address(lower: list[str]) -> bool:
    """A leading house number PLUS a street suffix. Both are required: "125"
    alone is not an address, and "main street" without a number is more
    plausibly a named place the operator registered."""
    return (
        len(lower) >= 2
        and lower[0].isdigit()
        and any(tok in _STREET_SUFFIXES for tok in lower[1:])
    )


def _as_object(lower: list[str]) -> DestinationRequest | None:
    """`<class>` or `<color> <class>` from the closed vocabulary, else None."""
    if len(lower) == 1 and lower[0] in _OBJECT_CLASSES:
        return DestinationRequest(kind="tracked_object", class_=lower[0])
    if (
        len(lower) == 2
        and lower[0] in _OBJECT_COLORS
        and lower[1] in _OBJECT_CLASSES
    ):
        return DestinationRequest(
            kind="tracked_object", class_=lower[1], color=lower[0]
        )
    return None


def _destination_decision(lower: list[str], cased: list[str]) -> RouteDecision:
    """Classify the tokens AFTER a destination preposition into a typed
    request — or fail closed as ambiguous when they name nothing resolvable.

    Order is deliberate: address (a house number is unmistakable) → tracked
    object (closed vocabulary) → named place (everything else the operator
    may have registered). A pronoun names nothing and never moves the robot.
    """
    lower, cased = _strip_articles(lower, cased)
    if not lower:
        return RouteDecision(RouteKind.AMBIGUOUS, "motion_without_destination")
    if all(tok in _PRONOUN_DESTS for tok in lower):
        return RouteDecision(RouteKind.AMBIGUOUS, "pronoun_destination")
    if _is_address(lower):
        return RouteDecision(
            RouteKind.DESTINATION, "destination_address",
            DestinationRequest(kind="address", query=" ".join(cased)))
    obj = _as_object(lower)
    if obj is not None:
        return RouteDecision(RouteKind.DESTINATION, "destination_tracked_object", obj)
    # A route named with a preposition ("go to route a") is still a route.
    if "route" in lower:
        return RouteDecision(
            RouteKind.DESTINATION, "destination_saved_route",
            DestinationRequest(kind="saved_route", query=" ".join(lower)))
    return RouteDecision(
        RouteKind.DESTINATION, "destination_named_place",
        DestinationRequest(kind="named_place", query=" ".join(cased)))


def _route_decision(lower: list[str]) -> RouteDecision:
    """A saved-route request: `run route A`, `follow the perimeter route`."""
    lower, _ = _strip_articles(lower, list(lower))
    if not lower or all(tok in _PRONOUN_DESTS for tok in lower):
        # "run it" / "follow that" name no route — never guess which one.
        return RouteDecision(RouteKind.AMBIGUOUS, "pronoun_destination")
    return RouteDecision(
        RouteKind.DESTINATION, "destination_saved_route",
        DestinationRequest(kind="saved_route", query=" ".join(lower)))


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
    # The case-preserving twin of `tokens`, aligned by dropping exactly the
    # tokens wake/courtesy stripping removed — so a destination query can
    # carry "125 Main Street" while routing stays lowercase.
    all_cased = _split_cased(text)
    cased = all_cased[len(all_cased) - len(tokens):] if len(all_cased) >= len(tokens) else tokens

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
    # A DESTINATION verb plus a preposition names a PLACE, not a maneuver:
    # it routes to the typed destination door, where a trusted resolver — not
    # this process and not a model — supplies the coordinates. A direction
    # word after the same verb is still ordinary motion and is checked first,
    # so "drive forward" and "go straight" are unchanged.
    if verb in _DEST_VERBS and rest:
        if rest[0] in _DIR_WORDS:
            return RouteDecision(RouteKind.MOTION, "explicit_motion_verb")
        if rest[0] in _DEST_PREPOSITIONS:
            return _destination_decision(rest[1:], cased[2:])
        # "go there", "head over" — a destination verb aimed at a pronoun
        # names nothing. Fail closed rather than guess where "there" is.
        if all(tok in _PRONOUN_DESTS for tok in rest):
            return RouteDecision(RouteKind.AMBIGUOUS, "pronoun_destination")
        # "go over that again", "go on" … → conversation (default below)
    if verb in _ROUTE_VERBS and rest and verb != "take":
        if "route" in rest:
            return _route_decision(rest)
        if all(tok in _PRONOUN_DESTS for tok in rest):
            return RouteDecision(RouteKind.AMBIGUOUS, "pronoun_destination")
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
    if verb == "take" and rest:
        if rest[0] == "me":
            if len(rest) >= 2 and rest[1] in _DEST_PREPOSITIONS:
                return _destination_decision(rest[2:], cased[3:])
            return RouteDecision(RouteKind.AMBIGUOUS, "motion_without_destination")
        # "take route A" — a saved route, not a passenger request.
        if "route" in rest:
            return _route_decision(rest)

    return RouteDecision(RouteKind.CONVERSATION, "default_conversation")
