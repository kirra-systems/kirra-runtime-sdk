"""KIRRA installer — hardware detection. Emits NAMES, never safety parameters.

The load-bearing rule (gap analysis, safety boundary): detection may SELECT a
validated platform entry; it must NEVER INFER a safety parameter. Everything
this module returns is an identifier or a boolean; every number the installed
system runs with comes from the signed/reviewed mapping (`platform_map.toml`)
and the compiled contract profiles — never from a probe.

A second rule, learned on hardware (docs/hardware/HARDWARE_FINDINGS_R2X3.md):
the Rosmaster car-type register is the board's CONFIGURED DRIVE MODEL, not
immutable chassis identity — genuine R2 (Ackermann) hardware shipped reporting
type 1 (X3/mecanum) because of Yahboom's cross-labeled image. So detection
cannot "identify the chassis" from the register alone; the installer flow is:
the operator DECLARES the target platform, and detection VERIFIES the board
mode (and refuses on mismatch, with the exact remediation). Detection also
runs standalone as a report (`kirra-install detect`).

All probes take injectable readers so the logic is host-testable without
hardware (the stub-harness pattern of robot/teardown_smoke_test.py).
"""

from __future__ import annotations

import glob
import os
import platform as _platform

# Vendor CARTYPE constants, read from a live Rosmaster_Lib instance on the
# bench (findings doc). The register value → drive-model NAME. Unknown values
# map to None → the caller refuses (no nearest-match, no default).
CARTYPE_NAMES = {
    1: "x3",        # mecanum drive model
    2: "x3-plus",   # mecanum (larger chassis)
    4: "x1",
    5: "r2",        # Ackermann drive model (steering servo live)
}


def detect_chipset(nv_tegra_release_path: str = "/etc/nv_tegra_release") -> dict:
    """Compute-board identification (non-safety, informational).

    Returns {"arch": str, "jetson": bool, "l4t": str|None}.
    """
    arch = _platform.machine()
    jetson = os.path.isfile(nv_tegra_release_path)
    l4t = None
    if jetson:
        try:
            with open(nv_tegra_release_path, "r", encoding="utf-8", errors="replace") as f:
                l4t = f.readline().strip() or None
        except OSError:
            jetson = False
    return {"arch": arch, "jetson": jetson, "l4t": l4t}


def read_board_car_type_via_vendor_lib(port: str, settle_s: float = 0.5):
    """The REAL board-mode reader: opens the Rosmaster serial port via the
    vendor lib and reads the car-type register. Returns int or None.

    ⚠ Opens the motor serial port — the consumer must NOT be running (single
    writer). Only invoked on-robot; host runs inject a stub instead.
    """
    try:
        import time

        from Rosmaster_Lib import Rosmaster  # vendor lib, robot-only
    except ImportError:
        return None
    try:
        bot = Rosmaster(com=port)
        bot.create_receive_threading()
        time.sleep(settle_s)
        t = bot.get_car_type_from_machine()
        return int(t) if t is not None else None
    except Exception:  # noqa: BLE001 — a probe failure is "unknown", refused upstream
        return None


def detect_board_mode(car_type_reader) -> dict:
    """Board drive-model mode via the injected reader.

    Returns {"car_type": int|None, "mode_name": str|None}. `mode_name` is None
    for an unreadable or unrecognized register — the caller must REFUSE, never
    guess.
    """
    raw = car_type_reader()
    if raw is None:
        return {"car_type": None, "mode_name": None}
    try:
        value = int(raw)
    except (TypeError, ValueError):
        return {"car_type": None, "mode_name": None}
    return {"car_type": value, "mode_name": CARTYPE_NAMES.get(value)}


def detect_serial_devices(dev_dir: str = "/dev") -> dict:
    """Report the serial-device symlinks the vendor images create. NAMES only;
    which DRIVER talks to which device comes from the mapping, not from here.

    Known wrinkle (bringup finding): /dev/rplidar and /dev/ydlidar may BOTH
    point at the same ttyUSB — presence of a symlink is not lidar identity.
    """
    out = {}
    for name in ("myserial", "ydlidar", "rplidar"):
        path = os.path.join(dev_dir, name)
        if os.path.islink(path) or os.path.exists(path):
            try:
                target = os.path.realpath(path)
            except OSError:
                target = None
            out[name] = target
    return out


def detect_cameras(dev_dir: str = "/dev") -> list:
    """Video capture devices, report-only. No camera driver configuration
    exists in the repo yet (gap analysis Part 2) — this enumerates so the
    install report is honest about what it saw and did NOT configure."""
    return sorted(glob.glob(os.path.join(dev_dir, "video*")))


def full_report(car_type_reader, dev_dir: str = "/dev",
                nv_tegra_release_path: str = "/etc/nv_tegra_release") -> dict:
    """The complete detection report (all NAMES/identifiers)."""
    return {
        "chipset": detect_chipset(nv_tegra_release_path),
        "board_mode": detect_board_mode(car_type_reader),
        "serial_devices": detect_serial_devices(dev_dir),
        "cameras": detect_cameras(dev_dir),
    }
