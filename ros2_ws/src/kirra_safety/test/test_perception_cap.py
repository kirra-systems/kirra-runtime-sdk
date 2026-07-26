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
    GOV_PASS,
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


from perception_cap import (  # noqa: E402
    GOV_CONFIRMING,
    GOV_INITIAL_HOLD,
    GOV_INVALID,
    GOV_RESTRICTED,
    GOV_RISING,
    GOV_TIME_BACKWARD,
    GOV_TIME_GAP,
    SpeedCapGovernor,
    SpeedCapGovernorConfig,
)


def governor():
    return SpeedCapGovernor(
        SpeedCapGovernorConfig(
            rise_rate_mps_per_s=0.50,
            clear_confirmations=5,
            maximum_dt_s=0.25,
            initial_cap_mps=0.0,
        )
    )


def test_temporal_governor_first_permissive_sample_remains_stopped():
    decision = governor().update(4.77, 1_000_000_000)

    assert decision.governed_cap_mps == 0.0
    assert decision.reason == GOV_INITIAL_HOLD
    assert decision.clear_streak == 1


def test_temporal_governor_restrictive_change_is_immediate():
    cap = governor()

    for index in range(10):
        cap.update(
            4.0,
            1_000_000_000 + index * 100_000_000,
        )

    previous_governed_cap_mps = cap.governed_cap_mps
    decision = cap.update(0.50, 2_000_000_000)

    # Raw safety tightened from 4.0 to 0.50, but handling that
    # restriction must never raise an already safer governed cap.
    assert decision.governed_cap_mps == previous_governed_cap_mps
    assert decision.governed_cap_mps <= 0.50
    assert decision.reason == GOV_RESTRICTED
    assert decision.clear_streak == 0


def test_temporal_governor_requires_clear_confirmation_streak():
    cap = governor()
    start = 1_000_000_000

    decisions = [
        cap.update(4.77, start + index * 100_000_000)
        for index in range(4)
    ]

    assert all(d.governed_cap_mps == 0.0 for d in decisions)
    assert decisions[-1].reason == GOV_CONFIRMING
    assert decisions[-1].clear_streak == 4


def test_temporal_governor_rises_only_at_configured_rate():
    cap = governor()
    start = 1_000_000_000

    decisions = [
        cap.update(4.77, start + index * 100_000_000)
        for index in range(6)
    ]

    # Confirmation becomes sufficient on the fifth observation. At 10 Hz,
    # a 0.50 m/s/s rise rate permits only 0.05 m/s per update.
    assert abs(decisions[4].governed_cap_mps - 0.05) < 1e-9
    assert abs(decisions[5].governed_cap_mps - 0.10) < 1e-9
    assert decisions[5].reason == GOV_RISING


def test_restrictive_sample_resets_clear_confirmation_streak():
    cap = governor()
    start = 1_000_000_000

    for index in range(4):
        cap.update(4.77, start + index * 100_000_000)

    decision = cap.update(0.0, start + 400_000_000)

    assert decision.governed_cap_mps == 0.0
    assert decision.reason == GOV_RESTRICTED
    assert decision.clear_streak == 0

    next_decision = cap.update(4.77, start + 500_000_000)
    assert next_decision.governed_cap_mps == 0.0
    assert next_decision.clear_streak == 1


def test_invalid_raw_caps_fail_closed_and_reset():
    for raw_cap in [float("nan"), float("inf"), float("-inf"), -0.01, None]:
        cap = governor()
        cap.update(4.77, 1_000_000_000)

        decision = cap.update(raw_cap, 1_100_000_000)

        assert decision.governed_cap_mps == 0.0
        assert decision.reason == GOV_INVALID
        assert decision.clear_streak == 0


def test_backward_monotonic_clock_fails_closed():
    cap = governor()
    cap.update(4.77, 1_000_000_000)

    decision = cap.update(4.77, 900_000_000)

    assert decision.governed_cap_mps == 0.0
    assert decision.reason == GOV_TIME_BACKWARD
    assert decision.clear_streak == 0


def test_excessive_processing_gap_fails_closed():
    cap = governor()
    cap.update(4.77, 1_000_000_000)

    decision = cap.update(4.77, 1_500_000_000)

    assert decision.governed_cap_mps == 0.0
    assert decision.reason == GOV_TIME_GAP
    assert decision.clear_streak == 0


def test_invalid_governor_configuration_is_rejected():
    import pytest

    with pytest.raises(ValueError):
        SpeedCapGovernorConfig(rise_rate_mps_per_s=0.0)

    with pytest.raises(ValueError):
        SpeedCapGovernorConfig(clear_confirmations=0)

    with pytest.raises(ValueError):
        SpeedCapGovernorConfig(maximum_dt_s=-1.0)

    with pytest.raises(ValueError):
        SpeedCapGovernorConfig(initial_cap_mps=-0.1)


def test_small_raw_cap_jitter_does_not_reset_confirmation():
    cap = governor()
    start = 1_000_000_000

    raw_caps = [0.73, 0.74, 0.72, 0.74, 0.73, 0.74]

    decisions = [
        cap.update(raw, start + index * 100_000_000)
        for index, raw in enumerate(raw_caps)
    ]

    assert decisions[4].governed_cap_mps > 0.0
    assert decisions[-1].reason in (GOV_RISING, GOV_PASS)
    assert decisions[-1].clear_streak >= 5


def test_raw_cap_drop_beyond_epsilon_is_restrictive():
    cap = governor()
    start = 1_000_000_000

    for index in range(8):
        cap.update(0.74, start + index * 100_000_000)

    decision = cap.update(0.69, start + 800_000_000)

    assert decision.reason == GOV_RESTRICTED
    assert decision.governed_cap_mps <= 0.69
    assert decision.clear_streak == 0


def test_restriction_epsilon_configuration_is_validated():
    import pytest

    with pytest.raises(ValueError):
        SpeedCapGovernorConfig(restriction_epsilon_mps=-0.01)

    with pytest.raises(ValueError):
        SpeedCapGovernorConfig(restriction_epsilon_mps=float("nan"))

def test_one_centimeter_per_second_drop_clamps_without_resetting_streak():
    cap = SpeedCapGovernor(
        SpeedCapGovernorConfig(
            rise_rate_mps_per_s=0.50,
            restriction_epsilon_mps=0.03,
            clear_confirmations=5,
            maximum_dt_s=0.25,
            initial_cap_mps=0.74,
        )
    )
    start = 2_000_000_000

    for index in range(5):
        cap.update(0.74, start + index * 100_000_000)

    previous_streak = cap.clear_streak
    decision = cap.update(0.73, start + 500_000_000)

    assert decision.governed_cap_mps == 0.73
    assert decision.reason == GOV_PASS
    assert decision.clear_streak == previous_streak


def test_material_drop_still_resets_streak_and_clamps_immediately():
    cap = SpeedCapGovernor(
        SpeedCapGovernorConfig(
            rise_rate_mps_per_s=0.50,
            restriction_epsilon_mps=0.03,
            clear_confirmations=5,
            maximum_dt_s=0.25,
            initial_cap_mps=0.74,
        )
    )
    start = 3_000_000_000

    for index in range(5):
        cap.update(0.74, start + index * 100_000_000)

    decision = cap.update(0.60, start + 500_000_000)

    assert decision.governed_cap_mps == 0.60
    assert decision.reason == GOV_RESTRICTED
    assert decision.clear_streak == 0
