"""telemetry — the verifier's observability endpoints + host vitals (thermal,
memory, load). The endpoints are the same posture-exempt GETs an operator uses
to tell 'LockedOut' from 'service down' (Bug 2); vitals come from /proc + /sys.
"""
import os

from doctor.core import detail, http_status

NAME = "telemetry"
DESCRIPTION = "verifier /health //ready //metrics + host vitals"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 12

WARN_C, FAIL_C = 85.0, 97.0     # Orin throttles near 90s; sustained ~97 is real trouble
WARN_MEM_PCT = 92


def run(ctx):
    details = []
    base = ctx["robot_env"].get("KIRRA_VERIFIER_URL", "http://localhost:8090").rstrip("/")
    for ep in ("/health", "/ready", "/metrics"):
        code = http_status(f"{base}{ep}")
        details.append(detail(f"GET {ep}", "PASS" if code == 200 else "WARN",
                              f"{code}" if code else "unreachable",
                              fix=None if code == 200 else "verifier down? R2_LIVE_LOOP_BRINGUP.md"))

    temps = []
    for zone in sorted(os.listdir("/sys/class/thermal") if os.path.isdir("/sys/class/thermal") else []):
        p = f"/sys/class/thermal/{zone}/temp"
        if zone.startswith("thermal_zone") and os.path.isfile(p):
            try:
                with open(p, encoding="utf-8") as f:
                    temps.append(int(f.read().strip()) / 1000.0)
            except (OSError, ValueError):
                pass
    if temps:
        t = max(temps)
        status = "FAIL" if t >= FAIL_C else ("WARN" if t >= WARN_C else "PASS")
        details.append(detail("max thermal zone", status, f"{t:.1f} °C",
                              fix=None if status == "PASS" else "check airflow/fan; reduce load"))
    else:
        details.append(detail("max thermal zone", "UNKNOWN", "no thermal zones readable"))

    try:
        with open("/proc/meminfo", encoding="utf-8") as f:
            mem = {l.split(":")[0]: int(l.split()[1]) for l in f if ":" in l}
        used_pct = 100 * (1 - mem["MemAvailable"] / mem["MemTotal"])
        details.append(detail("memory used", "WARN" if used_pct > WARN_MEM_PCT else "PASS",
                              f"{used_pct:.0f} %"))
    except Exception:  # noqa: BLE001
        details.append(detail("memory used", "UNKNOWN", "/proc/meminfo unreadable"))

    try:
        load1 = os.getloadavg()[0]
        ncpu = os.cpu_count() or 1
        details.append(detail("load average (1m)", "WARN" if load1 > 2 * ncpu else "PASS",
                              f"{load1:.2f} on {ncpu} cores"))
    except OSError:
        details.append(detail("load average (1m)", "UNKNOWN", "unavailable"))
    return {"details": details}
