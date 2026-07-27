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

sys.path.insert(
    0, os.path.join(os.path.dirname(__file__), "..", "kirra_safety")
)
from async_plan import (  # noqa: E402
    ABSENT, FAULT, IN_FLIGHT, ISSUE, NO_FRESH_INPUT, STALE, SUPERSEDED, USE,
    JobResult, decide_issue, evidence_sequences, resolve_result,
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
