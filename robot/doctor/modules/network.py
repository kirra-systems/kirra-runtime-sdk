"""network — interfaces, DNS, and time sync. Offline-tolerant: this robot may
legitimately run without internet, so external reachability is a WARN, never a
FAIL. No pings are sent (read-only + no traffic surprises) — DNS resolution and
kernel interface state only.
"""
import glob
import os
import socket

from doctor.core import detail, run_cmd

NAME = "network"
DESCRIPTION = "interfaces up, DNS, NTP time sync"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 12


def run(_ctx):
    details = []
    up = []
    for p in glob.glob("/sys/class/net/*"):
        name = os.path.basename(p)
        if name == "lo":
            continue
        try:
            with open(os.path.join(p, "operstate"), encoding="utf-8") as f:
                if f.read().strip() == "up":
                    up.append(name)
        except OSError:
            pass
    details.append(detail("interfaces up", "PASS" if up else "FAIL",
                          ", ".join(up) or "none (besides lo)",
                          fix=None if up else "check wifi/ethernet — no link at all"))

    try:
        socket.setdefaulttimeout(3)
        socket.getaddrinfo("github.com", 443)
        details.append(detail("DNS resolution", "PASS", "github.com resolves"))
    except OSError as e:
        details.append(detail("DNS resolution", "WARN", f"cannot resolve ({e})",
                              fix="offline is fine for driving; OTA/model pulls need DNS"))
    finally:
        socket.setdefaulttimeout(None)

    rc, out, _ = run_cmd(["timedatectl", "show", "-p", "NTPSynchronized"], timeout_s=5)
    if rc == 0 and "=yes" in out:
        details.append(detail("time sync (NTP)", "PASS", "synchronized"))
    elif rc == 0:
        details.append(detail("time sync (NTP)", "WARN", "not synchronized",
                              fix="timedatectl set-ntp true (cert/OTA freshness checks need sane time)"))
    else:
        details.append(detail("time sync (NTP)", "UNKNOWN", "timedatectl unavailable"))
    return {"details": details}
