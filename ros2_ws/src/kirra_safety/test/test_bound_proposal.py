"""
Unit tests for the atomic evidence-bound proposal envelope.

No ROS / rclpy needed — imports the pure `bound_proposal` module directly.

The load-bearing property is that the parse NEVER repairs. Every fault returns
None so the caller stops the robot; a repaired identity would be signed by the
verifier as though the doer had authored it, which is precisely the forgery the
binding exists to prevent.

Run:  pytest ros2_ws/src/kirra_safety/test/test_bound_proposal.py
"""

import json
import os
import sys

# Import the pure module standalone (no installed package, no ROS).
sys.path.insert(
    0, os.path.join(os.path.dirname(__file__), "..", "kirra_safety")
)
from bound_proposal import (  # noqa: E402
    BINDING_FIELDS, DIGEST_FIELDS, U64_MAX, build_release_binding,
    encode_bound_proposal, is_lowercase_sha256_hex, parse_bound_proposal,
    validate_release_binding,
)

PROFILE = "a" * 64
EVIDENCE = "b" * 64
PROPOSAL = "c" * 64


def _binding(**over):
    b = {
        "scan_sequence": 813,
        "camera_sequence": 41,
        "tracker_generation": 7,
        "profile_digest": PROFILE,
        "evidence_digest": EVIDENCE,
        "proposal_digest": PROPOSAL,
    }
    b.update(over)
    return b


# Distinct from None, so a test can deliberately build an envelope whose
# release_binding IS null.
_DEFAULT = object()


def _envelope(linear_x=0.4, linear_y=0.0, angular_z=0.1, binding=_DEFAULT, **over):
    e = {
        "linear_x": linear_x,
        "linear_y": linear_y,
        "angular_z": angular_z,
        "release_binding": _binding() if binding is _DEFAULT else binding,
    }
    e.update(over)
    return json.dumps(e)


# ---------------------------------------------------------------------------
# The happy path
# ---------------------------------------------------------------------------

def test_valid_envelope_parses_with_exact_values():
    p = parse_bound_proposal(_envelope())
    assert p is not None
    assert (p.linear_x, p.linear_y, p.angular_z) == (0.4, 0.0, 0.1)
    assert p.release_binding == _binding()


def test_binding_digests_are_passed_through_byte_for_byte():
    # The whole contract: what the doer wrote is what the verifier signs.
    digests = {
        "profile_digest": "0123456789abcdef" * 4,
        "evidence_digest": "fedcba9876543210" * 4,
        "proposal_digest": "00112233445566778899aabbccddeeff" * 2,
    }
    p = parse_bound_proposal(_envelope(binding=_binding(**digests)))
    assert p is not None
    for field, value in digests.items():
        assert p.release_binding[field] == value


def test_parsed_binding_carries_exactly_the_canonical_fields():
    p = parse_bound_proposal(_envelope())
    assert set(p.release_binding) == set(BINDING_FIELDS)


def test_unknown_binding_keys_are_dropped_not_forwarded():
    # A doer must not be able to smuggle unvalidated fields to the verifier.
    p = parse_bound_proposal(
        _envelope(binding=_binding(operator_override=True, priority="high")))
    assert p is not None
    assert set(p.release_binding) == set(BINDING_FIELDS)


def test_absent_camera_sequence_is_none():
    b = _binding()
    del b["camera_sequence"]
    p = parse_bound_proposal(_envelope(binding=b))
    assert p is not None and p.release_binding["camera_sequence"] is None


def test_null_camera_sequence_is_none():
    p = parse_bound_proposal(_envelope(binding=_binding(camera_sequence=None)))
    assert p is not None and p.release_binding["camera_sequence"] is None


def test_negative_and_zero_velocities_are_valid():
    # Reverse and full-stop are legitimate proposals; only NON-FINITE is not.
    p = parse_bound_proposal(_envelope(linear_x=-0.3, linear_y=0.0, angular_z=0.0))
    assert p is not None and p.linear_x == -0.3


def test_integer_velocities_are_accepted_as_floats():
    p = parse_bound_proposal(_envelope(linear_x=1, linear_y=0, angular_z=-1))
    assert p is not None
    assert isinstance(p.linear_x, float) and p.linear_x == 1.0


# ---------------------------------------------------------------------------
# Malformed / empty input — every fault is a None
# ---------------------------------------------------------------------------

def test_empty_and_whitespace_data_is_none():
    # The doer's explicit hold, and the default-constructed String message.
    for bad in ("", "   ", "\n", "\t\n "):
        assert parse_bound_proposal(bad) is None, repr(bad)


def test_non_string_input_is_none():
    for bad in (None, 42, 3.5, True, b"{}", [], {}, object()):
        assert parse_bound_proposal(bad) is None, repr(bad)


def test_undecodable_json_is_none():
    for bad in ("{", "not json", "{'single': 'quotes'}", '{"a":}', "[1,2,3"):
        assert parse_bound_proposal(bad) is None, repr(bad)


def test_non_object_json_top_level_is_none():
    for bad in ("[]", '"a string"', "42", "null", "true", "[{}]"):
        assert parse_bound_proposal(bad) is None, repr(bad)


def test_missing_velocity_component_is_none():
    for field in ("linear_x", "linear_y", "angular_z"):
        payload = json.loads(_envelope())
        del payload[field]
        assert parse_bound_proposal(json.dumps(payload)) is None, field


def test_non_finite_velocity_is_none():
    # Bare NaN/Infinity tokens are accepted by json.loads by default; the
    # parser must refuse them rather than hand a non-finite to the governor.
    for token in ("NaN", "Infinity", "-Infinity"):
        raw = '{"linear_x":%s,"linear_y":0.0,"angular_z":0.0,"release_binding":%s}' % (
            token, json.dumps(_binding()))
        assert parse_bound_proposal(raw) is None, token


def test_non_numeric_velocity_is_none():
    for bad in ('"0.4"', "true", "null", "[]", "{}"):
        raw = '{"linear_x":%s,"linear_y":0.0,"angular_z":0.0,"release_binding":%s}' % (
            bad, json.dumps(_binding()))
        assert parse_bound_proposal(raw) is None, bad


# ---------------------------------------------------------------------------
# The binding must be present and whole
# ---------------------------------------------------------------------------

def test_missing_release_binding_is_none():
    raw = json.dumps({"linear_x": 0.4, "linear_y": 0.0, "angular_z": 0.1})
    assert parse_bound_proposal(raw) is None


def test_null_or_non_object_release_binding_is_none():
    for bad in (None, [], "binding", 42, True):
        assert parse_bound_proposal(_envelope(binding=bad)) is None, repr(bad)


def test_each_missing_binding_field_is_none():
    # camera_sequence is the one legitimately-optional field.
    for field in ("scan_sequence", "tracker_generation") + DIGEST_FIELDS:
        b = _binding()
        del b[field]
        assert parse_bound_proposal(_envelope(binding=b)) is None, field


# ---------------------------------------------------------------------------
# Digest strictness — mirrors decode_lowercase_sha256 in policy_layer.rs
# ---------------------------------------------------------------------------

def test_uppercase_digest_is_none():
    # Refused, never folded: two spellings of one identity would let the same
    # evidence be presented under two different-looking bindings.
    for field in DIGEST_FIELDS:
        assert parse_bound_proposal(
            _envelope(binding=_binding(**{field: "A" * 64}))) is None, field
        assert parse_bound_proposal(
            _envelope(binding=_binding(**{field: "aBcD" + "e" * 60}))) is None, field


def test_wrong_length_digest_is_none():
    for field in DIGEST_FIELDS:
        for bad in ("", "a" * 63, "a" * 65, "a" * 32, "a" * 128):
            assert parse_bound_proposal(
                _envelope(binding=_binding(**{field: bad}))) is None, (field, len(bad))


def test_non_hex_digest_is_none():
    for field in DIGEST_FIELDS:
        for bad in ("g" * 64, "z" * 64, "-" * 64, " " * 64, "a" * 63 + "!",
                    "0x" + "a" * 62):
            assert parse_bound_proposal(
                _envelope(binding=_binding(**{field: bad}))) is None, (field, bad)


def test_non_string_digest_is_none():
    for field in DIGEST_FIELDS:
        for bad in (None, 42, True, [], {}):
            assert parse_bound_proposal(
                _envelope(binding=_binding(**{field: bad}))) is None, (field, bad)


def test_is_lowercase_sha256_hex_predicate():
    assert is_lowercase_sha256_hex("0" * 64)
    assert is_lowercase_sha256_hex("0123456789abcdef" * 4)
    for bad in ("A" * 64, "a" * 63, "a" * 65, "", None, 42, "g" * 64):
        assert not is_lowercase_sha256_hex(bad), repr(bad)


# ---------------------------------------------------------------------------
# Sequence strictness — mirrors ActuatorReleaseBinding::validate
# ---------------------------------------------------------------------------

def test_zero_sequences_are_none():
    # Zero is the verifier's explicit "no such frame" sentinel.
    for field in ("scan_sequence", "tracker_generation", "camera_sequence"):
        assert parse_bound_proposal(
            _envelope(binding=_binding(**{field: 0}))) is None, field


def test_negative_sequences_are_none():
    for field in ("scan_sequence", "tracker_generation", "camera_sequence"):
        assert parse_bound_proposal(
            _envelope(binding=_binding(**{field: -1}))) is None, field


def test_float_sequences_are_none():
    # serde's u64 refuses a JSON float, so accepting one would guarantee a 400.
    for field in ("scan_sequence", "tracker_generation", "camera_sequence"):
        for bad in (5.0, 5.5):
            assert parse_bound_proposal(
                _envelope(binding=_binding(**{field: bad}))) is None, (field, bad)


def test_non_numeric_sequences_are_none():
    for field in ("scan_sequence", "tracker_generation", "camera_sequence"):
        for bad in ("813", True, [], {}):
            assert parse_bound_proposal(
                _envelope(binding=_binding(**{field: bad}))) is None, (field, bad)


def test_sequence_at_u64_bounds():
    assert parse_bound_proposal(
        _envelope(binding=_binding(scan_sequence=U64_MAX))) is not None
    assert parse_bound_proposal(
        _envelope(binding=_binding(scan_sequence=U64_MAX + 1))) is None


# ---------------------------------------------------------------------------
# validate_release_binding directly
# ---------------------------------------------------------------------------

def test_validate_release_binding_returns_canonical_dict():
    out = validate_release_binding(_binding())
    assert out == _binding()
    assert set(out) == set(BINDING_FIELDS)


def test_validate_release_binding_rejects_non_dict():
    for bad in (None, [], "x", 42, True):
        assert validate_release_binding(bad) is None, repr(bad)


# ---------------------------------------------------------------------------
# encode_bound_proposal — canonical, and a round trip
# ---------------------------------------------------------------------------

def test_encode_round_trips_through_parse():
    raw = encode_bound_proposal(0.42, -0.1, 0.33, _binding())
    assert raw is not None
    p = parse_bound_proposal(raw)
    assert p is not None
    assert (p.linear_x, p.linear_y, p.angular_z) == (0.42, -0.1, 0.33)
    assert p.release_binding == _binding()


def test_encode_is_canonical_sorted_and_compact():
    raw = encode_bound_proposal(0.4, 0.0, 0.1, _binding())
    assert raw == json.dumps(json.loads(raw), sort_keys=True, separators=(",", ":"))
    assert ", " not in raw and '": ' not in raw


def test_encode_is_deterministic():
    a = encode_bound_proposal(0.4, 0.0, 0.1, _binding())
    b = encode_bound_proposal(0.4, 0.0, 0.1, dict(reversed(list(_binding().items()))))
    assert a == b


def test_encode_refuses_non_finite_velocity():
    for bad in (float("nan"), float("inf"), float("-inf")):
        assert encode_bound_proposal(bad, 0.0, 0.0, _binding()) is None, bad
        assert encode_bound_proposal(0.0, bad, 0.0, _binding()) is None, bad
        assert encode_bound_proposal(0.0, 0.0, bad, _binding()) is None, bad


def test_encode_refuses_invalid_binding():
    # A doer cannot publish an envelope its own interceptor would refuse.
    assert encode_bound_proposal(0.4, 0.0, 0.1, None) is None
    assert encode_bound_proposal(0.4, 0.0, 0.1, _binding(scan_sequence=0)) is None
    assert encode_bound_proposal(
        0.4, 0.0, 0.1, _binding(profile_digest="A" * 64)) is None


def test_encoded_null_camera_sequence_round_trips():
    raw = encode_bound_proposal(0.4, 0.0, 0.1, _binding(camera_sequence=None))
    assert '"camera_sequence":null' in raw
    assert parse_bound_proposal(raw).release_binding["camera_sequence"] is None


# ---------------------------------------------------------------------------
# build_release_binding — the doer's frame + digest composition
# ---------------------------------------------------------------------------

def _frame(**over):
    f = {
        "scan_sequence": 813,
        "camera_sequence": 41,
        "tracker_generation": 7,
        "profile_digest": PROFILE,
        "evidence_digest": EVIDENCE,
    }
    f.update(over)
    return f


def test_build_release_binding_composes_frame_and_digest():
    assert build_release_binding(_frame(), PROPOSAL) == _binding()


def test_build_release_binding_rejects_taj_invalid_frame_sentinel():
    # PerceptionFrameId::invalid() — zero sequences, empty digests.
    sentinel = {
        "scan_sequence": 0, "camera_sequence": None, "tracker_generation": 0,
        "profile_digest": "", "evidence_digest": "",
    }
    assert build_release_binding(sentinel, PROPOSAL) is None


def test_build_release_binding_rejects_missing_frame_or_digest():
    assert build_release_binding(None, PROPOSAL) is None
    assert build_release_binding(_frame(), None) is None
    assert build_release_binding(_frame(), "") is None
    assert build_release_binding(_frame(), "A" * 64) is None
    assert build_release_binding("not-a-frame", PROPOSAL) is None


def test_build_release_binding_output_survives_encode_and_parse():
    binding = build_release_binding(_frame(), PROPOSAL)
    raw = encode_bound_proposal(0.4, 0.0, 0.1, binding)
    assert parse_bound_proposal(raw).release_binding == binding
