"""snapshot — is there a RECENT per-robot config capture (the reflash-recovery
lifeline)? capture_from_robot.sh WRITES a snapshot, so this module does not run
it (diagnostics are read-only) — it reports presence + age and tells the
operator to refresh when stale.
"""
import os
import socket
import time

from doctor.core import detail

NAME = "snapshot"
DESCRIPTION = "per-robot config capture present + fresh (read-only status)"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 5

STALE_DAYS = 90


def run(ctx):
    cap = os.path.join(ctx["repo"], "robot", "install", "captured", socket.gethostname())
    if not os.path.isdir(cap):
        return {"details": [detail("capture dir", "WARN", f"none at {cap}",
                                   fix="robot/install/capture_from_robot.sh (makes REFLASH.md real)")]}
    try:
        newest = max(os.path.getmtime(os.path.join(r, f))
                     for r, _, fs in os.walk(cap) for f in fs)
        age_d = (time.time() - newest) / 86400
        status = "WARN" if age_d > STALE_DAYS else "PASS"
        return {"details": [detail("capture age", status, f"{age_d:.0f} days old",
                                   fix=None if status == "PASS"
                                   else "re-run capture_from_robot.sh after config changes")]}
    except ValueError:
        return {"details": [detail("capture dir", "WARN", "present but empty",
                                   fix="robot/install/capture_from_robot.sh")]}
