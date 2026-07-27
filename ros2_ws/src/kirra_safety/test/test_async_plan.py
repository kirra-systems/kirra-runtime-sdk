"""
Unit tests for the doer's non-blocking plan-cycle decisions.

No ROS / rclpy / requests — imports the pure `async_plan` module directly. Every
issue state and every resolution state is covered, including the two that exist
purely to stop the async rework introducing a hazard the synchronous loop could
not have: SUPERSEDED (a reply about a scan we have moved past) and the evidence
mismatch FAULT (the planner planned against a different Taj frame than the one
this job fetched).

Run:  pytest ros2_ws/src/kirra_safety/test/test_async_plan.py
"""

import os
import sys

import pytest

sys.path.insert(
    0, os.path.join(os.path.dirname(__file__), "..", "kirra_safety")
)
from async_plan import (  # noqa: E402
    ABSENT, ANNOUNCE, FAULT, IN_FLIGHT, ISSUE, NO_FRESH_INPUT, RECOVERED,
    SILENT, STALE, SUPERSEDED, USE,
    HoldMonitor, JobResult, decide_issue, evidence_sequences, job_overdue_s,
    resolve_result,
)

BUDGET_S = 0.25


def _result(**over):
    base = dict(
        requested_scan_sequence=10,
        scan_received_s=100.0,
        taj_scan_sequence=7,
        plan_scan_sequence=7,
        plan={"kind": "Motion"},
        camera_healthy=True,
        fault=None,
    )
    base.update(over)
    return JobResult(**base)


def _resolve(result, newest=10, last_used=9, now_s=100.05, budget_s=BUDGET_S):
    return resolve_result(result, newest, last_used, now_s, budget_s)


# ---------------------------------------------------------------------------
# decide_issue — every state
# ---------------------------------------------------------------------------

def test_issue_when_idle_with_fresh_input():
    assert decide_issue(job_in_flight=False, has_fresh_input=True) == ISSUE


def test_in_flight_blocks_a_second_job():
    # The concurrency bound: requests cannot be cancelled, so the only way to
    # avoid a pile-up is not to start one.
    assert decide_issue(job_in_flight=True, has_fresh_input=True) == IN_FLIGHT


def test_in_flight_wins_over_missing_input():
    assert decide_issue(job_in_flight=True, has_fresh_input=False) == IN_FLIGHT


def test_no_fresh_input_blocks_issue():
    assert decide_issue(job_in_flight=False, has_fresh_input=False) == NO_FRESH_INPUT


# ---------------------------------------------------------------------------
# resolve_result — every state
# ---------------------------------------------------------------------------

def test_use_for_the_newest_scan_within_budget():
    assert _resolve(_result()).state == USE


def test_absent_when_no_job_has_completed():
    resolution = _resolve(None)
    assert resolution.state == ABSENT
    assert "no completed job" in resolution.reason


def test_fault_is_reported_with_its_reason():
    resolution = _resolve(_result(fault="service-error:Timeout"))
    assert resolution.state == FAULT
    assert resolution.reason == "service-error:Timeout"


def test_fault_outranks_identity_and_staleness():
    # A failed job carries no evidence to compare, so the fault is the answer
    # even when the scan is also superseded and ancient.
    resolution = _resolve(
        _result(fault="service-error:ConnectionError", requested_scan_sequence=1),
        newest=99, last_used=98, now_s=1e6,
    )
    assert resolution.state == FAULT


def test_superseded_when_the_scan_was_already_used():
    # The strictly-advancing watermark: a result is consumed at most once.
    resolution = _resolve(_result(requested_scan_sequence=9), newest=10, last_used=9)
    assert resolution.state == SUPERSEDED
    assert "already used" in resolution.reason


def test_superseded_for_an_older_scan_after_a_newer_one_arrived():
    resolution = _resolve(_result(requested_scan_sequence=5), newest=12, last_used=8)
    assert resolution.state == SUPERSEDED


def test_superseded_when_the_result_claims_a_scan_the_node_never_saw():
    # Worker and node disagreeing about identity is refused, not guessed at.
    resolution = _resolve(_result(requested_scan_sequence=99), newest=10, last_used=9)
    assert resolution.state == SUPERSEDED
    assert "ahead of newest" in resolution.reason


def test_stale_when_the_perception_aged_out_of_the_budget():
    # Staleness is measured on the SCAN, not on when the job finished: the
    # safety question is how old the perception behind the proposal is.
    resolution = _resolve(_result(scan_received_s=100.0), now_s=100.0 + BUDGET_S + 0.01)
    assert resolution.state == STALE
    assert "budget" in resolution.reason


def test_exactly_at_the_budget_is_still_usable():
    assert _resolve(_result(scan_received_s=100.0), now_s=100.0 + BUDGET_S).state == USE


def test_evidence_mismatch_is_a_fault_not_a_stale_result():
    # The planner answered about a DIFFERENT Taj frame than this job fetched.
    # That is inconsistent evidence, not old evidence.
    resolution = _resolve(_result(taj_scan_sequence=7, plan_scan_sequence=8))
    assert resolution.state == FAULT
    assert "evidence mismatch" in resolution.reason


def test_missing_evidence_sequence_is_a_fault():
    for over in ({"taj_scan_sequence": None}, {"plan_scan_sequence": None}):
        assert _resolve(_result(**over)).state == FAULT, over


def test_resolution_order_identity_before_staleness():
    # A result about the wrong scan tells us nothing regardless of its age, so
    # supersession is decided first.
    resolution = _resolve(
        _result(requested_scan_sequence=3, scan_received_s=0.0),
        newest=10, last_used=9, now_s=1e6,
    )
    assert resolution.state == SUPERSEDED


# ---------------------------------------------------------------------------
# evidence_sequences
# ---------------------------------------------------------------------------

def test_evidence_sequences_extracts_both():
    taj = {"frame_id": {"scan_sequence": 4}}
    plan = {"perception_frame_id": {"scan_sequence": 4}}
    assert evidence_sequences(taj, plan) == (4, 4)


def test_evidence_sequences_rejects_unusable_values():
    # Taj's invalid-frame sentinel is scan_sequence 0; a JSON `true` is an int
    # subclass but not a sequence number.
    for bad in (0, -1, True, "4", None, 1.5):
        taj = {"frame_id": {"scan_sequence": bad}}
        assert evidence_sequences(taj, {})[0] is None, bad


def test_evidence_sequences_tolerates_missing_structure():
    assert evidence_sequences({}, {}) == (None, None)
    assert evidence_sequences(None, None) == (None, None)
    assert evidence_sequences({"frame_id": "nope"}, {"perception_frame_id": 3}) == (None, None)


# ---------------------------------------------------------------------------
# HoldMonitor — observability only; it can never change a verdict
# ---------------------------------------------------------------------------

STREAK = 4


def _monitor():
    return HoldMonitor(warn_streak=STREAK)


def _hold_n(monitor, n, state=FAULT):
    return [monitor.observe(state) for _ in range(n)]


def test_a_short_run_of_holds_says_nothing():
    # A slow round trip or a dropped frame must not log. Only a SUSTAINED
    # episode is worth an operator's attention.
    monitor = _monitor()
    for notice in _hold_n(monitor, STREAK - 1):
        assert notice.action == SILENT


def test_a_sustained_hold_is_announced_exactly_once():
    monitor = _monitor()
    notices = _hold_n(monitor, STREAK + 20)
    announced = [n for n in notices if n.action == ANNOUNCE]
    assert len(announced) == 1
    assert announced[0].state == FAULT
    assert announced[0].count == STREAK
    # The tick that crossed the threshold is the one that announced.
    assert notices[STREAK - 1].action == ANNOUNCE


def test_a_changed_cause_is_announced_again():
    # "The sidecar stopped timing out but now returns mismatched evidence" is a
    # different fault with a different fix, so it must not be swallowed by the
    # latch on the previous one.
    monitor = _monitor()
    _hold_n(monitor, STREAK)
    assert monitor.observe(STALE).action == ANNOUNCE
    assert monitor.observe(STALE).action == SILENT


def test_recovery_is_reported_and_re_arms_the_latch():
    monitor = _monitor()
    _hold_n(monitor, STREAK)
    recovered = monitor.observe(USE)
    assert recovered.action == RECOVERED
    assert recovered.state == FAULT
    assert recovered.count == STREAK
    assert monitor.consecutive == 0
    # Re-armed: the next episode announces rather than staying latched.
    assert [n.action for n in _hold_n(monitor, STREAK)][-1] == ANNOUNCE


def test_recovery_from_an_unannounced_run_is_silent():
    # Holding briefly and then driving on is normal. It is not an event.
    monitor = _monitor()
    _hold_n(monitor, STREAK - 1)
    assert monitor.observe(USE).action == SILENT


def test_one_success_breaks_the_streak():
    monitor = _monitor()
    for _ in range(20):
        _hold_n(monitor, STREAK - 1)
        assert monitor.observe(USE).action == SILENT
    assert monitor.consecutive == 0


def test_a_warn_streak_below_one_is_clamped_not_disabled():
    # A misconfigured 0 must not mean "announce nothing" — that would silently
    # remove the very thing this monitor exists for.
    monitor = HoldMonitor(warn_streak=0)
    assert monitor.observe(FAULT).action == ANNOUNCE


# ---------------------------------------------------------------------------
# job_overdue_s
# ---------------------------------------------------------------------------

BUDGET_JOB_S = 0.12
FACTOR = 5.0
DEADLINE_S = BUDGET_JOB_S * FACTOR


def test_no_job_in_flight_is_never_overdue():
    assert job_overdue_s(None, 1e9, BUDGET_JOB_S, FACTOR) is None


def test_a_job_inside_its_deadline_is_not_overdue():
    assert job_overdue_s(100.0, 100.0 + DEADLINE_S, BUDGET_JOB_S, FACTOR) is None


def test_a_job_past_its_deadline_reports_the_excess():
    over = job_overdue_s(100.0, 100.0 + DEADLINE_S + 0.25, BUDGET_JOB_S, FACTOR)
    assert over == pytest.approx(0.25)


def test_an_unusable_budget_disables_the_check():
    # Rather than turning a misconfigured timeout into an alarm every tick.
    for bad in (0.0, -1.0, float("nan"), float("inf")):
        assert job_overdue_s(100.0, 1e9, bad, FACTOR) is None, bad
