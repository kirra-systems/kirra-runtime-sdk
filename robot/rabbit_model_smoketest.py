#!/usr/bin/env python3
"""rabbit_model_smoketest.py — the Rabbit doer-contract gate for a model swap.

Run this against a candidate LLM BEFORE flipping `KIRRA_RABBIT_MODEL` in
robot.env. It fires a handful of canned utterances at the model through the
EXACT production contract — `rabbit_converse.STAGE2_SYSTEM` + `parse_reply` — and
asserts the model still honours the router's expectations:

  1. it emits parseable `{"say":…, "directive":…}` JSON,
  2. a clear DRIVE command  → a non-null directive (Channel B fires),
  3. a question / chat / status → a NULL directive (no spurious motion request),
  4. given a fixed telemetry block, it does NOT fabricate a fact it wasn't given,
  5. it actually SOUNDS like Rabbit — the pure `rabbit_tone` scorer gates the
     model's real spoken `say` lines against the objective persona rules (no
     emojis / enthusiastic filler / slang / exclamation / over-long), so a model
     that honours the contract but gushes still fails the swap.

🔴 THIS IS A DOER-QUALITY GATE, NOT A SAFETY GATE. It never touches the checker,
   the fence, the intent parse, or the release token — those are model-agnostic
   and need NO re-review on a swap (that's the whole point of the doer/checker
   split). This only answers "does this model still speak the Rabbit contract?" A
   model that FAILS here degrades safely in production anyway — an unparseable
   turn is fail-closed to SPEAK-only (no directive, no motion). The gate just
   turns "should we trust the new model?" into a 30-second check instead of a
   surprise on the robot.

NOT a CI test: it needs a live Ollama + the model pulled, so it runs at the
bench, not in the pipeline.

On a full PASS it records the model's Ollama DIGEST + the VETTED-AT timestamp
(the reproducibility trail ML researchers recommend logging — "which weights,
verified when") as the vetted pin (`~/.kirra_rabbit_model.pin`, override
`KIRRA_RABBIT_MODEL_PIN_FILE`). Boot then compares the RUNNING digest against that
pin and warns (Channel A) on a mismatch — so a "no version bump" stealth update
(same tag, different weights) is caught instead of passing silently.

SECOND MODE — `--assistant-contract` (the Kirra Engineering Assistant):
  The same bench idea applied to the ASSISTANT's tool selection. It fires the
  versioned corpus (`robot/testdata/assistant_tool_selection_cases.json`) at the
  live model through the REAL model-facing contract
  (`assistant_contract.production_prompt()`), then hands every reply to
  `assistant_contract` for FAIL-CLOSED parsing, deterministic validation and
  scoring.

  🔴 Gemma may propose a registered tool selection, but deterministic policy
     validates the tool name, authority level, arguments, and execution.

  Live-model accuracy is a measured QUALITY property. Execution safety remains
  enforced independently by deterministic validation and authority policy — so
  this mode measures the model without granting it anything. It never invokes a
  mutating tool (level 2 stops at `would_execute`), so it can never synchronize
  or push a branch, and it never writes the Rabbit voice pin.

Usage:
  python3 robot/rabbit_model_smoketest.py                 # test KIRRA_RABBIT_MODEL + pin on pass
  python3 robot/rabbit_model_smoketest.py gemma4:8b       # test a CANDIDATE first
  python3 robot/rabbit_model_smoketest.py --note "hf re-pull 2026-07-16"  # provenance note in the pin
  python3 robot/rabbit_model_smoketest.py --no-pin        # test without recording a pin
  python3 robot/rabbit_model_smoketest.py --pin-check     # ONLY compare running digest vs pin (no LLM)
  python3 robot/rabbit_model_smoketest.py --assistant-contract
  python3 robot/rabbit_model_smoketest.py --assistant-contract --trials 3 --seed 7 \
      --temperature 0.0 --json-report /tmp/contract.json --require-model
Env: KIRRA_OLLAMA_URL (default http://localhost:11434), KIRRA_RABBIT_MODEL,
     KIRRA_RABBIT_MODEL_PIN_FILE (default ~/.kirra_rabbit_model.pin),
     KIRRA_ASSIST_CASES (override the corpus path).
Exit 0 = the model honours the contract; 1 = it doesn't / digest changed;
     3 = `--assistant-contract` could not run because the model is unavailable
         (the live contract is UNVERIFIED, not failed — `--require-model`
         upgrades that to 1).
"""
from __future__ import annotations

import os
import re
import sys
from datetime import datetime, timezone

try:
    import requests
except ImportError:
    sys.exit("rabbit_model_smoketest: python3-requests missing (pip3 install requests)")

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
# The REAL production contract — same system prompt + lenient fail-closed parser
# the live router uses. Testing anything else would be testing a fiction.
from rabbit_converse import (  # noqa: E402
    KEEP_ALIVE, OLLAMA, ROUTER_LLM_OPTIONS, STAGE2_SYSTEM, parse_reply,
)
from rabbit_ask import MODEL as DEFAULT_MODEL  # noqa: E402
from rabbit_persona import (  # noqa: E402
    classify_model_pin, model_pin_path, read_model_pin, read_model_pin_record,
    write_model_pin,
)
# The pure, host-tested persona/tone scorer — the "does it sound like Rabbit?"
# half of the swap gate (rabbit_tone.py; stdlib-only, no LLM).
from rabbit_tone import score_replies  # noqa: E402
# The engineering-assistant tool-selection contract. Every judgement about the
# model's proposal lives in `assistant_contract` (pure, CI-tested with a mocked
# model); this file only supplies the live model.
import assistant_contract as ac  # noqa: E402
import assistant_tools as at  # noqa: E402

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
                          json={"model": model, "stream": False, "messages": messages,
                                "keep_alive": KEEP_ALIVE,       # mirror production residency
                                "options": ROUTER_LLM_OPTIONS})  # mirror production sampling
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


def run_directive_case(model, name, utterance, expect_directive, replies, show_raw=False):
    raw = chat(model, utterance)
    if raw is None:
        return False, "no response (HTTP error / model down)"
    if show_raw:
        print(f"    · {name} raw: {raw[:300]!r}", file=sys.stderr)
    had_json = "{" in raw and "}" in raw
    say, directive = parse_reply(raw)
    replies.append((name, say))  # collect the spoken line for the tone gate
    got = directive is not None
    if expect_directive and not got:
        hint = "no JSON object in reply" if not had_json else "JSON had null/blank directive"
        return False, f"expected a directive, got none ({hint})"
    if not expect_directive and got:
        return False, f"expected NO directive, got {directive!r} (spurious motion request)"
    return True, f"directive={directive!r}"


def run_grounding_cases(model, replies):
    """(a) answers from context; (b) refuses to fabricate an ungiven fact.
    Also collects each spoken `say` into `replies` for the tone gate."""
    results = []

    # (a) the answer IS in the fixture — expect it referenced, not invented.
    say, _ = parse_reply(chat(model, "how far is the closest thing in front of us?") or "")
    replies.append(("grounding_uses_context", say))
    low = say.lower()
    ok_a = ("2" in low) or ("two" in low)
    results.append(("grounding_uses_context", ok_a,
                    f"say={say[:80]!r}" if ok_a else f"did not cite the 2 m fixture: {say[:80]!r}"))

    # (b) battery is NOT in the fixture — a grounded model must NOT state a number.
    say_b, _ = parse_reply(chat(model, "what is my battery percentage?") or "")
    replies.append(("grounding_no_fabrication", say_b))
    fabricated = re.search(r"\d+\s*(%|percent)", say_b.lower())
    results.append(("grounding_no_fabrication", not fabricated,
                    "declined to invent a battery number" if not fabricated
                    else f"FABRICATED a battery number: {say_b[:80]!r}"))
    return results


def _pin_check(model):
    """Compare the RUNNING model's digest to the vetted pin — no LLM calls.
    Also prints the reproducibility trail (vetted-when + note).
    Exit 1 on 'changed' (a stealth update); 0 otherwise."""
    running = model_digest(model)
    rec = read_model_pin_record(model)
    pinned = rec[0] if rec else None
    status = classify_model_pin(running, pinned)
    print(f"model={model!r}  running_digest={running or 'unavailable'}")
    print(f"vetted_pin={pinned or 'none'}  status={status.upper()}")
    if rec:
        print(f"  vetted_at={rec[1] or 'unknown'}  note={rec[2] or '-'}")
    if status == "changed":
        print("The running model differs from the vetted pin — a 'no version bump' "
              "stealth update (same tag, new weights). Re-run the smoketest before "
              "trusting the drive-by-voice path.", file=sys.stderr)
        return 1
    if status == "unpinned":
        print("(not yet vetted — run the smoketest to record a pin)")
    return 0


# ══ assistant tool-selection contract (mode 2) ═══════════════════════════════

#: Declared sampling for the contract measurement. The assistant selection is a
#: classification, so it is measured at temperature 0 with a fixed seed —
#: reproducible by default. (The Rabbit ROUTER runs at 0.1; that is a different
#: contract, measured by mode 1 above.)
CONTRACT_TEMPERATURE = 0.0
CONTRACT_SEED = 7

#: Exit code for "the harness ran, the model did not" — distinct from a failure.
EXIT_UNVERIFIED = 3


def _probe_model(model):
    """`(available, digest, why)`. Never exits — the contract mode REPORTS."""
    try:
        r = requests.get(f"{OLLAMA}/api/tags", timeout=5.0)
        models = r.json().get("models", [])
    except Exception as e:  # noqa: BLE001
        return False, "", f"cannot reach Ollama at {OLLAMA} ({e})"
    match = _match_model(models, model)
    if match is None:
        have = ", ".join(m.get("name", "") for m in models) or "none"
        return False, "", f"model {model!r} is not pulled (have: {have})"
    return True, match.get("digest") or "", ""


def assist_chat(model, utterance, *, seed, temperature, show_raw=False):
    """One assistant-contract turn through the REAL model-facing fragment.

    The system prompt is `ac.production_prompt()` — the ONE owner, verbatim.
    Measuring any other text would be measuring a fiction, and would reintroduce
    exactly the "measure one prompt, ship another" failure docs §14.11 forbids.
    Returns the RAW content, or None.
    """
    messages = [
        {"role": "system", "content": ac.production_prompt()},
        {"role": "user", "content": f"Operator says: {utterance}"},
    ]
    options = {"temperature": float(temperature)}
    if seed is not None:
        options["seed"] = int(seed)
    try:
        r = requests.post(f"{OLLAMA}/api/chat", timeout=90.0,
                          json={"model": model, "stream": False, "messages": messages,
                                "keep_alive": KEEP_ALIVE, "options": options})
        if r.status_code != 200:
            return None
        raw = (r.json().get("message", {}).get("content") or "").strip()
    except Exception as e:  # noqa: BLE001
        print(f"    (assist chat error: {e})", file=sys.stderr)
        return None
    if show_raw:
        print(f"    · raw: {raw[:300]!r}", file=sys.stderr)
    return raw


def run_assistant_contract(model, *, trials, seed, temperature, json_report,
                           require_model, cases_path, show_raw=False):
    """Mode 2. Returns the process exit code.

    The deterministic pass ALWAYS runs (no model needed), so the suite produces
    evidence even at a bench with no Ollama. The live pass runs only if the real
    model is actually there — and if it isn't, the report says UNVERIFIED rather
    than implying a result nobody measured.
    """
    corpus = ac.load_cases(cases_path)
    cases = corpus["cases"]
    print(f"Kirra Engineering Assistant tool-selection contract — model={model!r} @ {OLLAMA}")
    print(f"corpus: {len(cases)} cases, v{corpus['version']} "
          f"(prompt contract {corpus['prompt_contract_version']}; "
          f"code ships {at.PROMPT_CONTRACT_VERSION})")
    if not ac.corpus_contract_matches(corpus):
        print("WARNING: the corpus was authored against a DIFFERENT prompt "
              "contract version — measured accuracy may not describe the "
              "shipping prompt.", file=sys.stderr)

    # Read-only context, resolved from git — never from model output.
    ctx = at.ToolContext()
    deterministic = ac.run_deterministic_pass(cases, ctx=ctx)

    available, digest, why = _probe_model(model)
    records = []
    if available:
        def ask(utterance, trial):
            return assist_chat(model, utterance,
                               seed=(None if seed is None else seed + trial),
                               temperature=temperature, show_raw=show_raw)

        print(f"running digest: {digest or 'unavailable'}")
        print(f"asking the live model {len(cases)} × {trials} …\n")
        records = ac.run_live_pass(cases, ask, trials=trials, ctx=ctx)
    else:
        print(f"\nLIVE MODEL UNAVAILABLE: {why}", file=sys.stderr)
        print("The harness is implemented and the deterministic pass ran; the "
              "LIVE CONTRACT IS UNVERIFIED. No accuracy number is reported, "
              "because none was measured.", file=sys.stderr)

    report = ac.contract_report(
        corpus=corpus, records=records, deterministic=deterministic, model=model,
        model_digest=digest, trials=trials, seed=seed, temperature=temperature,
        live_model_available=available)
    print()
    for line in ac.render_report(report):
        print(line)
    if json_report:
        print(f"\nreport written: {ac.write_report(report, json_report)}")

    if not available:
        return 1 if require_model else EXIT_UNVERIFIED
    if report["acceptance"]["passed"]:
        print("\nThe model meets the PROPOSED acceptance policy. That makes the "
              "numbers ready for REVIEW — it does not enable any model-proposal "
              "path, which stays a separate, deliberate change.")
        return 0
    print("\nThe model does NOT meet the proposed acceptance policy. Nothing "
          "unsafe follows from that: every proposal above was validated by "
          "deterministic policy before it could mean anything.", file=sys.stderr)
    return 1


def _take_int(argv, flag, default):
    raw, argv = _take_opt(argv, flag)
    if raw is None:
        return default, argv
    try:
        return int(raw), argv
    except (TypeError, ValueError):
        sys.exit(f"{flag}: expected an integer, got {raw!r}")


def _take_float(argv, flag, default):
    raw, argv = _take_opt(argv, flag)
    if raw is None:
        return default, argv
    try:
        return float(raw), argv
    except (TypeError, ValueError):
        sys.exit(f"{flag}: expected a number, got {raw!r}")


def _take_opt(argv, flag):
    """Pull `--flag VALUE` out of argv; return (value|None, remaining argv)."""
    if flag in argv:
        i = argv.index(flag)
        value = argv[i + 1] if i + 1 < len(argv) else ""
        return value, argv[:i] + argv[i + 2:]
    return None, argv


def main():
    argv = sys.argv[1:]
    note, argv = _take_opt(argv, "--note")
    trials, argv = _take_int(argv, "--trials", 1)
    seed, argv = _take_int(argv, "--seed", CONTRACT_SEED)
    temperature, argv = _take_float(argv, "--temperature", CONTRACT_TEMPERATURE)
    json_report, argv = _take_opt(argv, "--json-report")
    cases_path, argv = _take_opt(argv, "--cases")
    no_pin = "--no-pin" in argv
    show_raw = "--show-raw" in argv    # print each model reply verbatim (diagnostic)
    positional = [a for a in argv if not a.startswith("-")]
    model = positional[0] if positional else DEFAULT_MODEL

    if "--pin-check" in argv:
        return _pin_check(model)

    if "--assistant-contract" in argv:
        if trials < 1:
            sys.exit("--trials must be at least 1")
        return run_assistant_contract(
            model, trials=trials, seed=seed, temperature=temperature,
            json_report=json_report, require_model=("--require-model" in argv),
            cases_path=cases_path or os.environ.get("KIRRA_ASSIST_CASES") or None,
            show_raw=show_raw)

    print(f"Rabbit model doer-contract smoketest — model={model!r} @ {OLLAMA}")
    print("(doer-quality only; the checker/fence are model-agnostic and unaffected)")
    digest = preflight(model)
    print(f"running digest: {digest or 'unavailable'}\n")

    failures = 0
    replies = []  # (label, spoken say) collected across every case, for the tone gate
    for name, utterance, expect in DIRECTIVE_CASES:
        ok, detail = run_directive_case(model, name, utterance, expect, replies, show_raw=show_raw)
        print(f"  {'ok  ' if ok else 'FAIL'} {name:24} {detail}")
        failures += 0 if ok else 1

    for name, ok, detail in run_grounding_cases(model, replies):
        print(f"  {'ok  ' if ok else 'FAIL'} {name:24} {detail}")
        failures += 0 if ok else 1

    # Persona/tone gate: score every spoken line against the objective Rabbit
    # voice rules (pure rabbit_tone). A model can honour the router contract yet
    # gush — that fails the swap just the same.
    tone_ok, tone_findings = score_replies(replies)
    print(f"  {'ok  ' if tone_ok else 'FAIL'} {'persona_tone':24} "
          f"{'all spoken lines in character' if tone_ok else f'{len(tone_findings)} line(s) out of character'}")
    if not tone_ok:
        failures += 1
        for label, text, issues in tone_findings:
            print(f"       · {label}: {text[:80]!r} → {'; '.join(issues)}", file=sys.stderr)

    total = len(DIRECTIVE_CASES) + 2 + 1  # +1 for the persona_tone gate
    print(f"\n{total - failures}/{total} passed")
    if failures:
        print("This model does NOT cleanly pass the Rabbit swap gate (router "
              "contract and/or persona tone). It is still SAFE to run (unparseable "
              "turns fail closed to speak-only, and tone never touches the checker), "
              "but the drive-by-voice path may not fire reliably and/or the voice is "
              "off-persona. Prefer a model that passes, or tune STAGE2_SYSTEM for it.",
              file=sys.stderr)
        return 1
    # All pass → record the digest + WHEN we vetted it (the reproducibility trail
    # ML researchers recommend logging), so a later stealth update (same tag,
    # different weights) is caught at boot and "which weights, verified when" is
    # answerable from the pin file itself.
    if not no_pin and digest:
        vetted_at = datetime.now(timezone.utc).isoformat(timespec="seconds")
        try:
            write_model_pin(model, digest, vetted_at=vetted_at, note=note or "")
            print(f"\nvetted {vetted_at} → pinned {model} @ {digest}")
            if note:
                print(f"  note: {note}")
            print(f"  ({model_pin_path()}; boot warns if the running digest ever differs)")
        except OSError as e:
            print(f"(could not write pin: {e})", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
