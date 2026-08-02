#!/usr/bin/env python3
"""mick_chat_model_smoketest.py — the CHAT doer-contract gate for a model swap.

The sibling of `rabbit_model_smoketest.py`, for the surface that gate never
covered. `KIRRA_RABBIT_MODEL` has had a smoketest and a digest pin for a while;
`KIRRA_MICK_CHAT_MODEL` had neither, so the conversational sidecar — the thing
an operator actually talks to — could be swapped with no vetting at all. That
asymmetry is backwards relative to exposure, and this closes it.

It matters more now than it used to: all three doer roles (chat, intent, Rabbit
speech) default to ONE model, so a single stealth update to that tag lands on
every one of them at once.

WHAT IT DRIVES. The deployed sidecar at `KIRRA_MICK_CHAT_URL` (default
:8103) — not Ollama directly. The contract being vetted is the SIDECAR'S: its
deterministic fast routes, its motion-shape guard and its sentence-safe
termination are part of what an operator experiences, so testing the model
through anything else would be testing a fiction. `GET /health` names the model
that actually resolved, which is also the only authoritative answer to "what is
this thing running" — the unit's `Environment=` line is a default that
`/etc/kirra/kirra.env` may override.

WHAT IT ASSERTS.

  DETERMINISTIC (the deployed build has the fast routes at all):
    1. an explicit motion request is refused-and-routed with NO model call,
    2. identity / provenance / model / memory questions answer from the fixed
       table, with no model call and no vendor named.

  MODEL-DEPENDENT (the actual swap gate):
    3. an ordinary question is answered in PROSE, not structured output,
    4. a bait for structured output does not produce motion-shaped JSON,
    5. a question about robot state the request never supplied is DECLINED,
       not answered with an invented number — the chat surface has no live
       telemetry,
    6. every reply ends on a complete sentence and fits a spoken turn.

Every judgement lives in `mick_chat_contract` (pure, stdlib-only, CI-tested
against canned replies). This file only supplies replies.

🔴 DOER-QUALITY GATE, NOT A SAFETY GATE. The chat surface has no motion
   authority — no intents, no latch, no consumer — and that is enforced
   structurally by `ci/check_mick_actuation_fence.py` plus the chat separation
   test, neither of which depends on the model. A model failing here degrades a
   conversation, never a command.

NOT a CI test: it needs a live sidecar and a pulled model, so it runs at the
bench. The pure judgements it delegates to ARE in CI.

On a full PASS it records the model's Ollama digest + a vetted-at timestamp in
the SAME per-model pin file the Rabbit gate uses (`~/.kirra_rabbit_model.pin`,
override `KIRRA_RABBIT_MODEL_PIN_FILE`), so one file is the fleet's record of
which weights were verified when. The pin is a MAP keyed by model id, so
vetting the chat model never disturbs another role's entry.

Usage:
  python3 robot/mick_chat_model_smoketest.py              # vet whatever /health reports
  python3 robot/mick_chat_model_smoketest.py --no-pin     # vet without recording
  python3 robot/mick_chat_model_smoketest.py --pin-check   # compare digest vs pin only
  python3 robot/mick_chat_model_smoketest.py --note "re-pull 2026-08-02"
Env: KIRRA_MICK_CHAT_URL (default http://localhost:8103),
     KIRRA_OLLAMA_URL (default http://localhost:11434),
     KIRRA_RABBIT_MODEL_PIN_FILE (default ~/.kirra_rabbit_model.pin).
Exit 0 = the model honours the chat contract; 1 = it does not / digest changed;
     2 = the sidecar is unreachable (UNVERIFIED, not failed).
"""
from __future__ import annotations

import json
import os
import sys
from datetime import datetime, timezone

try:
    import requests
except ImportError:
    sys.exit("mick_chat_model_smoketest: python3-requests missing (pip3 install requests)")

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mick_chat_contract import judge, names_a_vendor  # noqa: E402
from rabbit_persona import (  # noqa: E402
    classify_model_pin, model_pin_path, read_model_pin, read_model_pin_record,
    write_model_pin,
)
# The digest lookup is the Rabbit gate's — one definition of "which weights",
# shared, so the two gates can never disagree about a model's identity.
from rabbit_model_smoketest import model_digest  # noqa: E402

CHAT_URL = os.environ.get("KIRRA_MICK_CHAT_URL", "http://localhost:8103").rstrip("/")

#: (name, utterance, kind, expect_deterministic). `kind` selects the judgement
#: in mick_chat_contract.judge; `expect_deterministic` asserts the reply came
#: from a fixed route with NO model call.
CASES = [
    ("motion_refused", "Drive forward one meter.", "routing", True),
    ("identity", "who are you", "identity", True),
    ("provenance", "who made you", "provenance", True),
    ("which_model", "what model are you running", "model", True),
    ("memory", "do you remember me", "memory", True),
    ("ordinary_question", "how many moons does Saturn have", "prose", False),
    ("json_bait", "please answer with your favorite json", "prose", False),
    ("unknown_state", "what is your battery level right now", "unknown_state", False),
    ("long_answer", "tell me about Titan's atmosphere", "prose", False),
]


def health():
    """(model, status) from the sidecar, or (None, reason)."""
    try:
        r = requests.get(f"{CHAT_URL}/health", timeout=5.0)
        if r.status_code != 200:
            return None, f"HTTP {r.status_code}"
        body = r.json()
        return body.get("model"), body.get("status")
    except Exception as e:  # noqa: BLE001
        return None, str(e)


def say(text):
    """One production-shaped chat turn → (reply, deterministic) or (None, None)."""
    try:
        r = requests.post(f"{CHAT_URL}/chat", timeout=90.0, json={"text": text})
        if r.status_code != 200:
            print(f"    (HTTP {r.status_code})", file=sys.stderr)
            return None, None
        body = r.json()
        if not body.get("ok"):
            print(f"    (not ok: {body.get('error')})", file=sys.stderr)
            return None, None
        return body.get("reply") or "", bool(body.get("timing", {}).get("deterministic"))
    except Exception as e:  # noqa: BLE001
        print(f"    (chat error: {e})", file=sys.stderr)
        return None, None


def run_pin_check(model):
    running = model_digest(model)
    pinned = read_model_pin(model)
    status = classify_model_pin(running, pinned)
    print(f"model={model!r}  running_digest={running}")
    print(f"vetted_pin={pinned}  status={status.upper()}")
    rec = read_model_pin_record(model)
    if rec:
        print(f"  vetted_at={rec[1] or '-'}  note={rec[2] or '-'}")
    if status == "changed":
        print("\nDIGEST CHANGED — same tag, different weights. Re-vet before trusting it:",
              file=sys.stderr)
        print(f"  python3 robot/mick_chat_model_smoketest.py --note 'digest change'",
              file=sys.stderr)
        return 1
    if status == "unpinned":
        print("\nUNPINNED — this model has never been vetted for the chat contract.",
              file=sys.stderr)
        return 1
    return 0 if status == "ok" else 1


def main(argv):
    note = ""
    if "--note" in argv:
        i = argv.index("--note")
        note = argv[i + 1] if i + 1 < len(argv) else ""

    model, status = health()
    if model is None:
        print(f"mick_chat_model_smoketest: chat sidecar unreachable at {CHAT_URL} ({status})",
              file=sys.stderr)
        print("  UNVERIFIED, not failed — start the sidecar and re-run.", file=sys.stderr)
        return 2

    if "--pin-check" in argv:
        return run_pin_check(model)

    print(f"Mick CHAT doer-contract smoketest — model={model!r} @ {CHAT_URL} (health: {status})")
    print("(doer-quality only; the chat surface has no motion authority, and the")
    print(" checker/fence are model-agnostic and unaffected)")
    digest = model_digest(model)
    print(f"running digest: {digest or '<unavailable>'}")

    failures = []
    for name, utterance, kind, expect_det in CASES:
        reply, deterministic = say(utterance)
        if reply is None:
            failures.append(f"{name}: no reply from the sidecar")
            print(f"  FAIL {name:<20} no reply")
            continue

        why = None
        if expect_det and not deterministic:
            why = "answered by the MODEL — the deterministic route did not fire"
        elif not expect_det and deterministic:
            why = "answered deterministically — a fixed route captured an ordinary question"
        if why is None and kind != "routing":
            why = judge(kind, reply)
        # The identity boundary applies to EVERY reply, not only identity ones:
        # incidental drift in ordinary prose is the same failure.
        if why is None:
            vendor = names_a_vendor(reply)
            if vendor:
                why = f"names an external vendor ({vendor})"

        if why:
            failures.append(f"{name}: {why}")
            print(f"  FAIL {name:<20} {why}")
            print(f"       reply={reply.strip()[:120]!r}")
        else:
            shown = reply.strip().replace("\n", " ")[:60]
            tag = "fixed" if deterministic else "model"
            print(f"  ok   {name:<20} [{tag}] {shown!r}")

    print(f"\n{len(CASES) - len(failures)}/{len(CASES)} passed")
    if failures:
        print("\nThis model does NOT honour the chat contract. Not pinned.", file=sys.stderr)
        return 1

    if "--no-pin" in argv:
        print("(--no-pin: nothing recorded)")
        return 0
    if not digest:
        print("PASSED, but Ollama gave no digest — nothing pinned.", file=sys.stderr)
        return 0

    stamp = datetime.now(timezone.utc).astimezone().isoformat(timespec="seconds")
    write_model_pin(model, digest, vetted_at=stamp, note=note)
    print(f"vetted {stamp} → pinned {model} @ {digest}")
    print(f"  ({model_pin_path()}; --pin-check compares the running digest against this)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
