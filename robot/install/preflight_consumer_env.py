#!/usr/bin/env python3
"""preflight_consumer_env.py — can the motor consumer actually START?

Answers, in ONE run, the question that cost 200+ restarts during R2 bring-up:
*which required settings are missing?* The consumer discovers them one at a
time — it reads a value, fails closed, systemd restarts it, it reads the next
value, fails closed — so learning eight facts took eight restart cycles and the
real cause was buried in the churn.

This is deliberately NOT the syntax linter. `lint_robot_env.sh` answers "is this
a valid shell file", which the Rabbit-only env that triggered the loop passed
cleanly while missing every actuation key. Two different questions; both matter;
only this one gates starting the service.

    robot/install/preflight_consumer_env.py                  # /etc/kirra/robot.env
    robot/install/preflight_consumer_env.py path/to/env      # any file
    robot/install/preflight_consumer_env.py --format=json    # for scripts

Exit 0 iff the file is sufficient for the configured drive mode. Read-only:
nothing is written, no device is opened, no service is touched — safe to run at
any time, including before the unit is enabled (which is the point).
"""
from __future__ import annotations

import json
import os
import shlex
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import consumer_config_contract as contract  # noqa: E402

DEFAULT_ENV = "/etc/kirra/robot.env"


def parse_env_file(path: str) -> dict:
    """Read `KEY=value` / `export KEY=value` lines, honouring shell quoting.

    Tolerant on purpose: a line this cannot parse is skipped rather than fatal,
    because a partial parse still lets us report the missing keys — and the
    syntax linter is the tool that owns malformed-file complaints.
    """
    env: dict[str, str] = {}
    with open(path, encoding="utf-8", errors="replace") as f:
        for raw in f:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            if line.startswith("export "):
                line = line[len("export "):].strip()
            if "=" not in line:
                continue
            key, _, value = line.partition("=")
            key = key.strip()
            if not key or not key.replace("_", "").isalnum():
                continue
            try:
                parts = shlex.split(value, comments=True)
                env[key] = parts[0] if parts else ""
            except ValueError:
                env[key] = value.strip().strip("\"'")
    return env


def render(path: str, env: dict) -> tuple[str, int]:
    r = contract.report(env)
    out = ["== consumer config preflight ==",
           f"  file: {path}",
           f"  drive mode: {r['mode']}",
           f"  required for this mode: {r['required_count']} key(s)"]

    if r["missing"]:
        out.append("")
        out.append(f"  ❌ {len(r['missing'])} REQUIRED key(s) missing — the consumer "
                   "would fail closed on each, one restart at a time:")
        out += [f"       {k}" for k in r["missing"]]
        out.append("")
        out.append("     fix them ALL before starting kirra-consumer.service; see")
        out.append("     robot/install/rabbit.env.example and env.template.")
    else:
        out.append(f"  ✔ every required key is set for mode {r['mode']}")

    if r["present_but_inapplicable"]:
        out.append("")
        out.append(f"  ⚠ set but NOT USED in mode {r['mode']} (harmless, but it will "
                   "mislead the next reader):")
        for k in r["present_but_inapplicable"]:
            why = "X3-only" if k in contract.X3_ONLY else \
                  "needs KIRRA_R2_ODOM_ENABLED=1" if k in contract.ODOM_REQUIRED else \
                  "needs KIRRA_R2_CLOSED_LOOP=1" if k in contract.CLOSED_LOOP_REQUIRED else \
                  "belongs to another drive mode"
            out.append(f"       {k}  ({why})")

    out.append("")
    out.append("  VERDICT: " + ("READY — the consumer has what it needs to start"
                                if r["sufficient"] else
                                "NOT READY — do not enable kirra-consumer.service yet"))
    return "\n".join(out), (0 if r["sufficient"] else 1)


def main(argv: list[str]) -> int:
    as_json = "--format=json" in argv
    positional = [a for a in argv if not a.startswith("-")]
    path = positional[0] if positional else os.environ.get("KIRRA_ROBOT_ENV", DEFAULT_ENV)

    if not os.path.isfile(path) or not os.access(path, os.R_OK):
        if as_json:
            print(json.dumps({"file": path, "readable": False, "sufficient": False,
                              "missing": list(contract.ALWAYS_REQUIRED)}, indent=2))
        else:
            print(f"== consumer config preflight ==\n  ❌ {path} is not readable\n"
                  "     create it — docs/hardware/R2_VOICE_AUDIO_SETUP.md §4")
        return 1

    env = parse_env_file(path)
    if as_json:
        report = contract.report(env)
        report["file"] = path
        report["readable"] = True
        print(json.dumps(report, indent=2))
        return 0 if report["sufficient"] else 1
    text, code = render(path, env)
    print(text)
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
