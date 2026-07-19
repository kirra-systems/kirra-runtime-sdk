#!/usr/bin/env python3
"""kitt_boot.py — KITT's power-on greeting and shutdown line. **SPEAK-ONLY.**

The boot greeting is deliberately NOT in kitt_watch.py (which stays silent on
boot by design — no false "recovered" chatter). It lives here as a one-shot the
systemd greet unit runs AFTER the stack is up, and it is HONEST: it only claims
"governor nominal" when it has actually read a FRESH, non-LockedOut posture from
the verifier. If the governor isn't ready within the deadline, it says so instead
of greeting into a lie.

  greet   → poll /metrics for a fresh non-LockedOut posture (up to a deadline),
            then speak the matching line (nominal / degraded / not-ready-yet).
  shutdown→ speak the power-down line.

🔴 CHANNEL A ONLY (docs/kitt/KITT_VOICE_LINES.md, rows A1–A3). Read-only GET +
   TTS. No /intent, no publisher, no serial, no release token — it cannot move
   the robot. Proactive SPEECH, never proactive motion.

Usage:
  ./robot/kitt_boot.py greet       # power-on greeting (posture-gated)
  ./robot/kitt_boot.py shutdown    # power-down line
Env: KIRRA_VERIFIER_URL (default http://localhost:8090), KIRRA_TTS_CMD,
     KIRRA_KITT_OPERATOR, KIRRA_KITT_GREET_DEADLINE_MS (default 20000).
"""
from __future__ import annotations

import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from kitt_persona import name_slot, speak  # noqa: E402

VERIFIER = os.environ.get("KIRRA_VERIFIER_URL", "http://localhost:8090").rstrip("/")
GREET_DEADLINE_S = int(os.environ.get("KIRRA_KITT_GREET_DEADLINE_MS", "20000")) / 1000.0

_FLEET_RE = re.compile(r"kirra_fleet_posture\{[^}]*\}\s+(\d+)")
_STALE_RE = re.compile(r"kirra_posture_cache_stale\{[^}]*\}\s+1\b")


def greeting_line(posture_code, fresh):
    """Pure: choose the boot line from a posture read.
    posture_code: 0 nominal / 1 degraded / 2 lockedout / None (no read).
    fresh: the read is fresh (posture present AND cache not stale)."""
    slot = name_slot()
    if fresh and posture_code == 0:
        return (f"Good morning{slot}. All systems online, governor nominal — "
                "I'm at your disposal.")
    if fresh and posture_code == 1:
        return (f"Good morning{slot}. I'm online, but starting in a degraded mode — "
                "I'll be cautious until it clears.")
    # LockedOut, no read, or stale → do NOT claim ready.
    return (f"I'm awake{slot}, but still checking myself over. "
            "Give me a moment before we go anywhere.")


def shutdown_line():
    return "Powering down. I've come to a safe stop — try not to miss me too much."


def _read_posture(timeout=2.0):
    """Worst-case posture code across nodes + freshness, from /metrics.
    Returns (code|None, fresh_bool). Lazy requests import so this module stays
    importable (and greeting_line unit-testable) without requests installed."""
    try:
        import requests
    except Exception:  # noqa: BLE001
        return None, False
    try:
        r = requests.get(f"{VERIFIER}/metrics", timeout=timeout)
        if r.status_code != 200:
            return None, False
        text = r.text
    except Exception:  # noqa: BLE001
        return None, False
    codes = [int(m) for m in _FLEET_RE.findall(text)]
    stale = bool(_STALE_RE.search(text))
    code = max(codes) if codes else None
    return code, (code is not None and not stale)


def greet(deadline_s=None, poll_s=1.0):
    """Poll until a fresh nominal posture (greet at once) or the deadline
    (greet with whatever the last honest read was)."""
    deadline_s = GREET_DEADLINE_S if deadline_s is None else deadline_s
    t0 = time.monotonic()
    last_code, last_fresh = None, False
    while True:
        code, fresh = _read_posture()
        if code is not None:  # keep the last KNOWN read; a transient outage ≠ ready
            last_code, last_fresh = code, fresh
        if fresh and code == 0:
            break
        if time.monotonic() - t0 >= deadline_s:
            break
        time.sleep(poll_s)
    speak(greeting_line(last_code, last_fresh))


def main():
    action = sys.argv[1] if len(sys.argv) > 1 else "greet"
    if action == "greet":
        greet()
    elif action == "shutdown":
        speak(shutdown_line())
    else:
        sys.exit("usage: kitt_boot.py [greet|shutdown]")


if __name__ == "__main__":
    main()
