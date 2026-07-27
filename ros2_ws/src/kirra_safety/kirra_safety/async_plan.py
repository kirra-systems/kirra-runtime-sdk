#!/usr/bin/env python3
"""
Pure decision logic for the doer's non-blocking plan cycle.

No ROS / rclpy, no `requests`, no clock reads, no globals, no environment. Every
function takes explicit values and returns a decision, so the whole issue /
resolve state machine is unit-testable without a ROS graph or a sidecar.

WHY THIS EXISTS
---------------
`occy_doer._tick` used to make two SEQUENTIAL BLOCKING POSTs (Taj, then the
planner) inside a 5 Hz timer callback — up to 120 ms of a 200 ms period. With
rclpy's single-threaded executor that stalls every other callback on the node,
including the scan subscription, so a slow or hung sidecar froze perception
ingest as well as planning.

The calls now run in one bounded background job. This module owns the two
decisions that keeps that safe:

  1. ISSUE   — may the tick start a job at all?
  2. RESOLVE — may the tick USE the result a job left behind?

THREAD DISCIPLINE THIS MODULE ASSUMES
-------------------------------------
The worker computes; the TIMER publishes. A worker deposits an immutable
`JobResult` into a slot and touches nothing else — no publisher, no tracker, no
goal/pose/scan state. Every actuation-relevant decision is made by the executor
thread calling into this module. These functions are pure, so they hold no lock
and can never be the thing that stalls a callback.

SCAN IDENTITY vs EVIDENCE IDENTITY — two different sequences
------------------------------------------------------------
ROS 2 removed `header.seq`, so a LaserScan carries no producer sequence number.
The doer therefore stamps its own monotonic `scan_sequence` on arrival: that is
WHICH LIDAR SCAN a job was issued for, and it is what supersession and staleness
are judged against.

Taj's `frame_id.scan_sequence` is a DIFFERENT counter — Taj's own per-request
counter (`kirra-sidecars/src/taj.rs`, `state.scan_sequence += 1`). Comparing it
to the node's counter would be meaningless. It is used for the one thing it can
prove: that the planner planned against the SAME Taj frame this job fetched
(`taj_scan_sequence == plan_scan_sequence`). A mismatch means the two sidecar
responses describe different perception, which is a fault, not a stale result.

WHY A RESULT IS USED AT MOST ONCE
---------------------------------
`last_used_scan_sequence` is a strictly-advancing watermark, the same discipline
the V2 release gate applies to command sequences. It stops a result being
republished after its scan has been superseded, and stops out-of-order delivery
walking the doer backwards. A tick that finds nothing new to say holds, rather
than repeating stale intent.
"""

from collections import namedtuple

# --- issue decision --------------------------------------------------------
ISSUE = "ISSUE"                    # start a job for the current snapshot
IN_FLIGHT = "IN_FLIGHT"            # a job is already running — never start a second
NO_FRESH_INPUT = "NO_FRESH_INPUT"  # nothing new to plan against

# --- result resolution -----------------------------------------------------
USE = "USE"                # publish a proposal built from this result
ABSENT = "ABSENT"          # no result yet (first ticks, or job still running)
SUPERSEDED = "SUPERSEDED"  # the result describes a scan we have moved past
STALE = "STALE"            # the perception behind it aged out of the budget
FAULT = "FAULT"            # the job failed, or its two responses disagree

# One completed job. Immutable, and built ONLY from values the worker copied out
# of the HTTP responses — never a live ROS message.
JobResult = namedtuple("JobResult", [
    "requested_scan_sequence",  # node-local id of the scan this job was issued for
    "scan_received_s",          # monotonic receipt time of that scan
    "taj_scan_sequence",        # Taj's own frame_id.scan_sequence (or None on fault)
    "plan_scan_sequence",       # the planner's echoed perception_frame_id (or None)
    "plan",                     # the planner response dict, or None on fault
    "camera_healthy",           # Taj's camera_healthy, surfaced for the tick to log
    "fault",                    # None, or a short stable reason tag
])

# The immutable snapshot a worker is handed. Contains plain Python values only:
# the tick copies everything it needs out of the ROS messages first, so the
# worker can never read a message the executor thread is mutating.
JobRequest = namedtuple("JobRequest", [
    "scan_sequence",
    "scan_received_s",
    "taj_url",
    "planner_url",
    "timeout_s",
    "perception",   # fully-built Taj request body
    "plan_fields",  # everything the plan body needs except the Taj-derived parts
])

Resolution = namedtuple("Resolution", ["state", "reason"])

_ABSENT = Resolution(ABSENT, "no completed job yet")


def decide_issue(job_in_flight, has_fresh_input):
    """May this tick start a plan job?

    `IN_FLIGHT` is the bound on concurrency: exactly one job may run, so a slow
    sidecar cannot cause unbounded thread creation however fast the timer fires.
    Requests cannot be cancelled, so the only way to avoid a pile-up is not to
    start one.
    """
    if job_in_flight:
        return IN_FLIGHT
    if not has_fresh_input:
        return NO_FRESH_INPUT
    return ISSUE


def resolve_result(
    result,
    newest_scan_sequence,
    last_used_scan_sequence,
    now_s,
    scan_stale_s,
):
    """May this tick publish a proposal built from `result`?

    Order is deliberate. Absence and faults are decided before identity, because
    a failed job carries no evidence to compare. Identity is decided before
    staleness, because a result about the wrong scan tells us nothing regardless
    of its age.

    Staleness is measured on the SCAN the result describes, not on when the job
    finished: the safety question is "how old is the perception behind this
    proposal", and `scan_stale_s` is the operator-set answer to exactly that.
    """
    if result is None:
        return _ABSENT

    if result.fault:
        return Resolution(FAULT, result.fault)

    # Evidence consistency: the planner must have planned against the SAME Taj
    # frame this job fetched. These are Taj's counter, not the lidar's.
    if (
        result.taj_scan_sequence is None
        or result.plan_scan_sequence is None
        or result.taj_scan_sequence != result.plan_scan_sequence
    ):
        return Resolution(
            FAULT,
            "evidence mismatch: taj={} plan={}".format(
                result.taj_scan_sequence, result.plan_scan_sequence
            ),
        )

    seq = result.requested_scan_sequence

    # Used at most once, and never backwards. A result for a scan we have
    # already acted on (or moved past) is refused rather than republished.
    if seq <= last_used_scan_sequence:
        return Resolution(
            SUPERSEDED,
            "scan {} already used (watermark {})".format(seq, last_used_scan_sequence),
        )

    # A result claiming a scan the node has never seen means the worker and the
    # node disagree about identity — refuse rather than guess.
    if seq > newest_scan_sequence:
        return Resolution(
            SUPERSEDED,
            "scan {} ahead of newest {}".format(seq, newest_scan_sequence),
        )

    age_s = now_s - result.scan_received_s
    if age_s > scan_stale_s:
        return Resolution(
            STALE, "perception {:.3f}s old (budget {:.3f}s)".format(age_s, scan_stale_s)
        )

    return Resolution(USE, "")


def evidence_sequences(taj_response, plan_response):
    """Pull the two evidence sequences out of the sidecar responses.

    Returns `(taj_scan_sequence, plan_scan_sequence)`, either of which is None
    when the response did not carry a usable one. Kept here (not in the worker)
    so the extraction rule is testable and the worker stays a thin shell.
    """
    taj_seq = _frame_scan_sequence(
        taj_response.get("frame_id") if isinstance(taj_response, dict) else None
    )
    plan_seq = _frame_scan_sequence(
        plan_response.get("perception_frame_id")
        if isinstance(plan_response, dict)
        else None
    )
    return taj_seq, plan_seq


def _frame_scan_sequence(frame):
    if not isinstance(frame, dict):
        return None
    value = frame.get("scan_sequence")
    # bool is an int subclass; a JSON `true` is not a sequence number.
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        return None
    return value
