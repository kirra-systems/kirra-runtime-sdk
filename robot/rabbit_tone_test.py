#!/usr/bin/env python3
"""Host tests for the pure persona/tone scorer in `rabbit_tone.py`.

Pure + stdlib-only (no HTTP/LLM), so the model-swap tone gate's decision logic
runs on a plain host. The live use — scoring a candidate model's real replies in
`rabbit_model_smoketest` — is the seam.

Runs standalone (`python3 robot/rabbit_tone_test.py`, exit 1 on failure); also
importable under pytest.
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rabbit_tone import score_replies, score_tone  # noqa: E402

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


def test_in_character_replies_pass_clean():
    for good in (
        "All systems are nominal.",
        "All systems are nominal. I'd suggest easing off the accelerator to "
        "preserve battery.",
        "I'm afraid I can't determine that right now; the governor is unreachable.",
        "Nearest obstacle is roughly two metres ahead.",
        "",  # a pure-directive turn with no spoken line
    ):
        ok, issues = score_tone(good)
        check(ok, f"in-character reply should pass, got {issues}: {good!r}")


def test_enthusiastic_filler_fails():
    ok, issues = score_tone("Awesome! Sure thing, happy to help!")
    check(not ok, "gushing reply must fail")
    joined = " ".join(issues)
    check("awesome" in joined and "sure thing" in joined and "happy to help" in joined,
          f"filler tokens flagged: {issues}")
    check("exclamation" in joined, "exclamation flagged")


def test_slang_fails():
    ok, issues = score_tone("Yeah, gonna get right on that, dude.")
    check(not ok, "slang reply must fail")
    j = " ".join(issues)
    check("yeah" in j and "gonna" in j and "dude" in j, f"slang flagged: {issues}")


def test_slang_whole_word_only_no_false_positive():
    # 'cool' is slang, but 'coolant' / 'supervisor' must NOT trip a substring match.
    ok, issues = score_tone("The coolant temperature is nominal and the supervisor is online.")
    check(ok, f"substrings of slang words must not false-fire: {issues}")


def test_emoji_fails():
    ok, issues = score_tone("On our way to the dock \U0001F697")
    check(not ok and any("emoji" in i for i in issues), f"emoji must fail: {issues}")


def test_exclamation_alone_fails():
    ok, issues = score_tone("Right away.")
    check(ok, "a calm statement passes")
    ok2, issues2 = score_tone("Right away!")
    check(not ok2 and any("exclamation" in i for i in issues2),
          f"exclamation must fail: {issues2}")


def test_too_long_fails():
    ok, issues = score_tone("One. Two. Three. Four.")
    check(not ok and any("too long" in i for i in issues), f"over-long must fail: {issues}")


def test_score_replies_batch_and_findings():
    ok, findings = score_replies([
        ("status", "All systems are nominal."),
        ("greet", "Awesome, happy to help!"),
        ("drive_confirm", ""),  # no spoken line
    ])
    check(not ok, "a batch with one bad reply fails")
    check(len(findings) == 1 and findings[0][0] == "greet",
          f"only the offending reply is reported: {findings}")

    ok2, findings2 = score_replies([("a", "Understood."), ("b", "Noted.")])
    check(ok2 and not findings2, "an all-clean batch passes")


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
    if _FAILURES:
        print(f"\n{len(_FAILURES)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"rabbit_tone_test: all {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
