#!/usr/bin/env python3
"""kitt_watch.py — Stage 3 KITT: proactive event speech. **SPEAK-ONLY.**

KITT talks to YOU, unprompted — "I'm holding a safe stop," "all systems nominal,"
"I've had to refuse that." A read-only watcher that polls two signals, detects
state TRANSITIONS, and speaks in persona on a change (never on steady state).

  poll (read-only):                        announce on TRANSITION only:
    GET /metrics  kirra_fleet_posture ─►   Nominal↔Degraded↔LockedOut, cache-stale
    GET /narration/last (the #893 relay) ─► a NEW checker DENY (with its reason)

🔴 CHANNEL A ONLY (docs/hardware/KITT_CONVERSATION_DESIGN.md). This process makes
   read-only GETs and prints/speaks text — NO /intent POST, NO publisher, NO
   serial, NO release token. It observes and narrates; it cannot move the robot.
   Proactive SPEECH, never proactive MOTION.

Discipline: announces ONLY on a state change (steady state is silent), establishes
a baseline on the first poll WITHOUT speaking (no boot-time chatter), and rate-
limits so a flap can't spam. If a signal is unreachable it stays silent for that
signal (never false-announces "recovered" from missing data).

Usage:
  ./robot/kitt_watch.py                 # run alongside the loop; speaks on events
Env: KIRRA_VERIFIER_URL / KIRRA_MICK_URL / KIRRA_TTS_CMD (inherited from kitt_ask);
     KIRRA_KITT_WATCH_MS  poll interval (default 1500)
     KIRRA_KITT_WATCH_MIN_GAP_MS  min ms between spoken lines (default 2500)
"""
import os
import re
import sys
import time

import requests

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from kitt_ask import MICK, VERIFIER, speak  # noqa: E402

POLL_S = int(os.environ.get("KIRRA_KITT_WATCH_MS", "1500")) / 1000.0
MIN_GAP_S = int(os.environ.get("KIRRA_KITT_WATCH_MIN_GAP_MS", "2500")) / 1000.0
POSTURE_NAME = {0: "nominal", 1: "degraded", 2: "locked out"}

_FLEET_RE = re.compile(r"kirra_fleet_posture\{[^}]*\}\s+(\d+)")
_STALE_RE = re.compile(r"kirra_posture_cache_stale\{[^}]*\}\s+1\b")


def poll_posture():
    """Worst-case posture code across nodes + cache-stale, from /metrics.
    Returns (code|None, stale_bool). code None = no posture data (stay silent)."""
    try:
        r = requests.get(f"{VERIFIER}/metrics", timeout=2.0)
        if r.status_code != 200:
            return None, False
        text = r.text
    except Exception:  # noqa: BLE001
        return None, False
    codes = [int(m) for m in _FLEET_RE.findall(text)]
    stale = bool(_STALE_RE.search(text))
    return (max(codes) if codes else None), stale


def poll_verdict():
    """The checker's last verdict tuple from the #893 relay, or None if
    unavailable. ('none',) = relay up but nothing judged yet."""
    try:
        r = requests.get(f"{MICK}/narration/last", timeout=2.0)
        if r.status_code != 200:
            return None
        j = r.json()
    except Exception:  # noqa: BLE001
        return None
    if not isinstance(j, dict) or "last" not in j:
        return None
    last = j["last"]
    if not isinstance(last, dict):
        return ("none",)
    return ("verdict", last.get("action"), last.get("deny_code"), last.get("explanation"))


def posture_line(old, new, stale_now, stale_before):
    if stale_now and not stale_before:
        return "I've lost a fresh read on my safety state — I'm holding until it clears."
    if old is None or new is None or new == old:
        return None
    if new > old:  # escalation
        if new == 2:
            return ("I'm locking out and holding a safe stop. "
                    "I'll need a manual reset before we continue.")
        return ("Heads up — I've dropped into a degraded mode. "
                "I'll slow and stop as needed to keep us safe.")
    # recovery
    if new == 0:
        return "All systems nominal. We're clear to move again."
    return "Recovering — back to a degraded mode."


def verdict_line(v):
    # v = ('verdict', action, deny_code, explanation)
    _, _action, code, expl = v
    if not code:
        return None  # not a deny → not noteworthy
    if expl:
        return f"I've had to refuse a command. {expl}"
    return f"I've had to refuse a command ({code})."


def main():
    print("kitt_watch: proactive event speech — announces on change (Ctrl-C quits).",
          file=sys.stderr)
    # baseline WITHOUT speaking
    posture, stale = poll_posture()
    verdict = poll_verdict()
    last_spoke = 0.0

    def announce(line):
        nonlocal last_spoke
        if not line:
            return
        now = time.monotonic()
        if now - last_spoke < MIN_GAP_S:
            return  # coalesce flaps
        last_spoke = now
        speak(line)

    while True:
        time.sleep(POLL_S)
        new_posture, new_stale = poll_posture()
        announce(posture_line(posture, new_posture, new_stale, stale))
        # only overwrite known state (don't lose baseline on a transient outage)
        if new_posture is not None:
            posture = new_posture
        stale = new_stale

        new_verdict = poll_verdict()
        if (new_verdict is not None and new_verdict != verdict
                and new_verdict[0] == "verdict"):
            announce(verdict_line(new_verdict))
        if new_verdict is not None:
            verdict = new_verdict


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\nkitt_watch: stopped", file=sys.stderr)
