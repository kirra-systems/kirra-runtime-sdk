#!/usr/bin/env python3
"""env_migration_report.py — what does THIS robot.env need, without touching it?

`/etc/kirra/robot.env` is hand-tuned per robot: measured R2 calibration, the
governor verifying key, the profile digest. An installer that overwrites it
destroys measurements someone took with a ruler and a stopwatch. So this tool
NEVER writes — it reports, and the operator decides.

It also never prints a secret. `KIRRA_GOVERNOR_VK_HEX` is the key the consumer
verifies release tokens against; a migration report that echoes it turns a
support paste into a key disclosure. Secrets are reported as
present/valid/fingerprint only.

    robot/install/env_migration_report.py [path]
    robot/install/env_migration_report.py --json

Six stable sections, always in this order and always present (an empty section
prints "(none)", so a reader can tell "nothing to do" from "not checked"):

    REQUIRED MISSING · INVALID VALUES · MODE-INAPPLICABLE
    OBSOLETE · PRESERVED · OPERATOR ACTION REQUIRED

Exit 0 iff nothing requires operator action.
"""
from __future__ import annotations

import hashlib
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import consumer_config_contract as contract  # noqa: E402
from preflight_consumer_env import parse_env_file  # noqa: E402

DEFAULT_ENV = "/etc/kirra/robot.env"

# Never echoed. Reported as presence + a short fingerprint so two robots can be
# compared, or a rotation confirmed, without the value leaving the machine.
SECRET_KEYS = ("KIRRA_GOVERNOR_VK_HEX", "KIRRA_ADMIN_TOKEN",
               "KIRRA_MICK_AUDITOR_TOKEN", "KIRRA_SUPERVISOR_RESET_KEY")

# Renamed/withdrawn keys. Present → the file predates a rename and the operator
# should know, but it is never a failure: a stale key is inert.
OBSOLETE_KEYS = {
    "KIRRA_R2_CLOSED_LOOP_KP": "superseded by the measured calibration profile",
    "KIRRA_MOTOR_BAUD": "the consumer fixes the baud rate for the R2 base",
    "KIRRA_VENDOR_CAR_TYPE": "renamed KIRRA_EXPECTED_CAR_TYPE (X3 only)",
}

# Values that must parse as a number for the consumer to start.
NUMERIC_KEYS = ("KIRRA_FRESHNESS_WINDOW_MS", "KIRRA_CONTROL_PERIOD_MS",
                "KIRRA_MISSED_PERIODS", "KIRRA_STOP_DECEL_MPS2",
                "KIRRA_DEMO_VX_MAX", "KIRRA_DEMO_VZ_MAX",
                "KIRRA_R2_WHEELBASE_M", "KIRRA_R2_V_PER_PWM", "KIRRA_R2_PWM_MAX",
                "KIRRA_R2_STEER_UNITS_PER_RAD", "KIRRA_R2_DELTA_MAX_RAD",
                "KIRRA_R2_STEER_SIGN", "KIRRA_R2_CENTER_TRIM",
                "KIRRA_R2_M_PER_TICK", "KIRRA_R2_V_PER_PWM_RIGHT")


def fingerprint(value: str) -> str:
    """A short, stable, non-reversible tag for a secret."""
    return hashlib.sha256(value.encode()).hexdigest()[:12]


def redact(key: str, value: str) -> str:
    if key in SECRET_KEYS:
        return f"<set, {len(value)} chars, fp={fingerprint(value)}>"
    return value


def invalid_values(env: dict) -> list[tuple[str, str]]:
    out = []
    for key in NUMERIC_KEYS:
        v = (env.get(key) or "").strip()
        if not v:
            continue
        try:
            float(v)
        except ValueError:
            out.append((key, f"{v!r} is not a number"))
    vk = (env.get("KIRRA_GOVERNOR_VK_HEX") or "").strip()
    if vk:
        if len(vk) % 2 or any(c not in "0123456789abcdefABCDEF" for c in vk):
            out.append(("KIRRA_GOVERNOR_VK_HEX", "not valid hex"))
        elif len(vk) != 64:
            out.append(("KIRRA_GOVERNOR_VK_HEX",
                        f"{len(vk)} hex chars — an Ed25519 key is 64"))
    return out


def build(env: dict) -> dict:
    r = contract.report(env)
    invalid = invalid_values(env)
    obsolete = [(k, why) for k, why in OBSOLETE_KEYS.items() if env.get(k)]
    required = set(contract.required_keys(env))
    preserved = sorted(k for k in env if k in required and (env.get(k) or "").strip())
    action = []
    if r["missing"]:
        action.append(f"set {len(r['missing'])} required key(s) — see REQUIRED MISSING")
    if invalid:
        action.append(f"correct {len(invalid)} invalid value(s)")
    if not r["missing"] and not invalid:
        action.append("none — this file is sufficient for the configured mode")
    return {"mode": r["mode"], "missing": r["missing"], "invalid": invalid,
            "inapplicable": r["present_but_inapplicable"], "obsolete": obsolete,
            "preserved": preserved, "action": action,
            "ok": not r["missing"] and not invalid}


def render(path: str, env: dict, rep: dict) -> str:
    out = ["== robot.env migration report ==",
           f"  file: {path}   (READ-ONLY — nothing was modified)",
           f"  drive mode: {rep['mode']}", ""]

    def section(title, rows):
        out.append(f"-- {title} --")
        out.extend(f"     {r}" for r in rows) if rows else out.append("     (none)")
        out.append("")

    section("REQUIRED MISSING", rep["missing"])
    section("INVALID VALUES", [f"{k}: {why}" for k, why in rep["invalid"]])
    section(f"MODE-INAPPLICABLE (set, unused in {rep['mode']})", rep["inapplicable"])
    section("OBSOLETE", [f"{k}: {why}" for k, why in rep["obsolete"]])
    section("PRESERVED", [f"{k}={redact(k, env.get(k, ''))}" for k in rep["preserved"]])
    section("OPERATOR ACTION REQUIRED", rep["action"])
    return "\n".join(out)


def main(argv) -> int:
    as_json = "--json" in argv
    positional = [a for a in argv if not a.startswith("-")]
    path = positional[0] if positional else os.environ.get("KIRRA_ROBOT_ENV", DEFAULT_ENV)
    if not os.path.isfile(path):
        msg = f"{path} does not exist — nothing to migrate; the installer will create it"
        print(json.dumps({"file": path, "exists": False}) if as_json else msg)
        return 1
    env = parse_env_file(path)
    rep = build(env)
    if as_json:
        rep["file"] = path
        # Redact before serialising — the json form is the one most likely to be
        # pasted into a ticket.
        rep["preserved"] = [{"key": k, "value": redact(k, env.get(k, ""))}
                            for k in rep["preserved"]]
        print(json.dumps(rep, indent=2))
    else:
        print(render(path, env, rep))
    return 0 if rep["ok"] else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
