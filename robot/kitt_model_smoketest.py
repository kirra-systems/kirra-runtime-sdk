#!/usr/bin/env python3
"""kitt_model_smoketest.py — the KITT doer-contract gate for a model swap.

Run this against a candidate LLM BEFORE flipping `KIRRA_KITT_MODEL` in
robot.env. It fires a handful of canned utterances at the model through the
EXACT production contract — `kitt_converse.STAGE2_SYSTEM` + `parse_reply` — and
asserts the model still honours the router's expectations:

  1. it emits parseable `{"say":…, "directive":…}` JSON,
  2. a clear DRIVE command  → a non-null directive (Channel B fires),
  3. a question / chat / status → a NULL directive (no spurious motion request),
  4. given a fixed telemetry block, it does NOT fabricate a fact it wasn't given.

🔴 THIS IS A DOER-QUALITY GATE, NOT A SAFETY GATE. It never touches the checker,
   the fence, the intent parse, or the release token — those are model-agnostic
   and need NO re-review on a swap (that's the whole point of the doer/checker
   split). This only answers "does this model still speak the KITT contract?" A
   model that FAILS here degrades safely in production anyway — an unparseable
   turn is fail-closed to SPEAK-only (no directive, no motion). The gate just
   turns "should we trust the new model?" into a 30-second check instead of a
   surprise on the robot.

NOT a CI test: it needs a live Ollama + the model pulled, so it runs at the
bench, not in the pipeline.

On a full PASS it records the model's Ollama DIGEST as the vetted pin
(`~/.kirra_kitt_model.pin`, override `KIRRA_KITT_MODEL_PIN_FILE`). Boot then
compares the RUNNING digest against that pin and warns (Channel A) on a
mismatch — so a "no version bump" stealth update (same tag, different weights)
is caught instead of passing silently.

Usage:
  python3 robot/kitt_model_smoketest.py                 # test KIRRA_KITT_MODEL + pin on pass
  python3 robot/kitt_model_smoketest.py gemma4:8b       # test a CANDIDATE first
  python3 robot/kitt_model_smoketest.py --no-pin        # test without recording a pin
  python3 robot/kitt_model_smoketest.py --pin-check     # ONLY compare running digest vs pin (no LLM)
Env: KIRRA_OLLAMA_URL (default http://localhost:11434), KIRRA_KITT_MODEL,
     KIRRA_KITT_MODEL_PIN_FILE (default ~/.kirra_kitt_model.pin).
Exit 0 = the model honours the contract; 1 = it doesn't / digest changed.
"""
from __future__ import annotations

import os
import re
import sys

try:
    import requests
except ImportError:
    sys.exit("kitt_model_smoketest: python3-requests missing (pip3 install requests)")

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
# The REAL production contract — same system prompt + lenient fail-closed parser
# the live router uses. Testing anything else would be testing a fiction.
from kitt_converse import OLLAMA, STAGE2_SYSTEM, parse_reply  # noqa: E402
from kitt_ask import MODEL as DEFAULT_MODEL  # noqa: E402
from kitt_persona import (  # noqa: E402
    classify_model_pin, model_pin_path, read_model_pin, write_model_pin,
)

# A fixed, self-contained telemetry block (the shape context_for builds). The
# grounding cases below assert the model answers ONLY from this.
FIXTURE_CTX = (
    "TELEMETRY (ground truth — answer only from this):\n"
    "- operator: unknown (don't guess a name)\n"
    "- fleet posture: nominal (no reported faults)\n"
    "- last verdict: no actuator command has been judged yet\n"
    "- perception: nearest obstacle straight ahead 2.00 m ahead; 1 objects "
    "detected; drivable corridor ~1.20 m wide lane"
)

# (name, utterance, expect_directive) — the router contract.
DIRECTIVE_CASES = [
    ("drive_creep", "creep forward one meter", True),
    ("drive_goto", "take us to the door", True),
    ("question_see", "what do you see?", False),
    ("chat_weather", "nice weather today", False),
    ("status_ok", "are we OK?", False),
]


def chat(model, utterance):
    """One production-shaped turn; returns the model's RAW content (or None)."""
    messages = [
        {"role": "system", "content": STAGE2_SYSTEM},
        {"role": "user", "content": f"{FIXTURE_CTX}\n\nOperator says: {utterance}"},
    ]
    try:
        r = requests.post(f"{OLLAMA}/api/chat", timeout=60.0,
                          json={"model": model, "stream": False, "messages": messages})
        if r.status_code != 200:
            return None
        return (r.json().get("message", {}).get("content") or "").strip()
    except Exception as e:  # noqa: BLE001
        print(f"    (chat error: {e})", file=sys.stderr)
        return None


def _match_model(models, model):
    """The /api/tags entry for `model` (exact or base-name match), or None."""
    for m in models:
        name = m.get("name", "")
        if name == model or name.split(":")[0] == model.split(":")[0]:
            return m
    return None


def model_digest(model):
    """The Ollama content digest for `model` (its identity — changes on a stealth
    'no version bump' update), or None if unavailable. Needs a live Ollama."""
    try:
        r = requests.get(f"{OLLAMA}/api/tags", timeout=5.0)
        m = _match_model(r.json().get("models", []), model)
        return (m or {}).get("digest") or None
    except Exception:  # noqa: BLE001
        return None


def pin_status(model=None):
    """(status, running_digest, pinned_digest) — status per classify_model_pin.
    Used by the boot warning. Best-effort; 'unavailable' if Ollama is unreachable."""
    model = model or DEFAULT_MODEL
    running = model_digest(model)
    pinned = read_model_pin(model)
    return classify_model_pin(running, pinned), running, pinned


def preflight(model):
    """Fail loudly if Ollama is down or the model isn't pulled; return its digest."""
    try:
        r = requests.get(f"{OLLAMA}/api/tags", timeout=5.0)
        models = r.json().get("models", [])
    except Exception as e:  # noqa: BLE001
        sys.exit(f"FATAL: can't reach Ollama at {OLLAMA} ({e}). Start it: `ollama serve`.")
    match = _match_model(models, model)
    if match is None:
        have = ", ".join(m.get("name", "") for m in models) or "none"
        sys.exit(f"FATAL: model {model!r} not pulled. `ollama pull {model}` (have: {have}).")
    return match.get("digest") or None


def run_directive_case(model, name, utterance, expect_directive):
    raw = chat(model, utterance)
    if raw is None:
        return False, "no response (HTTP error / model down)"
    had_json = "{" in raw and "}" in raw
    _say, directive = parse_reply(raw)
    got = directive is not None
    if expect_directive and not got:
        hint = "no JSON object in reply" if not had_json else "JSON had null/blank directive"
        return False, f"expected a directive, got none ({hint})"
    if not expect_directive and got:
        return False, f"expected NO directive, got {directive!r} (spurious motion request)"
    return True, f"directive={directive!r}"


def run_grounding_cases(model):
    """(a) answers from context; (b) refuses to fabricate an ungiven fact."""
    results = []

    # (a) the answer IS in the fixture — expect it referenced, not invented.
    say, _ = parse_reply(chat(model, "how far is the closest thing in front of us?") or "")
    low = say.lower()
    ok_a = ("2" in low) or ("two" in low)
    results.append(("grounding_uses_context", ok_a,
                    f"say={say[:80]!r}" if ok_a else f"did not cite the 2 m fixture: {say[:80]!r}"))

    # (b) battery is NOT in the fixture — a grounded model must NOT state a number.
    say_b, _ = parse_reply(chat(model, "what is my battery percentage?") or "")
    fabricated = re.search(r"\d+\s*(%|percent)", say_b.lower())
    results.append(("grounding_no_fabrication", not fabricated,
                    "declined to invent a battery number" if not fabricated
                    else f"FABRICATED a battery number: {say_b[:80]!r}"))
    return results


def _pin_check(model):
    """Compare the RUNNING model's digest to the vetted pin — no LLM calls.
    Exit 1 on 'changed' (a stealth update); 0 otherwise."""
    status, running, pinned = pin_status(model)
    print(f"model={model!r}  running_digest={running or 'unavailable'}")
    print(f"vetted_pin={pinned or 'none'}  status={status.upper()}")
    if status == "changed":
        print("The running model differs from the vetted pin — a 'no version bump' "
              "stealth update (same tag, new weights). Re-run the smoketest before "
              "trusting the drive-by-voice path.", file=sys.stderr)
        return 1
    if status == "unpinned":
        print("(not yet vetted — run the smoketest to record a pin)")
    return 0


def main():
    argv = sys.argv[1:]
    no_pin = "--no-pin" in argv
    positional = [a for a in argv if not a.startswith("-")]
    model = positional[0] if positional else DEFAULT_MODEL

    if "--pin-check" in argv:
        return _pin_check(model)

    print(f"KITT model doer-contract smoketest — model={model!r} @ {OLLAMA}")
    print("(doer-quality only; the checker/fence are model-agnostic and unaffected)")
    digest = preflight(model)
    print(f"running digest: {digest or 'unavailable'}\n")

    failures = 0
    for name, utterance, expect in DIRECTIVE_CASES:
        ok, detail = run_directive_case(model, name, utterance, expect)
        print(f"  {'ok  ' if ok else 'FAIL'} {name:24} {detail}")
        failures += 0 if ok else 1

    for name, ok, detail in run_grounding_cases(model):
        print(f"  {'ok  ' if ok else 'FAIL'} {name:24} {detail}")
        failures += 0 if ok else 1

    total = len(DIRECTIVE_CASES) + 2
    print(f"\n{total - failures}/{total} passed")
    if failures:
        print("This model does NOT cleanly honour the KITT router contract. It is "
              "still SAFE to run (unparseable turns fail closed to speak-only), but "
              "the drive-by-voice path may not fire reliably. Prefer a model that "
              "passes, or tune STAGE2_SYSTEM for it.", file=sys.stderr)
        return 1
    # All pass → record the digest we just vetted, so a later stealth update
    # (same tag, different weights) is caught at boot.
    if not no_pin and digest:
        try:
            write_model_pin(model, digest)
            print(f"\nvetted → pinned {model} @ {digest}")
            print(f"  ({model_pin_path()}; boot warns if the running digest ever differs)")
        except OSError as e:
            print(f"(could not write pin: {e})", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
