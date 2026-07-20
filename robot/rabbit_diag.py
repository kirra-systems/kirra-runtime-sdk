#!/usr/bin/env python3
"""rabbit_diag — DETERMINISTIC voice-triggered diagnostics (Channel A).

"Rabbit, run diagnostics" / "run a self check" / "check yourself" → run the
read-only kirra_doctor default set → speak the SHORT summary (counts + at most
three plain issue sentences — never paths/details; the CLI/JSON has those).

Deterministic like rabbit_ota: a regex matcher, NO LLM inference — a diagnostics
request must never depend on a model's mood, and the LLM must never be able to
'decide' to run (or fake) a self-check. Matched utterances are handled BEFORE
the LLM/movement path in rabbit_converse.handle_turn.

🔴 Channel A only: kirra_doctor is read-only observability. No /intent, no
motion, no writes — and no diagnostic verdict gates the checker/governor/fence
(they fail closed on their own).
"""
import re

# Movement-safe by construction: every alternative requires a self-check noun/
# reflexive ("check yourself", "self test", "diagnostics") — none of these
# overlap the OTA matcher ("check for updates") or a drive utterance.
_DIAG_RE = re.compile(
    r"\b(run\s+(a\s+)?(self[\s-]*)?(check|test|diagnostics?)"
    r"|(self[\s-]*)(check|test|diagnostics?)"
    r"|check\s+yourself"
    r"|diagnose\s+yourself"
    r"|how\s+healthy\s+are\s+you)\b",
    re.IGNORECASE,
)


def matches(utterance):
    """Pure matcher (unit-tested separately from execution)."""
    return bool(_DIAG_RE.search(utterance or ""))


def run_and_summarize():
    """Run the default read-only module set; return the spoken summary."""
    try:
        from kirra_doctor import collect  # lazy: same dir (repo or /opt/kirra/robot)
        from doctor.core import speech_summary
        return speech_summary(collect())
    except Exception:  # noqa: BLE001 — the internal-error voice line, no traceback
        return ("I couldn't complete my self-check — the diagnostics runner hit "
                "an internal error. Try kirra_doctor from a terminal.")


def handle(utterance):
    """rabbit_ota.handle contract: the spoken reply, or None if not a
    diagnostics request (the turn falls through to the LLM router)."""
    if not matches(utterance):
        return None
    return run_and_summarize()
