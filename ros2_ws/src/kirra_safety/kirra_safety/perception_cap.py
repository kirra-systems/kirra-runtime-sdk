#!/usr/bin/env python3
"""
Pure, fail-closed perception-cap clamp for the cmd_vel interceptor.

Taj (the geometric perception layer, ADR-0015) turns the lidar scan into a drivable
corridor and publishes an **assured-clear-distance (ACD) speed cap** — the speed from
which the robot can still stop within the clear distance ahead. This module applies that
cap to the doer's proposed forward speed BEFORE the command reaches the KIRRA governor:
Taj *tightens* the envelope, the governor still *bounds* the result.

It is deliberately a pure function (no ROS, no I/O) so the safety-critical clamp is unit
testable standalone — the same discipline as `enforcement_decision.py`.

Fail-closed rules (a perception fault must never let the robot speed up):
  - `enabled` False                 → no derate at all (opt-in; byte-identical prior path).
  - cap missing / older than `stale_s` → STOP (0.0). A silent or stale Taj is a fault.
  - cap non-finite or negative        → STOP (0.0). A malformed cap is a fault.
  - otherwise                          → clamp |speed| to the cap, preserving direction.
"""

import math


# Result reasons (also used as the enforcement-action suffix for monitoring).
DISABLED = "PERCEPTION_DISABLED"
STALE = "PERCEPTION_STALE"
INVALID = "PERCEPTION_INVALID"
CAPPED = "PERCEPTION_CAP"
PASS = "PERCEPTION_PASS"


def apply_perception_cap(proposed_mps, cap_mps, cap_age_s, enabled, stale_s):
    """
    Return (effective_speed_mps, reason).

    `proposed_mps`  the doer's proposed forward speed (signed; sign = direction).
    `cap_mps`       the latest ACD cap from Taj, or None if none received yet.
    `cap_age_s`     wall-clock age of that cap in seconds, or None.
    `enabled`       whether the perception derate is active (opt-in).
    `stale_s`       max cap age before it is treated as a perception fault.
    """
    if not enabled:
        return proposed_mps, DISABLED

    # Missing or stale cap → the perception feed is not trustworthy → fail closed.
    if cap_mps is None or cap_age_s is None or cap_age_s > stale_s:
        return 0.0, STALE

    # Malformed cap → fail closed (never trust a non-finite / negative bound).
    if not math.isfinite(cap_mps) or cap_mps < 0.0:
        return 0.0, INVALID

    sign = 1.0 if proposed_mps >= 0.0 else -1.0
    magnitude = min(abs(proposed_mps), cap_mps)
    reason = CAPPED if magnitude < abs(proposed_mps) else PASS
    return sign * magnitude, reason
