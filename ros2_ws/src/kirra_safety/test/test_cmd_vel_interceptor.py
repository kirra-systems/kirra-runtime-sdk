"""
Callback-level tests for the evidence-bound cmd_vel interceptor.

These pin the invariants that only exist at the node's input callback — what
reaches the HTTP request, what does NOT reach it, and what gets relayed — which
no pure helper can express on its own. (The envelope grammar itself is covered
purely in test_bound_proposal.py; this file deliberately does not re-test it.)

NO ROS GRAPH IS NEEDED. `rclpy` / `geometry_msgs` / `std_msgs` are stubbed into
sys.modules before the node module is imported, and the node instance is built
without running `Node.__init__` — so the callback runs against ordinary Python
objects. `requests` is replaced by a recorder, so "made no HTTP request" is an
assertion about a call count rather than about a network.

Run:  pytest ros2_ws/src/kirra_safety/test/test_cmd_vel_interceptor.py
"""

import json
import math
import os
import sys
import threading
import types

import pytest
import requests as real_requests


# ---------------------------------------------------------------------------
# ROS stubs — installed BEFORE importing the node module
# ---------------------------------------------------------------------------

class _Vec3:
    def __init__(self):
        self.x = 0.0
        self.y = 0.0
        self.z = 0.0


class Twist:
    def __init__(self):
        self.linear = _Vec3()
        self.angular = _Vec3()


class String:
    def __init__(self):
        self.data = ''


class Float64:
    def __init__(self):
        self.data = 0.0


class UInt8MultiArray:
    def __init__(self):
        self.data = []


def _install_ros_stubs():
    rclpy = types.ModuleType('rclpy')
    rclpy.init = lambda *a, **k: None
    rclpy.spin = lambda *a, **k: None
    rclpy.try_shutdown = lambda *a, **k: None

    rclpy_node = types.ModuleType('rclpy.node')

    class Node:
        def __init__(self, *a, **k):
            pass

    rclpy_node.Node = Node
    rclpy.node = rclpy_node

    rclpy_qos = types.ModuleType('rclpy.qos')
    rclpy_qos.QoSProfile = lambda **k: dict(k)
    rclpy_qos.ReliabilityPolicy = types.SimpleNamespace(RELIABLE=1, BEST_EFFORT=2)
    rclpy_qos.HistoryPolicy = types.SimpleNamespace(KEEP_LAST=1)
    rclpy.qos = rclpy_qos

    geometry_msgs = types.ModuleType('geometry_msgs')
    geometry_msgs_msg = types.ModuleType('geometry_msgs.msg')
    geometry_msgs_msg.Twist = Twist
    geometry_msgs.msg = geometry_msgs_msg

    std_msgs = types.ModuleType('std_msgs')
    std_msgs_msg = types.ModuleType('std_msgs.msg')
    std_msgs_msg.String = String
    std_msgs_msg.Float64 = Float64
    std_msgs_msg.UInt8MultiArray = UInt8MultiArray
    std_msgs.msg = std_msgs_msg

    sys.modules.update({
        'rclpy': rclpy,
        'rclpy.node': rclpy_node,
        'rclpy.qos': rclpy_qos,
        'geometry_msgs': geometry_msgs,
        'geometry_msgs.msg': geometry_msgs_msg,
        'std_msgs': std_msgs,
        'std_msgs.msg': std_msgs_msg,
    })


_install_ros_stubs()

# The node module imports `kirra_safety.*`, so the PACKAGE root goes on the path.
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "kirra_safety"))

from kirra_safety import cmd_vel_interceptor as itc  # noqa: E402
from kirra_safety.bound_proposal import encode_bound_proposal  # noqa: E402


# ---------------------------------------------------------------------------
# Fakes
# ---------------------------------------------------------------------------

PROFILE = "a" * 64
EVIDENCE = "b" * 64
PROPOSAL = "c" * 64

WHEELBASE_M = 0.2

# A valid evidence-bound V2 release: 176-byte payload + 96-byte token.
V2_PAYLOAD = bytes(range(176))
V2_TOKEN = bytes(255 - (i % 256) for i in range(96))


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


def _msg(data):
    m = String()
    m.data = data
    return m


def _envelope_msg(linear_x=0.4, linear_y=0.0, angular_z=0.0, binding=None):
    raw = encode_bound_proposal(
        linear_x, linear_y, angular_z, _binding() if binding is None else binding)
    assert raw is not None, "test envelope must be encodable"
    return _msg(raw)


class _Publisher:
    def __init__(self):
        self.published = []

    def publish(self, msg):
        self.published.append(msg)


class _Logger:
    def __init__(self):
        self.records = []

    def _log(self, level, msg):
        self.records.append((level, msg))

    def info(self, m):
        self._log('info', m)

    def warn(self, m):
        self._log('warn', m)

    def error(self, m):
        self._log('error', m)

    def debug(self, m):
        self._log('debug', m)

    def fatal(self, m):
        self._log('fatal', m)


class _Response:
    def __init__(self, status_code=200, body=None, raises=None):
        self.status_code = status_code
        self._body = body
        self._raises = raises

    def json(self):
        if self._raises is not None:
            raise self._raises
        return self._body


class _Requests:
    """Recorder standing in for the `requests` module."""

    # The node catches these by identity, so reuse the real classes.
    Timeout = real_requests.Timeout
    ConnectionError = real_requests.ConnectionError

    def __init__(self, responses=None):
        self.calls = []
        self._responses = list(responses or [])

    def post(self, url, headers=None, json=None, timeout=None):
        self.calls.append({'url': url, 'headers': headers,
                           'json': json, 'timeout': timeout})
        if not self._responses:
            return _Response(200, _ok_body())
        nxt = self._responses.pop(0)
        if isinstance(nxt, Exception):
            raise nxt
        return nxt

    @property
    def bodies(self):
        return [c['json'] for c in self.calls]


def _ok_body(v=0.4, s=0.0, release=True, **over):
    body = {
        'action': 'ALLOW',
        'enforced_linear_velocity_mps': v,
        'enforced_steering_angle_deg': s,
    }
    if release:
        body['release'] = {
            'version': 2,
            'payload_hex': V2_PAYLOAD.hex(),
            'token_hex': V2_TOKEN.hex(),
            'wheelbase_m': WHEELBASE_M,
            'key_id': 'ab' * 32,
        }
    body.update(over)
    return body


def _node(requests_stub=None, use_perception_cap=False):
    """Build a CmdVelInterceptor without running Node.__init__."""
    n = object.__new__(itc.CmdVelInterceptor)

    n._kirra_url = 'http://kirra.test'
    n._kirra_token = 'test-token'
    n._client_id = 'test-client'
    n._wheelbase_m = WHEELBASE_M
    n._wheelbase_mismatch = False
    n._max_speed_mps = 1.8
    n._timeout_s = 0.05
    n._fallback = 'stop'
    n._use_perception_cap = use_perception_cap
    n._cap_stale_s = 0.3

    n._current_velocity_mps = 0.0
    n._current_steering_deg = 0.0
    n._lock = threading.Lock()
    n._perception_cap = None
    n._perception_cap_time = None

    n._pub_safe = _Publisher()
    n._pub_action = _Publisher()
    n._pub_release = _Publisher()
    n._release_relay_announced = False
    n._release_malformed_announced = False
    n._proposal_invalid_announced = False

    n._logger = _Logger()
    n.get_logger = lambda: n._logger
    n.get_parameter = lambda name: types.SimpleNamespace(value='/kirra/release')

    n._requests = requests_stub or _Requests()
    return n


def _run(node, msg):
    """Drive one input message with the node's own requests recorder installed."""
    saved = itc.requests
    itc.requests = node._requests
    try:
        node._on_cmd_vel(msg)
    finally:
        itc.requests = saved
    return node._requests


def _actions(node):
    return [m.data for m in node._pub_action.published]


def _safe_twists(node):
    return node._pub_safe.published


# ---------------------------------------------------------------------------
# The valid path — binding reaches the request unchanged
# ---------------------------------------------------------------------------

def test_valid_proposal_posts_binding_unchanged():
    n = _node()
    r = _run(n, _envelope_msg())

    assert len(r.calls) == 1
    body = r.bodies[0]
    assert body['release_binding'] == _binding()


def test_posted_binding_is_byte_identical_to_the_envelope():
    # Not merely equal-ish: every digest string must survive with its exact
    # characters, and the whole binding must serialize to what was published.
    sent = _binding(
        profile_digest="0123456789abcdef" * 4,
        evidence_digest="fedcba9876543210" * 4,
        proposal_digest="00112233445566778899aabbccddeeff" * 2,
    )
    n = _node()
    r = _run(n, _envelope_msg(binding=sent))

    posted = r.bodies[0]['release_binding']
    assert posted == sent
    assert json.dumps(posted, sort_keys=True) == json.dumps(sent, sort_keys=True)


def test_valid_proposal_posts_the_kinematic_command_alongside_the_binding():
    n = _node()
    r = _run(n, _envelope_msg(linear_x=0.4, angular_z=0.0))

    body = r.bodies[0]
    assert body['linear_velocity_mps'] == pytest.approx(0.4)
    assert 'current_velocity_mps' in body and 'steering_angle_deg' in body
    assert body['release_binding'] == _binding()


def test_twist_values_and_binding_come_from_the_same_message():
    # Two DIFFERENT atomic messages back to back: each request must pair that
    # message's velocities with that message's binding — never a crossed pair.
    first_binding = _binding(scan_sequence=100, proposal_digest="1" * 64)
    second_binding = _binding(scan_sequence=200, proposal_digest="2" * 64)

    n = _node(_Requests([_Response(200, _ok_body(v=0.3)),
                         _Response(200, _ok_body(v=0.9))]))
    _run(n, _envelope_msg(linear_x=0.3, binding=first_binding))
    r = _run(n, _envelope_msg(linear_x=0.9, binding=second_binding))

    assert len(r.calls) == 2
    assert r.bodies[0]['linear_velocity_mps'] == pytest.approx(0.3)
    assert r.bodies[0]['release_binding'] == first_binding
    assert r.bodies[1]['linear_velocity_mps'] == pytest.approx(0.9)
    assert r.bodies[1]['release_binding'] == second_binding


def test_angular_z_reaches_the_request_as_bicycle_steering():
    # The reconstructed Twist — not the raw envelope — drives the conversion.
    n = _node()
    r = _run(n, _envelope_msg(linear_x=0.5, angular_z=0.4))

    expected = math.degrees(math.atan2(0.4 * WHEELBASE_M, 0.5))
    assert r.bodies[0]['steering_angle_deg'] == pytest.approx(expected)


def test_lateral_component_reaches_the_speed_magnitude():
    n = _node()
    r = _run(n, _envelope_msg(linear_x=0.3, linear_y=0.4))

    assert r.bodies[0]['linear_velocity_mps'] == pytest.approx(0.5)


# ---------------------------------------------------------------------------
# Malformed / empty / unbound — stop, and NO request
# ---------------------------------------------------------------------------

MALFORMED = [
    ("empty", ""),
    ("whitespace", "   "),
    ("not-json", "not json at all"),
    ("json-array", "[]"),
    ("json-null", "null"),
    ("bare-twist-like", json.dumps({"linear": {"x": 0.4}, "angular": {"z": 0.1}})),
    ("truncated", '{"linear_x":0.4,'),
]


@pytest.mark.parametrize("label,data", MALFORMED)
def test_malformed_proposal_stops_and_makes_no_request(label, data):
    n = _node()
    r = _run(n, _msg(data))

    assert r.calls == [], label
    assert len(_safe_twists(n)) == 1, label
    stopped = _safe_twists(n)[0]
    assert (stopped.linear.x, stopped.linear.y, stopped.angular.z) == (0.0, 0.0, 0.0)
    assert _actions(n) == ['BLOCKED:INVALID_BOUND_PROPOSAL'], label
    assert n._pub_release.published == [], label


def test_missing_binding_fails_closed():
    raw = json.dumps({"linear_x": 0.4, "linear_y": 0.0, "angular_z": 0.1})
    n = _node()
    r = _run(n, _msg(raw))

    assert r.calls == []
    assert _actions(n) == ['BLOCKED:INVALID_BOUND_PROPOSAL']


def test_null_binding_fails_closed():
    raw = json.dumps({"linear_x": 0.4, "linear_y": 0.0,
                      "angular_z": 0.1, "release_binding": None})
    n = _node()
    assert _run(n, _msg(raw)).calls == []
    assert _actions(n) == ['BLOCKED:INVALID_BOUND_PROPOSAL']


@pytest.mark.parametrize("label,binding", [
    ("uppercase-profile", _binding(profile_digest="A" * 64)),
    ("uppercase-evidence", _binding(evidence_digest="B" * 64)),
    ("uppercase-proposal", _binding(proposal_digest="C" * 64)),
    ("short-digest", _binding(profile_digest="a" * 63)),
    ("long-digest", _binding(profile_digest="a" * 65)),
    ("non-hex-digest", _binding(evidence_digest="g" * 64)),
    ("empty-digest", _binding(proposal_digest="")),
    ("zero-scan", _binding(scan_sequence=0)),
    ("zero-tracker", _binding(tracker_generation=0)),
    ("zero-camera", _binding(camera_sequence=0)),
    ("negative-scan", _binding(scan_sequence=-1)),
    ("float-scan", _binding(scan_sequence=5.0)),
    ("string-scan", _binding(scan_sequence="813")),
])
def test_invalid_binding_fields_fail_closed_without_a_request(label, binding):
    # Built by hand: encode_bound_proposal would refuse these outright, which
    # is the point — this is what an out-of-spec doer would put on the wire.
    raw = json.dumps({"linear_x": 0.4, "linear_y": 0.0,
                      "angular_z": 0.1, "release_binding": binding})
    n = _node()
    r = _run(n, _msg(raw))

    assert r.calls == [], label
    assert _actions(n) == ['BLOCKED:INVALID_BOUND_PROPOSAL'], label
    assert n._pub_release.published == [], label


def test_non_finite_velocity_fails_closed_without_a_request():
    raw = ('{"linear_x":NaN,"linear_y":0.0,"angular_z":0.0,"release_binding":%s}'
           % json.dumps(_binding()))
    n = _node()
    assert _run(n, _msg(raw)).calls == []
    assert _actions(n) == ['BLOCKED:INVALID_BOUND_PROPOSAL']


# ---------------------------------------------------------------------------
# No stale reuse across messages
# ---------------------------------------------------------------------------

def test_no_stale_binding_is_reused_after_a_malformed_message():
    good = _binding(scan_sequence=100, proposal_digest="1" * 64)
    n = _node(_Requests([_Response(200, _ok_body())]))

    _run(n, _envelope_msg(linear_x=0.4, binding=good))
    assert len(n._requests.calls) == 1

    # A malformed message must not re-send the previous good binding.
    _run(n, _msg("garbage"))
    assert len(n._requests.calls) == 1, "malformed message must make no request"
    assert _actions(n)[-1] == 'BLOCKED:INVALID_BOUND_PROPOSAL'


def test_after_a_malformed_message_the_next_valid_one_uses_its_own_binding():
    first = _binding(scan_sequence=100, proposal_digest="1" * 64)
    second = _binding(scan_sequence=200, proposal_digest="2" * 64)
    n = _node(_Requests([_Response(200, _ok_body()), _Response(200, _ok_body())]))

    _run(n, _envelope_msg(binding=first))
    _run(n, _msg(""))
    r = _run(n, _envelope_msg(binding=second))

    assert len(r.calls) == 2
    assert r.bodies[1]['release_binding'] == second


def test_malformed_message_zeroes_the_rate_limit_reference_speed():
    # `current_velocity_mps` feeds the governor's rate-of-change check. After a
    # stop it must report zero, not the last enforced speed.
    n = _node(_Requests([_Response(200, _ok_body(v=1.2)), _Response(200, _ok_body())]))
    _run(n, _envelope_msg(linear_x=1.2))
    assert n._current_velocity_mps == pytest.approx(1.2)

    _run(n, _msg("garbage"))
    assert n._current_velocity_mps == 0.0

    r = _run(n, _envelope_msg(linear_x=0.4))
    assert r.bodies[-1]['current_velocity_mps'] == 0.0


def test_repeated_malformed_messages_log_once_then_rearm():
    n = _node(_Requests([_Response(200, _ok_body())]))
    for _ in range(5):
        _run(n, _msg("garbage"))
    warns = [m for lvl, m in n._logger.records if lvl == 'warn']
    assert len(warns) == 1, "latched log must not spam per message"

    _run(n, _envelope_msg())          # a valid envelope re-arms the latch
    _run(n, _msg("garbage"))
    warns = [m for lvl, m in n._logger.records if lvl == 'warn']
    assert len(warns) == 2


# ---------------------------------------------------------------------------
# Release relay — exactly 176 + 96, and never on a refusal
# ---------------------------------------------------------------------------

def test_valid_response_relays_exactly_176_payload_plus_96_token():
    n = _node()
    _run(n, _envelope_msg())

    assert len(n._pub_release.published) == 1
    frame = bytes(n._pub_release.published[0].data)
    assert len(frame) == 272
    assert frame[:176] == V2_PAYLOAD
    assert frame[176:] == V2_TOKEN


def test_v1_length_release_is_not_relayed():
    # A 32-byte V1 payload carries no evidence binding — relaying it would be a
    # silent downgrade of the live path.
    v1 = _ok_body()
    v1['release']['payload_hex'] = bytes(range(32)).hex()
    n = _node(_Requests([_Response(200, v1)]))
    _run(n, _envelope_msg())

    assert n._pub_release.published == []


def test_no_relay_when_response_carries_no_release():
    n = _node(_Requests([_Response(200, _ok_body(release=False))]))
    _run(n, _envelope_msg())

    assert n._pub_release.published == []
    assert len(_safe_twists(n)) == 1        # still forwarded to the motors


def test_no_relay_on_denied_response():
    n = _node(_Requests([_Response(400, {'error': 'denied'})]))
    _run(n, _envelope_msg())

    assert n._requests.calls != []
    assert n._pub_release.published == []
    stopped = _safe_twists(n)[0]
    assert stopped.linear.x == 0.0


def test_no_relay_on_posture_lockout():
    n = _node(_Requests([_Response(403, {'posture': 'locked_out'})]))
    _run(n, _envelope_msg())

    assert n._pub_release.published == []
    assert 'BLOCKED:HTTP_403' in _actions(n)


def test_no_relay_on_contract_violating_200():
    # A 200 without the canonical enforcement keys stops the robot.
    n = _node(_Requests([_Response(200, {'action': 'ALLOW'})]))
    _run(n, _envelope_msg())

    assert n._pub_release.published == []
    assert _safe_twists(n)[0].linear.x == 0.0


def test_no_relay_on_malformed_release_hex():
    bad = _ok_body()
    bad['release']['token_hex'] = 'zz' * 96
    n = _node(_Requests([_Response(200, bad)]))
    _run(n, _envelope_msg())

    assert n._pub_release.published == []


# ---------------------------------------------------------------------------
# Preserved safety behaviour
# ---------------------------------------------------------------------------

def test_wheelbase_mismatch_latches_and_stops_subsequent_valid_proposals():
    mismatched = _ok_body()
    mismatched['release']['wheelbase_m'] = WHEELBASE_M * 2
    n = _node(_Requests([_Response(200, mismatched)]))

    _run(n, _envelope_msg())
    assert n._wheelbase_mismatch is True
    assert 'BLOCKED:WHEELBASE_MISMATCH' in _actions(n)
    assert n._pub_release.published == []

    # Latched: a later perfectly valid proposal must not even be sent.
    before = len(n._requests.calls)
    _run(n, _envelope_msg())
    assert len(n._requests.calls) == before
    assert _safe_twists(n)[-1].linear.x == 0.0


def test_missing_reported_wheelbase_fails_closed():
    body = _ok_body()
    del body['release']['wheelbase_m']
    n = _node(_Requests([_Response(200, body)]))
    _run(n, _envelope_msg())

    assert n._wheelbase_mismatch is True
    assert n._pub_release.published == []


def test_timeout_falls_back_to_stop_and_relays_nothing():
    n = _node(_Requests([real_requests.Timeout()]))
    _run(n, _envelope_msg())

    assert _safe_twists(n)[0].linear.x == 0.0
    assert 'TIMEOUT:STOP' in _actions(n)
    assert n._pub_release.published == []


def test_timeout_passthrough_publishes_the_reconstructed_twist():
    # The fallback must publish a Twist rebuilt from the envelope — publishing
    # the raw String onto the Twist topic would be a type error at runtime.
    n = _node(_Requests([real_requests.Timeout()]))
    n._fallback = 'passthrough'
    _run(n, _envelope_msg(linear_x=0.6, angular_z=0.2))

    published = _safe_twists(n)[0]
    assert isinstance(published, Twist)
    assert published.linear.x == pytest.approx(0.6)
    assert published.angular.z == pytest.approx(0.2)


def test_connection_error_passthrough_publishes_the_reconstructed_twist():
    n = _node(_Requests([real_requests.ConnectionError()]))
    n._fallback = 'passthrough'
    _run(n, _envelope_msg(linear_x=0.7))

    published = _safe_twists(n)[0]
    assert isinstance(published, Twist)
    assert published.linear.x == pytest.approx(0.7)


def test_connection_error_stops_by_default():
    n = _node(_Requests([real_requests.ConnectionError()]))
    _run(n, _envelope_msg())

    assert _safe_twists(n)[0].linear.x == 0.0
    assert 'CONNECTION_ERROR:STOP' in _actions(n)


def test_perception_cap_tightens_the_posted_speed_and_keeps_the_binding():
    n = _node(use_perception_cap=True)
    n._perception_cap = 0.2
    n._perception_cap_time = itc.time.monotonic()

    r = _run(n, _envelope_msg(linear_x=1.0))
    body = r.bodies[0]
    assert body['linear_velocity_mps'] == pytest.approx(0.2)
    assert body['release_binding'] == _binding()


def test_stale_perception_cap_fails_closed_but_still_carries_the_binding():
    n = _node(use_perception_cap=True)
    n._perception_cap = 0.5
    n._perception_cap_time = itc.time.monotonic() - 10.0

    r = _run(n, _envelope_msg(linear_x=1.0))
    body = r.bodies[0]
    assert body['linear_velocity_mps'] == pytest.approx(0.0)
    assert body['release_binding'] == _binding()


def test_enforced_response_is_forwarded_not_the_proposal():
    n = _node(_Requests([_Response(200, _ok_body(v=0.15, s=0.0))]))
    _run(n, _envelope_msg(linear_x=1.0))

    published = _safe_twists(n)[0]
    assert published.linear.x == pytest.approx(0.15)


def test_headers_carry_token_and_client_id():
    n = _node()
    r = _run(n, _envelope_msg())

    headers = r.calls[0]['headers']
    assert headers['Authorization'] == 'Bearer test-token'
    assert headers['x-kirra-client-id'] == 'test-client'


def test_request_targets_the_actuator_endpoint():
    n = _node()
    r = _run(n, _envelope_msg())
    assert r.calls[0]['url'] == 'http://kirra.test/actuator/motion/command'
