#!/usr/bin/env python3
"""
Wall-clock step detection for the release-token freshness check (#1214).

The verifier signs `issued_at_ms` / `expires_at_ms` in one process; the motor
consumer compares them against its own clock in another, usually on another
host. That comparison needs a SHARED EPOCH, so both ends read wall clock
(`SystemTime::now()` and `time.time()` respectively) — a raw monotonic clock
cannot be compared across processes, so it is not an option here.

AOU-TIMESYNC-001 requires that source be synchronized **and monotonic**, and
says plainly what happens when the second property fails:

    an out-of-domain or unsynchronized timestamp silently disables the deadline
    barrier: no fault is raised, the check simply stops meaning what it asserts

That is the gap this module closes. Nothing in the release path could tell a
stepped wall clock from a working one, so the freshness check would go on
returning confident answers about a quantity that no longer meant anything.

## What is already caught without this

Token lifetime is short (`ROS_BOUND_RELEASE_LIFETIME_MS`, 200 ms). A step
LARGER than the lifetime is already fail-closed by the gate's existing checks:
a big backward step makes every token look `FutureIssued`, a big forward step
makes every token look `Expired`. Either way the robot stops.

The dangerous window is a step SMALLER than the token lifetime. A 150 ms
backward step against a 200 ms lifetime does not trip either check — it just
quietly extends every token's life by 150 ms, so a command that should have
expired stays releasable. That is precisely the "drift within the plausible
range" the AoU says runtime checks do not catch, and it is what this detects.

## How

The monotonic clock is the authority on how much time actually passed. Sampling
both clocks together and differencing their elapsed values isolates the step:
under normal operation the two advance together (NTP *slew* is bounded at a few
hundred ppm — well under a millisecond across a control period), so a
divergence beyond tolerance means the wall clock jumped rather than flowed.

Scheduling jitter, a slow frame, or a long silence do NOT produce divergence:
both clocks are affected equally, so the difference stays near zero.

## Why a hold, rather than a one-frame refusal

After a step the wall clock is at a NEW offset, and the very next sample shows
the two clocks advancing together again — the step is a one-off event, not a
persistent condition. Refusing only the frame that detected it would resume
releasing immediately, while tokens minted before the step are still inside
their validity window under the OLD offset. Those tokens have genuinely
ambiguous validity: the consumer cannot tell whether it or the signer was the
one that moved.

So the guard holds for one maximum token lifetime, measured on the MONOTONIC
clock, which is the one clock the step did not affect. Once that much real time
has passed, every pre-step token is definitively expired under any offset, and
releasing is safe again. The hold is exactly as long as the ambiguity lasts —
no longer, and not arbitrary.
"""

from collections import namedtuple


#: Maximum wall-versus-monotonic divergence tolerated between two samples, in
#: milliseconds, before the wall clock is judged to have stepped.
#:
#: Generous relative to what correct operation produces (disciplined slew across
#: a 100 ms control period contributes well under a millisecond) and still far
#: below the 200 ms token lifetime, so a step small enough to evade this guard is
#: also too small to meaningfully extend a token's life.
DEFAULT_STEP_TOLERANCE_MS = 50

#: Returned by `observe`. `ok` is the only field the caller must act on; the rest
#: exist so a refusal can be diagnosed from a log line rather than reproduced.
ClockVerdict = namedtuple("ClockVerdict", ["ok", "reason", "divergence_ms", "hold_remaining_ms"])

STEP_DETECTED = "CLOCK_STEP_DETECTED"
STEP_HOLD = "CLOCK_STEP_HOLD"
CLOCK_OK = "CLOCK_OK"
#: A monotonic clock that went backwards is not a wall-clock step — it is a
#: broken platform assumption, and it is reported separately so it is never
#: mistaken for the recoverable case.
MONOTONIC_REGRESSED = "MONOTONIC_REGRESSED"


class ClockStepGuard:
    """Detects wall-clock steps by cross-checking against the monotonic clock.

    Pure and deterministic: the caller supplies every clock reading, so the
    behaviour under a step is testable without touching the system clock.
    """

    def __init__(self, maximum_token_lifetime_ms, tolerance_ms=DEFAULT_STEP_TOLERANCE_MS):
        if not isinstance(maximum_token_lifetime_ms, int) or maximum_token_lifetime_ms <= 0:
            raise ValueError("maximum_token_lifetime_ms must be a positive int")
        if not isinstance(tolerance_ms, int) or tolerance_ms <= 0:
            raise ValueError("tolerance_ms must be a positive int")
        self._hold_ms = maximum_token_lifetime_ms
        self._tolerance_ms = tolerance_ms
        self._previous = None
        self._hold_until_mono_ms = None

    @property
    def holding(self) -> bool:
        return self._hold_until_mono_ms is not None

    def observe(self, wall_ms: int, mono_ms: int) -> ClockVerdict:
        """Judge one paired clock sample.

        `wall_ms` is the epoch reading the token check will use; `mono_ms` is a
        monotonic reading taken as close to it as possible.
        """
        previous = self._previous
        self._previous = (wall_ms, mono_ms)

        # First sample establishes the baseline. There is nothing to difference
        # against, so nothing can be concluded — and concluding "fine" by default
        # is correct here: a guard that refused its own first frame would stop a
        # healthy robot at every startup.
        if previous is None:
            return ClockVerdict(True, CLOCK_OK, None, 0)

        previous_wall_ms, previous_mono_ms = previous
        monotonic_delta_ms = mono_ms - previous_mono_ms

        # A monotonic clock that regressed violates the one assumption this
        # detector rests on, so it cannot be used to judge the wall clock. Fail
        # closed and say which clock is at fault — treating this as a wall-clock
        # step would point the operator at the wrong system.
        if monotonic_delta_ms < 0:
            self._hold_until_mono_ms = None
            return ClockVerdict(False, MONOTONIC_REGRESSED, monotonic_delta_ms, None)

        wall_delta_ms = wall_ms - previous_wall_ms
        divergence_ms = wall_delta_ms - monotonic_delta_ms

        if abs(divergence_ms) > self._tolerance_ms:
            # Held from THIS sample's monotonic reading: the step's own arrival
            # is when the ambiguity starts.
            self._hold_until_mono_ms = mono_ms + self._hold_ms
            return ClockVerdict(False, STEP_DETECTED, divergence_ms, self._hold_ms)

        if self._hold_until_mono_ms is not None:
            remaining_ms = self._hold_until_mono_ms - mono_ms
            if remaining_ms > 0:
                return ClockVerdict(False, STEP_HOLD, divergence_ms, remaining_ms)
            # Every token minted before the step is now expired under any
            # offset, so the ambiguity is over.
            self._hold_until_mono_ms = None

        return ClockVerdict(True, CLOCK_OK, divergence_ms, 0)
