"""cold_boot — wraps cold_boot_drill.sh. OPT-IN (DEFAULT=False): it sources ROS
and inspects live topics, which is slow and environment-dependent — it is the
POST-boot acceptance drill, not a background health check. Run deliberately:
kirra_doctor --module cold_boot (with the stack up).
"""
import os

from doctor.core import detail, run_cmd

NAME = "cold_boot"
DESCRIPTION = "post-boot acceptance (wraps cold_boot_drill.sh; slow, needs ROS)"
DEFAULT, HEAVY, TIMEOUT_S = False, True, 120


def run(ctx):
    script = os.path.join(ctx["repo"], "robot", "cold_boot_drill.sh")
    if not os.path.isfile(script):
        return {"details": [detail("cold_boot_drill.sh", "UNKNOWN",
                                   "not found (running outside the repo checkout?)")]}
    rc, out, err = run_cmd(["bash", script], timeout_s=115)
    tail = " | ".join((out.strip().splitlines() or [err.strip()])[-3:])
    if rc == -1:
        return {"details": [detail("drill", "UNKNOWN", tail or "timed out",
                                   fix="run it directly with the stack up")]}
    return {"details": [detail("drill", "PASS" if rc == 0 else "FAIL", tail[:200],
                               fix=None if rc == 0 else "run robot/cold_boot_drill.sh for details")]}
