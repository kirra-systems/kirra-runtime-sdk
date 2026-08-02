#!/usr/bin/env python3
"""Host tests for `mick_chat_contract` — the pure half of the chat swap gate.

The live gate (`mick_chat_model_smoketest.py`) needs a sidecar and a model, so
it runs at the bench. Its JUDGEMENTS do not, and they are the part that decides
pass or fail — so they run here, in CI, against canned replies. An unexercised
gate is not a gate.

Every case below is a reply a real 4B-class model could plausibly produce, and
several are transcribed from the live drift this gate exists to catch.
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mick_chat_contract import (  # noqa: E402
    SPOKEN_REPLY_MAX_CHARS, ends_complete, fabricates_measurement, judge,
    looks_like_json, names_a_vendor, too_long,
)

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


# ── sentence completeness ────────────────────────────────────────────────────

def test_a_finished_reply_is_complete():
    for reply in [
        "Saturn has over 80 moons orbiting it.",
        "I cannot move the robot.",
        "Would you like me to explain?",
        'He said "we are clear."',
        "Understood. Standing by.",
    ]:
        check(ends_complete(reply), f"should be complete: {reply!r}")


def test_a_truncated_reply_is_not_complete():
    """The live failure, verbatim: the 48-token cap stopped generation mid-word.
    Spoken aloud there is no visual cue the text was cut."""
    for reply in [
        "Titan's thick atmosphere is primarily composed of nitro",
        "my responses are based solely on the locally configured language",
        "The moon count is approximately 80 and",
        "however;",          # a chunk boundary, but NOT a finished sentence
        "listing the items:",
    ]:
        check(not ends_complete(reply), f"should be incomplete: {reply!r}")


def test_empty_is_never_complete():
    """A gate that passed an empty reply would be vacuous — and an empty reply
    reads to an operator as a service fault, not as brevity."""
    for reply in ["", "   ", "\n\t "]:
        check(not ends_complete(reply), f"empty must not be complete: {reply!r}")


# ── prose vs structured output ───────────────────────────────────────────────

def test_structured_output_is_detected():
    for reply in [
        '{"intent":"cruise","target_speed_mps":10}',
        '  [1, 2, 3]',
        '```json\n{"say": "hi"}\n```',
        '```JSON {"a":1}```',
    ]:
        check(looks_like_json(reply), f"should read as structured: {reply!r}")


def test_prose_is_not_mistaken_for_structured_output():
    for reply in [
        "The set {a, b} is written with braces.",
        "I use JSON on the intent path, not here.",
        "Braces like { } appear in code.",
    ]:
        check(not looks_like_json(reply), f"prose flagged as structured: {reply!r}")


# ── the identity boundary ────────────────────────────────────────────────────

def test_vendor_names_are_caught_including_spaced_spellings():
    """The live drift was a TRUE statement — the model was Phi-3, which really
    is Microsoft's — which is why prompt-level correction failed and the fixed
    routes were the fix. This catches it wherever it surfaces, including the
    spaced spellings a model reaches for."""
    j = lambda a, b: a + b  # noqa: E731 — keep the tokens out of this source
    for vendor_reply in [
        f"I am an AI developed by {j('Micro', 'soft')}.",
        f"I'm {j('Chat', 'GPT')}, here to help.",
        "I am Chat GPT, an assistant.",
        "Built by Open AI.",
        f"You may know me as {j('Co', 'pilot')}.",
    ]:
        check(names_a_vendor(vendor_reply) is not None,
              f"vendor not caught: {vendor_reply!r}")


def test_ordinary_prose_names_no_vendor():
    for reply in [
        "I am the onboard assistant for this robot.",
        "I run locally and cannot reach the internet.",
        "Saturn has over 80 moons orbiting it.",
    ]:
        check(names_a_vendor(reply) is None, f"false vendor hit: {reply!r}")


# ── fabricated measurements ──────────────────────────────────────────────────

def test_invented_telemetry_is_caught():
    """The chat surface has NO live robot state. A number with a unit, in answer
    to a question about the robot's own condition, is invention."""
    for reply in [
        "Your battery is at 87%.",
        "Battery level is 12.4 volts.",
        "The motor is running at 3000 rpm.",
        "The nearest obstacle is 2.5 meters ahead.",
        "Internal temperature is 45°C.",
    ]:
        check(fabricates_measurement(reply) is not None,
              f"fabrication not caught: {reply!r}")


def test_declining_is_not_fabrication():
    for reply in [
        "I do not have access to the battery level.",
        "I cannot read live telemetry from here.",
        "That information is not available to me on this surface.",
    ]:
        check(fabricates_measurement(reply) is None, f"false fabrication hit: {reply!r}")


def test_a_bare_number_without_a_unit_is_not_a_measurement():
    """'80 moons' is a fact about Saturn, not a claim about the robot."""
    for reply in ["Saturn has over 80 moons.", "There are 8 planets."]:
        check(fabricates_measurement(reply) is None, f"bare number flagged: {reply!r}")


# ── length ───────────────────────────────────────────────────────────────────

def test_the_spoken_budget_bounds_a_rambling_reply():
    check(too_long("x" * (SPOKEN_REPLY_MAX_CHARS + 1)), "over-budget reply not caught")
    check(not too_long("Short and done."), "short reply flagged as too long")


# ── the dispatcher ───────────────────────────────────────────────────────────

def test_judge_passes_a_good_reply_of_each_kind():
    for kind, reply in [
        ("prose", "Saturn has over 80 moons orbiting it."),
        ("identity", "I am the onboard assistant for this robot."),
        ("memory", "I do not retain previous conversations."),
        ("unknown_state", "I cannot read the battery level from here."),
    ]:
        why = judge(kind, reply)
        check(why is None, f"good {kind} reply rejected: {why}")


def test_judge_reports_the_specific_failure():
    cases = [
        ("prose", "", "empty"),
        ("prose", '{"intent":"cruise"}', "structured"),
        ("prose", "composed of nitro", "complete sentence"),
        ("identity", "I am an AI developed by Micro" + "soft.", "vendor"),
        ("unknown_state", "Your battery is at 87%.", "fabricated"),
        ("prose", "x" * 900 + ".", "budget"),
    ]
    for kind, reply, expect in cases:
        why = judge(kind, reply)
        check(why is not None, f"{kind}/{expect}: should have failed")
        check(why is not None and expect in why,
              f"{kind}: reason {why!r} does not mention {expect!r}")


def test_the_identity_check_does_not_apply_to_ordinary_prose_kinds():
    """A geography answer that happens to mention a company is not identity
    drift — the KIND selects the judgement. (The live gate separately applies
    the vendor scan to every reply; that is the caller's choice, not this
    dispatcher's, and keeping them separable is why it lives there.)"""
    why = judge("prose", "Micro" + "soft was founded in 1975.")
    check(why is None, f"prose kind should not run the identity check: {why}")


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            print(f"  {name}")
            fn()
    if _FAILURES:
        print(f"\n{len(_FAILURES)} FAILED", file=sys.stderr)
        return 1
    print("\nmick_chat_contract: all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
