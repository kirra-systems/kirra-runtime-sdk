"""storage — disk headroom, /etc/kirra presence, mount writability. Full disks
kill the audit chain, model pulls, and journal — before anything else notices.
"""
import os
import shutil

from doctor.core import detail

NAME = "storage"
DESCRIPTION = "disk usage, /etc/kirra, mount writability"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 10

WARN_PCT, FAIL_PCT = 85, 95


def run(_ctx):
    details = []
    try:
        du = shutil.disk_usage("/")
        pct = 100 * du.used / du.total
        status = "FAIL" if pct >= FAIL_PCT else ("WARN" if pct >= WARN_PCT else "PASS")
        details.append(detail("root disk usage", status,
                              f"{pct:.0f} % of {du.total // 2**30} GiB",
                              fix=None if status == "PASS" else "free space (old models/logs/build artifacts)"))
    except OSError as e:
        details.append(detail("root disk usage", "UNKNOWN", str(e)))

    etc = "/etc/kirra"
    details.append(detail("/etc/kirra", "PASS" if os.path.isdir(etc) else "FAIL",
                          "present" if os.path.isdir(etc) else "missing",
                          fix=None if os.path.isdir(etc) else "robot/install/install_kirra.sh"))

    # root remounted read-only (a classic SD/eMMC failure symptom) → FAIL.
    ro = False
    try:
        with open("/proc/mounts", encoding="utf-8") as f:
            for line in f:
                parts = line.split()
                if len(parts) >= 4 and parts[1] == "/" and "ro" in parts[3].split(","):
                    ro = True
    except OSError:
        pass
    details.append(detail("root mount", "FAIL" if ro else "PASS",
                          "READ-ONLY (fs error? dying storage?)" if ro else "rw",
                          fix="dmesg | grep -i 'remount.*ro'; back up now" if ro else None))

    home = os.path.expanduser("~")
    details.append(detail("home writable", "PASS" if os.access(home, os.W_OK) else "WARN", home))
    return {"details": details}
