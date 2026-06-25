"""
Unit tests for the pure, fail-closed perception-cap clamp.

No ROS / rclpy needed — imports the pure `perception_cap` module directly. The
load-bearing cases are the fail-closed ones: a missing/stale/malformed cap must
STOP the robot (0.0), never let the proposed command through unchanged.

Run:  pytest ros2_ws/src/kirra_safety/test/test_perception_cap.py
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "kirra_safety"))
from perception_cap import (  # noqa: E402
    apply_perception_cap,
    DISABLED,
    STALE,
    INVALID,
    CAPPED,
    PASS,
)

STALE_S = 0.5


def test_disabled_is_a_pure_passthrough():
    # Opt-in: when off, the proposed command is untouched (byte-identical prior path).
    v, reason = apply_perception_cap(1.8, cap_mps=0.0, cap_age_s=0.0, enabled=False, stale_s=STALE_S)
    assert v == 1.8 and reason == DISABLED


def test_missing_cap_fails_closed():
    v, reason = apply_perception_cap(1.8, cap_mps=None, cap_age_s=None, enabled=True, stale_s=STALE_S)
    assert v == 0.0 and reason == STALE


def test_stale_cap_fails_closed():
    # A cap older than the staleness budget is a perception fault → stop.
    v, reason = apply_perception_cap(1.8, cap_mps=3.0, cap_age_s=0.9, enabled=True, stale_s=STALE_S)
    assert v == 0.0 and reason == STALE


def test_nonfinite_or_negative_cap_fails_closed():
    for bad in (float("nan"), float("inf"), -1.0):
        v, reason = apply_perception_cap(1.8, cap_mps=bad, cap_age_s=0.0, enabled=True, stale_s=STALE_S)
        assert v == 0.0 and reason == INVALID


def test_fresh_cap_clamps_when_proposed_exceeds_it():
    # Proposed 1.8 m/s, Taj allows only 0.9 → clamp to 0.9.
    v, reason = apply_perception_cap(1.8, cap_mps=0.9, cap_age_s=0.1, enabled=True, stale_s=STALE_S)
    assert v == 0.9 and reason == CAPPED


def test_fresh_cap_passes_when_proposed_is_under_it():
    # Proposed 0.4 m/s, Taj allows 3.0 → unchanged (Taj only tightens, never loosens).
    v, reason = apply_perception_cap(0.4, cap_mps=3.0, cap_age_s=0.1, enabled=True, stale_s=STALE_S)
    assert v == 0.4 and reason == PASS


def test_zero_cap_holds_the_robot():
    # Taj at the MRC floor (obstacle in the standoff / unhealthy corridor) → hold.
    v, reason = apply_perception_cap(1.8, cap_mps=0.0, cap_age_s=0.0, enabled=True, stale_s=STALE_S)
    assert v == 0.0 and reason == CAPPED


def test_reverse_direction_is_preserved_and_clamped():
    # Backing up at -1.8, cap 0.5 → -0.5 (magnitude clamped, sign kept).
    v, reason = apply_perception_cap(-1.8, cap_mps=0.5, cap_age_s=0.1, enabled=True, stale_s=STALE_S)
    assert v == -0.5 and reason == CAPPED
