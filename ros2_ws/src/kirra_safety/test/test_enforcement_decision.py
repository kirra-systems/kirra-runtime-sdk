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


# ---------------------------------------------------------------------------
# Track-A A3 — wheelbase_consistent (single wheelbase source)
# ---------------------------------------------------------------------------

# Same standalone-module import style as the header import above (the
# sys.path insert makes `enforcement_decision` importable from any cwd;
# the packaged `kirra_safety.` form only resolves in an installed/colcon
# environment — review #904).
from enforcement_decision import (  # noqa: E402
    wheelbase_consistent, WHEELBASE_TOLERANCE_M,
)


def test_wheelbase_exact_match_is_consistent():
    assert wheelbase_consistent(0.229, 0.229)


def test_wheelbase_float_jitter_within_tolerance_is_consistent():
    assert wheelbase_consistent(0.229, 0.229 + WHEELBASE_TOLERANCE_M / 2)


def test_wheelbase_real_mismatch_is_inconsistent():
    # The exact live bug shape: interceptor default 0.2 vs courier 0.5 /
    # robotaxi 2.8 / the measured 0.229 — every pair must FAIL the check.
    for param, reported in ((0.2, 0.5), (0.2, 2.8), (0.2, 0.229), (0.5, 0.229)):
        assert not wheelbase_consistent(param, reported), (param, reported)


def test_wheelbase_missing_or_nonfinite_reported_fails_closed():
    # A verifier that mints but cannot say which wheelbase it used is a fault.
    for bad in (None, float("nan"), float("inf"), "0.229", True):
        assert not wheelbase_consistent(0.229, bad), bad


def test_wheelbase_bad_param_fails_closed():
    for bad in (None, float("nan"), "0.229"):
        assert not wheelbase_consistent(bad, 0.229), bad


# ---------------------------------------------------------------------------
# Live-loop relay — release_frame (release object -> 128-byte wire frame)
# ---------------------------------------------------------------------------

from enforcement_decision import (  # noqa: E402
    release_frame, RELEASE_PAYLOAD_LEN, RELEASE_TOKEN_LEN,
)

PAYLOAD = bytes(range(RELEASE_PAYLOAD_LEN))            # 32 distinct bytes
TOKEN = bytes(255 - (i % 256) for i in range(RELEASE_TOKEN_LEN))  # 96 bytes


def _release(payload=PAYLOAD, token=TOKEN, **extra):
    r = {"payload_hex": payload.hex(), "token_hex": token.hex()}
    r.update(extra)
    return r


def test_release_frame_valid_is_exact_carriage():
    frame = release_frame(_release())
    assert frame == PAYLOAD + TOKEN            # byte-exact, payload first
    assert len(frame) == 128                    # the consumer's strict length


def test_release_frame_extra_keys_ignored():
    # sequence/issued_at_ms/key_id/wheelbase_m ride alongside — carriage only
    # cares about the two hex fields.
    assert release_frame(_release(sequence=7, key_id="ab" * 32)) == PAYLOAD + TOKEN


def test_release_frame_absent_or_non_dict_is_none():
    for bad in (None, [], "release", 42, True):
        assert release_frame(bad) is None, bad


def test_release_frame_missing_or_non_string_hex_is_none():
    assert release_frame({"payload_hex": PAYLOAD.hex()}) is None      # no token
    assert release_frame({"token_hex": TOKEN.hex()}) is None          # no payload
    assert release_frame(_release() | {"payload_hex": None}) is None
    assert release_frame(_release() | {"token_hex": 123}) is None
    assert release_frame(_release() | {"payload_hex": list(PAYLOAD)}) is None


def test_release_frame_undecodable_hex_is_none():
    assert release_frame(_release() | {"payload_hex": "zz" * 32}) is None
    assert release_frame(_release() | {"token_hex": TOKEN.hex()[:-1]}) is None  # odd length


def test_release_frame_wrong_lengths_are_none():
    # The #901 lesson made structural: never slice/pad — off-by-one either
    # side of both fields is refused, no frame offered at all.
    for n in (0, RELEASE_PAYLOAD_LEN - 1, RELEASE_PAYLOAD_LEN + 1):
        assert release_frame(_release(payload=bytes(n))) is None, n
    for n in (0, RELEASE_TOKEN_LEN - 1, RELEASE_TOKEN_LEN + 1):
        assert release_frame(_release(token=bytes(n))) is None, n
