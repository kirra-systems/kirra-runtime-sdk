#!/usr/bin/env python3
"""
Tests for the wall-clock step detector (#1214).

Pure — no ROS, no .so, no system clock. Every reading is injected, so the
behaviour under a clock step is exercised directly rather than reproduced.

Run:  pytest robot/clock_step_guard_test.py
"""

import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from clock_step_guard import (  # noqa: E402
    ClockStepGuard,
    CLOCK_OK,
    DEFAULT_STEP_TOLERANCE_MS,
    MONOTONIC_REGRESSED,
    STEP_DETECTED,
    STEP_HOLD,
)

LIFETIME_MS = 200


def guard(tolerance_ms=DEFAULT_STEP_TOLERANCE_MS):
    return ClockStepGuard(LIFETIME_MS, tolerance_ms=tolerance_ms)


def run(g, samples):
    """Feed (wall, mono) pairs, return the verdicts."""
    return [g.observe(w, m) for w, m in samples]


# ---------------------------------------------------------------------------
# Normal operation must not be disturbed. A guard that refuses healthy frames
# stops the robot for no reason, which is its own safety problem.
# ---------------------------------------------------------------------------


def test_the_first_sample_is_accepted():
    # Nothing to difference against. Refusing here would stop a healthy robot at
    # every startup.
    v = guard().observe(1_700_000_000_000, 5_000)
    assert v.ok and v.reason == CLOCK_OK
    assert v.divergence_ms is None


def test_clocks_advancing_together_stay_ok():
    g = guard()
    wall, mono = 1_700_000_000_000, 5_000
    # The first observation is the baseline and reports no divergence, so the
    # zero-divergence assertion starts from the second.
    wall += 100
    mono += 100
    assert g.observe(wall, mono).ok
    for _ in range(50):
        wall += 100
        mono += 100
        v = g.observe(wall, mono)
        assert v.ok, v
        assert v.divergence_ms == 0


def test_scheduling_jitter_does_not_read_as_a_step():
    # A late frame delays BOTH clocks equally, so the difference stays at zero.
    # This is the case a naive "wall clock moved a lot" check would fail.
    g = guard()
    wall, mono = 1_700_000_000_000, 5_000
    for delay in (100, 340, 95, 1_500, 102):
        wall += delay
        mono += delay
        v = g.observe(wall, mono)
        assert v.ok, f"a {delay} ms gap must not read as a step: {v}"


def test_bounded_slew_stays_within_tolerance():
    # Disciplined NTP slew is a few hundred ppm — well under a millisecond per
    # control period. Accumulating it must never trip the guard.
    g = guard()
    wall, mono = 1_700_000_000_000, 5_000
    for _ in range(200):
        mono += 100
        wall += 100 + 1  # ~1% — far more aggressive than real slew
        assert g.observe(wall, mono).ok


# ---------------------------------------------------------------------------
# The failure this exists to catch.
# ---------------------------------------------------------------------------


def test_a_backward_step_smaller_than_the_token_lifetime_is_detected():
    # THE case. A 150 ms backward step against a 200 ms token lifetime trips
    # neither `Expired` nor `FutureIssued` — it silently extends every token's
    # life by 150 ms, so a command that should have expired stays releasable.
    g = guard()
    g.observe(1_700_000_000_000, 5_000)
    v = g.observe(1_700_000_000_100 - 150, 5_100)
    assert not v.ok
    assert v.reason == STEP_DETECTED
    assert v.divergence_ms == -150


def test_a_forward_step_smaller_than_the_token_lifetime_is_detected():
    g = guard()
    g.observe(1_700_000_000_000, 5_000)
    v = g.observe(1_700_000_000_100 + 150, 5_100)
    assert not v.ok
    assert v.reason == STEP_DETECTED
    assert v.divergence_ms == 150


def test_a_large_step_is_detected_too():
    # Already fail-closed at the gate via Expired/FutureIssued, but the guard
    # must not have a blind spot at the top end.
    g = guard()
    g.observe(1_700_000_000_000, 5_000)
    v = g.observe(1_700_000_060_000, 5_100)
    assert not v.ok and v.reason == STEP_DETECTED


def test_a_step_exactly_at_tolerance_is_accepted_and_just_past_it_is_not():
    # The boundary is stated, so a later change to the comparison shows up here
    # rather than silently widening what counts as healthy.
    g = guard(tolerance_ms=50)
    g.observe(1_000_000, 5_000)
    assert g.observe(1_000_150, 5_100).ok, "exactly at tolerance is not a step"

    g2 = guard(tolerance_ms=50)
    g2.observe(1_000_000, 5_000)
    assert not g2.observe(1_000_151, 5_100).ok, "one ms past tolerance is a step"


# ---------------------------------------------------------------------------
# The hold. This is the part that makes detection actually safe.
# ---------------------------------------------------------------------------


def test_the_hold_outlasts_every_token_minted_before_the_step():
    # After a step the clocks advance together again, so the step itself is a
    # one-off. Resuming immediately would release tokens minted under the OLD
    # offset that are still inside their validity window. The hold runs for one
    # maximum token lifetime on the MONOTONIC clock — the one the step did not
    # touch — after which every pre-step token is expired under any offset.
    g = guard()
    g.observe(1_000_000, 5_000)
    step = g.observe(1_000_100 - 150, 5_100)
    assert step.reason == STEP_DETECTED
    assert step.hold_remaining_ms == LIFETIME_MS

    wall, mono = 1_000_100 - 150, 5_100
    refused = 0
    while True:
        wall += 50
        mono += 50
        v = g.observe(wall, mono)
        if v.ok:
            break
        assert v.reason == STEP_HOLD, v
        refused += 1
        assert refused < 100, "the hold must terminate, not latch forever"

    elapsed_ms = mono - 5_100
    assert elapsed_ms >= LIFETIME_MS, (
        f"released after only {elapsed_ms} ms of real time; a token minted just "
        f"before the step could still have been inside its {LIFETIME_MS} ms window"
    )


def test_the_hold_counts_down_on_real_elapsed_time():
    # The remaining hold is accounted against the monotonic clock — the one the
    # step did not affect — so it tracks how much REAL time has passed since the
    # ambiguity began, not how much the untrusted clock claims has passed.
    #
    # Note this cannot be probed by moving the wall clock alone: doing so is
    # itself a fresh step and restarts the hold (see the test below). The
    # observable consequence is the countdown, so that is what is pinned.
    g = guard()
    g.observe(1_000_000, 5_000)
    step = g.observe(1_000_100 + 300, 5_100)  # forward step, hold starts
    assert step.hold_remaining_ms == LIFETIME_MS

    wall, mono = 1_000_400, 5_100
    for expected_remaining in (150, 100, 50):
        wall += 50
        mono += 50
        v = g.observe(wall, mono)
        assert not v.ok and v.reason == STEP_HOLD, v
        assert v.hold_remaining_ms == expected_remaining, (
            f"hold must count down with real elapsed time: {v}"
        )


def test_a_second_step_during_a_hold_restarts_it():
    g = guard()
    g.observe(1_000_000, 5_000)
    g.observe(1_000_100 - 150, 5_100)

    g.observe(1_000_050, 5_150)  # in hold
    second = g.observe(1_000_100 - 200, 5_200)  # another step
    assert second.reason == STEP_DETECTED
    assert second.hold_remaining_ms == LIFETIME_MS, "the clock is unsettled again"


def test_normal_operation_resumes_after_the_hold():
    # Non-vacuity for the hold tests: the guard must not simply latch. A
    # detector that never releases is indistinguishable from a broken robot.
    g = guard()
    g.observe(1_000_000, 5_000)
    g.observe(1_000_100 - 150, 5_100)

    wall, mono = 1_000_100 - 150, 5_100
    for _ in range(10):
        wall += 100
        mono += 100
        g.observe(wall, mono)

    assert not g.holding
    wall += 100
    mono += 100
    v = g.observe(wall, mono)
    assert v.ok and v.reason == CLOCK_OK


# ---------------------------------------------------------------------------
# The assumption the detector itself rests on.
# ---------------------------------------------------------------------------


def test_a_regressed_monotonic_clock_is_reported_as_itself():
    # The monotonic clock is the authority this whole check depends on. If it
    # regresses, the guard cannot judge the wall clock at all — and reporting
    # that as a wall-clock step would send an operator to the wrong system.
    g = guard()
    g.observe(1_000_000, 5_000)
    v = g.observe(1_000_100, 4_900)
    assert not v.ok
    assert v.reason == MONOTONIC_REGRESSED
    assert v.hold_remaining_ms is None


def test_construction_rejects_unusable_parameters():
    for bad in (0, -1, None, 1.5, "200"):
        try:
            ClockStepGuard(bad)
        except (ValueError, TypeError):
            continue
        raise AssertionError(f"accepted maximum_token_lifetime_ms={bad!r}")

    for bad in (0, -1, None, 1.5):
        try:
            ClockStepGuard(LIFETIME_MS, tolerance_ms=bad)
        except (ValueError, TypeError):
            continue
        raise AssertionError(f"accepted tolerance_ms={bad!r}")
