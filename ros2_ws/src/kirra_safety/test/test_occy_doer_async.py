"""
Callback-level tests for the doer's non-blocking plan cycle.

These pin the invariants that only exist where the timer callback meets the
background worker — that `_tick` returns while HTTP is still blocked, that a
slow sidecar cannot pile up threads, and that a reply about the wrong scan is
discarded rather than published. The decision algebra itself is covered purely
in test_async_plan.py; this file deliberately does not re-test it.

NO ROS GRAPH IS NEEDED. `rclpy` / the message packages are stubbed into
sys.modules before the node module is imported, and the node instance is built
without running `Node.__init__` — so callbacks run against ordinary Python
objects. `requests` is replaced by a recorder whose responses (and blocking) the
test controls, so "did not wait for HTTP" is an assertion about wall-clock and a
call count rather than about a network.

Run:  pytest ros2_ws/src/kirra_safety/test/test_occy_doer_async.py
"""

import ast
import json
import os
import sys
import threading
import time
import types

import pytest


# ---------------------------------------------------------------------------
# ROS stubs — installed BEFORE importing the node module
# ---------------------------------------------------------------------------

class _Vec3:
    def __init__(self):
        self.x = 0.0
        self.y = 0.0
        self.z = 0.0


class _Quat:
    def __init__(self):
        self.x = 0.0
        self.y = 0.0
        self.z = 0.0
        self.w = 1.0


class _Stamp:
    def __init__(self, sec=0, nanosec=0):
        self.sec = sec
        self.nanosec = nanosec


class _Header:
    def __init__(self, sec=0, nanosec=0):
        self.stamp = _Stamp(sec, nanosec)


class String:
    def __init__(self):
        self.data = ''


class PoseStamped:
    pass


class Odometry:
    pass


class LaserScan:
    """Only the fields the doer reads."""

    def __init__(self, stamp_ms=1_000, ranges=None):
        self.header = _Header(stamp_ms // 1000, (stamp_ms % 1000) * 1_000_000)
        self.angle_min = -1.5
        self.angle_increment = 0.01
        self.range_min = 0.1
        self.range_max = 12.0
        self.ranges = [1.0] * 8 if ranges is None else list(ranges)


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
    geometry_msgs_msg.PoseStamped = PoseStamped
    geometry_msgs.msg = geometry_msgs_msg

    nav_msgs = types.ModuleType('nav_msgs')
    nav_msgs_msg = types.ModuleType('nav_msgs.msg')
    nav_msgs_msg.Odometry = Odometry
    nav_msgs.msg = nav_msgs_msg

    sensor_msgs = types.ModuleType('sensor_msgs')
    sensor_msgs_msg = types.ModuleType('sensor_msgs.msg')
    sensor_msgs_msg.LaserScan = LaserScan
    sensor_msgs.msg = sensor_msgs_msg

    std_msgs = types.ModuleType('std_msgs')
    std_msgs_msg = types.ModuleType('std_msgs.msg')
    std_msgs_msg.String = String
    std_msgs.msg = std_msgs_msg

    sys.modules.update({
        'rclpy': rclpy,
        'rclpy.node': rclpy_node,
        'rclpy.qos': rclpy_qos,
        'geometry_msgs': geometry_msgs,
        'geometry_msgs.msg': geometry_msgs_msg,
        'nav_msgs': nav_msgs,
        'nav_msgs.msg': nav_msgs_msg,
        'sensor_msgs': sensor_msgs,
        'sensor_msgs.msg': sensor_msgs_msg,
        'std_msgs': std_msgs,
        'std_msgs.msg': std_msgs_msg,
    })


_install_ros_stubs()

# The node module imports `kirra_safety.*`, so the directory CONTAINING the
# `kirra_safety` python package goes on the path — that is the ROS package root
# one level up, not `src/` (which would resolve `kirra_safety` to the ROS
# package directory and give a namespace package with no modules in it).
_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(_HERE, "..", "kirra_safety"))
sys.path.insert(0, os.path.join(_HERE, ".."))

from kirra_safety import occy_doer as doer  # noqa: E402
from kirra_safety.async_plan import HoldMonitor, JobResult  # noqa: E402
from kirra_safety.bound_proposal import (  # noqa: E402
    build_release_binding, encode_bound_proposal,
)
from kirra_safety.doer_core import decide, extend_corridor_back  # noqa: E402
from kirra_safety.sensor_freshness import (  # noqa: E402
    FrameHealthConfig, FrameHealthTracker,
)

DOER_SOURCE = os.path.join(_HERE, "..", "kirra_safety", "occy_doer.py")

# A worker that never returns must never make a test hang either.
JOIN_TIMEOUT_S = 5.0

PROFILE = "a" * 64
EVIDENCE = "b" * 64
PROPOSAL = "c" * 64


# ---------------------------------------------------------------------------
# Fakes
# ---------------------------------------------------------------------------

class _Publisher:
    def __init__(self):
        self.published = []

    def publish(self, msg):
        self.published.append(msg.data)


class _Logger:
    def __init__(self):
        self.records = []

    def _log(self, level, msg):
        self.records.append((level, str(msg)))

    info = lambda self, m: self._log('info', m)      # noqa: E731
    warn = lambda self, m: self._log('warn', m)      # noqa: E731
    error = lambda self, m: self._log('error', m)    # noqa: E731
    debug = lambda self, m: self._log('debug', m)    # noqa: E731
    fatal = lambda self, m: self._log('fatal', m)    # noqa: E731

    def text(self):
        return " | ".join(m for _, m in self.records)


class _Response:
    def __init__(self, body):
        self._body = body

    def json(self):
        return self._body


class _Requests:
    """Recorder standing in for the `requests` module.

    `gate` (an Event) lets a test hold a POST open inside the worker for as long
    as it likes — which is how "the tick did not wait for HTTP" is asserted
    without a real slow server.
    """

    def __init__(self, taj=None, plan=None, gate=None, raises=None):
        self.calls = []
        self.entered = threading.Event()
        self._taj = taj if taj is not None else _taj_body()
        self._plan = plan if plan is not None else _plan_body()
        self._gate = gate
        self._raises = raises or {}
        self._lock = threading.Lock()

    def post(self, url, json=None, timeout=None):
        with self._lock:
            self.calls.append({'url': url, 'json': json, 'timeout': timeout})
        self.entered.set()
        if self._gate is not None:
            self._gate.wait(JOIN_TIMEOUT_S)
        which = 'perception' if url.endswith('/perception') else 'plan'
        if which in self._raises:
            raise self._raises[which]
        return _Response(self._taj if which == 'perception' else self._plan)

    def get(self, url, timeout=None):  # pragma: no cover — Mick is off by default
        with self._lock:
            self.calls.append({'url': url, 'json': None, 'timeout': timeout})
        return _Response({})

    @property
    def urls(self):
        with self._lock:
            return [c['url'] for c in self.calls]

    def count(self, suffix):
        return sum(1 for u in self.urls if u.endswith(suffix))


def _frame_id(scan_sequence=11, **over):
    frame = {
        'scan_sequence': scan_sequence,
        'camera_sequence': 3,
        'tracker_generation': 2,
        'profile_digest': PROFILE,
        'evidence_digest': EVIDENCE,
    }
    frame.update(over)
    return frame


def _taj_body(**over):
    body = {
        # Taj returns corridor polylines as [x, y] pairs.
        'left': [[0.0, 0.6], [6.0, 0.6]],
        'right': [[0.0, -0.6], [6.0, -0.6]],
        'objects': [],
        'pedestrians': [],
        'predicted_vrus': [],
        'frame_id': _frame_id(),
        'camera_healthy': True,
    }
    body.update(over)
    return body


def _plan_body(**over):
    body = {
        'kind': 'Motion',
        'verdict': 'Accept',
        'trajectory': [
            {'x': 0.0, 'y': 0.0, 'speed': 0.8},
            {'x': 1.0, 'y': 0.0, 'speed': 0.8},
            {'x': 2.0, 'y': 0.0, 'speed': 0.8},
        ],
        'perception_frame_id': _frame_id(),
        'proposal_digest': PROPOSAL,
    }
    body.update(over)
    return body


def _node(requests_stub, **over):
    """Build an OccyDoer without running Node.__init__ (no ROS graph)."""
    n = object.__new__(doer.OccyDoer)

    n._taj = 'http://taj.test'
    n._planner = 'http://planner.test'
    n._mick = ''
    n._mick_seq = 0
    n._mick_ignored = set()
    n._cruise = 1.2
    n._max_v = 1.2
    n._max_w = 1.5
    n._lookahead = 0.8
    n._goal_tol = 0.25
    n._extent = 8.0
    n._scan_stale_s = 0.25
    n._timeout_s = 0.05
    n._back_m = 0.5

    n._frame_health_enabled = False
    n._frame_health = FrameHealthTracker(FrameHealthConfig.disarmed())
    n._frame_health_reason = None

    n._job_lock = threading.Lock()
    n._job_in_flight = False
    n._pending_result = None
    n._job_started_s = None
    n._scan_seq = 0
    n._last_used_scan_sequence = 0
    n._last_issued_scan_sequence = 0
    n._hold_monitor = HoldMonitor()
    n._job_overdue_warned = False

    n._mick_lock = threading.Lock()
    n._mick_in_flight = False
    n._mick_result = None

    n._vehicle = {
        'class': 'courier', 'wheelbase_m': 0.2, 'half_length_m': 0.18,
        'half_width_m': 0.15, 'max_speed_mps': 1.2, 'max_steering_deg': 30.0,
        'rss_lateral_alignment_tolerance_m': 0.6,
        'lateral_clearance_target_m': 0.6,
    }
    n._camera_armed = False
    n._camera_max_age_ms = doer.DEFAULT_CAMERA_MAX_AGE_MS

    n._pose = (0.0, 0.0, 0.0, 0.0)
    n._goal = (5.0, 0.0)
    n._scan = None
    n._camera = None
    n._object_goal = None
    n._object_refusal = None
    n._camera_healthy = True

    n._pub = _Publisher()
    n._logger = _Logger()
    n.get_logger = lambda: n._logger

    for k, v in over.items():
        setattr(n, k, v)

    doer.requests = requests_stub
    doer.REQUESTS_AVAILABLE = True
    return n


def _result_for(node, scan_sequence, **over):
    """A completed job's result for a given scan, as a worker would build it."""
    base = dict(
        requested_scan_sequence=scan_sequence,
        scan_received_s=node._scan[1],
        taj_scan_sequence=11,
        plan_scan_sequence=11,
        plan=_plan_body(),
        camera_healthy=True,
        fault=None,
    )
    base.update(over)
    return JobResult(**base)


def _push_scan(node, stamp_ms=1_000):
    node._on_scan(LaserScan(stamp_ms=stamp_ms))


def _await_idle(node, timeout_s=JOIN_TIMEOUT_S):
    """Wait for the in-flight job to deposit its result."""
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        with node._job_lock:
            if not node._job_in_flight:
                return True
        time.sleep(0.002)
    return False


def _live_workers():
    return [t for t in threading.enumerate() if t.name.startswith('occy-doer-')]


@pytest.fixture(autouse=True)
def _no_worker_leaks():
    """Every test must leave the worker pool empty.

    A leaked worker would make a later test's thread count meaningless, and a
    leak IS the failure mode this PR is guarding against.
    """
    yield
    for t in _live_workers():
        t.join(JOIN_TIMEOUT_S)
    assert not [t for t in _live_workers() if t.is_alive()], 'worker leaked'


# ---------------------------------------------------------------------------
# The tick does not block
# ---------------------------------------------------------------------------

def test_tick_returns_while_http_is_still_blocked():
    gate = threading.Event()
    req = _Requests(gate=gate)
    n = _node(req)
    _push_scan(n)
    try:
        started = time.monotonic()
        n._tick()
        elapsed = time.monotonic() - started
        # The worker holds a POST open for as long as the gate is shut. If the
        # tick had done the HTTP inline it could not have returned at all.
        assert req.entered.wait(JOIN_TIMEOUT_S), 'worker never issued its POST'
        assert elapsed < 0.5, f'tick blocked for {elapsed:.3f}s'
        with n._job_lock:
            assert n._job_in_flight
    finally:
        gate.set()
        _await_idle(n)


def test_repeated_ticks_start_exactly_one_job_while_one_is_in_flight():
    gate = threading.Event()
    req = _Requests(gate=gate)
    n = _node(req)
    _push_scan(n)
    try:
        for _ in range(20):
            _push_scan(n)   # new perception every tick, as on a live robot
            n._tick()
        assert req.entered.wait(JOIN_TIMEOUT_S)
        # One chain: one Taj POST, no planner POST yet (it is gated behind it),
        # and exactly one worker thread however fast the timer fires.
        assert req.count('/perception') == 1, req.urls
        assert req.count('/plan') == 0, req.urls
        assert len(_live_workers()) == 1
    finally:
        gate.set()
        _await_idle(n)
    assert req.count('/plan') == 1


def test_a_new_job_starts_once_the_previous_one_finished():
    n = _node(_Requests())
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    assert doer.requests.count('/perception') == 2


# ---------------------------------------------------------------------------
# Faults hold
# ---------------------------------------------------------------------------

@pytest.mark.parametrize('which', ['perception', 'plan'])
@pytest.mark.parametrize('exc', [TimeoutError('t'), ConnectionError('c')])
def test_sidecar_timeout_or_failure_eventually_holds(which, exc):
    n = _node(_Requests(raises={which: exc}))
    _push_scan(n)
    n._tick()                     # issues the job; publishes a hold (no result yet)
    assert _await_idle(n)
    n._pub.published.clear()
    _push_scan(n)
    n._tick()                     # consumes the faulted result
    assert n._pub.published == [doer.HOLD_ENVELOPE]
    assert 'plan-fault:service-error' in n._logger.text()


@pytest.mark.parametrize('bad, tag', [
    ({'plan': ['not', 'an', 'object']}, 'planner-response-not-an-object'),
    ({'taj': ['not', 'an', 'object']}, 'taj-response-not-an-object'),
])
def test_a_sidecar_returning_a_non_object_holds(bad, tag):
    # Named per sidecar, so the operator sees WHICH one answered wrongly rather
    # than a generic AttributeError from the first `.get`.
    n = _node(_Requests(**bad))
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    _push_scan(n)
    n._pub.published.clear()
    n._tick()
    assert n._pub.published == [doer.HOLD_ENVELOPE]
    assert tag in n._logger.text()


def test_evidence_mismatch_between_taj_and_plan_holds():
    # The planner answered about a different Taj frame than this job fetched.
    n = _node(_Requests(plan=_plan_body(
        perception_frame_id=_frame_id(scan_sequence=99))))
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    _push_scan(n)
    n._pub.published.clear()
    n._tick()
    assert n._pub.published == [doer.HOLD_ENVELOPE]
    assert 'evidence mismatch' in n._logger.text()


def test_an_unbindable_plan_holds():
    # Taj's invalid-frame sentinel: no evidence identity to bind to.
    sentinel = _frame_id(scan_sequence=7, profile_digest='', evidence_digest='')
    n = _node(_Requests(taj=_taj_body(frame_id=sentinel),
                        plan=_plan_body(perception_frame_id=sentinel)))
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    _push_scan(n)
    n._pub.published.clear()
    n._tick()
    assert n._pub.published == [doer.HOLD_ENVELOPE]
    assert 'plan-unbound' in n._logger.text()


# ---------------------------------------------------------------------------
# Which results get used
# ---------------------------------------------------------------------------

def test_result_for_the_newest_scan_is_published():
    n = _node(_Requests())
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    n._pub.published.clear()
    n._tick()
    assert len(n._pub.published) == 1
    envelope = json.loads(n._pub.published[0])
    assert envelope['release_binding']['proposal_digest'] == PROPOSAL
    assert envelope['linear_x'] > 0.0
    assert n._last_used_scan_sequence == 1


def test_a_superseded_result_is_discarded():
    n = _node(_Requests())
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    # Rewind the watermark past the pending result: exactly the state a tick is
    # in when a later scan's result has already been consumed.
    n._last_used_scan_sequence = 5
    n._scan_seq = 9
    n._pub.published.clear()
    n._tick()
    assert n._pub.published == [doer.HOLD_ENVELOPE]
    assert 'plan-superseded' in n._logger.text()


def test_an_old_successful_result_is_not_reused_after_a_newer_scan():
    n = _node(_Requests())
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    n._pub.published.clear()
    n._tick()                          # consumes result #1 → publishes
    assert len(n._pub.published) == 1
    first = n._pub.published[0]
    assert first != doer.HOLD_ENVELOPE
    assert n._last_used_scan_sequence == 1
    assert _await_idle(n)

    # A late reply about scan 1 lands after it has already been acted on — an
    # out-of-order or duplicate completion. The plan itself is PERFECTLY VALID;
    # only its identity is spent. It must be discarded, not republished: a robot
    # must never act twice on one perception frame.
    with n._job_lock:
        n._pending_result = _result_for(n, scan_sequence=1)
    _push_scan(n)
    n._pub.published.clear()
    n._tick()
    assert n._pub.published == [doer.HOLD_ENVELOPE]
    assert first not in n._pub.published
    assert 'plan-superseded' in n._logger.text()
    assert n._last_used_scan_sequence == 1
    assert _await_idle(n)


def test_no_new_scan_means_no_new_job():
    # Re-issuing against a scan already planned for would spend a sidecar
    # round-trip on a result the watermark is guaranteed to discard.
    n = _node(_Requests())
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    assert doer.requests.count('/perception') == 1
    for _ in range(5):
        n._tick()                      # same scan, nothing new to plan against
        assert _await_idle(n)
    assert doer.requests.count('/perception') == 1
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    assert doer.requests.count('/perception') == 2


def test_stale_perception_holds_even_when_the_result_is_valid():
    n = _node(_Requests())
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    # Age the scan past the budget without changing its identity.
    scan, _ = n._scan
    n._scan = (scan, time.monotonic() - (n._scan_stale_s + 1.0))
    n._pub.published.clear()
    n._tick()
    # The tick's own arrival-freshness guard fires first; either way it holds
    # and no motion is proposed on perception older than the budget.
    assert n._pub.published == [doer.HOLD_ENVELOPE]


def test_exactly_one_publish_per_tick():
    n = _node(_Requests())
    _push_scan(n)
    for _ in range(4):
        before = len(n._pub.published)
        n._tick()
        assert len(n._pub.published) - before == 1
        assert _await_idle(n)
        _push_scan(n)
    assert _await_idle(n)


# ---------------------------------------------------------------------------
# Thread discipline
# ---------------------------------------------------------------------------

def test_the_worker_never_publishes_and_never_mutates_node_state():
    n = _node(_Requests())
    _push_scan(n)
    before = (n._goal, n._pose, n._scan, n._scan_seq,
              n._last_used_scan_sequence, n._camera_healthy)
    n._pub.published.clear()
    n._maybe_issue(_snapshot(n))
    assert _await_idle(n)
    # The job ran to completion and deposited a result...
    with n._job_lock:
        assert n._pending_result is not None
    # ...and published nothing, and left every piece of node state alone.
    assert n._pub.published == []
    assert (n._goal, n._pose, n._scan, n._scan_seq,
            n._last_used_scan_sequence, n._camera_healthy) == before


def test_workers_are_daemons_so_shutdown_cannot_hang():
    gate = threading.Event()
    n = _node(_Requests(gate=gate))
    _push_scan(n)
    try:
        n._tick()
        workers = _live_workers()
        assert workers, 'no worker started'
        # `daemon=True` IS the teardown contract: the interpreter exits without
        # joining these, so a worker wedged on a hung sidecar cannot keep the
        # process alive.
        assert all(t.daemon for t in workers)
    finally:
        gate.set()
        _await_idle(n)


def test_the_node_never_joins_a_worker():
    # The other half of the teardown contract: a daemon thread still blocks
    # shutdown if something waits on it. Nothing in this module may.
    for sub in ast.walk(_module_ast()):
        if (isinstance(sub, ast.Call) and isinstance(sub.func, ast.Attribute)
                and sub.func.attr == 'join'):
            pytest.fail(f'thread join at line {sub.lineno} could hang teardown')


def test_the_job_lock_is_never_held_across_network_io():
    # If `_execute_job` took `_job_lock`, this deliberately-held lock would
    # deadlock the worker and the wait would time out.
    n = _node(_Requests())
    _push_scan(n)
    request = _snapshot(n)
    done = threading.Event()
    result = {}

    def run():
        result['value'] = n._execute_job(request)
        done.set()

    t = threading.Thread(target=run, name='occy-doer-probe', daemon=True)
    with n._job_lock:
        t.start()
        assert done.wait(JOIN_TIMEOUT_S), '_execute_job blocked on _job_lock'
    t.join(JOIN_TIMEOUT_S)
    assert result['value'].fault is None


def test_a_scan_arriving_mid_job_advances_identity_without_touching_the_slot():
    gate = threading.Event()
    n = _node(_Requests(gate=gate))
    _push_scan(n)
    try:
        n._tick()
        assert doer.requests.entered.wait(JOIN_TIMEOUT_S)
        _push_scan(n)                 # scan #2 lands while job #1 is in flight
        assert n._scan_seq == 2
        with n._job_lock:
            assert n._pending_result is None
    finally:
        gate.set()
        assert _await_idle(n)
    # Job #1's result is about scan #1, which is still un-consumed and still
    # within budget, so it is usable — supersession is by watermark, not by
    # "a newer scan exists".
    with n._job_lock:
        assert n._pending_result.requested_scan_sequence == 1


# ---------------------------------------------------------------------------
# Hold observability
#
# Every hold below was ALREADY fail-closed before these landed. What is being
# tested is that a hold which repeats forever stops being invisible: a wedged
# sidecar and a robot with nothing to do produce identical motion, and only a
# log line can tell an operator which one they are looking at.
# ---------------------------------------------------------------------------

def _warns(node):
    return [m for level, m in node._logger.records if level == 'warn']


def test_a_sustained_hold_warns_once_and_names_the_cause():
    # The sidecar is down for good: every tick holds ABSENT/FAULT.
    n = _node(_Requests(raises={'perception': ConnectionError('down')}))
    for _ in range(40):
        _push_scan(n)
        n._tick()
        assert _await_idle(n)
    holding = [m for m in _warns(n) if 'plan cycle holding' in m]
    assert len(holding) == 1, _warns(n)
    assert 'FAULT' in holding[0]
    assert 'no motion is being proposed' in holding[0]
    # Safety is unchanged: it held on every single tick regardless.
    assert set(n._pub.published) == {doer.HOLD_ENVELOPE}


def test_a_brief_hold_is_not_announced():
    # The first ticks of any healthy start-up hold while the pipeline fills.
    # That must not warn, or the warning means nothing.
    n = _node(_Requests())
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    assert [m for m in _warns(n) if 'plan cycle' in m] == []


def test_recovery_is_reported_and_re_arms():
    n = _node(_Requests(raises={'plan': TimeoutError('slow')}))
    for _ in range(20):
        _push_scan(n)
        n._tick()
        assert _await_idle(n)
    assert len([m for m in _warns(n) if 'plan cycle holding' in m]) == 1

    # The planner comes back.
    doer.requests = _Requests()
    for _ in range(3):
        _push_scan(n)
        n._tick()
        assert _await_idle(n)
    recovered = [m for _, m in n._logger.records if 'plan cycle recovered' in m]
    assert len(recovered) == 1
    assert 'FAULT' in recovered[0]

    # And it goes down again — a second episode must be announced, not
    # swallowed by the first one's latch.
    doer.requests = _Requests(raises={'plan': TimeoutError('slow again')})
    for _ in range(20):
        _push_scan(n)
        n._tick()
        assert _await_idle(n)
    assert len([m for m in _warns(n) if 'plan cycle holding' in m]) == 2


def test_an_unbindable_plan_counts_as_a_hold_not_a_recovery():
    # The result RESOLVED usable and only failed at the emit stage. If USE were
    # folded in at resolution time this episode would look like it recovered on
    # every tick and never warn.
    sentinel = _frame_id(scan_sequence=7, profile_digest='', evidence_digest='')
    n = _node(_Requests(taj=_taj_body(frame_id=sentinel),
                        plan=_plan_body(perception_frame_id=sentinel)))
    for _ in range(30):
        _push_scan(n)
        n._tick()
        assert _await_idle(n)
    holding = [m for m in _warns(n) if 'plan cycle holding' in m]
    assert len(holding) == 1
    assert 'UNBOUND' in holding[0]


def test_an_overdue_job_warns_once_and_does_not_start_a_replacement():
    # The failure this exists for: `requests` timeouts bound each socket
    # operation, not the whole request, so a sidecar that never closes the
    # connection holds the one permitted worker open forever. Before this, the
    # doer stopped planning for good and said nothing.
    gate = threading.Event()
    req = _Requests(gate=gate)
    n = _node(req, _timeout_s=0.001)   # 2 ms budget → overdue almost at once
    try:
        _push_scan(n)
        n._tick()
        assert req.entered.wait(JOIN_TIMEOUT_S)
        deadline = time.monotonic() + JOIN_TIMEOUT_S
        overdue = []
        while time.monotonic() < deadline and not overdue:
            _push_scan(n)
            n._tick()
            overdue = [m for m in _warns(n) if 'plan job overdue' in m]
            time.sleep(0.005)
        assert len(overdue) == 1, _warns(n)
        assert 'the doer stays held' in overdue[0]
        # Latched: ticking on does not repeat it...
        for _ in range(10):
            _push_scan(n)
            n._tick()
        assert len([m for m in _warns(n) if 'plan job overdue' in m]) == 1
        # ...and the one-job bound is intact — no replacement worker was
        # spawned against a sidecar that is already sick.
        assert len(_live_workers()) == 1
        assert req.count('/perception') == 1
    finally:
        gate.set()
        _await_idle(n)


def test_an_overdue_job_is_reported_even_when_the_tick_holds_for_another_reason():
    # A dead lidar and a wedged sidecar are two faults. The stale-scan guard
    # returns before the plan cycle, so the overdue check runs ahead of it.
    gate = threading.Event()
    n = _node(_Requests(gate=gate), _timeout_s=0.001)
    try:
        _push_scan(n)
        n._tick()
        assert doer.requests.entered.wait(JOIN_TIMEOUT_S)
        scan, _ = n._scan
        n._scan = (scan, time.monotonic() - 10.0)   # lidar goes silent too
        deadline = time.monotonic() + JOIN_TIMEOUT_S
        while time.monotonic() < deadline:
            n._tick()
            if [m for m in _warns(n) if 'plan job overdue' in m]:
                break
            time.sleep(0.005)
        assert [m for m in _warns(n) if 'plan job overdue' in m]
        assert 'stale-scan' in n._logger.text()     # it did take the early return
    finally:
        gate.set()
        _await_idle(n)


def test_a_job_inside_its_budget_never_warns_overdue():
    n = _node(_Requests())
    for _ in range(5):
        _push_scan(n)
        n._tick()
        assert _await_idle(n)
    assert [m for m in _warns(n) if 'overdue' in m] == []


def test_the_overdue_latch_re_arms_between_jobs():
    n = _node(_Requests())
    n._job_overdue_warned = True
    _push_scan(n)
    n._tick()                       # no job overdue → the latch resets
    assert n._job_overdue_warned is False
    assert _await_idle(n)


# ---------------------------------------------------------------------------
# Regression: the emitted envelope is what the synchronous doer emitted
# ---------------------------------------------------------------------------

def _synchronous_reference(node, taj, plan_response, gx, gy, speed):
    """The pre-async emit path, frozen.

    Deliberately a literal transcription of the synchronous `_tick` tail rather
    than a call into the new code, so this test can actually catch the async
    rework changing what goes on the wire.
    """
    left, right = extend_corridor_back(
        taj.get('left', []), taj.get('right', []), node._back_m)
    plan_req = {
        'ego': {'x': 0.0, 'y': 0.0, 'heading': 0.0, 'speed': float(speed)},
        'goal': {'x': gx, 'y': gy},
        'cruise': node._cruise,
        'left': left,
        'right': right,
        'objects': taj.get('objects', []),
        'pedestrians': taj.get('pedestrians', []),
        'predicted_vrus': taj.get('predicted_vrus', []),
        'perception_frame_id': taj.get('frame_id'),
        'vehicle': node._vehicle,
    }
    v, w, _reason = decide(
        plan_response, node._lookahead, node._max_v, node._max_w)
    binding = build_release_binding(
        plan_response.get('perception_frame_id'),
        plan_response.get('proposal_digest'))
    return plan_req, encode_bound_proposal(v, 0.0, w, binding)


@pytest.mark.parametrize('speed', [0.0, 0.7])
@pytest.mark.parametrize('trajectory', [
    [{'x': 0.0, 'y': 0.0, 'speed': 0.8}, {'x': 1.0, 'y': 0.0, 'speed': 0.8},
     {'x': 2.0, 'y': 0.0, 'speed': 0.8}],
    [{'x': 0.0, 'y': 0.0, 'speed': 0.5}, {'x': 1.0, 'y': 0.4, 'speed': 0.5},
     {'x': 2.0, 'y': 1.1, 'speed': 0.5}],
])
def test_emitted_envelope_matches_the_synchronous_implementation(speed, trajectory):
    taj = _taj_body()
    plan_response = _plan_body(trajectory=trajectory)
    n = _node(_Requests(taj=taj, plan=plan_response), _pose=(0.0, 0.0, 0.0, speed))
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    n._pub.published.clear()
    n._tick()
    assert _await_idle(n)

    expected_plan_req, expected_envelope = _synchronous_reference(
        n, taj, plan_response, gx=5.0, gy=0.0, speed=speed)
    assert expected_envelope is not None
    assert n._pub.published[0] == expected_envelope
    # The request the worker sent must also be the request the synchronous
    # implementation sent — the envelope can only be right if the plan was
    # authored from the same inputs.
    plan_calls = [c['json'] for c in doer.requests.calls if c['url'].endswith('/plan')]
    assert plan_calls[0] == expected_plan_req


def test_the_published_envelope_is_a_valid_bound_proposal():
    n = _node(_Requests())
    _push_scan(n)
    n._tick()
    assert _await_idle(n)
    n._pub.published.clear()
    n._tick()
    assert _await_idle(n)
    envelope = json.loads(n._pub.published[0])
    assert set(envelope) >= {'linear_x', 'linear_y', 'angular_z', 'release_binding'}
    assert envelope['release_binding'] == {
        'scan_sequence': 11, 'camera_sequence': 3, 'tracker_generation': 2,
        'profile_digest': PROFILE, 'evidence_digest': EVIDENCE,
        'proposal_digest': PROPOSAL,
    }


# ---------------------------------------------------------------------------
# Structural: no HTTP survives on the callback path
# ---------------------------------------------------------------------------

CALLBACKS = ('_tick', '_on_scan', '_on_odom', '_on_goal', '_on_camera',
             '_on_object_goal')

HTTP_ATTRS = ('post', 'get', 'put', 'patch', 'delete', 'request', 'head')


def _module_ast():
    with open(DOER_SOURCE) as fh:
        return ast.parse(fh.read())


def _methods():
    tree = _module_ast()
    cls = next(n for n in tree.body
               if isinstance(n, ast.ClassDef) and n.name == 'OccyDoer')
    return {m.name: m for m in cls.body if isinstance(m, ast.FunctionDef)}


def _http_calls_in(node):
    """`requests.<verb>(...)` occurrences directly inside one function body."""
    found = []
    for sub in ast.walk(node):
        if not isinstance(sub, ast.Call):
            continue
        func = sub.func
        if (isinstance(func, ast.Attribute) and func.attr in HTTP_ATTRS
                and isinstance(func.value, ast.Name) and func.value.id == 'requests'):
            found.append(f'requests.{func.attr} at line {sub.lineno}')
    return found


def _self_calls_in(node):
    return {sub.func.attr for sub in ast.walk(node)
            if isinstance(sub, ast.Call) and isinstance(sub.func, ast.Attribute)
            and isinstance(sub.func.value, ast.Name) and sub.func.value.id == 'self'}


def test_no_http_is_reachable_from_any_callback():
    """Transitive, not just direct.

    A direct-only check passes the moment the call moves one helper deeper,
    which is exactly the regression this guards: `_poll_mick`'s blocking GET
    survived a direct-only scan of `_tick`.
    """
    methods = _methods()
    offenders = {}

    def walk(name, seen):
        if name in seen or name not in methods:
            return
        seen.add(name)
        for hit in _http_calls_in(methods[name]):
            offenders.setdefault(name, []).append(hit)
        for callee in _self_calls_in(methods[name]):
            walk(callee, seen)

    for entry in CALLBACKS:
        assert entry in methods, entry
        walk(entry, set())

    assert offenders == {}, f'blocking HTTP reachable from a callback: {offenders}'


def test_the_worker_entry_points_are_the_only_http_holders():
    # The counterpart assertion: the check above would also pass if the HTTP
    # had simply been deleted. It must still exist — in the workers.
    methods = _methods()
    holders = {name for name, node in methods.items() if _http_calls_in(node)}
    assert holders == {'_execute_job', '_run_mick_fetch'}, holders


def test_every_thread_the_node_starts_is_a_daemon():
    for sub in ast.walk(_module_ast()):
        if (isinstance(sub, ast.Call) and isinstance(sub.func, ast.Attribute)
                and sub.func.attr == 'Thread'):
            kwargs = {kw.arg: kw.value for kw in sub.keywords}
            assert 'daemon' in kwargs, f'Thread at line {sub.lineno} sets no daemon'
            assert kwargs['daemon'].value is True, f'line {sub.lineno}'


# ---------------------------------------------------------------------------
# Helpers used by the thread-discipline tests
# ---------------------------------------------------------------------------

def _snapshot(node):
    """The JobRequest a nominal tick would build, without publishing."""
    scan = node._scan[0]
    stamp_ms = int(scan.header.stamp.sec * 1000 + scan.header.stamp.nanosec // 1_000_000)
    return doer.JobRequest(
        scan_sequence=node._scan_seq,
        scan_received_s=node._scan[1],
        taj_url=node._taj,
        planner_url=node._planner,
        timeout_s=node._timeout_s,
        perception={
            'angle_min_rad': float(scan.angle_min),
            'angle_increment_rad': float(scan.angle_increment),
            'range_min_m': float(scan.range_min),
            'range_max_m': float(scan.range_max),
            'ranges': [float(r) for r in scan.ranges],
            'stamp_ms': stamp_ms, 'forward_extent_m': node._extent,
        },
        plan_fields={
            'speed': 0.0, 'gx': 5.0, 'gy': 0.0,
            'cruise': node._cruise, 'back_m': node._back_m,
            'vehicle': node._vehicle, 'goal_fields': None,
        },
    )


def test_jobresult_is_immutable():
    # The slot hands a value across a thread boundary; a namedtuple makes
    # "the timer cannot be handed something the worker still mutates" structural.
    result = JobResult(1, 0.0, 2, 2, {}, True, None)
    with pytest.raises(AttributeError):
        result.fault = 'nope'
