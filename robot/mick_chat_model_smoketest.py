#!/usr/bin/env python3
"""mick_chat_model_smoketest.py — the CHAT doer-contract gate for a model swap.

The sibling of `rabbit_model_smoketest.py`, for the surface that gate never
covered. `KIRRA_RABBIT_MODEL` has had a smoketest and a digest pin for a while;
`KIRRA_MICK_CHAT_MODEL` had neither, so the conversational sidecar — the thing
an operator actually talks to — could be swapped with no vetting at all.

It matters more now that all three doer roles (chat, intent, Rabbit speech)
default to ONE model: a single stealth update to that tag lands on every one of
them at once.

WHAT IT DRIVES. The deployed sidecar at `--url` (default :8103) — not Ollama
directly. The contract being vetted is the SIDECAR'S: its deterministic fast
routes, its motion-shape guard and its sentence-safe termination are part of
what an operator experiences, so testing the model through anything else would
be testing a fiction. `GET /health` names the model that actually resolved,
which is also the only authoritative answer to "what is this thing running" —
the unit's `Environment=` line is a default that `/etc/kirra/kirra.env` may
override.

PACING (why `--delay-seconds` exists). The chat service is behind a rate
limiter that admits a short burst and then settles to roughly one request per
second. Nine back-to-back turns spend the burst on the first few cases and take
`429` for the rest, which the earlier version of this script reported as a
model-contract failure — a false accusation against a model that was never
asked. The fix belongs HERE, not in the limiter: a bench tool must fit the
production service, never the other way round. So every case is paced, and a
`429` is classified as INFRASTRUCTURE, never as model quality.

Waiting before launching the script does not help on its own — the burst
allowance is consumed by the script's own first few requests, so the spacing
has to be between cases.

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
bench. The pure judgements it delegates to ARE in CI, and so is this file's
pacing/classification logic (`mick_chat_model_smoketest_test.py`).

On a full PASS it records the model's Ollama digest + a vetted-at timestamp in
the SAME per-model pin file the Rabbit gate uses (`~/.kirra_rabbit_model.pin`,
override `KIRRA_RABBIT_MODEL_PIN_FILE`), so one file is the fleet's record of
which weights were verified when. The pin is a MAP keyed by model id, so
vetting the chat model never disturbs another role's entry.

Usage:
  python3 robot/mick_chat_model_smoketest.py              # vet whatever /health reports
  python3 robot/mick_chat_model_smoketest.py --delay-seconds 2.0
  python3 robot/mick_chat_model_smoketest.py --no-pin     # vet without recording
  python3 robot/mick_chat_model_smoketest.py --pin-check   # compare digest vs pin only
  python3 robot/mick_chat_model_smoketest.py --note "re-pull 2026-08-02"

Exit codes:
  0  PASS — the model honours the chat contract (and was pinned, unless
     --no-pin), or under --pin-check its running digest matches the vetted one.
  1  NEGATIVE VERDICT about this model. Either it was asked and answered badly
     (a contract failure), or --pin-check found its weights CHANGED under the
     same tag, or it is UNPINNED — never vetted. All three are answers about
     the model's standing, and all three are fixed by a human deciding
     something, not by retrying.
  2  NO VERDICT was produced. The invocation was wrong (argparse usage error),
     a dependency is missing, or the service could not be exercised:
     unreachable, rate limited, a transport/backend error, or Ollama down so
     no running digest exists. These share a code because they mean the same
     thing to a caller — nothing was learned about the model, and a retry may
     well succeed. They are never conflated in the OUTPUT, which always says
     which one happened.
"""
from __future__ import annotations

import argparse
import os
import sys
import time
from datetime import datetime, timezone

EXIT_OK = 0
EXIT_MODEL_CONTRACT = 1
EXIT_NO_VERDICT = 2

try:
    import requests
except ImportError:
    # EXIT_NO_VERDICT, not the bare sys.exit(<string>) that would exit 1: a
    # missing dependency means the model was never asked, which is the same
    # thing to a caller as an unreachable service. Exiting 1 here would put a
    # tooling problem in the same bucket as "this model answers badly" — the
    # exact conflation this script exists to avoid (review: Copilot on #1301).
    print("mick_chat_model_smoketest: python3-requests missing (pip3 install requests)",
          file=sys.stderr)
    sys.exit(EXIT_NO_VERDICT)

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mick_chat_contract import judge, names_a_vendor  # noqa: E402
from rabbit_persona import (  # noqa: E402
    classify_model_pin, model_pin_path, read_model_pin, read_model_pin_record,
    write_model_pin,
)
# The digest lookup is the Rabbit gate's — one definition of "which weights",
# shared, so the two gates can never disagree about a model's identity.
from rabbit_model_smoketest import model_digest  # noqa: E402

DEFAULT_URL = os.environ.get("KIRRA_MICK_CHAT_URL", "http://localhost:8103")

#: Seconds between cases. The service admits a short burst then settles to
#: ~1 request/second, so 1.0 is the floor and this leaves a small margin for
#: clock jitter rather than sitting exactly on the boundary.
DEFAULT_DELAY_SECONDS = 1.1
MAX_DELAY_SECONDS = 10.0

#: Per-request timeout. Generous: a cold model on an Orin can take tens of
#: seconds for the first token, and a timeout misreported as a model failure
#: is the same false accusation this script exists to avoid.
DEFAULT_TIMEOUT_SECONDS = 90
MIN_TIMEOUT_SECONDS = 1
MAX_TIMEOUT_SECONDS = 300

# Transport outcomes. RATE_LIMITED is split out from TRANSPORT_ERROR because
# it is the one an operator can FIX from the command line (--delay-seconds),
# and saying so is more useful than a generic failure.
OK = "ok"
RATE_LIMITED = "rate_limited"
TRANSPORT_ERROR = "transport_error"

#: Seam for tests — patched to record pacing without actually waiting.
_sleep = time.sleep

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


# ── argument parsing ─────────────────────────────────────────────────────────

def _delay_seconds(text):
    """A pacing delay in [0, MAX_DELAY_SECONDS]. 0 is allowed deliberately: a
    caller testing against a stub or a limiter-free build should be able to run
    at full speed without editing the script."""
    try:
        value = float(text)
    except (TypeError, ValueError):
        raise argparse.ArgumentTypeError(f"not a number: {text!r}")
    if value != value or value in (float("inf"), float("-inf")):
        raise argparse.ArgumentTypeError(f"must be finite, got {text!r}")
    if not 0.0 <= value <= MAX_DELAY_SECONDS:
        raise argparse.ArgumentTypeError(
            f"must be between 0.0 and {MAX_DELAY_SECONDS} seconds, got {value}")
    return value


def _timeout_seconds(text):
    try:
        value = int(text)
    except (TypeError, ValueError):
        raise argparse.ArgumentTypeError(f"not an integer: {text!r}")
    if not MIN_TIMEOUT_SECONDS <= value <= MAX_TIMEOUT_SECONDS:
        raise argparse.ArgumentTypeError(
            f"must be between {MIN_TIMEOUT_SECONDS} and {MAX_TIMEOUT_SECONDS} "
            f"seconds, got {value}")
    return value


def build_parser():
    parser = argparse.ArgumentParser(
        prog="mick_chat_model_smoketest.py",
        description=(
            "Vet the deployed chat sidecar's model against the chat doer "
            "contract, then pin its Ollama digest. Doer-QUALITY only: the chat "
            "surface has no motion authority, and the checker/fence are "
            "model-agnostic and unaffected."),
        epilog=(
            "Pacing: the chat service admits a short burst then settles to "
            "~1 request/second. The cases are spaced by --delay-seconds so a "
            "429 never gets misreported as a model failure. A 429 is an "
            "INFRASTRUCTURE result (exit 2), never a model-contract one.\n\n"
            "Env: KIRRA_MICK_CHAT_URL (default for --url), "
            "KIRRA_OLLAMA_URL, KIRRA_RABBIT_MODEL_PIN_FILE."),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--url", default=DEFAULT_URL, metavar="URL",
        help="chat sidecar base URL (default: %(default)s)")
    parser.add_argument(
        "--delay-seconds", type=_delay_seconds, default=DEFAULT_DELAY_SECONDS,
        metavar="S",
        help=f"seconds to wait BETWEEN cases, never before the first "
             f"(0.0-{MAX_DELAY_SECONDS}; default: %(default)s)")
    parser.add_argument(
        "--timeout-seconds", type=_timeout_seconds, default=DEFAULT_TIMEOUT_SECONDS,
        metavar="S",
        help=f"per-request timeout ({MIN_TIMEOUT_SECONDS}-{MAX_TIMEOUT_SECONDS}; "
             f"default: %(default)s)")
    parser.add_argument(
        "--no-pin", action="store_true",
        help="run the contract but record no pin")
    parser.add_argument(
        "--pin-check", action="store_true",
        help="ONLY compare the running digest against the pin (no model calls)")
    parser.add_argument(
        "--note", default="", metavar="TEXT",
        help="provenance note recorded alongside the pin")
    return parser


# ── transport ────────────────────────────────────────────────────────────────

def _status_detail(resp):
    """'HTTP 429 (Retry-After: 1)' — the status plus whatever the service told
    us about when to come back."""
    detail = f"HTTP {resp.status_code}"
    retry = (getattr(resp, "headers", None) or {}).get("Retry-After")
    return f"{detail} (Retry-After: {retry})" if retry else detail


def health(url, timeout):
    """(outcome, model, detail)."""
    try:
        resp = requests.get(f"{url}/health", timeout=timeout)
    except Exception as e:  # noqa: BLE001
        return TRANSPORT_ERROR, None, f"{type(e).__name__}: {e}"
    if resp.status_code == 429:
        return RATE_LIMITED, None, _status_detail(resp)
    if resp.status_code != 200:
        return TRANSPORT_ERROR, None, _status_detail(resp)
    try:
        body = resp.json()
    except Exception as e:  # noqa: BLE001
        return TRANSPORT_ERROR, None, f"unparseable /health body: {e}"
    return OK, body.get("model"), body.get("status")


def say(url, text, timeout):
    """One production-shaped chat turn → (outcome, reply, deterministic, detail).

    A non-200, a service-level `ok:false` (e.g. Ollama unreachable) and an
    exception are all TRANSPORT results: the model either was not asked or its
    answer never arrived, so none of them is evidence about model quality."""
    try:
        resp = requests.post(f"{url}/chat", timeout=timeout, json={"text": text})
    except Exception as e:  # noqa: BLE001
        return TRANSPORT_ERROR, None, None, f"{type(e).__name__}: {e}"
    if resp.status_code == 429:
        return RATE_LIMITED, None, None, _status_detail(resp)
    if resp.status_code != 200:
        return TRANSPORT_ERROR, None, None, _status_detail(resp)
    try:
        body = resp.json()
    except Exception as e:  # noqa: BLE001
        return TRANSPORT_ERROR, None, None, f"unparseable /chat body: {e}"
    if not body.get("ok"):
        return TRANSPORT_ERROR, None, None, f"service error: {body.get('error')}"
    return (OK, body.get("reply") or "",
            bool(body.get("timing", {}).get("deterministic")), None)


# ── the contract run ─────────────────────────────────────────────────────────

def evaluate(kind, reply, deterministic, expect_deterministic):
    """The per-case verdict: a failure REASON, or None on pass. Pure."""
    if expect_deterministic and not deterministic:
        return "answered by the MODEL — the deterministic route did not fire"
    if not expect_deterministic and deterministic:
        return "answered deterministically — a fixed route captured an ordinary question"
    if kind != "routing":
        why = judge(kind, reply)
        if why:
            return why
    # The identity boundary applies to EVERY reply, not only identity ones:
    # incidental drift in ordinary prose is the same failure.
    vendor = names_a_vendor(reply)
    if vendor:
        return f"names an external vendor ({vendor})"
    return None


def run_cases(url, delay_seconds, timeout_seconds):
    """Drive every case, paced. → (failures, infrastructure).

    `infrastructure` is (outcome, case_name, detail) or None. A transport
    result ABORTS the run: once the service stops answering honestly the
    remaining cases can only produce more of the same, and continuing would
    print a wall of failures that reads like a bad model."""
    failures = []
    for index, (name, utterance, kind, expect_deterministic) in enumerate(CASES):
        # Pace BETWEEN cases only. Sleeping before the first would delay every
        # run for nothing — the burst allowance is spent by our own requests,
        # not by whatever came before us.
        if index:
            _sleep(delay_seconds)

        outcome, reply, deterministic, detail = say(url, utterance, timeout_seconds)
        if outcome != OK:
            print(f"  ---- {name:<20} {detail}")
            return failures, (outcome, name, detail)

        why = evaluate(kind, reply, deterministic, expect_deterministic)
        if why:
            failures.append(f"{name}: {why}")
            print(f"  FAIL {name:<20} {why}")
            print(f"       reply={reply.strip()[:120]!r}")
        else:
            shown = reply.strip().replace("\n", " ")[:60]
            tag = "fixed" if deterministic else "model"
            print(f"  ok   {name:<20} [{tag}] {shown!r}")
    return failures, None


def report_infrastructure(outcome, name, detail, delay_seconds):
    """Say plainly that no model verdict was produced, and why."""
    if outcome == RATE_LIMITED:
        suggested = min(max(delay_seconds * 2, 2.0), MAX_DELAY_SECONDS)
        print("\nINFRASTRUCTURE FAILURE: chat service rate limited the smoketest",
              file=sys.stderr)
        print(f"  case: {name}    status: {detail}", file=sys.stderr)
        print("  This is NOT a model-contract failure — the model was never asked.",
              file=sys.stderr)
        print(f"  The service admits a short burst then ~1 request/second; this run",
              file=sys.stderr)
        print(f"  paced at {delay_seconds}s. Re-run with more room:", file=sys.stderr)
        print(f"    python3 robot/mick_chat_model_smoketest.py --delay-seconds {suggested}",
              file=sys.stderr)
    else:
        print("\nINFRASTRUCTURE FAILURE: chat service transport error",
              file=sys.stderr)
        print(f"  case: {name}    status: {detail}", file=sys.stderr)
        print("  This is NOT a model-contract failure — no answer reached us.",
              file=sys.stderr)
    print("  Nothing pinned.", file=sys.stderr)


def run_pin_check(model):
    """Compare the running digest against the vetted pin.

    `changed` and `unpinned` are NEGATIVE VERDICTS about the model (exit 1):
    each is a real answer about its standing, resolved by a human re-vetting,
    not by retrying. `unavailable` is NOT — it means Ollama gave no running
    digest, so there was nothing to compare and nothing was learned. Returning
    1 there would report an infrastructure outage as a judgement against the
    model, which is the conflation this whole script exists to avoid
    (review: Copilot on #1301)."""
    running = model_digest(model)
    pinned = read_model_pin(model)
    status = classify_model_pin(running, pinned)
    print(f"model={model!r}  running_digest={running}")
    print(f"vetted_pin={pinned}  status={status.upper()}")
    rec = read_model_pin_record(model)
    if rec:
        print(f"  vetted_at={rec[1] or '-'}  note={rec[2] or '-'}")
    if status == "ok":
        return EXIT_OK
    if status == "changed":
        print("\nDIGEST CHANGED — same tag, different weights. Re-vet before trusting it:",
              file=sys.stderr)
        print("  python3 robot/mick_chat_model_smoketest.py --note 'digest change'",
              file=sys.stderr)
        return EXIT_MODEL_CONTRACT
    if status == "unpinned":
        print("\nUNPINNED — this model has never been vetted for the chat contract.",
              file=sys.stderr)
        return EXIT_MODEL_CONTRACT
    # 'unavailable' — no running digest to compare against.
    print("\nNO RUNNING DIGEST — Ollama did not report one, so the pin could not be",
          file=sys.stderr)
    print("  checked. This is NOT a judgement about the model; nothing was compared.",
          file=sys.stderr)
    return EXIT_NO_VERDICT


def main(argv=None):
    args = build_parser().parse_args(argv)
    url = args.url.rstrip("/")

    outcome, model, status = health(url, args.timeout_seconds)
    if outcome != OK:
        if outcome == RATE_LIMITED:
            report_infrastructure(outcome, "health", status, args.delay_seconds)
        else:
            print(f"mick_chat_model_smoketest: chat sidecar unreachable at {url} "
                  f"({status})", file=sys.stderr)
            print("  UNVERIFIED, not failed — start the sidecar and re-run.",
                  file=sys.stderr)
        return EXIT_NO_VERDICT

    if args.pin_check:
        return run_pin_check(model)

    print(f"Mick CHAT doer-contract smoketest — model={model!r} @ {url} (health: {status})")
    print("(doer-quality only; the chat surface has no motion authority, and the")
    print(" checker/fence are model-agnostic and unaffected)")
    digest = model_digest(model)
    print(f"running digest: {digest or '<unavailable>'}")
    print(f"pacing: {args.delay_seconds}s between cases, {args.timeout_seconds}s timeout")

    failures, infrastructure = run_cases(url, args.delay_seconds, args.timeout_seconds)

    if infrastructure:
        report_infrastructure(*infrastructure, args.delay_seconds)
        return EXIT_NO_VERDICT

    print(f"\n{len(CASES) - len(failures)}/{len(CASES)} passed")
    if failures:
        print("\nThis model does NOT honour the chat contract. Not pinned.", file=sys.stderr)
        return EXIT_MODEL_CONTRACT

    if args.no_pin:
        print("(--no-pin: nothing recorded)")
        return EXIT_OK
    if not digest:
        print("PASSED, but Ollama gave no digest — nothing pinned.", file=sys.stderr)
        return EXIT_OK

    stamp = datetime.now(timezone.utc).astimezone().isoformat(timespec="seconds")
    write_model_pin(model, digest, vetted_at=stamp, note=args.note)
    print(f"vetted {stamp} → pinned {model} @ {digest}")
    print(f"  ({model_pin_path()}; --pin-check compares the running digest against this)")
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
