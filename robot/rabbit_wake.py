#!/usr/bin/env python3
"""rabbit_wake.py — DETERMINISTIC wake-listener controls (Channel A).

"Rabbit, go to sleep" / "stop listening" / "start listening" → write the wake
control file `robot/wake_word.py` polls, and speak a confirmation. Deterministic
like rabbit_ota / rabbit_diag: a regex matcher, NO LLM inference — whether the
robot's ambient microphone is open must never depend on a model's mood, and the
LLM must never be able to 'decide' to reopen it. Matched BEFORE the LLM/movement
path in rabbit_converse.handle_turn.

🔴 Channel A only: this writes ONE local state file. No /intent, no motion, no
authority — and note the asymmetry: MUTING always works via any trigger (PTT or
wake), but a muted wake listener cannot hear "start listening" (its mic is
closed) — resuming needs the PTT button / Enter, or the nap timer expiring.
The confirmations say so (voice lines W2–W4).

State file (KIRRA_WAKE_STATE_FILE, default /tmp/kirra_rabbit_wake.state):
  {"mode": "awake" | "nap" | "mute", "until_ms": N, "set_at_ms": N}
wake_word.py fails OPEN on an absent/corrupt file (listening) — safe because a
wake trigger carries no actuation authority; it dead-ends at the fence.
"""
from __future__ import annotations

import json
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from rabbit_persona import name_slot  # noqa: E402 — stdlib-only helper

STATE_FILE = os.environ.get("KIRRA_WAKE_STATE_FILE", "/tmp/kirra_rabbit_wake.state")
NAP_MIN = float(os.environ.get("KIRRA_WAKE_NAP_MIN", "30"))

# Movement-safe by construction: every alternative requires a listening/sleep
# noun or an explicit ear reference — none overlaps the OTA matcher ("check for
# updates"), the diagnostics matcher ("check yourself"), or a drive utterance
# ("stop" alone is NOT matched; it must be "stop listening").
_NAP_RE = re.compile(
    r"\b(go\s+to\s+sleep|take\s+a\s+nap|sleep\s+for\s+a\s+(while|bit))\b",
    re.IGNORECASE,
)
_MUTE_RE = re.compile(
    r"\b(stop\s+listening|mute\s+(your(self)?\s+)?(ears?|mic(rophone)?)"
    r"|turn\s+off\s+(the\s+)?(wake\s+word|listening))\b",
    re.IGNORECASE,
)
_RESUME_RE = re.compile(
    r"\b(start\s+listening|listen\s+again|wake\s+word\s+on"
    r"|unmute\s+(your(self)?\s+)?(ears?|mic(rophone)?))\b",
    re.IGNORECASE,
)


def classify(utterance: str | None) -> str | None:
    """Pure matcher: 'nap' | 'mute' | 'resume' | None. Mute wins over nap when
    both appear (the more conservative state)."""
    u = utterance or ""
    if _MUTE_RE.search(u):
        return "mute"
    if _NAP_RE.search(u):
        return "nap"
    if _RESUME_RE.search(u):
        return "resume"
    return None


def state_for(action: str, now_ms: int, nap_min: float = NAP_MIN) -> dict:
    """Pure state builder for a classified action."""
    if action == "nap":
        return {"mode": "nap", "until_ms": now_ms + int(nap_min * 60_000),
                "set_at_ms": now_ms}
    if action == "mute":
        return {"mode": "mute", "until_ms": 0, "set_at_ms": now_ms}
    return {"mode": "awake", "until_ms": 0, "set_at_ms": now_ms}


def reply_for(action: str, nap_min: float = NAP_MIN) -> str:
    """The spoken confirmation (voice lines W2–W4)."""
    if action == "nap":
        mins = int(nap_min)
        return (f"Going quiet{name_slot()} — I'll stop listening for about "
                f"{mins} minutes. The button still works if you need me.")
    if action == "mute":
        return (f"Ears off{name_slot()}. I won't listen for my name until you "
                "press the button and ask me to start listening again.")
    return f"I'm listening again{name_slot()}."


def _write_state(state: dict) -> bool:
    try:
        tmp = STATE_FILE + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(state, f)
        os.replace(tmp, STATE_FILE)
        return True
    except OSError:
        return False


def handle(utterance: str | None) -> str | None:
    """rabbit_ota.handle contract: the spoken reply, or None if not a wake
    control (the turn falls through to the next matcher / the LLM router)."""
    action = classify(utterance)
    if action is None:
        return None
    if not _write_state(state_for(action, int(time.time() * 1000))):
        return ("I couldn't reach my wake control — the listening state is "
                "unchanged. Check the wake state file from a terminal.")
    return reply_for(action)


if __name__ == "__main__":  # manual poke: python3 rabbit_wake.py "go to sleep"
    print(handle(" ".join(sys.argv[1:])) or "(not a wake control)")
