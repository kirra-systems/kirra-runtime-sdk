"""autostart — wraps preflight_autostart.sh. OPT-IN (DEFAULT=False): the
preflight uses plain `sudo cat` reads that can BLOCK on a password prompt when
run non-interactively, so it must not sit in the default (boot/voice/timer)
path. Run it deliberately: kirra_doctor --module autostart (from a terminal).
"""
import os

from doctor.core import detail, run_cmd

NAME = "autostart"
DESCRIPTION = "autostart readiness (wraps preflight_autostart.sh; needs sudo)"
DEFAULT, HEAVY, TIMEOUT_S = False, True, 60


def run(ctx):
    script = os.path.join(ctx["repo"], "robot", "install", "preflight_autostart.sh")
    if not os.path.isfile(script):
        return {"details": [detail("preflight_autostart.sh", "UNKNOWN",
                                   "not found (running outside the repo checkout?)")]}
    rc, out, err = run_cmd(["bash", script], timeout_s=55)
    tail = " | ".join((out.strip().splitlines() or [err.strip()])[-3:])
    if rc == -1:
        return {"details": [detail("preflight", "UNKNOWN", tail or "timed out (sudo prompt?)",
                                   fix="run it in a terminal: robot/install/preflight_autostart.sh")]}
    return {"details": [detail("preflight", "PASS" if rc == 0 else "FAIL", tail[:200],
                               fix=None if rc == 0 else "run it directly for the per-check fixes")]}
