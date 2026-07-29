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

#1212: the temporal cap-stabilization state machine that used to live here is GONE.
It now lives in the Rust checker (`kirra_trajectory::cap_stabilizer`), which is the
safety authority; `perception_governor` relays the stabilized cap Taj serves and
computes nothing. It was removed rather than left as advisory because an unused
copy of a safety state machine drifts silently — the tests keep passing while
nothing enforces it, which is the exact condition that made the original placement
a problem.

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

# Relay of the checker-owned stabilized cap (#1212).
STABILIZED_OK = "CAP_RELAY_OK"
# The response carries no evidence that the checker stabilized anything: no
# `cap_reason`, or one that is not a usable code. Distinct from MALFORMED because the
# operator action differs — this one means the sidecar is old or not configured to
# stabilize, and no amount of restarting this node will fix it.
STABILIZED_UNSTABILIZED = "CAP_RELAY_UNSTABILIZED"
# A stabilized cap was claimed but the value is unusable (absent, non-numeric,
# non-finite, negative), or the bound it must be clamped against is unusable.
STABILIZED_MALFORMED = "CAP_RELAY_MALFORMED"



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


def resolve_stabilized_cap(data, effective_raw_cap_mps):
    """
    Return (cap_mps, reason) for the checker-owned stabilized cap in a Taj response.

    #1212: this node RELAYS the cap the Rust checker stabilized; it does not compute
    one. The temporal state machine that used to live here has moved into
    `kirra_trajectory::cap_stabilizer`, which is the safety authority.

    Fail closed when the field is absent, non-numeric, non-finite or negative. An
    older Taj that does not serve a stabilized cap is NOT a licence to fall back on
    the raw one — falling back would silently restore the pre-#1212 behaviour on
    exactly the deployments that had not been upgraded, which is the worst place for
    it to happen and the hardest to notice.

    Fail closed ALSO when the response carries no `cap_reason`. The value alone
    cannot be trusted: a sidecar that does not stabilize still populates
    `stabilized_speed_cap_mps`, with the RAW cap, so accepting the number on its own
    would relay an unstabilized cap under a stabilized name — the precise failure
    this function exists to prevent, and one that leaves no trace because the number
    looks entirely reasonable. `cap_reason` is the authorship claim; without it there
    is nothing to relay.

    The result is additionally clamped by `effective_raw_cap_mps` so an unhealthy
    frame can never be widened by the relay. An unusable clamp bound is itself a
    fault — clamping against NaN would pass the stabilized value through untouched.
    """
    if not isinstance(data, dict):
        return 0.0, STABILIZED_MALFORMED

    # Authorship first. A number without a claimed author is not a stabilized cap
    # whatever its value, so this is checked before the value is even read.
    reason = data.get("cap_reason")
    if not isinstance(reason, str) or not reason:
        return 0.0, STABILIZED_UNSTABILIZED

    value = data.get("stabilized_speed_cap_mps")
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return 0.0, STABILIZED_MALFORMED
    value = float(value)
    if not math.isfinite(value) or value < 0.0:
        return 0.0, STABILIZED_MALFORMED

    if isinstance(effective_raw_cap_mps, bool) or not isinstance(
        effective_raw_cap_mps, (int, float)
    ):
        return 0.0, STABILIZED_MALFORMED
    bound = float(effective_raw_cap_mps)
    if not math.isfinite(bound) or bound < 0.0:
        return 0.0, STABILIZED_MALFORMED

    return min(value, bound), STABILIZED_OK
