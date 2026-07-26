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
from dataclasses import dataclass


# Result reasons (also used as the enforcement-action suffix for monitoring).
DISABLED = "PERCEPTION_DISABLED"
STALE = "PERCEPTION_STALE"
INVALID = "PERCEPTION_INVALID"
CAPPED = "PERCEPTION_CAP"
PASS = "PERCEPTION_PASS"

# Temporal speed-cap governor reasons.
GOV_INITIAL_HOLD = "CAP_INITIAL_HOLD"
GOV_CONFIRMING = "CAP_CONFIRMING"
GOV_RISING = "CAP_RISING"
GOV_PASS = "CAP_PASS"
GOV_RESTRICTED = "CAP_RESTRICTED"
GOV_INVALID = "CAP_INVALID"
GOV_TIME_BACKWARD = "CAP_TIME_BACKWARD"
GOV_TIME_GAP = "CAP_TIME_GAP"


@dataclass(frozen=True)
class SpeedCapGovernorConfig:
    """Configuration for asymmetric perception-cap stabilization."""

    rise_rate_mps_per_s: float = 0.50
    restriction_epsilon_mps: float = 0.03
    clear_confirmations: int = 5
    maximum_dt_s: float = 0.25
    initial_cap_mps: float = 0.0

    def __post_init__(self):
        if (
            not math.isfinite(self.rise_rate_mps_per_s)
            or self.rise_rate_mps_per_s <= 0.0
        ):
            raise ValueError("rise_rate_mps_per_s must be finite and positive")
        if (
            not math.isfinite(self.restriction_epsilon_mps)
            or self.restriction_epsilon_mps < 0.0
        ):
            raise ValueError(
                "restriction_epsilon_mps must be finite and non-negative"
            )
        if (
            isinstance(self.clear_confirmations, bool)
            or not isinstance(self.clear_confirmations, int)
            or self.clear_confirmations < 1
        ):
            raise ValueError("clear_confirmations must be a positive integer")
        if not math.isfinite(self.maximum_dt_s) or self.maximum_dt_s <= 0.0:
            raise ValueError("maximum_dt_s must be finite and positive")
        if not math.isfinite(self.initial_cap_mps) or self.initial_cap_mps < 0.0:
            raise ValueError("initial_cap_mps must be finite and non-negative")


@dataclass(frozen=True)
class SpeedCapDecision:
    """One deterministic temporal-governor decision."""

    raw_cap_mps: float
    governed_cap_mps: float
    reason: str
    clear_streak: int


class SpeedCapGovernor:
    """Asymmetric temporal governor for Taj's raw ACD speed cap.

    Restrictive raw-cap changes are recognized immediately. They may lower the
    governed cap but can never raise it. Permissive changes require consecutive
    confirmation and then rise at a bounded rate.

    Invalid data and monotonic-clock faults reset to the fail-closed floor.
    """

    def __init__(self, config=None):
        self._config = config or SpeedCapGovernorConfig()
        self.reset()

    @property
    def governed_cap_mps(self):
        return self._governed_cap_mps

    @property
    def clear_streak(self):
        return self._clear_streak

    def reset(self):
        self._governed_cap_mps = self._config.initial_cap_mps
        self._clear_streak = 0
        self._last_now_ns = None
        self._last_raw_cap_mps = None

    def _fail_closed(self, raw_cap_mps, reason, now_ns=None):
        self._governed_cap_mps = 0.0
        self._clear_streak = 0
        self._last_now_ns = now_ns
        self._last_raw_cap_mps = None

        return SpeedCapDecision(
            raw_cap_mps=(
                float(raw_cap_mps)
                if isinstance(raw_cap_mps, (int, float))
                and not isinstance(raw_cap_mps, bool)
                else float("nan")
            ),
            governed_cap_mps=0.0,
            reason=reason,
            clear_streak=0,
        )

    def update(self, raw_cap_mps, now_ns):
        """Apply one raw cap observation at a monotonic timestamp."""

        if (
            isinstance(now_ns, bool)
            or not isinstance(now_ns, int)
            or now_ns < 0
        ):
            return self._fail_closed(
                raw_cap_mps,
                GOV_TIME_BACKWARD,
            )

        if (
            isinstance(raw_cap_mps, bool)
            or not isinstance(raw_cap_mps, (int, float))
            or not math.isfinite(raw_cap_mps)
            or raw_cap_mps < 0.0
        ):
            return self._fail_closed(
                raw_cap_mps,
                GOV_INVALID,
                now_ns,
            )

        raw_cap_mps = float(raw_cap_mps)

        if self._last_now_ns is None:
            self._last_now_ns = now_ns
            self._last_raw_cap_mps = raw_cap_mps

            if raw_cap_mps < self._governed_cap_mps:
                self._governed_cap_mps = raw_cap_mps
                return SpeedCapDecision(
                    raw_cap_mps,
                    self._governed_cap_mps,
                    GOV_RESTRICTED,
                    0,
                )

            self._clear_streak = 1
            return SpeedCapDecision(
                raw_cap_mps,
                self._governed_cap_mps,
                GOV_INITIAL_HOLD,
                self._clear_streak,
            )

        if now_ns < self._last_now_ns:
            return self._fail_closed(
                raw_cap_mps,
                GOV_TIME_BACKWARD,
                now_ns,
            )

        dt_s = (
            now_ns - self._last_now_ns
        ) / 1_000_000_000.0

        if dt_s > self._config.maximum_dt_s:
            return self._fail_closed(
                raw_cap_mps,
                GOV_TIME_GAP,
                now_ns,
            )

        previous_raw_cap_mps = self._last_raw_cap_mps
        self._last_now_ns = now_ns
        self._last_raw_cap_mps = raw_cap_mps

        # A decrease in the sensor's raw safety envelope is restrictive even
        # when the stabilized output is already below the new raw cap.
        raw_became_more_restrictive = (
            previous_raw_cap_mps is not None
            and raw_cap_mps
            < previous_raw_cap_mps - self._config.restriction_epsilon_mps
        )

        if raw_became_more_restrictive:
            # Never increase the governed cap while handling a restrictive
            # observation.
            self._governed_cap_mps = min(
                self._governed_cap_mps,
                raw_cap_mps,
            )
            self._clear_streak = 0

            return SpeedCapDecision(
                raw_cap_mps,
                self._governed_cap_mps,
                GOV_RESTRICTED,
                0,
            )

        # The governed output must never exceed the newest raw safety bound.
        # Small decreases inside the jitter epsilon clamp immediately but do
        # not restart the clear-confirmation history.
        if raw_cap_mps < self._governed_cap_mps:
            decrease_mps = (
                self._governed_cap_mps - raw_cap_mps
            )
            self._governed_cap_mps = raw_cap_mps

            if (
                decrease_mps
                > self._config.restriction_epsilon_mps
            ):
                self._clear_streak = 0
                return SpeedCapDecision(
                    raw_cap_mps,
                    self._governed_cap_mps,
                    GOV_RESTRICTED,
                    0,
                )

            return SpeedCapDecision(
                raw_cap_mps,
                self._governed_cap_mps,
                GOV_PASS,
                self._clear_streak,
            )

        if raw_cap_mps == self._governed_cap_mps:
            self._clear_streak = 0
            return SpeedCapDecision(
                raw_cap_mps,
                self._governed_cap_mps,
                GOV_PASS,
                0,
            )

        self._clear_streak += 1

        if self._clear_streak < self._config.clear_confirmations:
            return SpeedCapDecision(
                raw_cap_mps,
                self._governed_cap_mps,
                GOV_CONFIRMING,
                self._clear_streak,
            )

        maximum_rise = (
            self._config.rise_rate_mps_per_s * dt_s
        )

        self._governed_cap_mps = min(
            raw_cap_mps,
            self._governed_cap_mps + maximum_rise,
        )

        reason = (
            GOV_PASS
            if self._governed_cap_mps >= raw_cap_mps
            else GOV_RISING
        )

        return SpeedCapDecision(
            raw_cap_mps,
            self._governed_cap_mps,
            reason,
            self._clear_streak,
        )

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
