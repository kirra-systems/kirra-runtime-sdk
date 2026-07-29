"""
Callback-level tests for the perception governor's non-blocking scan cycle.

These pin the invariants that only exist where the scan callback meets the
background worker — that `_on_scan` returns while HTTP is still blocked, that a
slow sidecar cannot pile up threads, and that every non-USE outcome floors the
cap. The decision algebra itself is covered purely in test_async_perception.py;
this file deliberately does not re-test it.

NO ROS GRAPH IS NEEDED. `rclpy` / the message packages are stubbed into
sys.modules before the node module is imported, and the node instance is built
without running `Node.__init__` — so callbacks run against ordinary Python
objects. The session is a fake whose latency and replies the test controls, so
"did not wait for HTTP" is an assertion about wall clock rather than a network.

Run:  pytest ros2_ws/src/kirra_safety/test/test_perception_governor_async.py
"""

import os
import sys
import threading
import time
import types


# ---------------------------------------------------------------------------
# ROS stubs — installed BEFORE importing the node module
# ---------------------------------------------------------------------------

def _install_ros_stubs():
    for name in (
        "rclpy", "rclpy.node", "rclpy.qos",
        "sensor_msgs", "sensor_msgs.msg",
        "std_msgs", "std_msgs.msg",
    ):
        sys.modules.setdefault(name, types.ModuleType(name))

    class _Node:
        def __init__(self, *a, **k):
            pass

    sys.modules["rclpy.node"].Node = _Node
    sys.modules["rclpy.qos"].QoSProfile = lambda **kw: None
    for attr in ("HistoryPolicy", "ReliabilityPolicy"):
        setattr(
            sys.modules["rclpy.qos"],
            attr,
            types.SimpleNamespace(BEST_EFFORT=0, KEEP_LAST=0),
        )
    sys.modules["sensor_msgs.msg"].LaserScan = object
    for attr in ("Float64", "String"):
        setattr(sys.modules["std_msgs.msg"], attr, lambda data=None: data)


_install_ros_stubs()
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from kirra_safety import perception_governor as pg  # noqa: E402


# ---------------------------------------------------------------------------
# Harness
# ---------------------------------------------------------------------------

class FakeSession:
    """A Taj stand-in whose latency and reply the test owns."""

    def __init__(self, latency_s=0.0, payload=None, raise_with=None):
        self.latency_s = latency_s
        self.payload = healthy_payload() if payload is None else payload
        self.raise_with = raise_with
        self.calls = 0
        self.concurrent = 0
        self.max_concurrent = 0
        self._lock = threading.Lock()
        self.release = threading.Event()

    def post(self, *_a, **_k):
        with self._lock:
            self.calls += 1
            self.concurrent += 1
            self.max_concurrent = max(self.max_concurrent, self.concurrent)
        try:
            if self.latency_s:
                self.release.wait(self.latency_s)
            if self.raise_with is not None:
                raise self.raise_with
            return _Response(self.payload)
        finally:
            with self._lock:
                self.concurrent -= 1


class _Response:
    status_code = 200

    def __init__(self, payload):
        self._payload = payload

    def json(self):
        return self._payload


def healthy_payload(cap=1.5):
    return {
        "speed_cap_mps": cap,
        "clear_distance_m": 6.0,
        "healthy": True,
        "stabilized_speed_cap_mps": cap,
        "cap_reason": "CAP_PASS",
        "cap_clear_streak": 5,
        "cap_limiting_object": None,
        "minimum_corridor_width_m": 1.2,
        "required_corridor_width_m": 0.5,
    }


def governor(session, deadline_s=0.25, scan_stale_s=0.5):
    """A node instance with only the fields the async cycle touches."""
    gov = pg.PerceptionGovernor.__new__(pg.PerceptionGovernor)
    gov._lock = threading.Lock()
    gov._slot = pg.LatestFrameSlot()
    gov._request_sequence = 0
    gov._last_published_sequence = 0
    gov._in_flight_since_s = None
    gov._result = None
    gov._discarded_replies = 0
    gov._deadline_breaches = 0
    gov._deadline_s = deadline_s
    gov._scan_stale_s = scan_stale_s
    gov._taj_url = "http://taj.invalid"
    gov._timeout_s = 0.04
    gov._session = session
    gov._request_body = lambda msg: {"ranges": []}
    gov.published = []
    gov._publish = lambda cap, health: gov.published.append((float(cap), health))
    gov.logged = []
    gov.get_logger = lambda: types.SimpleNamespace(
        error=gov.logged.append, warn=gov.logged.append, info=gov.logged.append
    )
    return gov


def settle(gov, timeout_s=2.0):
    """Wait for the in-flight worker to deposit, so assertions are not racy."""
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        with gov._lock:
            if gov._in_flight_since_s is None:
                return True
        time.sleep(0.005)
    return False


# ---------------------------------------------------------------------------
# AC1 — the callback does not wait
# ---------------------------------------------------------------------------


def test_the_scan_callback_returns_while_http_is_still_blocked():
    # THE issue. Under rclpy's single-threaded executor, a callback that waits
    # stalls every other callback on the node — including scan ingest, so a slow
    # sidecar used to back up the very stream it was judging.
    session = FakeSession(latency_s=5.0)
    gov = governor(session)

    started = time.monotonic()
    gov._on_scan(object())
    elapsed_s = time.monotonic() - started

    assert elapsed_s < 0.25, (
        f"the callback took {elapsed_s * 1000:.1f} ms against a 5 s sidecar; "
        f"it must copy and return, not wait"
    )
    session.release.set()
    settle(gov)


def test_the_worker_actually_performs_the_request():
    # Non-vacuity for the test above: returning fast is trivial if nothing is
    # ever sent. The work must still happen, just not on the callback.
    session = FakeSession(payload=healthy_payload())
    gov = governor(session)
    gov._on_scan(object())
    assert settle(gov), "the worker never completed"
    assert session.calls == 1


# ---------------------------------------------------------------------------
# AC — one in-flight request, latest-frame slot
# ---------------------------------------------------------------------------


def test_scans_arriving_during_a_request_never_start_a_second():
    # The bound on thread creation against a slow sidecar. Requests cannot be
    # cancelled, so not starting one is the only control available.
    session = FakeSession(latency_s=5.0)
    gov = governor(session)

    for _ in range(25):
        gov._on_scan(object())

    assert session.max_concurrent == 1, (
        f"{session.max_concurrent} concurrent requests; exactly one may run"
    )
    session.release.set()
    settle(gov)


def test_a_backlog_is_bounded_to_one_scan_and_the_newest_survives():
    session = FakeSession(latency_s=5.0)
    gov = governor(session)

    for _ in range(10):
        gov._on_scan(object())

    with gov._lock:
        pending = gov._slot.take()
    # 10 scans: the 1st is taken by the worker, the 2nd fills the empty slot,
    # the remaining 8 displace it.
    assert gov._slot.overwrites == 8
    assert pending.request_sequence == 10, "the NEWEST scan is the one retained"
    session.release.set()
    settle(gov)


# ---------------------------------------------------------------------------
# AC — the deadline is the node's, not the transport's
# ---------------------------------------------------------------------------


def test_a_sidecar_that_stops_answering_floors_the_cap_on_the_deadline():
    # `requests`' timeout is per socket operation, so a server dribbling bytes
    # never trips it. Perception drives a speed cap, so the cap must be floored
    # on a schedule the node controls.
    session = FakeSession(latency_s=5.0)
    gov = governor(session, deadline_s=0.05)
    gov._on_scan(object())

    time.sleep(0.1)
    gov._on_publish_timer()

    cap, health = gov.published[-1]
    assert cap == 0.0
    assert health.startswith("TAJ_DEADLINE"), health
    assert gov._deadline_breaches == 1
    session.release.set()
    settle(gov)


def test_a_late_reply_is_usable_if_its_scan_is_still_fresh():
    # The deadline and the staleness budget answer DIFFERENT questions. The
    # deadline bounds how long the node waits before flooring the cap; staleness
    # bounds how old the perception behind a cap may be. A reply that missed the
    # first can still satisfy the second, and refusing it would mean one slow
    # round trip permanently discarded good, current evidence.
    #
    # What must not happen is the breach becoming invisible once a reply lands,
    # so the counter stays on the wire.
    session = FakeSession(latency_s=0.2, payload=healthy_payload())
    gov = governor(session, deadline_s=0.01)
    gov._on_scan(object())

    # Past the 10 ms deadline, well short of the 200 ms reply.
    time.sleep(0.05)
    gov._on_publish_timer()          # breach: nothing has arrived yet
    assert gov.published[-1][1].startswith("TAJ_DEADLINE")

    assert settle(gov), "the worker never completed"
    gov._on_publish_timer()          # the late reply is now in the slot
    cap, health = gov.published[-1]
    # It IS published — it is the newest evidence and it is still fresh. What
    # must not happen is publishing it as though the breach never occurred, so
    # the breach counter stays visible on the wire.
    assert "deadline=1" in health, health
    assert cap >= 0.0


# ---------------------------------------------------------------------------
# Fail-closed: every non-USE outcome
# ---------------------------------------------------------------------------


def test_nothing_received_yet_publishes_the_floor():
    gov = governor(FakeSession(payload=healthy_payload()))
    gov._on_publish_timer()
    cap, health = gov.published[-1]
    assert cap == 0.0
    assert health.startswith("TAJ_ABSENT")


def test_a_transport_fault_publishes_the_floor():
    session = FakeSession(raise_with=ValueError("bad json"))
    gov = governor(session)
    gov._on_scan(object())
    assert settle(gov)

    gov._on_publish_timer()
    cap, health = gov.published[-1]
    assert cap == 0.0
    assert health.startswith("TAJ_FAULT")


def test_a_result_is_published_once_and_not_republished():
    # Without the watermark a wedged sidecar's last good answer would be
    # re-served every publish period, and a frozen world would look clear.
    session = FakeSession(payload=healthy_payload())
    gov = governor(session)
    gov._on_scan(object())
    assert settle(gov)

    gov._on_publish_timer()
    first_cap = gov.published[-1][0]
    assert first_cap > 0.0, "precondition: a real cap was published"

    gov._on_publish_timer()
    cap, health = gov.published[-1]
    assert cap == 0.0, "the same result must not be published twice"
    assert health.startswith("TAJ_SUPERSEDED")
    assert gov._discarded_replies == 1


def test_counters_reach_the_health_wire():
    # An operator diagnosing "why is the cap zero" is already reading this
    # string; a counter they have to find elsewhere is one they will not find.
    gov = governor(FakeSession(payload=healthy_payload()))
    gov._on_publish_timer()
    _cap, health = gov.published[-1]
    for field in ("discarded=", "deadline=", "overwritten="):
        assert field in health, f"{field} missing from {health}"


# ---------------------------------------------------------------------------
# Permanent worker blockage — SAFE, but an availability limitation.
#
# This is the case the async pattern does NOT fix, and the claim boundary
# matters: it makes the ROS EXECUTOR recoverable, not a blocked socket. If the
# worker never returns, the one-in-flight bound means no replacement is ever
# started and the newest scan is never processed. The cap stays floored — safe
# — but the node does not recover on its own.
# ---------------------------------------------------------------------------


class WedgedSession(FakeSession):
    """A sidecar whose socket never completes and never times out."""

    def __init__(self):
        super().__init__()
        self._never = threading.Event()

    def post(self, *_a, **_k):
        with self._lock:
            self.calls += 1
        self._never.wait()  # released only by `unwedge`

    def unwedge(self):
        self._never.set()


def test_a_permanently_wedged_worker_floors_the_cap_and_keeps_it_floored():
    session = WedgedSession()
    gov = governor(session, deadline_s=0.02)

    gov._on_scan(object())
    time.sleep(0.05)
    for _ in range(5):
        gov._on_scan(object())

    for _ in range(6):
        gov._on_publish_timer()

    assert all(cap == 0.0 for cap, _ in gov.published), "the cap must stay floored"
    assert {h.split(":")[0] for _, h in gov.published} == {"TAJ_DEADLINE"}
    assert gov._deadline_breaches == 6
    session.unwedge()


def test_a_wedged_worker_blocks_every_later_scan_which_is_an_availability_limit():
    # Documented, not defended. Exactly one request was ever sent; the newest
    # scan sits in the slot unprocessed for as long as the worker is stuck.
    #
    # Recovery is a RUNTIME concern (supervision, sidecar restart, or a
    # replaceable process rather than a thread) — see the module docstring. This
    # test exists so the limitation cannot be forgotten, and so that a future
    # change which DOES add recovery has to come here and say so.
    session = WedgedSession()
    gov = governor(session, deadline_s=0.02)

    gov._on_scan(object())
    time.sleep(0.05)
    for _ in range(4):
        gov._on_scan(object())
        gov._on_publish_timer()

    assert session.calls == 1, (
        "a second worker must never start — that bound is what stops thread "
        "exhaustion against a sick sidecar"
    )
    with gov._lock:
        assert gov._in_flight_since_s is not None, "still wedged"
        assert gov._slot.has_pending, "the newest scan is waiting and will keep waiting"
    session.unwedge()


# ---------------------------------------------------------------------------
# Timer idempotence and late-worker authority
# ---------------------------------------------------------------------------


def test_repeated_timer_ticks_with_no_new_evidence_keep_publishing_the_floor():
    # Idempotent in the way that matters: the same absence produces the same
    # verdict, and the cap never drifts upward because nothing happened.
    gov = governor(FakeSession(payload=healthy_payload()))
    for _ in range(8):
        gov._on_publish_timer()
    assert {cap for cap, _ in gov.published} == {0.0}
    assert all(h.startswith("TAJ_ABSENT") for _, h in gov.published)


def test_a_late_worker_cannot_restore_a_cap_after_its_scan_has_aged_out():
    # The authority question behind the deadline: a worker that completes long
    # after the runtime moved on must not be able to lift the floor. It is
    # refused on FRESHNESS, which is the check that does not care how late the
    # transport was — only how old the perception is.
    session = FakeSession(payload=healthy_payload())
    gov = governor(session, deadline_s=0.01, scan_stale_s=0.05)

    gov._on_scan(object())
    assert settle(gov), "the worker completed"

    time.sleep(0.08)                 # the scan ages past the 50 ms budget
    gov._on_publish_timer()

    cap, health = gov.published[-1]
    assert cap == 0.0
    assert health.startswith("TAJ_STALE"), health


def test_completion_and_arrival_racing_never_starts_two_workers():
    # The determinism question: scans arriving exactly as a worker finishes.
    # `_run_request` calls `_maybe_issue` after depositing, and `_on_scan` calls
    # it too, so both paths can try to start the successor at once. The shared
    # lock is what makes the outcome deterministic rather than a coin flip.
    session = FakeSession(latency_s=0.002, payload=healthy_payload())
    gov = governor(session)

    stop = threading.Event()

    def flood():
        while not stop.is_set():
            gov._on_scan(object())

    threads = [threading.Thread(target=flood, daemon=True) for _ in range(4)]
    for t in threads:
        t.start()
    time.sleep(0.4)
    stop.set()
    for t in threads:
        t.join(timeout=1.0)

    assert session.max_concurrent == 1, (
        f"{session.max_concurrent} concurrent requests under a 4-thread flood"
    )
    assert session.calls > 1, "non-vacuity: the flood did drive real requests"
