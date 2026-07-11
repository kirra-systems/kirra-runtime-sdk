#!/usr/bin/env python3
"""
Pure response -> decision logic for the Kirra cmd_vel interceptor.

No ROS / rclpy dependency — mirrors the pure-transform / ROS-shim split used in
the parko sensor mappings, so the fail-closed decision is unit-testable without
booting ROS.

CONTRACT: a 200 from the gateway is REQUIRED to carry the canonical enforcement
keys (`action`, `enforced_linear_velocity_mps`, `enforced_steering_angle_deg`)
with FINITE enforced values. Anything else — a non-200 status, a malformed or
missing key, or a non-finite enforced value — is a FAULT and yields `Stop`.

There is deliberately NO fallback to the original (unclamped) command: a 200
that does not carry a valid enforced command stops the robot rather than
forwarding the original. Forwarding the original would be the exact fail-OPEN
this interceptor exists to prevent (it would push the un-clamped command to the
motors), so it is removed entirely.
"""

import math
from collections import namedtuple

# Forward the ENFORCED command to the motors.
Forward = namedtuple("Forward", ["action", "enforced_v", "enforced_s"])
# Stop the robot (fail-closed); `reason` is a short tag for logging/monitoring.
Stop = namedtuple("Stop", ["reason"])


def _is_finite_number(x):
    """True only for a real, finite int/float.

    Rejects None, bool (an int subclass but not a measurement), non-numeric
    types (e.g. a JSON string), NaN, and +/-Inf — none of which may ever be
    turned into a motor command.
    """
    if isinstance(x, bool):
        return False
    if not isinstance(x, (int, float)):
        return False
    return math.isfinite(x)


def decide_enforcement(status_code, parsed, proposed=None):
    """Map an HTTP (status_code, parsed-body) pair to a fail-closed decision.

    Returns `Forward(action, enforced_v, enforced_s)` ONLY for a valid 200 that
    carries the canonical enforcement keys with finite enforced values; returns
    `Stop(reason)` for every other case.

    `proposed` is accepted for signature/compat but intentionally UNUSED: the
    original-command fallback has been removed (see module docstring).
    """
    del proposed  # explicitly unused — no fallback to the original command

    if status_code != 200:
        if status_code in (403, 503):
            return Stop("POSTURE_BLOCKED_HTTP_{}".format(status_code))
        return Stop("HTTP_{}".format(status_code))

    if not isinstance(parsed, dict):
        return Stop("MALFORMED_200_NOT_JSON_OBJECT")

    action = parsed.get("action")
    enforced_v = parsed.get("enforced_linear_velocity_mps")
    enforced_s = parsed.get("enforced_steering_angle_deg")

    # All three canonical keys are REQUIRED on a 200.
    if action is None or enforced_v is None or enforced_s is None:
        return Stop("MALFORMED_200_MISSING_CANONICAL_KEYS")

    # Never build a Twist from a non-finite / non-numeric enforced value.
    if not _is_finite_number(enforced_v) or not _is_finite_number(enforced_s):
        return Stop("MALFORMED_200_NONFINITE_ENFORCED_VALUE")

    return Forward(str(action), float(enforced_v), float(enforced_s))


# ---------------------------------------------------------------------------
# Track-A A3 — single wheelbase source
# ---------------------------------------------------------------------------

# Absolute tolerance for the wheelbase cross-check (meters). The two values are
# the SAME physical constant carried through config and one JSON float
# round-trip, so any real difference is a config error, not noise; 1e-6 m
# (a micrometer) admits float serialization jitter and nothing else.
WHEELBASE_TOLERANCE_M = 1e-6


def wheelbase_consistent(param_m, reported_m):
    """True iff the interceptor's configured wheelbase matches the wheelbase the
    verifier reports it used for the steering→angular conversion (the active
    class contract's wheelbase — the same L the P6 lateral-accel check runs
    against).

    FAIL-CLOSED shape: a missing/non-finite/non-numeric REPORTED value returns
    False (a verifier that mints releases but cannot say which wheelbase it
    used is a fault, not a pass). The caller latches a permanent stop on False:
    a mismatch means executed yaw = commanded yaw × (param/reported) — what
    Kirra approved is not what the motors would do.
    """
    if not _is_finite_number(param_m) or not _is_finite_number(reported_m):
        return False
    return abs(float(param_m) - float(reported_m)) <= WHEELBASE_TOLERANCE_M
