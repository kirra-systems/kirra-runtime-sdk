#!/usr/bin/env python3
"""
Pure decision logic for the perception governor's non-blocking scan cycle.

No ROS / rclpy, no `requests`, no clock reads, no globals, no environment. Every
function takes explicit values and returns a decision, so the whole issue /
resolve state machine is unit-testable without a ROS graph or a sidecar.

Sibling of `async_plan`, which does the same job for the doer's plan cycle. The
vocabulary is deliberately shared where the meaning is shared, and deliberately
different where it is not — see SCAN-DRIVEN, NOT TIMER-DRIVEN below.

WHY THIS EXISTS
---------------
`perception_governor._on_scan` used to POST to Taj and block on the reply INSIDE
the scan subscription callback. Under rclpy's single-threaded executor that
couples sensor ingest to a network round trip: while the callback waits, no
other callback on the node runs, and inbound scans queue behind it. A slow or
wedged sidecar therefore backs up the very ingest it is supposed to be judging.

Worse, nothing established that a reply belonged to the scan currently in hand.
A reply for scan N applied to scan N+3 is perception evidence attributed to the
wrong instant — and the #1214 evidence-binding work cannot help, because that
mismatch happens BEFORE the frame identity is stamped onto anything.

SCAN-DRIVEN, NOT TIMER-DRIVEN
-----------------------------
`async_plan`'s doer runs on a timer and asks "may I start a job this tick?".
Perception is driven by scan ARRIVAL, which is not under the node's control, so
the shape differs in one important way: a scan that arrives while a job is in
flight must go somewhere.

It goes into a LATEST-FRAME SLOT of exactly one entry. A newer scan OVERWRITES
an older un-started one rather than queueing behind it, so the backlog is
bounded by construction rather than by a queue length nobody tuned. Overwriting
is correct for perception in a way it would not be for commands: the newest scan
is strictly better evidence than the one it replaces, and the older scan's only
remaining value would be to make the robot act on a staler view of the world.

Overwrites are COUNTED. Bounded-by-construction is a design property; how often
it actually engages is an operational fact, and a governor silently dropping
every second scan should be visible to whoever is wondering why the cap is low.

THREAD DISCIPLINE THIS MODULE ASSUMES
-------------------------------------
The worker computes; the PUBLISH TIMER publishes. A worker deposits an immutable
`PerceptionResult` into a slot and touches nothing else — no publisher, no
counters, no slot state beyond its own deposit. Every publish decision is made
by the executor thread calling into this module. These functions are pure, so
they hold no lock and can never be the thing that stalls a callback.

The timer is not an implementation detail that could be dropped by having the
worker publish directly. It is what makes the DEADLINE enforceable: a worker
blocked on a socket cannot notice that it is late, so something else has to be
running in order to floor the cap on time. See `deadline_breached`.

WHY A DEADLINE IS NOT A SOCKET TIMEOUT
--------------------------------------
`requests`' timeout is per socket operation, not wall-clock. A server that
dribbles bytes holds a worker open indefinitely without ever tripping it — the
same hazard `async_plan.job_overdue_s` exists for. Here it is not merely an
observability problem: perception drives a SPEED CAP, so a sidecar that stops
answering must floor that cap on a schedule the node controls, not whenever the
transport eventually gives up.

So the deadline is judged against the node's own monotonic clock, independently
of whether the HTTP call has returned, and a breach publishes the MRC floor.

EVERY NON-USE OUTCOME FLOORS THE CAP
------------------------------------
There is exactly one publishable outcome, `USE`. Absent, superseded, stale,
faulted and deadline-breached all mean the same thing to the wheels: the node
cannot presently justify a non-zero speed cap, so it publishes zero. They are
kept as distinct reasons because the operator response differs, not because the
verdict does.

WHAT THIS DOES NOT FIX — a claim boundary worth stating
-------------------------------------------------------
This makes the ROS EXECUTOR recoverable. It does not make a blocked socket
recoverable. Those are different claims and conflating them would be the most
consequential misreading of this module.

If a worker never returns — a socket that neither completes nor times out — the
one-in-flight bound means no replacement is ever started, and the newest scan
sits in the slot unprocessed for as long as that lasts. The cap stays floored,
so the outcome is SAFE; but the node does not recover on its own, and
availability is lost indefinitely.

That bound is not a defect to remove. It is what stops a sick sidecar exhausting
threads, and abandoning a worker whose socket may still be live would trade a
bounded availability loss for an unbounded resource one.

Recovery therefore belongs to the RUNTIME, not to this module, and the options
are process-level rather than thread-level: supervision that restarts the node,
a sidecar circuit-breaker and restart, a replaceable worker PROCESS rather than
a thread, or transport cancellation where the transport supports it. None is
implemented here. Pinned by
`test_a_wedged_worker_blocks_every_later_scan_which_is_an_availability_limit`,
so a future change that does add recovery has to come here and say so.

TRANSPORT IS TRANSITIONAL
------------------------
JSON over HTTP is not the intended long-term carrier for a per-scan safety
signal, and nothing here should be read as endorsing it. This work makes the
CURRENT transport non-blocking and identity-checked; it does not make it the
right transport.

The migration target is a typed carrier — ROS 2 messages, or iceoryx2 shared
memory (`tools/iceoryx2-spike/`, #270/#339) — where a reply cannot be
mis-attributed because there is no request/response pairing to get wrong, and
where serialization is not on the path at all.

Two things here survive that migration and two do not. The DECISIONS survive:
one in flight, newest-scan-wins, publish-at-most-once, deadline-floors-the-cap
are properties of the cycle, not of HTTP. The request/response sequence pairing
and the JSON body do not — they exist because this transport cannot express
identity itself.
"""

from collections import namedtuple

# --- issue decision --------------------------------------------------------
ISSUE = "ISSUE"                    # start a request for the pending scan
IN_FLIGHT = "IN_FLIGHT"            # a request is already running — never start a second
NO_PENDING = "NO_PENDING"          # the slot is empty; nothing to ask about

# --- result resolution -----------------------------------------------------
USE = "USE"                # publish the cap this result carries
ABSENT = "ABSENT"          # no result yet (first cycles, or request still running)
SUPERSEDED = "SUPERSEDED"  # the result describes a scan we have already published or moved past
STALE = "STALE"            # the scan behind it aged out of the budget
FAULT = "FAULT"            # the request failed, or the response was unusable

#: Outcomes that publish the MRC floor. Every resolution except `USE`, listed
#: explicitly rather than derived as "not USE" so that adding a state forces a
#: deliberate decision about which side of the line it falls on.
FLOOR_STATES = frozenset({ABSENT, SUPERSEDED, STALE, FAULT})

#: One completed request. Immutable, and built ONLY from values the worker
#: copied out of the HTTP response — never a live ROS message.
PerceptionResult = namedtuple("PerceptionResult", [
    "request_sequence",   # node-local id of the scan this request was issued for
    "scan_received_s",    # monotonic receipt time of that scan
    "response",           # the Taj response dict, or None on fault
    "fault",              # None, or a short stable reason tag
])

#: The immutable snapshot a worker is handed. Plain Python values only: the scan
#: callback copies everything it needs out of the ROS message first, so the
#: worker can never read a message the executor thread is mutating.
PerceptionRequest = namedtuple("PerceptionRequest", [
    "request_sequence",
    "scan_received_s",
    "taj_url",
    "timeout_s",
    "body",               # fully-built Taj request body
])

Resolution = namedtuple("Resolution", ["state", "reason"])

_ABSENT = Resolution(ABSENT, "no completed request yet")


class LatestFrameSlot:
    """A one-entry slot holding the newest scan not yet handed to a worker.

    Bounded by construction: `offer` replaces whatever is there. The count of
    replacements is the observable an operator needs — see the module docstring.

    Not thread-safe by itself. The caller holds it under the same lock it uses
    for the in-flight flag, because "is a job running" and "what would it run
    on" have to be read together or the answer is a race.
    """

    def __init__(self):
        self._pending = None
        self._overwrites = 0

    @property
    def overwrites(self) -> int:
        """How many un-started scans have been displaced by a newer one."""
        return self._overwrites

    @property
    def has_pending(self) -> bool:
        return self._pending is not None

    def offer(self, request) -> bool:
        """Place `request` in the slot. Returns True if it displaced another.

        The displaced scan is NOT counted as a fault. Displacement is the
        intended behaviour under load — the newer scan is better evidence — and
        counting it as an error would make normal operation at a high scan rate
        look like a malfunction.
        """
        displaced = self._pending is not None
        if displaced:
            self._overwrites += 1
        self._pending = request
        return displaced

    def take(self):
        """Remove and return the pending request, or None."""
        pending, self._pending = self._pending, None
        return pending


def decide_issue(request_in_flight: bool, has_pending: bool) -> str:
    """May a request be started right now?

    `IN_FLIGHT` is the bound on concurrency: exactly one request may run, so a
    slow sidecar cannot cause unbounded thread creation however fast scans
    arrive. Requests cannot be cancelled, so the only way to avoid a pile-up is
    not to start one.
    """
    if request_in_flight:
        return IN_FLIGHT
    if not has_pending:
        return NO_PENDING
    return ISSUE


def resolve_result(
    result,
    newest_request_sequence: int,
    last_published_sequence: int,
    now_s: float,
    scan_stale_s: float,
) -> Resolution:
    """May this cycle publish the cap `result` carries?

    Order is deliberate. Absence and faults are decided before identity, because
    a failed request carries no evidence to compare. Identity is decided before
    staleness, because a result about the wrong scan tells us nothing regardless
    of its age.

    Staleness is measured on the SCAN the result describes, not on when the
    request completed: the safety question is "how old is the perception behind
    this cap", and `scan_stale_s` is the operator-set answer to exactly that.
    """
    if result is None:
        return _ABSENT

    if result.fault:
        return Resolution(FAULT, result.fault)

    sequence = result.request_sequence

    # Published at most once, and never backwards. A result for a scan already
    # acted on is refused rather than republished — otherwise a wedged sidecar's
    # last good answer would be re-served forever and a frozen world would look
    # like a clear one.
    if sequence <= last_published_sequence:
        return Resolution(
            SUPERSEDED,
            "scan {} already published (watermark {})".format(
                sequence, last_published_sequence
            ),
        )

    # A result claiming a scan the node has never issued means the worker and the
    # node disagree about identity — refuse rather than guess which is right.
    if sequence > newest_request_sequence:
        return Resolution(
            SUPERSEDED,
            "scan {} ahead of newest issued {}".format(
                sequence, newest_request_sequence
            ),
        )

    age_s = now_s - result.scan_received_s
    if age_s > scan_stale_s:
        return Resolution(
            STALE,
            "scan {:.3f}s old (budget {:.3f}s)".format(age_s, scan_stale_s),
        )

    return Resolution(USE, "")


def deadline_breached(in_flight_since_s, now_s: float, deadline_s: float) -> bool:
    """Has the outstanding request passed the node's own deadline?

    Judged on the node's monotonic clock, deliberately independent of whether
    the HTTP call has returned. `requests`' timeout is per socket operation, so
    a server dribbling bytes never trips it; perception drives a speed cap, so
    the cap has to be floored on a schedule the node controls rather than
    whenever the transport eventually gives up.

    `in_flight_since_s` is None when no request is running. A non-positive or
    non-finite deadline disables the check rather than converting an operator's
    configuration mistake into a permanent floor.
    """
    if in_flight_since_s is None:
        return False
    if not (deadline_s > 0.0) or deadline_s != deadline_s or deadline_s == float("inf"):
        return False
    return (now_s - in_flight_since_s) > deadline_s
