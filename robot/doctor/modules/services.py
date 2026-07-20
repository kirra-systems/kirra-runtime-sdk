"""services — loop service ports + systemd unit states.

Port listeners are the primary signal (this robot runs the loop by hand today);
systemd states are secondary because the units are deliberately STAGED, NOT
ENABLED (R2_AUTOSTART_CHECKLIST.md) — an inactive unit is a WARN, never a FAIL.
"""
from doctor.core import detail, port_listening, run_cmd

NAME = "services"
DESCRIPTION = "loop services (ports) + systemd unit states"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 15

PORTS = (("verifier", 8090), ("mick", 8102), ("ollama", 11434))
UNITS = ("kirra-verifier.service", "kirra-mick.service", "kirra-planner.service",
         "kirra-taj.service", "kirra-consumer.service", "kirra-ros-stack.service",
         "kirra-rabbit-watch.service")


def run(_ctx):
    details = []
    for name, port in PORTS:
        if port_listening(port):
            details.append(detail(f"{name} port :{port}", "PASS", "listening"))
        else:
            details.append(detail(f"{name} port :{port}", "WARN", "not listening",
                                  fix="bring up the loop — R2_LIVE_LOOP_BRINGUP.md"))
    for unit in UNITS:
        rc, out, _ = run_cmd(["systemctl", "show", unit, "--no-pager",
                              "-p", "ActiveState,SubState,NRestarts,ExecMainPID"], timeout_s=5)
        if rc != 0:
            details.append(detail(unit, "UNKNOWN", "systemctl unavailable"))
            continue
        props = dict(line.split("=", 1) for line in out.strip().splitlines() if "=" in line)
        state = props.get("ActiveState", "?")
        info = (f"{state}/{props.get('SubState', '?')} pid={props.get('ExecMainPID', '?')} "
                f"restarts={props.get('NRestarts', '?')}")
        if state == "active":
            n = int(props.get("NRestarts", "0") or 0)
            details.append(detail(unit, "WARN" if n > 3 else "PASS",
                                  info, fix="journalctl -u " + unit if n > 3 else None))
        elif state == "failed":
            details.append(detail(unit, "FAIL", info, fix=f"journalctl -u {unit} -e"))
        else:  # inactive = staged-not-enabled by design
            details.append(detail(unit, "PASS", f"{info} (staged, not enabled — by design)"))
    return {"details": details}
