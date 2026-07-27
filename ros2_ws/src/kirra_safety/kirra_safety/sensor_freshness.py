#!/usr/bin/env python3
"""
Frame-health detectors for the R2 perception ingress.

No ROS / rclpy dependency — the same pure-transform / ROS-shim split the rest of
this package uses, so every decision is unit-testable without booting ROS or a
lidar. `FrameHealthTracker.observe` reads no clock and touches no I/O: it is a
pure function of the observations handed to it.

WHY THIS EXISTS
---------------
Every freshness check on the R2 path is ARRIVAL-time based: the doer stamps
`time.monotonic()` when a scan lands, and `sensor_monitor` counts arrivals to
estimate Hz. All of that measures "did a message show up", not "is the message
NEW".

A lidar driver that wedges and republishes its last buffer at the full rate
therefore passes every existing check — `scan_stale_s`, `hz_estimate`,
`lidar_min_hz`. The doer keeps planning against a frozen world at full
confidence, and the checker bounds a corridor it believes is current. That is a
blind-driving path that looks perfectly healthy from the outside.

These detectors close it by tracking frame IDENTITY progression rather than
arrival timing.

THE PRIMARY SIGNAL IS THE HEADER STAMP
--------------------------------------
A frame is "new" iff its producer-assigned header stamp ADVANCED. That is the
one signal a wedged driver cannot fake while replaying an old buffer.

Content fingerprinting is SUPPORTING EVIDENCE ONLY, never a trigger. A
stationary robot facing a static scene legitimately produces near-identical
scans indefinitely; treating that as a fault would stop a healthy robot for
standing still. So a repeated fingerprint only ever sharpens the DIAGNOSIS of a
stamp that has already stopped advancing — it distinguishes "the driver is
replaying a buffer" from "the stamp is broken but data is still moving". With
the stamp advancing, identical content is explicitly healthy.

SINGLE HICCUP vs SUSTAINED STALL
--------------------------------
One duplicated or missing frame must not stop the robot — that would trade a
blind-driving bug for a nuisance-stop bug. Non-advancing arrivals are counted
CONSECUTIVELY and only fail closed once the count exceeds `stall_budget`; any
advance resets the count to zero.

Recovery mirrors the verifier's AV recovery hysteresis
(`src/recovery_hysteresis.rs`): after a fail-closed, `recovery_streak`
consecutive advancing frames are required within `recovery_window_s`, and the
window expiring resets the streak. One good frame does not re-arm motion.

DISARMED BY DEFAULT
-------------------
`FrameHealthConfig.disarmed()` is a total no-op that always resolves DISARMED,
so the nominal path is byte-identical until an operator arms it. The two
threshold detectors are independently disarmed by sentinel: `min_rate_hz = 0.0`
and `max_invalid_ray_ratio = 1.0` mean "never fire".
"""

import math
import struct
from collections import namedtuple
from hashlib import blake2b

# --- resolution states (mirrors kirra_trajectory::vru_channel::VruResolution) ---
DISARMED = "DISARMED"        # detector off — the checker path is untouched
HEALTHY = "HEALTHY"          # frames are advancing (or within the stall budget)
FAIL_CLOSED = "FAIL_CLOSED"  # sustained fault — the caller must hold

# --- specific reason codes -------------------------------------------------
# Deliberately distinct rather than one generic "stale lidar": each names a
# different physical fault with a different fix, and an operator reading the log
# should not have to guess which one fired.
REASON_OK = "OK"
# The stamp stopped advancing AND the payload is byte-identical — the signature
# of a driver replaying a wedged buffer. The highest-confidence diagnosis.
REASON_STAMP_FROZEN_CONTENT_REPEATED = "STAMP_FROZEN_CONTENT_REPEATED"
# The stamp stopped advancing but the payload keeps changing — a broken/absent
# stamp on live data. Still a fault: the checker's freshness reasoning and the
# evidence binding both depend on a trustworthy stamp.
REASON_STAMP_NOT_ADVANCING = "STAMP_NOT_ADVANCING"
# The stamp went BACKWARDS — a driver restart, a clock step, or two publishers
# racing on one topic. Never a benign duplicate.
REASON_STAMP_REGRESSED = "STAMP_REGRESSED"
# Measured arrival rate below the configured floor (opt-in).
REASON_RATE_BELOW_FLOOR = "RATE_BELOW_FLOOR"
# Too large a fraction of rays are non-finite or outside the sensor's own
# declared range bounds (opt-in).
REASON_INVALID_RAY_RATIO = "INVALID_RAY_RATIO"
# Armed but nothing has arrived yet — "the sensor did not look" is never "clear".
REASON_NO_FRAMES = "NO_FRAMES"

# Recovery semantics, mirrored from src/recovery_hysteresis.rs so the doer and
# the verifier agree on what "recovered" means.
DEFAULT_RECOVERY_STREAK = 5
DEFAULT_RECOVERY_WINDOW_S = 10.0
# Consecutive non-advancing arrivals tolerated before failing closed. At a 10 Hz
# scan this is ~300 ms of repeat — long enough to ride out a single hiccup or a
# duplicated frame, far short of a sustained stall.
DEFAULT_STALL_BUDGET = 3


class FrameHealthConfig(namedtuple(
    "FrameHealthConfig",
    ["enabled", "stall_budget", "recovery_streak", "recovery_window_s",
     "min_rate_hz", "max_invalid_ray_ratio"],
)):
    """Detector configuration. Every detector is off unless explicitly armed."""

    __slots__ = ()

    @classmethod
    def disarmed(cls):
        """The nominal no-op: the tracker always resolves DISARMED."""
        return cls(
            enabled=False,
            stall_budget=DEFAULT_STALL_BUDGET,
            recovery_streak=DEFAULT_RECOVERY_STREAK,
            recovery_window_s=DEFAULT_RECOVERY_WINDOW_S,
            min_rate_hz=0.0,          # sentinel: rate detector off
            max_invalid_ray_ratio=1.0,  # sentinel: ray-ratio detector off
        )

    @classmethod
    def armed(cls, **over):
        """Armed with the documented defaults; `over` tunes individual knobs."""
        base = cls.disarmed()._asdict()
        base["enabled"] = True
        base.update(over)
        return cls(**base)


# One arriving frame, reduced to the facts the detectors need. `stamp_ms` is the
# PRODUCER's header stamp (not arrival). `monotonic_s` is the receiver's clock,
# used only for rate and the recovery window — never for freshness.
FrameObservation = namedtuple(
    "FrameObservation", ["stamp_ms", "fingerprint", "monotonic_s", "invalid_ratio"]
)

# The resolution handed back to the caller. `detail` is a short human string for
# the log; `reason` is the stable code.
FrameHealth = namedtuple("FrameHealth", ["state", "reason", "detail"])

_HEALTHY = FrameHealth(HEALTHY, REASON_OK, "")
_DISARMED = FrameHealth(DISARMED, REASON_OK, "")


def scan_fingerprint(ranges):
    """A stable 16-byte digest of a scan's payload.

    Hashes the PACKED BYTES rather than the float values, so a NaN ray compares
    equal to itself (`nan != nan` would otherwise make every frame containing a
    dropout look new — precisely inverting the detector on a dirty sensor).

    Deterministic across processes, unlike `hash()` under PYTHONHASHSEED.
    """
    values = [float(r) for r in ranges]
    packed = struct.pack("<%dd" % len(values), *values)
    return blake2b(packed, digest_size=16).digest()


def invalid_ray_ratio(ranges, range_min, range_max):
    """Fraction of rays that are non-finite or outside the sensor's own bounds.

    Returns 0.0 for an empty scan: "no rays" is an emptiness problem for the
    rate/stall detectors to speak to, not a data-quality one. Non-finite bounds
    disable the range comparison rather than marking every ray invalid.
    """
    total = len(ranges)
    if total == 0:
        return 0.0
    bounded = math.isfinite(range_min) and math.isfinite(range_max) and range_min <= range_max
    bad = 0
    for r in ranges:
        value = float(r)
        if not math.isfinite(value):
            bad += 1
        elif bounded and not (range_min <= value <= range_max):
            bad += 1
    return bad / total


class FrameHealthTracker:
    """Tracks frame-identity progression across arrivals.

    Stateful across calls but deterministic: `observe` reads no clock and no
    I/O, so a test drives it entirely by the observations it passes.
    """

    def __init__(self, config=None):
        self._cfg = config or FrameHealthConfig.disarmed()
        self._last_stamp_ms = None
        self._last_fingerprint = None
        self._stall_count = 0          # consecutive non-advancing arrivals
        self._failed = False           # latched until the recovery streak completes
        self._fail_reason = REASON_OK
        self._recovery_streak = 0
        self._recovery_started_s = None
        self._health = _DISARMED if not self._cfg.enabled else FrameHealth(
            HEALTHY, REASON_NO_FRAMES, "armed; awaiting the first frame")

    @property
    def config(self):
        return self._cfg

    @property
    def health(self):
        """The latest resolution, without observing a new frame."""
        return self._health

    def observe(self, observation):
        """Fold one arriving frame in and return the resulting health."""
        if not self._cfg.enabled:
            self._health = _DISARMED
            return self._health

        advanced, identity_reason = self._classify_identity(observation)
        self._last_stamp_ms = observation.stamp_ms
        self._last_fingerprint = observation.fingerprint

        if advanced:
            self._stall_count = 0
        else:
            self._stall_count += 1

        # Threshold detectors are independent of identity: a scan can advance
        # perfectly while arriving too slowly or carrying mostly garbage.
        quality_reason, quality_detail = self._classify_quality(observation)

        if quality_reason is not None:
            return self._fail(observation, quality_reason, quality_detail)

        if not advanced and self._stall_count > self._cfg.stall_budget:
            return self._fail(
                observation, identity_reason,
                "%d consecutive non-advancing frames (budget %d); last stamp %s ms"
                % (self._stall_count, self._cfg.stall_budget, observation.stamp_ms),
            )

        if self._failed:
            # Latched: one good frame is not recovery. Mirrors the verifier's
            # AV recovery hysteresis — a streak, inside a window.
            return self._advance_recovery(observation, advanced)

        self._health = _HEALTHY
        return self._health

    # --- internals ---------------------------------------------------------

    def _classify_identity(self, observation):
        """(advanced, reason_if_not) for this frame's producer identity.

        The stamp is the sole trigger. The fingerprint only chooses BETWEEN the
        two non-advancing reasons, so an unchanging scene with a healthy stamp
        can never be read as a fault.
        """
        stamp = observation.stamp_ms
        if self._last_stamp_ms is None:
            return True, None  # first frame — nothing to compare against yet
        if stamp > self._last_stamp_ms:
            return True, None
        if stamp < self._last_stamp_ms:
            return False, REASON_STAMP_REGRESSED
        repeated = (
            observation.fingerprint is not None
            and observation.fingerprint == self._last_fingerprint
        )
        return False, (REASON_STAMP_FROZEN_CONTENT_REPEATED if repeated
                       else REASON_STAMP_NOT_ADVANCING)

    def _classify_quality(self, observation):
        """(reason, detail) for the opt-in threshold detectors, or (None, None)."""
        if self._cfg.min_rate_hz > 0.0 and observation.monotonic_s is not None:
            rate = self._measured_rate(observation.monotonic_s)
            if rate is not None and rate < self._cfg.min_rate_hz:
                return (REASON_RATE_BELOW_FLOOR,
                        "%.2f Hz below the %.2f Hz floor"
                        % (rate, self._cfg.min_rate_hz))
        if self._cfg.max_invalid_ray_ratio < 1.0 and observation.invalid_ratio is not None:
            if observation.invalid_ratio > self._cfg.max_invalid_ray_ratio:
                return (REASON_INVALID_RAY_RATIO,
                        "%.1f%% of rays invalid (max %.1f%%)"
                        % (observation.invalid_ratio * 100.0,
                           self._cfg.max_invalid_ray_ratio * 100.0))
        return None, None

    def _measured_rate(self, monotonic_s):
        """Instantaneous arrival rate from the previous arrival, or None."""
        previous = getattr(self, "_last_monotonic_s", None)
        self._last_monotonic_s = monotonic_s
        if previous is None:
            return None
        delta = monotonic_s - previous
        return (1.0 / delta) if delta > 0.0 else None

    def _fail(self, observation, reason, detail):
        self._failed = True
        self._fail_reason = reason
        self._recovery_streak = 0
        self._recovery_started_s = None
        self._health = FrameHealth(FAIL_CLOSED, reason, detail)
        return self._health

    def _advance_recovery(self, observation, advanced):
        """Count advancing frames toward re-arming; hold FAIL_CLOSED until done."""
        if not advanced:
            self._recovery_streak = 0
            self._recovery_started_s = None
            self._health = FrameHealth(
                FAIL_CLOSED, self._fail_reason, "recovery reset — frame did not advance")
            return self._health

        now = observation.monotonic_s
        if self._recovery_started_s is None:
            self._recovery_started_s = now
        elif (now is not None and self._recovery_started_s is not None
                and (now - self._recovery_started_s) >= self._cfg.recovery_window_s):
            # Window expired before the streak completed — start a fresh one, so
            # a trickle of good frames spread over minutes never re-arms motion.
            self._recovery_streak = 0
            self._recovery_started_s = now

        self._recovery_streak += 1
        if self._recovery_streak >= self._cfg.recovery_streak:
            self._failed = False
            self._fail_reason = REASON_OK
            self._recovery_streak = 0
            self._recovery_started_s = None
            self._health = _HEALTHY
            return self._health

        self._health = FrameHealth(
            FAIL_CLOSED, self._fail_reason,
            "recovering %d/%d advancing frames"
            % (self._recovery_streak, self._cfg.recovery_streak))
        return self._health
