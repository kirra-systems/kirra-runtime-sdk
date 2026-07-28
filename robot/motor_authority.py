#!/usr/bin/env python3
"""motor_authority.py — who actually owns the motor serial port?

**The invariant is serial authority, not vendor branding.**

The ADR-0033 chokepoint says exactly one process may hold the configured motor
device. Everything downstream of that — the release-token verify, TIOCEXCL, the
0600 udev rule — assumes it. So the question a diagnostic must answer is
"*which PIDs hold this device, and is the consumer the only one?*"

It is NOT "does any process anywhere have a vendor name in its path?" That
question produced four live false positives during R2 bring-up, and acting on
them did real damage:

| Matched | What it actually was | Damage |
|---|---|---|
| `yahboom` in `PATH` | `ollama.service` | **disabled** — had to be restored by hand |
| `yahboomcar_ros2_ws` in exe path | the YDLIDAR driver | **killed** — lidar down |
| `yahboom` in the unit | the OLED display service | **disabled** |
| `avahi-daemon: running [yahboom.local]` | the mDNS hostname | reported as a motor base |

None of them had `/dev/myserial` open. A lidar in a vendor-named workspace is a
lidar. A hostname is a string. The vendor's *name* is evidence of nothing.

So: resolve the configured port through its symlinks, find the PIDs actually
holding that device, and compare against the consumer's systemd MainPID. Vendor
name matching survives only as SECONDARY evidence for *dormant* autostart units
(a unit that is enabled but not running holds no fd to inspect) — and it may
never, on its own, justify stopping or disabling anything.

Pure where it matters: `classify`, `verdict_for` and the parsing helpers take
injected data and do no I/O, so every state below is host-testable with no
robot, no serial port and no systemd. Only `collect()` touches the system.
"""
from __future__ import annotations

import os
import re
import subprocess

# Verdicts. PASS/FAIL follow the doctor's vocabulary; UNKNOWN is used only when
# the evidence is genuinely unavailable, never to soften a real conflict.
PASS = "PASS"
FAIL = "FAIL"
UNKNOWN = "UNKNOWN"

CONSUMER_UNIT = "kirra-consumer.service"
DEFAULT_MOTOR_PORT = "/dev/myserial"

# Executables/commands that ARE a Rosmaster motor base. Deliberately narrow:
# every entry names a motor-base program, not a vendor, a workspace or a host.
# Used only as secondary evidence for dormant units — never to kill a process
# that does not hold the motor device.
MOTOR_BASE_SIGNATURES = (
    "rosmaster_main",          # the vendor bringup node
    "Rosmaster_Lib",           # the vendor driver library entry point
    "ros_robot_controller",    # the alternate vendor base node
    "yahboomcar_bringup",      # the base bringup package (NOT the workspace name)
    "yahboomcar_base_node",
)

# Things that live in a vendor workspace, or carry the vendor's name, and are
# NOT a motor base. Present so the intent is executable, not just documented.
NOT_A_MOTOR_BASE = (
    "ydlidar",       # a sensor
    "avahi-daemon",  # the mDNS hostname yahboom.local
    "ollama",        # an unrelated service that merely had the ws on PATH
    "oled",          # the display
    "astra",         # the camera
    "usb_cam",
)


def resolve_device(path: str) -> str:
    """Follow `/dev/myserial → /dev/ttyUSB1` so the symlink and the underlying
    tty compare equal. A dangling/absent link returns the path unchanged — the
    caller reports "missing", which is a different failure from "contended"."""
    try:
        return os.path.realpath(path)
    except OSError:
        return path


def parse_mainpid(show_output: str) -> int | None:
    """MainPID from `systemctl show -p MainPID -p ActiveState`. 0 means the
    unit is not running, which is not the same as "no such unit"."""
    for line in (show_output or "").splitlines():
        if line.startswith("MainPID="):
            try:
                pid = int(line.split("=", 1)[1].strip())
            except ValueError:
                return None
            return pid or None
    return None


def parse_active_state(show_output: str) -> str:
    for line in (show_output or "").splitlines():
        if line.startswith("ActiveState="):
            return line.split("=", 1)[1].strip()
    return "unknown"


def parse_lsof_pids(lsof_output: str) -> list[int]:
    """PIDs from `lsof -t <dev>` (one per line). Tolerates blank/garbage lines
    so a partial read never invents a holder."""
    pids = []
    for line in (lsof_output or "").splitlines():
        line = line.strip()
        if line.isdigit():
            pids.append(int(line))
    return pids


def is_motor_base(exe: str, cmdline: str) -> bool:
    """Does this process look like a Rosmaster MOTOR BASE?

    Requires a signature from `MOTOR_BASE_SIGNATURES` — a program name, not a
    directory. An explicit exclusion list wins first, so a lidar node whose exe
    lives under `~/yahboomcar_ros2_ws` can never match on the path alone.
    """
    hay = f"{exe} {cmdline}"
    low = hay.lower()
    if any(x in low for x in NOT_A_MOTOR_BASE):
        return False
    return any(sig.lower() in low for sig in MOTOR_BASE_SIGNATURES)


class Holder:
    """One process holding the motor device."""

    def __init__(self, pid: int, exe: str = "", cmdline: str = "", comm: str = ""):
        self.pid = pid
        self.exe = exe
        self.cmdline = cmdline
        self.comm = comm

    def label(self) -> str:
        return self.comm or os.path.basename(self.exe) or self.cmdline[:40] or f"pid {self.pid}"

    def __repr__(self):
        return f"Holder(pid={self.pid}, exe={self.exe!r})"


def classify(holders, consumer_pid, consumer_active):
    """The whole decision, as pure data → (verdict, summary, evidence lines).

    | holders | consumer | verdict |
    |---|---|---|
    | exactly the consumer pid | active | PASS — sole opener |
    | consumer + others | active | FAIL — contended |
    | others only | active | FAIL — consumer does not own its port |
    | others only | inactive | FAIL — something else owns the actuation boundary |
    | none | inactive | PASS (reported) — nothing owns it, nothing claims to |
    | none | active | FAIL — the consumer claims active but holds no fd |
    """
    others = [h for h in holders if h.pid != consumer_pid]
    has_consumer = consumer_pid is not None and any(h.pid == consumer_pid for h in holders)
    ev = [f"holder pid={h.pid} ({h.label()}) exe={h.exe or '?'}" for h in holders] or ["no holders"]

    if not holders:
        if consumer_active:
            return (FAIL,
                    f"{CONSUMER_UNIT} is active (pid {consumer_pid}) but holds no handle "
                    "on the motor port — it is not the writer it claims to be",
                    ev)
        return (PASS, "no process holds the motor port and the consumer is not active", ev)

    if has_consumer and not others:
        return (PASS,
                f"held only by {CONSUMER_UNIT} pid={consumer_pid}",
                ev)

    if has_consumer and others:
        names = ", ".join(f"pid {h.pid} ({h.label()})" for h in others)
        return (FAIL,
                f"{CONSUMER_UNIT} pid={consumer_pid} shares the motor port with {names} "
                "— the single-writer chokepoint is broken",
                ev)

    names = ", ".join(f"pid {h.pid} ({h.label()})" for h in others)
    return (FAIL,
            f"the motor port is held by {names} and NOT by {CONSUMER_UNIT}"
            + (f" (pid {consumer_pid})" if consumer_pid else " (not running)"),
            ev)


def verdict_for(port, resolved, holders, consumer_pid, consumer_active, exists=True):
    """`classify` plus the device-missing case, as one dict a caller renders."""
    if not exists:
        return {"verdict": FAIL, "port": port, "resolved": resolved,
                "summary": f"{port} does not exist", "evidence": [],
                "holders": [], "consumer_pid": consumer_pid,
                "consumer_active": consumer_active}
    v, summary, ev = classify(holders, consumer_pid, consumer_active)
    return {"verdict": v, "port": port, "resolved": resolved, "summary": summary,
            "evidence": ev, "holders": list(holders), "consumer_pid": consumer_pid,
            "consumer_active": consumer_active}


# ── system probes (the only I/O in this module) ──────────────────────────────

def _read(path: str) -> str:
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def proc_exe(pid: int) -> str:
    try:
        return os.readlink(f"/proc/{pid}/exe")
    except OSError:
        return ""


def proc_cmdline(pid: int) -> str:
    return _read(f"/proc/{pid}/cmdline").replace("\0", " ").strip()


def proc_comm(pid: int) -> str:
    return _read(f"/proc/{pid}/comm").strip()


def find_holder_pids(resolved: str) -> list[int]:
    """PIDs holding `resolved`, from /proc/<pid>/fd. Same-uid visibility only,
    which is what the consumer's own sentinel already relies on; `lsof` is used
    as a fallback when available because it can see other uids."""
    pids = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        pid = int(entry)
        fd_dir = f"/proc/{pid}/fd"
        try:
            fds = os.listdir(fd_dir)
        except OSError:
            continue
        for fd in fds:
            try:
                if os.path.realpath(os.path.join(fd_dir, fd)) == resolved:
                    pids.append(pid)
                    break
            except OSError:
                continue
    try:
        out = subprocess.run(["lsof", "-t", resolved], capture_output=True,
                             text=True, timeout=5).stdout
        for pid in parse_lsof_pids(out):
            if pid not in pids:
                pids.append(pid)
    except Exception:  # noqa: BLE001 — lsof absent/denied is fine, /proc stands
        pass
    return sorted(pids)


def consumer_state(unit: str = CONSUMER_UNIT):
    """(MainPID|None, ActiveState) for the consumer unit."""
    try:
        out = subprocess.run(
            ["systemctl", "show", unit, "-p", "MainPID", "-p", "ActiveState"],
            capture_output=True, text=True, timeout=5).stdout
    except Exception:  # noqa: BLE001 — no systemd (container/CI)
        return None, "unknown"
    return parse_mainpid(out), parse_active_state(out)


def collect(port: str | None = None, unit: str = CONSUMER_UNIT) -> dict:
    """The live authority answer for `port` (default `KIRRA_MOTOR_PORT`)."""
    port = port or os.environ.get("KIRRA_MOTOR_PORT") or DEFAULT_MOTOR_PORT
    resolved = resolve_device(port)
    exists = os.path.exists(port)
    pid, state = consumer_state(unit)
    active = state == "active"
    holders = []
    if exists:
        holders = [Holder(p, proc_exe(p), proc_cmdline(p), proc_comm(p))
                   for p in find_holder_pids(resolved)]
    return verdict_for(port, resolved, holders, pid, active, exists)


def render(result: dict) -> list[str]:
    """Operator-facing lines: what was checked, what holds it, what follows."""
    port, resolved = result["port"], result["resolved"]
    dev = port if port == resolved else f"{port} → {resolved}"
    lines = [f"motor port: {dev}",
             f"consumer:   {CONSUMER_UNIT} "
             f"{'active' if result['consumer_active'] else 'not active'}"
             + (f" (MainPID {result['consumer_pid']})" if result["consumer_pid"] else ""),
             f"verdict:    {result['verdict']} — {result['summary']}"]
    lines += [f"  · {e}" for e in result["evidence"]]
    return lines


if __name__ == "__main__":
    import sys
    r = collect(sys.argv[1] if len(sys.argv) > 1 else None)
    print("\n".join(render(r)))
    raise SystemExit(0 if r["verdict"] == PASS else 1)
