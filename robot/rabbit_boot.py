#!/usr/bin/env python3
"""rabbit_boot.py — Rabbit's power-on greeting and shutdown line. **SPEAK-ONLY.**

The boot greeting is deliberately NOT in rabbit_watch.py (which stays silent on
boot by design — no false "recovered" chatter). It lives here as a one-shot the
systemd greet unit runs AFTER the stack is up, and it is HONEST: it only claims
"governor nominal" when it has actually read a FRESH, non-LockedOut posture from
the verifier. If the governor isn't ready within the deadline, it says so instead
of greeting into a lie.

  greet   → poll /metrics for a fresh non-LockedOut posture (up to a deadline),
            then speak the matching line (nominal / degraded / not-ready-yet).
  shutdown→ speak the power-down line.

🔴 CHANNEL A ONLY (docs/rabbit/RABBIT_VOICE_LINES.md, rows A1–A3 + A5 model-drift +
   A6 misconfig self-check). Read-only GET + TTS + a read-only config doctor. No
   /intent, no publisher, no serial, no release token — it cannot move the robot.
   Proactive SPEECH, never proactive motion.

Usage:
  ./robot/rabbit_boot.py greet       # power-on greeting (posture-gated)
  ./robot/rabbit_boot.py shutdown    # power-down line
Env: KIRRA_VERIFIER_URL (default http://localhost:8090), KIRRA_TTS_CMD,
     KIRRA_RABBIT_OPERATOR, KIRRA_RABBIT_GREET_DEADLINE_MS (default 20000).
"""
from __future__ import annotations

import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from rabbit_persona import name_slot, speak  # noqa: E402

VERIFIER = os.environ.get("KIRRA_VERIFIER_URL", "http://localhost:8090").rstrip("/")
GREET_DEADLINE_S = int(os.environ.get("KIRRA_RABBIT_GREET_DEADLINE_MS", "20000")) / 1000.0

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


def misconfig_line(top_issue=None):
    """A6: the boot self-check found a FAIL (a drifted device, a missing engine,
    a broken prerequisite). Speaks at most ONE issue; the full report is
    kirra_doctor from a terminal. Back-compatible: no-arg = generic advisory."""
    base = f"A heads-up{name_slot()}: my self-check found a configuration problem."
    tail = top_issue if top_issue else \
        "Run the voice doctor before relying on me — my ears or voice may be off."
    return f"{base} {tail} Run kirra doctor for the full report." if top_issue else f"{base} {tail}"


def _maybe_warn_misconfigured():
    """Channel-A advisory from the boot self-check. Prefers the full kirra_doctor
    framework (default read-only module set): logs the summary to stderr every
    boot, but SPEAKS only on a FAIL (voice line A6 + the top issue) — WARNs are
    common (staged-not-enabled services, NTP) and must not become boot nag.
    Falls back to the standalone kirra_voice_doctor.sh if the framework isn't
    staged. Best-effort: a self-check advisory must never break boot."""
    here = os.path.dirname(os.path.abspath(__file__))
    try:
        from kirra_doctor import collect  # lazy; same dir (repo or /opt/kirra/robot)
        from doctor.core import issues, speech_summary
        report = collect()
        print(f"rabbit_boot self-check: {speech_summary(report)}", file=sys.stderr)
        if report["status"] == "FAIL":
            top = issues(report)[0]
            speak(misconfig_line(f"The {top[0]['name']} module has a problem"
                                 + (f": {top[1]['check']}." if top[1] else ".")))
        return
    except Exception:  # noqa: BLE001 — fall back to the shell voice doctor
        pass
    import subprocess
    doctor = os.path.join(here, "kirra_voice_doctor.sh")
    if not os.path.exists(doctor):
        return
    try:
        rc = subprocess.run(["bash", doctor, "--quiet"], timeout=15,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode
    except Exception:  # noqa: BLE001
        return
    if rc != 0:
        speak(misconfig_line())


def _maybe_warn_model_changed():
    """Channel-A advisory if the running LLM's digest differs from the vetted pin
    — a 'no version bump' stealth update (same tag, new weights). Best-effort and
    lazy (needs requests + a live Ollama on the robot); never breaks boot, and
    only speaks on a CONFIRMED change (unpinned/unavailable stay silent — no nag)."""
    try:
        from rabbit_model_smoketest import pin_status  # lazy: pulls requests
        status, _running, _pinned = pin_status()
    except Exception:  # noqa: BLE001 — a model-pin advisory must never break boot
        return
    if status == "changed":
        speak(f"A heads-up{name_slot()}: my language model has changed since it was "
              "last vetted. I'd re-run the model check before trusting me to drive.")


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
    _maybe_warn_model_changed()
    _maybe_warn_misconfigured()


def main():
    action = sys.argv[1] if len(sys.argv) > 1 else "greet"
    if action == "greet":
        greet()
    elif action == "shutdown":
        speak(shutdown_line())
    else:
        sys.exit("usage: rabbit_boot.py [greet|shutdown]")


if __name__ == "__main__":
    main()
