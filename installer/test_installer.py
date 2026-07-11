#!/usr/bin/env python3
"""Host test for the detecting installer — stub probes, no hardware (CI-run).

Pins the fail-closed contract of detect→verify→install and, critically, the
SELECT-NOT-INFER boundary: the rendered config must contain only values copied
from platform_map.toml / the selected class NAME — the test greps the rendered
output for any number that could only have come from a probe (there are none
to find, and the assertion keeps it that way).
"""

from __future__ import annotations

import os
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from detect import detect_board_mode, full_report  # noqa: E402
from kirra_install import load_map, render_config, verify_platform  # noqa: E402

failures: list[str] = []


def check(cond: bool, msg: str) -> None:
    if not cond:
        failures.append(msg)


def fake_dev_dir(names) -> str:
    d = tempfile.mkdtemp(prefix="kirra_install_test_dev_")
    for n in names:
        # regular files stand in for the device symlinks
        with open(os.path.join(d, n), "w", encoding="utf-8") as f:
            f.write("")
    return d


def report_for(car_type, devices=("myserial", "ydlidar")) -> dict:
    return full_report(
        car_type_reader=lambda: car_type,
        dev_dir=fake_dev_dir(devices),
        nv_tegra_release_path="/nonexistent",
    )


def main() -> int:
    platforms = load_map()

    # (1) board in R2 mode + devices present → r2 verifies clean.
    r = report_for(5)
    check(verify_platform("r2", r, platforms) == [],
          "(1) r2 target with car_type=5 and devices must verify clean")
    print("(1) r2 + mode 5 → verify OK")

    # (2) THE bench finding: R2 target but board reports mode 1 (cross-labeled
    #     X3 image) → refused, with the flash-the-R2-image remediation named.
    r = report_for(1)
    fails = verify_platform("r2", r, platforms)
    check(len(fails) == 1 and "car_type=1" in fails[0] and "R2 base image" in fails[0],
          f"(2) r2 target on mode-1 board must refuse with the R2-image remediation, got {fails}")
    print("(2) r2 + mode 1 → refused, remediation names the vendor R2 image")

    # (3) unreadable car type → refuse (never guess).
    r = report_for(None)
    fails = verify_platform("x3", r, platforms)
    check(any("UNREADABLE" in f for f in fails),
          f"(3) unreadable board mode must refuse, got {fails}")
    print("(3) unreadable mode → refused")

    # (4) unknown platform name → refuse, no nearest-match.
    fails = verify_platform("r2-plus", report_for(5), platforms)
    check(len(fails) == 1 and "unknown platform" in fails[0] and "nearest" in fails[0],
          f"(4) unknown platform must refuse without nearest-match, got {fails}")
    print("(4) unknown platform → refused (no nearest-match)")

    # (5) missing lidar device → refuse.
    r = report_for(1, devices=("myserial",))
    fails = verify_platform("x3", r, platforms)
    check(any("lidar serial device" in f for f in fails),
          f"(5) missing lidar device must refuse, got {fails}")
    print("(5) missing lidar → refused")

    # (6) rendered config: values are copied from the mapping (class name,
    #     car type, ports, baud) and NOTHING resembling an inferred safety
    #     parameter appears (no wheelbase, no envelope, no decel numbers).
    cfg = render_config("x3", platforms["x3"])
    joined = "\n".join(cfg.values())
    check("KIRRA_VEHICLE_CLASS=courier" in joined, "(6) class must be SELECTED by name")
    check("KIRRA_EXPECTED_CAR_TYPE=1" in joined, "(6) expected car type from the mapping")
    check("KIRRA_LIDAR_BAUD=512000" in joined, "(6) lidar baud from the mapping")
    for forbidden in ("WHEELBASE", "VX_MAX", "VZ_MAX", "STOP_DECEL"):
        check(forbidden not in joined,
              f"(6) installer must NOT write {forbidden} — safety numbers come from "
              f"the class profile / deployment review, never the installer")
    print("(6) rendered config: select-not-infer holds (class name + mapping values only)")

    # (7) detect_board_mode never invents a name for an unknown register value.
    check(detect_board_mode(lambda: 99)["mode_name"] is None,
          "(7) unknown car-type value must map to None (refused upstream)")
    print("(7) unknown register value → no name invented")

    print()
    if failures:
        for f in failures:
            print(f"FAIL {f}")
        print(f"\ninstaller test FAILED ({len(failures)} mismatch(es))")
        return 1
    print("installer test: OK — detect/verify/install are fail-closed and select-not-infer.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
