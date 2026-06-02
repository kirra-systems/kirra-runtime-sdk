"""
Unit tests for the pure cmd_vel-interceptor enforcement decision.

No ROS / rclpy needed — imports the pure `enforcement_decision` module directly.
The load-bearing case is the malformed/contract-violating 200: it MUST Stop,
never Forward the original (unclamped) command.

Run:  pytest ros2_ws/src/kirra_safety/test/test_enforcement_decision.py
"""

import os
import sys

# Import the pure module standalone (no installed package, no ROS).
sys.path.insert(
    0, os.path.join(os.path.dirname(__file__), "..", "kirra_safety")
)
from enforcement_decision import decide_enforcement, Forward, Stop  # noqa: E402

# The original (un-clamped) command. A fail-closed decision must NEVER forward
# these values.
PROPOSED = {"linear_velocity_mps": 100.0, "steering_angle_deg": 90.0}


def _ok(action, v, s):
    return {
        "action": action,
        "enforced_linear_velocity_mps": v,
        "enforced_steering_angle_deg": s,
    }


def test_good_200_allow_forwards_enforced():
    d = decide_enforcement(200, _ok("Allow", 10.0, 2.0), PROPOSED)
    assert isinstance(d, Forward)
    assert d.action == "Allow"
    assert d.enforced_v == 10.0 and d.enforced_s == 2.0


def test_clamp_200_forwards_clamped_not_original():
    d = decide_enforcement(200, _ok("ClampLinear", 35.0, 0.0), PROPOSED)
    assert isinstance(d, Forward)
    assert d.action == "ClampLinear"
    assert d.enforced_v == 35.0  # the clamped ceiling, NOT the original 100.0


def test_clamp_steering_200_forwards_enforced():
    d = decide_enforcement(200, _ok("ClampSteering", 2.0, 35.0), PROPOSED)
    assert isinstance(d, Forward)
    assert d.enforced_s == 35.0  # NOT the original 90.0


def test_200_missing_enforced_linear_stops():
    parsed = {"action": "Allow", "enforced_steering_angle_deg": 0.0}
    assert isinstance(decide_enforcement(200, parsed, PROPOSED), Stop)


def test_200_missing_enforced_steering_stops():
    parsed = {"action": "Allow", "enforced_linear_velocity_mps": 10.0}
    assert isinstance(decide_enforcement(200, parsed, PROPOSED), Stop)


def test_200_missing_action_stops():
    parsed = {
        "enforced_linear_velocity_mps": 10.0,
        "enforced_steering_angle_deg": 2.0,
    }
    assert isinstance(decide_enforcement(200, parsed, PROPOSED), Stop)


def test_200_null_enforced_value_stops():
    assert isinstance(
        decide_enforcement(200, _ok("Allow", None, 0.0), PROPOSED), Stop
    )


def test_200_nan_enforced_value_stops():
    assert isinstance(
        decide_enforcement(200, _ok("Allow", float("nan"), 0.0), PROPOSED), Stop
    )


def test_200_inf_enforced_value_stops():
    assert isinstance(
        decide_enforcement(200, _ok("ClampLinear", float("inf"), 0.0), PROPOSED),
        Stop,
    )


def test_200_nonnumeric_enforced_value_stops():
    assert isinstance(
        decide_enforcement(200, _ok("Allow", "fast", 0.0), PROPOSED), Stop
    )


def test_200_bool_enforced_value_stops():
    # bool is an int subclass but is not a valid measurement.
    assert isinstance(
        decide_enforcement(200, _ok("Allow", True, 0.0), PROPOSED), Stop
    )


def test_200_non_json_body_stops():
    assert isinstance(decide_enforcement(200, None, PROPOSED), Stop)


def test_non_200_statuses_stop():
    for code in (400, 403, 503, 500):
        assert isinstance(decide_enforcement(code, None, PROPOSED), Stop), code


def test_stop_reason_is_a_short_tag():
    d = decide_enforcement(200, {"action": "Allow"}, PROPOSED)
    assert isinstance(d, Stop)
    assert isinstance(d.reason, str) and d.reason
