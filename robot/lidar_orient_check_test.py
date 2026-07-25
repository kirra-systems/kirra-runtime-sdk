#!/usr/bin/env python3
"""lidar_orient_check_test — host tests for the pure lidar-orientation geometry.
CI-safe: no ROS, no hardware (the /scan grab is the only live part, and it is not
exercised here). Runner style matches rabbit_voice_test.py (assert-based, stdlib).
"""
from __future__ import annotations

import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lidar_orient_check as loc  # noqa: E402


def test_wrap180() -> None:
    assert loc.wrap180(0.0) == 0.0
    assert loc.wrap180(190.0) == -170.0
    assert loc.wrap180(-190.0) == 170.0
    assert loc.wrap180(360.0) == 0.0


def test_direction_label_cardinals() -> None:
    assert loc.direction_label(0.0) == "FRONT"
    assert loc.direction_label(-0.8) == "FRONT"        # the real front-box reading
    assert loc.direction_label(91.6) == "LEFT"         # the real left-box reading
    assert loc.direction_label(-90.0) == "RIGHT"
    assert loc.direction_label(180.0) == "BEHIND"
    assert loc.direction_label(-180.0) == "BEHIND"
    assert loc.direction_label(45.0) == "off the main axes"


def test_valid_returns_filters_out_of_band_and_nonfinite() -> None:
    # 5 rays from -180° stepping +90°: -180, -90, 0, 90, 180.
    ranges = [0.0, 2.0, 1.0, float("inf"), 60.0]   # 0.0 too small, inf/60 out of band
    v = loc.valid_returns(math.radians(-180.0), math.radians(90.0), ranges, 0.01, 50.0)
    # only the -90° (2.0) and 0° (1.0) rays survive
    angs = sorted(round(a) for a, _ in v)
    assert angs == [-90, 0], v


def test_direction_min_picks_nearest_within_tolerance() -> None:
    valid = [(-0.8, 1.39), (91.6, 0.97), (-90.0, 2.28), (2.0, 4.22)]
    assert abs(loc.direction_min(valid, 0.0) - 1.39) < 1e-9   # front box, not the 4.22 ray
    assert abs(loc.direction_min(valid, 90.0) - 0.97) < 1e-9
    assert abs(loc.direction_min(valid, -90.0) - 2.28) < 1e-9
    assert loc.direction_min(valid, 180.0) == float("inf")    # nothing behind


def test_summarize_front_box_is_aligned() -> None:
    # Reproduces the real bring-up reading: nearest 1.39 m at -0.8° → FRONT, aligned.
    valid = [(-0.8, 1.39), (91.6, 2.12), (-90.0, 2.28)]
    r = loc.summarize(valid)
    assert r["nearest_label"] == "FRONT"
    assert r["forward_aligned"] is True
    assert abs(r["front_m"] - 1.39) < 1e-9


def test_summarize_rotated_lidar_is_not_aligned() -> None:
    # A front box that shows up at +91.6° means the sensor is rotated (front reads
    # as left) → nearest labels LEFT and forward_aligned is False.
    valid = [(91.6, 0.97), (-0.8, 4.22)]
    r = loc.summarize(valid)
    assert r["nearest_label"] == "LEFT"
    assert r["forward_aligned"] is False


def test_summarize_none_on_no_returns() -> None:
    assert loc.summarize([]) is None
    assert "DATA problem" in loc.render(None)


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    print("lidar_orient_check host tests (no hardware):")
    failed = 0
    for t in tests:
        try:
            t()
            print(f"  ok   {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"  FAIL {t.__name__}: {e}")
    print(f"\n{len(tests) - failed}/{len(tests)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(_run_all())
