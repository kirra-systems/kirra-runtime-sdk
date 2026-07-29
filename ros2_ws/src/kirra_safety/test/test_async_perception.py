#!/usr/bin/env python3
"""
Tests for the perception governor's non-blocking scan cycle (#1218).

Pure — no ROS, no sidecar, no clock. Every value is injected, so the states that
matter (a reply for a superseded scan, a sidecar that stops answering) are
exercised directly rather than provoked.

Run:  pytest ros2_ws/src/kirra_safety/test/test_async_perception.py
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "kirra_safety"))
from async_perception import (  # noqa: E402
    ABSENT,
    FAULT,
    FLOOR_STATES,
    IN_FLIGHT,
    ISSUE,
    LatestFrameSlot,
    NO_PENDING,
    PerceptionRequest,
    PerceptionResult,
    STALE,
    SUPERSEDED,
    USE,
    deadline_breached,
    decide_issue,
    resolve_result,
)

STALE_S = 0.5


def result(sequence=5, scan_received_s=100.0, response=None, fault=None):
    return PerceptionResult(
        request_sequence=sequence,
        scan_received_s=scan_received_s,
        response=response if response is not None else {"speed_cap_mps": 1.0},
        fault=fault,
    )


def request(sequence=5):
    return PerceptionRequest(
        request_sequence=sequence,
        scan_received_s=100.0,
        taj_url="http://localhost:8101",
        timeout_s=0.04,
        body={},
    )


# ---------------------------------------------------------------------------
# decide_issue — the concurrency bound
# ---------------------------------------------------------------------------


def test_a_pending_scan_with_nothing_in_flight_is_issued():
    assert decide_issue(request_in_flight=False, has_pending=True) == ISSUE


def test_only_one_request_may_be_in_flight():
    # The bound on thread creation. Requests cannot be cancelled, so the only
    # way to avoid a pile-up against a slow sidecar is not to start one.
    assert decide_issue(request_in_flight=True, has_pending=True) == IN_FLIGHT


def test_in_flight_wins_over_pending():
    # Both true is the interesting case: scans are arriving AND a request is
    # running. It must not start a second.
    assert decide_issue(request_in_flight=True, has_pending=True) == IN_FLIGHT


def test_nothing_pending_is_not_an_issue():
    assert decide_issue(request_in_flight=False, has_pending=False) == NO_PENDING


# ---------------------------------------------------------------------------
# LatestFrameSlot — bounded by construction, and visibly so
# ---------------------------------------------------------------------------


def test_the_slot_holds_the_newest_scan_and_discards_the_older():
    slot = LatestFrameSlot()
    assert not slot.has_pending

    assert slot.offer(request(1)) is False
    assert slot.has_pending
    assert slot.offer(request(2)) is True, "the second offer displaced the first"

    taken = slot.take()
    assert taken.request_sequence == 2, "the NEWER scan survives, not the older"
    assert not slot.has_pending
    assert slot.take() is None


def test_overwrites_are_counted():
    # Bounded-by-construction is a design property; how often it engages is an
    # operational fact. A governor silently dropping every second scan should be
    # visible to whoever is wondering why the cap is low.
    slot = LatestFrameSlot()
    assert slot.overwrites == 0

    slot.offer(request(1))
    assert slot.overwrites == 0, "the first offer displaces nothing"

    for _ in range(4):
        slot.offer(request(9))
    assert slot.overwrites == 4


def test_taking_and_re_offering_does_not_count_an_overwrite():
    # The steady state under load: take, run, a new scan arrives. That is not a
    # displacement, and counting it as one would make normal operation at a high
    # scan rate look like a malfunction.
    slot = LatestFrameSlot()
    for sequence in range(1, 20):
        slot.offer(request(sequence))
        slot.take()
    assert slot.overwrites == 0


# ---------------------------------------------------------------------------
# resolve_result — one publishable outcome, everything else floors
# ---------------------------------------------------------------------------


def test_a_fresh_result_for_the_newest_scan_is_used():
    verdict = resolve_result(
        result(sequence=5, scan_received_s=100.0),
        newest_request_sequence=5,
        last_published_sequence=4,
        now_s=100.1,
        scan_stale_s=STALE_S,
    )
    assert verdict.state == USE


def test_no_result_yet_is_absent_not_a_fault():
    # The first cycles, and every cycle while a request is running. Reporting
    # this as a fault would make a healthy startup look broken.
    verdict = resolve_result(None, 5, 0, 100.0, STALE_S)
    assert verdict.state == ABSENT


def test_a_faulted_request_is_reported_as_its_own_fault():
    verdict = resolve_result(
        result(fault="TAJ_TIMEOUT"), 5, 4, 100.1, STALE_S
    )
    assert verdict.state == FAULT
    assert verdict.reason == "TAJ_TIMEOUT"


def test_a_result_for_an_already_published_scan_is_refused():
    # THE republish hazard. Without the watermark a wedged sidecar's last good
    # answer would be re-served every cycle, and a frozen world would look
    # exactly like a clear one.
    verdict = resolve_result(
        result(sequence=5), newest_request_sequence=7,
        last_published_sequence=5, now_s=100.1, scan_stale_s=STALE_S,
    )
    assert verdict.state == SUPERSEDED


def test_a_result_ahead_of_anything_issued_is_refused():
    # The worker and the node disagree about identity. Refuse rather than guess
    # which one is right.
    verdict = resolve_result(
        result(sequence=9), newest_request_sequence=5,
        last_published_sequence=0, now_s=100.1, scan_stale_s=STALE_S,
    )
    assert verdict.state == SUPERSEDED


def test_staleness_is_measured_on_the_scan_not_on_the_reply():
    # The safety question is how old the PERCEPTION is, not how recently the
    # sidecar got back to us. A fast reply about an old scan is still old.
    verdict = resolve_result(
        result(sequence=5, scan_received_s=100.0),
        newest_request_sequence=5,
        last_published_sequence=4,
        now_s=100.0 + STALE_S + 0.001,
        scan_stale_s=STALE_S,
    )
    assert verdict.state == STALE


def test_a_scan_exactly_at_the_staleness_budget_is_still_usable():
    # The boundary is stated so a later change to the comparison shows up here
    # rather than silently widening what counts as fresh.
    verdict = resolve_result(
        result(sequence=5, scan_received_s=100.0),
        newest_request_sequence=5,
        last_published_sequence=4,
        now_s=100.0 + STALE_S,
        scan_stale_s=STALE_S,
    )
    assert verdict.state == USE


def test_identity_is_decided_before_staleness():
    # A result about the wrong scan tells us nothing regardless of its age, and
    # reporting it as STALE would send an operator to look at scan rates when
    # the actual problem is identity.
    verdict = resolve_result(
        result(sequence=3, scan_received_s=0.0),
        newest_request_sequence=9,
        last_published_sequence=5,
        now_s=1_000.0,          # wildly stale as well
        scan_stale_s=STALE_S,
    )
    assert verdict.state == SUPERSEDED


def test_a_fault_is_decided_before_identity():
    # A failed request carries no evidence to compare, so comparing it would be
    # inventing a verdict from a value that was never populated.
    verdict = resolve_result(
        result(sequence=99, fault="TAJ_UNREACHABLE"),
        newest_request_sequence=5,
        last_published_sequence=4,
        now_s=100.1,
        scan_stale_s=STALE_S,
    )
    assert verdict.state == FAULT


def test_every_non_use_state_floors_the_cap():
    # The safety property the whole module exists to deliver, asserted as a
    # partition rather than trusted to the call sites. FLOOR_STATES is written
    # out explicitly in the module, so this catches a new state being added
    # without a decision about which side of the line it falls on.
    all_states = {USE} | FLOOR_STATES
    assert USE not in FLOOR_STATES
    for state in (ABSENT, SUPERSEDED, STALE, FAULT):
        assert state in FLOOR_STATES, state
    assert len(all_states) == 5, "a state was added without classifying it"


# ---------------------------------------------------------------------------
# deadline_breached — the node's clock, not the transport's
# ---------------------------------------------------------------------------


def test_no_request_in_flight_cannot_breach():
    assert deadline_breached(None, 100.0, 0.2) is False


def test_a_request_inside_its_deadline_has_not_breached():
    assert deadline_breached(100.0, 100.15, 0.2) is False


def test_a_request_past_its_deadline_has_breached():
    # THE case: the sidecar has not answered and `requests` has not given up,
    # because its timeout is per socket operation. Perception drives a speed
    # cap, so the cap must be floored on the node's schedule.
    assert deadline_breached(100.0, 100.25, 0.2) is True


def test_the_deadline_boundary_is_exclusive():
    # Binary-exact values throughout: 0.25 and 100.25 are representable, so this
    # measures the comparison rather than float representation. With 0.2,
    # `100.2 - 100.0` is 0.20000000000000284 and "exactly at the deadline" is
    # not a state the test could reach.
    assert deadline_breached(100.0, 100.25, 0.25) is False, "exactly at is not past"
    assert deadline_breached(100.0, 100.25 + 0.0625, 0.25) is True


def test_an_unusable_deadline_disables_the_check_rather_than_floors_forever():
    # Converting an operator's configuration mistake into a permanent floor
    # would be a robot that never moves and never says why.
    for bad in (0.0, -1.0, float("nan"), float("inf")):
        assert deadline_breached(100.0, 1_000.0, bad) is False, bad


# ---------------------------------------------------------------------------
# Structural guard on the node itself (#1218 AC1).
#
# Parsed, not imported: the node needs rclpy and sensor_msgs, which are not
# available in this host test environment. Parsing is also the stronger check —
# it asserts a property of the SOURCE, so it holds whether or not the branch
# happens to be exercised at runtime.
# ---------------------------------------------------------------------------

import ast  # noqa: E402

_NODE_SOURCE = os.path.join(
    os.path.dirname(__file__), "..", "kirra_safety", "perception_governor.py"
)

#: Attribute names that would mean the callback is waiting on something.
_BLOCKING = {"post", "get", "put", "request", "send", "join", "sleep", "wait", "acquire"}


def _method_bodies():
    tree = ast.parse(open(_NODE_SOURCE, encoding="utf-8").read())
    cls = next(n for n in ast.walk(tree) if isinstance(n, ast.ClassDef))
    return {f.name: f for f in cls.body if isinstance(f, ast.FunctionDef)}


def _reachable_attrs(entry, methods, seen=None):
    """Attribute names reachable from `entry`, following self-calls."""
    seen = seen or set()
    if entry in seen or entry not in methods:
        return set()
    seen.add(entry)
    found = set()
    for node in ast.walk(methods[entry]):
        if isinstance(node, ast.Attribute):
            found.add(node.attr)
        if (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == "self"
        ):
            found |= _reachable_attrs(node.func.attr, methods, seen)
    return found


def test_the_scan_callback_reaches_no_blocking_call():
    # AC1, as a property of the source. `_on_scan` runs on rclpy's
    # single-threaded executor, so anything it waits on stalls every other
    # callback on the node — including the scan subscription itself, which is
    # how a slow sidecar used to back up the ingest it was judging.
    methods = _method_bodies()
    assert "_on_scan" in methods, "the callback was renamed; update this guard"

    reachable = _reachable_attrs("_on_scan", methods)
    offending = sorted(reachable & _BLOCKING)
    assert not offending, (
        f"_on_scan can reach blocking call(s) {offending}. The callback must "
        f"copy and return; the worker does the network."
    )


def test_the_guard_would_notice_a_blocking_call():
    # Non-vacuity: the walker must actually follow self-calls into helpers,
    # otherwise the test above passes for any code at all.
    methods = _method_bodies()
    assert "_fetch" in methods, "the worker body was renamed; update this guard"

    # `_fetch` is where the blocking call legitimately lives, so it must trip
    # the same detector the callback is checked against.
    reachable = _reachable_attrs("_fetch", methods)
    assert reachable & _BLOCKING, (
        "the detector found nothing blocking in the worker, so finding nothing "
        "in the callback proves nothing"
    )
