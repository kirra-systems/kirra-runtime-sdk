#!/usr/bin/env python3
"""mick_chat_contract.py — the PURE half of the chat-model swap gate.

Every judgement the chat smoketest makes about a model's reply lives here as a
total, stdlib-only function over a plain string. The live half
(`mick_chat_model_smoketest.py`) only supplies replies; it decides nothing. That
split is the same one `rabbit_tone.py` has against `rabbit_model_smoketest.py`,
and it exists so the judgements are CI-testable without a model, an Ollama, or a
running sidecar — an unexercised gate is not a gate.

🔴 DOER-QUALITY ONLY, NOT SAFETY. Nothing here touches the checker, the fence,
   the intent parse, or the release token. A model that fails these still
   degrades safely in production: the chat surface has no motion authority at
   all, so the worst outcome of a bad conversational model is a bad
   conversation. This gate answers one question — "does this model still speak
   the chat contract?" — and turns it into a bench check instead of a surprise
   on the robot.
"""
from __future__ import annotations

import re

#: Terminal punctuation that ENDS a sentence. Mirrors `SENTENCE_TERMINALS` in
#: crates/kirra-sidecars/src/chat.rs — ';' and ':' are chunk boundaries there
#: but do NOT end a sentence, and the same must hold here or the two halves
#: would disagree about what "finished" means.
SENTENCE_TERMINALS = (".", "?", "!")

#: Closers that may legitimately follow the terminal punctuation.
TRAILING_CLOSERS = ('"', "'", ")", "]", "}", "”", "’")

#: Assistant vendors the robot must never claim to be, or be named after. Built
#: at import from halves so this module's own source cannot satisfy a grep for
#: the joined token (the same trick the Rust separation test uses).
EXTERNAL_VENDORS = tuple(
    a + b for a, b in [
        ("Micro", "soft"), ("Co", "pilot"), ("Chat", "GPT"), ("Open", "AI"),
        ("Clau", "de"), ("Gem", "ini"), ("Ant", "hropic"), ("Goo", "gle"),
    ]
)

#: Units that turn a bare number into a MEASUREMENT CLAIM. The chat surface has
#: no live robot state — posture, sensors and diagnostics are known only if the
#: caller put them in the request text — so a number carrying one of these, in
#: answer to a question about the robot's own state, is fabrication rather than
#: conversation.
MEASUREMENT_UNITS = (
    "%", "percent", "volt", "amp", "celsius", "fahrenheit", "degree",
    "metre", "meter", "meters", "metres", "km", "mph", "rpm", "hz",
)

#: A reply longer than this is too long to speak aloud in one turn. Generous on
#: purpose: this catches a model that ignores the brevity instruction outright,
#: not one that runs a sentence over.
SPOKEN_REPLY_MAX_CHARS = 600


def looks_like_json(text: str) -> bool:
    """True if the reply is (or opens as) a JSON object or array rather than
    prose. The chat surface answers in sentences; a model emitting structured
    output there is speaking the WRONG contract, even when the guard catches it
    before an operator sees it."""
    t = text.strip()
    if t.startswith("```"):
        t = t.lstrip("`").lstrip()
        for lang in ("json", "javascript"):
            if t.lower().startswith(lang):
                t = t[len(lang):].lstrip()
    return t.startswith("{") or t.startswith("[")


def ends_complete(text: str) -> bool:
    """True if the reply ends on a complete sentence.

    Empty text is NOT complete — an empty reply reads as a service fault, and a
    gate that passed it would be vacuous."""
    t = text.rstrip()
    while t and t[-1] in TRAILING_CLOSERS:
        t = t[:-1].rstrip()
    return bool(t) and t.endswith(SENTENCE_TERMINALS)


def names_a_vendor(text: str) -> "str | None":
    """The first external assistant vendor named in `text`, or None.

    Case-insensitive, and tolerant of the spaced spellings a model reaches for
    ("chat gpt", "open ai") — matching only the joined form would let the exact
    drift this gate exists to catch walk straight through."""
    folded = re.sub(r"[^a-z]+", "", text.lower())
    for vendor in EXTERNAL_VENDORS:
        if vendor.lower() in folded:
            return vendor
    return None


def fabricates_measurement(text: str) -> "str | None":
    """The first fabricated measurement in `text`, or None.

    A measurement is a number adjacent to a unit. Applied ONLY to replies
    answering a question about the robot's own state that the request did not
    supply — asking "how far is a kilometre" is not fabrication, which is why
    the caller chooses where this runs rather than it being applied blanket."""
    for m in re.finditer(r"(\d+(?:\.\d+)?)\s*([%a-zA-Z°]+)", text):
        number, word = m.group(1), m.group(2).lower().lstrip("°")
        for unit in MEASUREMENT_UNITS:
            if word.startswith(unit) or word == unit.rstrip("s"):
                return f"{number} {m.group(2)}"
    if re.search(r"\d+\s*°", text):
        return re.search(r"\d+\s*°\S*", text).group(0)
    return None


def too_long(text: str, budget: int = SPOKEN_REPLY_MAX_CHARS) -> bool:
    """True if the reply exceeds the spoken-turn budget."""
    return len(text.strip()) > budget


def judge(kind: str, reply: str) -> "str | None":
    """The one dispatcher the live gate calls: a failure REASON, or None on pass.

    `kind` names what the utterance was probing, so the caller's case table
    stays declarative and every judgement stays here."""
    if not reply.strip():
        return "empty reply"
    if too_long(reply):
        return f"reply is {len(reply.strip())} chars (budget {SPOKEN_REPLY_MAX_CHARS})"
    if looks_like_json(reply):
        return "replied with structured output instead of prose"
    if not ends_complete(reply):
        return f"reply does not end on a complete sentence: …{reply.strip()[-40:]!r}"

    if kind in ("identity", "provenance", "model", "memory"):
        vendor = names_a_vendor(reply)
        if vendor:
            return f"names an external vendor ({vendor})"
    if kind == "unknown_state":
        made_up = fabricates_measurement(reply)
        if made_up:
            return f"fabricated a measurement it was never given ({made_up})"
    return None
