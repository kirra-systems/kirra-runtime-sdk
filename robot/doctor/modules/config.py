"""config — robot.env presence, structure, and lint (composes lint_robot_env.sh).

The verifier's own config self-audit (env_config.rs unknown-var sweep + the
EffectiveConfig digest in the audit chain) runs verifier-side at ITS boot; this
module covers the robot-side env file the doer scripts source.
"""
import os

from doctor.core import detail, run_cmd

NAME = "config"
DESCRIPTION = "robot.env present, well-formed, and lint-clean"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 20


def run(ctx):
    details = []
    path = ctx["robot_env_path"]
    if os.path.isfile(path) and os.access(path, os.R_OK):
        details.append(detail("robot.env readable", "PASS", path))
    else:
        details.append(detail("robot.env readable", "FAIL", path,
                              fix="create it — docs/hardware/R2_VOICE_AUDIO_SETUP.md §4"))
        return {"details": details}

    env = ctx["robot_env"]
    # MODE-AWARE. KIRRA_EXPECTED_CAR_TYPE is an X3-ONLY guard (the consumer
    # reads it only on the X3 path); warning about it in r2_ackermann told the
    # operator to set a variable that mode ignores. Requirements come from the
    # one shared contract so the doctor, preflight and installer cannot drift.
    import sys as _sys
    _sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__)))))
    try:
        import consumer_config_contract as contract
        missing = contract.missing_keys(env)
        mode = contract.drive_mode(env)
        details.append(detail("drive mode", "PASS", mode))
        for key in contract.required_keys(env):
            if env.get(key):
                details.append(detail(f"{key} set", "PASS", env[key]))
        if missing:
            details.append(detail(
                "required actuation config", "FAIL",
                f"{len(missing)} missing for mode {mode}: " + ", ".join(missing),
                fix="robot/install/preflight_consumer_env.py lists the full set "
                    "in one run — see robot/install/env.template"))
        else:
            details.append(detail("required actuation config", "PASS",
                                  f"complete for mode {mode}"))
        for key in contract.inapplicable_keys(env):
            if env.get(key):
                details.append(detail(f"{key} set", "WARN",
                                      f"set but not used in mode {mode}",
                                      fix=f"remove it, or switch modes"))
    except Exception as e:  # noqa: BLE001 — never crash the doctor
        details.append(detail("required actuation config", "UNKNOWN",
                              f"contract unavailable: {e}"))
    placeholders = [k for k, v in env.items() if "__FILLED" in v]
    if placeholders:
        details.append(detail("placeholder values", "FAIL", ", ".join(placeholders),
                              fix="fill the __FILLED_...__ values (installer/enrollment)"))
    else:
        details.append(detail("placeholder values", "PASS", "none"))

    lint = os.path.join(ctx["repo"], "robot", "install", "lint_robot_env.sh")
    if os.path.isfile(lint):
        rc, out, err = run_cmd(["bash", lint], timeout_s=15)
        last = (out.strip().splitlines() or [err.strip() or "no output"])[-1]
        details.append(detail("lint_robot_env.sh", "PASS" if rc == 0 else "FAIL",
                              last[:160], fix=None if rc == 0 else "run it directly for the full report"))
    else:
        details.append(detail("lint_robot_env.sh", "UNKNOWN",
                              "script not found (running outside the repo checkout?)"))
    return {"details": details}
