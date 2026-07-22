#!/usr/bin/env python3
"""serial_exclusivity — the ADR-0033 **Tier-3** serial-authority sentinel.

Discharges `AOU-ACTUATION-SERIAL-001`: on the ros2_ws/R2 topology, PO-2
independence (`OCCY_DFA.md` §3) requires that the actuator serial device
(`/dev/myserial`, the Rosmaster expansion board) is reachable by the verifying
motor consumer ONLY. The stock Yahboom udev rule ships the port **MODE 0777 —
world-writable** (`robot/install/captured/yahboom/udev/serial.rules`), so on an
untightened image ANY process can drive the motors with no ROS at all,
bypassing the checker entirely. That is the #887 gap below the bus.

Three composed layers, weakest to strongest:

  1. **Boot sentinel (this module + the consumer's preflight):** at consumer
     startup, the device must be OWNED by the consumer's uid with NO group/
     other access (mode 0600), and no other same-uid process may already hold
     it open. Violations → the consumer REFUSES to start (fail-closed), unless
     `KIRRA_ALLOW_SHARED_SERIAL=1` explicitly acknowledges a bring-up run
     (the parko `PARKO_ALLOW_SCENE_GATE_WITHOUT_PRODUCER` precedent: the
     escape hatch exists, silently degraded authority does not).
  2. **Kernel exclusivity (TIOCEXCL):** after the vendor lib opens the port,
     the consumer sets TIOCEXCL on the fd — further opens by any non-root
     process fail with EBUSY for the LIFETIME of the consumer's session.
     Owner+mode keep other users out; TIOCEXCL keeps even same-user
     stragglers out while driving.
  3. **OS provisioning:** `robot/install/99-kirra-serial-exclusivity.rules`
     re-owns the CH340 to the consumer user at MODE 0600 (overrides the
     vendor 0777 rule by lexical order), installed by `install_kirra.sh`.

HONEST LIMIT of the holder scan: an unprivileged process can only read its
own user's `/proc/<pid>/fd`. That composes soundly with layer 1 — once the
mode is 0600+owner, only same-uid processes CAN hold the port, and those are
exactly the ones the scan sees. On an untightened (0777) port the scan is
best-effort and the mode violation itself is already a refusal.

Pure decision logic below is host-tested in `robot/serial_exclusivity_test.py`
(CI robot lane); only `stat`/`/proc`/`ioctl` touch the OS.
"""
from __future__ import annotations

import os
import stat as stat_mod

# Access bits that must be CLEAR on the actuator device: any group/other
# read/write/exec. (Group READ alone would already let a group member snoop
# the MCU protocol; there is no legitimate second reader — keep 0600 strict.)
_FORBIDDEN_MODE_BITS = 0o077

ACK_ENV = "KIRRA_ALLOW_SHARED_SERIAL"


# ---------------------------------------------------------------------------
# Pure decision core (host-testable: plain ints and lists in, verdicts out)
# ---------------------------------------------------------------------------

def mode_violations(st_mode: int, st_uid: int, euid: int, path: str) -> list[str]:
    """Owner + permission-bit check against the AOU-ACTUATION-SERIAL-001
    contract (owner = the consumer's uid, mode 0600)."""
    out = []
    if st_uid != euid:
        out.append(
            f"{path} is owned by uid {st_uid}, not the consumer's uid {euid} — "
            "another user's processes can hold the actuator port"
        )
    loose = st_mode & _FORBIDDEN_MODE_BITS
    if loose:
        out.append(
            f"{path} mode {stat_mod.S_IMODE(st_mode):04o} grants group/other access "
            f"(loose bits {loose:04o}; required 0600) — any process can open the "
            "actuator port below the checker (stock vendor rule ships 0777)"
        )
    return out


def holder_violations(holders: list[tuple[int, str]], path: str) -> list[str]:
    """Other processes already holding the device open at consumer startup."""
    return [
        f"{path} is already open in pid {pid} ({comm or 'unknown'}) — the consumer "
        "must be the sole opener (stop the vendor autostart / bench tool first)"
        for pid, comm in holders
    ]


def ack_given(env_value: str | None) -> bool:
    """Is the shared-serial bring-up acknowledgment set? (Same truthy set as
    the consumer's other flags.)"""
    return (env_value or "").strip().lower() in ("1", "true", "yes", "on")


def startup_verdict(violations: list[str], acknowledged: bool) -> tuple[str, str]:
    """The fail-closed startup policy: ('ok'|'acknowledged'|'refuse', message).

    Violations with no acknowledgment REFUSE startup — a consumer that cannot
    claim sole authority over the port must not pretend the chokepoint holds
    (safe-and-loud, not safe-and-silent). The acknowledgment admits a bring-up
    run but keeps the violations in the operator's face.
    """
    if not violations:
        return "ok", "serial exclusivity: OK (owner+mode 0600, no other holder)"
    body = "; ".join(violations)
    if acknowledged:
        return "acknowledged", (
            f"serial exclusivity DEGRADED (acknowledged via {ACK_ENV}=1 — bring-up "
            f"only, PO-2 serial authority is NOT enforced this run): {body}"
        )
    return "refuse", (
        f"serial exclusivity violated: {body}. Fix: install "
        "robot/install/99-kirra-serial-exclusivity.rules (owner=<consumer user>, "
        "MODE=0600), replug/`udevadm trigger`, and stop any other opener "
        f"(disable_vendor_autostart.sh). {ACK_ENV}=1 acknowledges a bring-up run."
    )


# ---------------------------------------------------------------------------
# OS-touching helpers (thin; not exercised by the host tests)
# ---------------------------------------------------------------------------

def find_holders(path: str, exclude_pids: tuple[int, ...] = ()) -> list[tuple[int, str]]:
    """Best-effort scan of /proc/<pid>/fd for open handles on `path` (see the
    module doc for why same-uid visibility is exactly what layer 1 needs)."""
    try:
        target = os.path.realpath(path)
    except OSError:
        return []
    holders: list[tuple[int, str]] = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        pid = int(entry)
        if pid == os.getpid() or pid in exclude_pids:
            continue
        fd_dir = f"/proc/{pid}/fd"
        try:
            fds = os.listdir(fd_dir)
        except OSError:
            continue  # not ours to read (different uid) or gone
        for fd in fds:
            try:
                if os.path.realpath(os.path.join(fd_dir, fd)) == target:
                    try:
                        with open(f"/proc/{pid}/comm", encoding="utf-8") as f:
                            comm = f.read().strip()
                    except OSError:
                        comm = ""
                    holders.append((pid, comm))
                    break
            except OSError:
                continue
    return holders


def preflight(path: str) -> list[str]:
    """The boot-sentinel check bundle: stat + holder scan → violation list.
    A missing/unstattable device is its own violation (the consumer would
    fail on open anyway, but the sentinel names it first)."""
    try:
        st = os.stat(path)
    except OSError as e:
        return [f"{path} not stattable: {e}"]
    v = mode_violations(st.st_mode, st.st_uid, os.geteuid(), path)
    v += holder_violations(find_holders(path), path)
    return v


def claim_exclusive(fd: int) -> bool:
    """Layer 2: set TIOCEXCL on the opened port — the kernel then refuses
    further non-root opens (EBUSY) until the fd closes. Returns success."""
    try:
        import fcntl
        import termios
        fcntl.ioctl(fd, termios.TIOCEXCL)
        return True
    except OSError:
        return False


def serial_fd_of(rosmaster_obj) -> int | None:
    """Locate the pyserial fd inside the vendor Rosmaster object. Tries the
    known attribute (`.ser`) first, then scans for any attribute exposing a
    live `fileno()`. Defensive: a vendor-lib layout change degrades to None
    (the caller warns; layers 1 and 3 still hold), never a crash."""
    candidates = []
    known = getattr(rosmaster_obj, "ser", None)
    if known is not None:
        candidates.append(known)
    try:
        candidates += list(vars(rosmaster_obj).values())
    except TypeError:
        pass
    for obj in candidates:
        fileno = getattr(obj, "fileno", None)
        if callable(fileno):
            try:
                fd = fileno()
            except Exception:  # noqa: BLE001 — closed/pseudo file objects
                continue
            if isinstance(fd, int) and fd >= 0:
                return fd
    return None
