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
their validity window under the OLD offset.

## The reference model — how long the hold must be

Let `L` be the maximum token lifetime and `δ` the magnitude of a BACKWARD step.
Take a token minted at signer time `T`, expiring at `T + L`. After the step the
consumer's wall clock reads `true_time − δ`, so it goes on accepting while:

    true_time − δ  <  T + L        ⟺        true_time  <  T + L + δ

The worst case is a token minted at the instant of the step (`T = t₀`), so the
last real moment at which a pre-step token can still be accepted is:

    t₀ + L + δ

The hold therefore runs for **`L + δ`**, not `L`. A hold of `L` alone leaves a
`δ`-wide window in which a stale token is admitted as fresh — the exact failure
this module exists to prevent, reintroduced at the boundary.

Worth noting why `δ` cannot be dropped as "large steps are caught anyway": a
large backward step does not prevent acceptance, it *delays* it. Immediately
after the step the token reads as `FutureIssued`; it becomes acceptable again at
`t₀ + δ` and stays acceptable for `L`. The acceptance window is shifted, not
removed, which is why the hold has to span it.

**Origin.** The hold is measured from the monotonic reading of the sample that
DETECTED the step. The step itself occurred somewhere in the interval since the
previous sample, so the detecting sample is at or after it — which is the
correct worst case here, because the quantity being bounded is the LATEST
possible pre-step mint.

**Direction.** A FORWARD step moves the consumer's clock ahead, so tokens expire
sooner: fail-closed, no extension possible. `δ` is added only for backward
steps. The base `L` hold still applies in both directions, since a forward step
is equally good evidence that the clock is not to be trusted for an interval.

**Clock.** All of this is measured on the MONOTONIC clock, the one the step did
not affect. Measuring the hold on the clock under suspicion would let a forward
step end its own hold early.

## Threat model: process restart

The hold lives in memory. A consumer restarted during the ambiguity window
starts with no baseline, accepts its first sample, and is releasing again
immediately.

This is deliberately not persisted, because it would be the only part of the
consumer's replay protection that was. The release gate's sequence watermark,
nonce watermark and perception-frame watermark are all in-memory too, so a
restarted consumer already accepts any sequence and any nonce. Persisting the
clock hold alone would close the narrowest of those windows while leaving the
wider ones open, and would misrepresent restart as a bounded event when it is a
full re-establishment of trust.

The honest statement is therefore: **a consumer restart resets all replay
protection, including this hold.** Restart is a new trust epoch, not a bounded
interruption, and no guard here claims continuity across it.

Note this cuts further than the hold: a pre-restart release token remains
acceptable after a restart while it is inside its own wall-clock lifetime,
because the sequence and nonce watermarks that would have refused it were
cleared too. That is #1230, and it is why persisting this hold alone would have
been the wrong fix — it would have defended the narrowest window while leaving
the wider one open and looking closed. See `docs/safety/EVIDENCE_BINDING.md`
§4.7.
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
            # A BACKWARD step extends every pre-step token's apparent life by its
            # own magnitude, so the hold must cover `L + δ` — see the reference
            # model in the module docstring. A forward step only shortens
            # validity, so it contributes no extension.
            backward_ms = max(0, -divergence_ms)
            hold_ms = self._hold_ms + backward_ms
            # Measured from THIS sample: the step occurred at or before it, and
            # the quantity being bounded is the LATEST possible pre-step mint.
            self._hold_until_mono_ms = mono_ms + hold_ms
            return ClockVerdict(False, STEP_DETECTED, divergence_ms, hold_ms)

        if self._hold_until_mono_ms is not None:
            remaining_ms = self._hold_until_mono_ms - mono_ms
            if remaining_ms > 0:
                return ClockVerdict(False, STEP_HOLD, divergence_ms, remaining_ms)
            # Every token minted before the step is now expired under any
            # offset, so the ambiguity is over.
            self._hold_until_mono_ms = None

        return ClockVerdict(True, CLOCK_OK, divergence_ms, 0)
