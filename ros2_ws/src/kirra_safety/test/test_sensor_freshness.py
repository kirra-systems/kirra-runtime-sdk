"""
Unit tests for the R2 frame-health detectors.

No ROS / rclpy needed — imports the pure `sensor_freshness` module directly.

THE ACCEPTANCE TEST is `test_full_rate_frozen_buffer_fails_closed`: a lidar
driver republishing its last buffer at the full rate, which every arrival-time
check in the stack (scan_stale_s, hz_estimate, lidar_min_hz) reports as
perfectly healthy. Nothing before this module could tell it from a live sensor.

The counterweight is `test_static_scene_with_advancing_stamps_stays_healthy`: a
stationary robot facing a wall produces byte-identical scans forever, and that
must NEVER be a stop condition. Together those two pin the whole design — the
stamp decides, the fingerprint only explains.

Run:  pytest ros2_ws/src/kirra_safety/test/test_sensor_freshness.py
"""

import os
import sys

# Import the pure module standalone (no installed package, no ROS).
sys.path.insert(
    0, os.path.join(os.path.dirname(__file__), "..", "kirra_safety")
)
from sensor_freshness import (  # noqa: E402
    DEFAULT_RECOVERY_STREAK, DISARMED, FAIL_CLOSED, HEALTHY,
    REASON_INVALID_RAY_RATIO, REASON_NO_FRAMES, REASON_OK,
    REASON_RATE_BELOW_FLOOR, REASON_STAMP_FROZEN_CONTENT_REPEATED,
    REASON_STAMP_NOT_ADVANCING, REASON_STAMP_REGRESSED,
    FrameHealthConfig, FrameHealthTracker, FrameObservation,
    invalid_ray_ratio, scan_fingerprint,
)

# A realistic 10 Hz scan cadence.
PERIOD_S = 0.1
PERIOD_MS = 100


def _obs(stamp_ms, fingerprint, monotonic_s, invalid_ratio=0.0):
    return FrameObservation(stamp_ms=stamp_ms, fingerprint=fingerprint,
                            monotonic_s=monotonic_s, invalid_ratio=invalid_ratio)


def _live_stream(tracker, n, start_stamp=1000, start_s=10.0, fingerprint=None):
    """Feed `n` normally-advancing frames; returns the last health."""
    health = None
    for i in range(n):
        fp = fingerprint if fingerprint is not None else b"frame-%d" % i
        health = tracker.observe(
            _obs(start_stamp + i * PERIOD_MS, fp, start_s + i * PERIOD_S))
    return health


# ---------------------------------------------------------------------------
# THE ACCEPTANCE TEST — a wedged driver at full rate
# ---------------------------------------------------------------------------

def test_full_rate_frozen_buffer_fails_closed():
    tracker = FrameHealthTracker(FrameHealthConfig.armed())
    _live_stream(tracker, 5)

    # The driver wedges: same header stamp, same payload, still 10 Hz.
    frozen_stamp, frozen_fp = 1500, b"wedged-buffer"
    healths = [
        tracker.observe(_obs(frozen_stamp, frozen_fp, 11.0 + i * PERIOD_S))
        for i in range(6)
    ]

    # Inside the stall budget it must NOT stop — one hiccup is not a stall.
    assert healths[0].state == HEALTHY
    # Beyond the budget it must fail closed, naming the wedged-driver signature.
    final = healths[-1]
    assert final.state == FAIL_CLOSED
    assert final.reason == REASON_STAMP_FROZEN_CONTENT_REPEATED
    assert "non-advancing" in final.detail


def test_frozen_buffer_would_pass_every_arrival_based_check():
    """Non-vacuity: the frozen stream this module refuses is exactly the stream
    an arrival-time check calls healthy — arrivals are perfectly on cadence."""
    arrivals = [11.0 + i * PERIOD_S for i in range(6)]
    gaps = [b - a for a, b in zip(arrivals, arrivals[1:])]
    assert all(abs(g - PERIOD_S) < 1e-9 for g in gaps)      # never "stale"
    assert abs(1.0 / gaps[0] - 10.0) < 1e-6                  # never "low rate"

    tracker = FrameHealthTracker(FrameHealthConfig.armed())
    _live_stream(tracker, 5)
    for t in arrivals:
        health = tracker.observe(_obs(1500, b"wedged-buffer", t))
    assert health.state == FAIL_CLOSED


# ---------------------------------------------------------------------------
# THE COUNTERWEIGHT — content repetition is evidence, never a trigger
# ---------------------------------------------------------------------------

def test_static_scene_with_advancing_stamps_stays_healthy():
    # A stationary robot facing a wall: byte-identical payload, healthy stamp.
    # Failing this would stop a perfectly healthy robot for standing still.
    tracker = FrameHealthTracker(FrameHealthConfig.armed())
    health = _live_stream(tracker, 50, fingerprint=b"unchanging-wall")
    assert health.state == HEALTHY
    assert health.reason == REASON_OK


def test_repeated_content_alone_never_fails_closed_however_long():
    tracker = FrameHealthTracker(FrameHealthConfig.armed())
    for i in range(500):
        health = tracker.observe(
            _obs(1000 + i * PERIOD_MS, b"same", 10.0 + i * PERIOD_S))
        assert health.state == HEALTHY, i


def test_changing_content_with_frozen_stamp_is_a_distinct_reason():
    # Live data, broken stamp: still a fault (the checker's freshness reasoning
    # and the evidence binding both need a trustworthy stamp), but a DIFFERENT
    # diagnosis than a replayed buffer.
    tracker = FrameHealthTracker(FrameHealthConfig.armed())
    _live_stream(tracker, 5)
    for i in range(6):
        health = tracker.observe(_obs(1500, b"moving-%d" % i, 11.0 + i * PERIOD_S))
    assert health.state == FAIL_CLOSED
    assert health.reason == REASON_STAMP_NOT_ADVANCING


# ---------------------------------------------------------------------------
# Single hiccup vs sustained stall
# ---------------------------------------------------------------------------

def test_single_duplicated_frame_does_not_stop_motion():
    tracker = FrameHealthTracker(FrameHealthConfig.armed())
    _live_stream(tracker, 5)
    dup = tracker.observe(_obs(1400, b"frame-4", 11.0))   # one repeat
    assert dup.state == HEALTHY
    resumed = tracker.observe(_obs(1500, b"frame-5", 11.1))
    assert resumed.state == HEALTHY


def test_stall_budget_is_the_boundary():
    cfg = FrameHealthConfig.armed(stall_budget=3)
    tracker = FrameHealthTracker(cfg)
    _live_stream(tracker, 3)
    states = [tracker.observe(_obs(1200, b"frozen", 11.0 + i * PERIOD_S)).state
              for i in range(4)]
    assert states[:3] == [HEALTHY, HEALTHY, HEALTHY]   # at the budget
    assert states[3] == FAIL_CLOSED                     # one beyond it


def test_an_advance_resets_the_stall_count():
    # Alternating hiccups must not accumulate into a stop.
    tracker = FrameHealthTracker(FrameHealthConfig.armed(stall_budget=2))
    stamp, t = 1000, 10.0
    for i in range(20):
        tracker.observe(_obs(stamp, b"a", t))          # repeat
        t += PERIOD_S
        stamp += PERIOD_MS
        health = tracker.observe(_obs(stamp, b"b", t))  # advance
        t += PERIOD_S
    assert health.state == HEALTHY


def test_stamp_regression_is_its_own_reason():
    # A driver restart or clock step — never a benign duplicate.
    tracker = FrameHealthTracker(FrameHealthConfig.armed(stall_budget=1))
    _live_stream(tracker, 5)
    tracker.observe(_obs(500, b"old", 11.0))
    health = tracker.observe(_obs(400, b"older", 11.1))
    assert health.state == FAIL_CLOSED
    assert health.reason == REASON_STAMP_REGRESSED


# ---------------------------------------------------------------------------
# Recovery — a streak, not a single good frame
# ---------------------------------------------------------------------------

def test_recovery_requires_the_full_streak():
    tracker = FrameHealthTracker(FrameHealthConfig.armed(stall_budget=1))
    _live_stream(tracker, 2)
    for i in range(3):                                  # drive it into fail-closed
        tracker.observe(_obs(1100, b"frozen", 11.0 + i * PERIOD_S))
    assert tracker.health.state == FAIL_CLOSED

    stamp, t = 2000, 12.0
    for n in range(1, DEFAULT_RECOVERY_STREAK):
        health = tracker.observe(_obs(stamp, b"live-%d" % n, t))
        assert health.state == FAIL_CLOSED, n          # still held
        assert "recovering %d/" % n in health.detail
        stamp += PERIOD_MS
        t += PERIOD_S

    health = tracker.observe(_obs(stamp, b"live-final", t))
    assert health.state == HEALTHY


def test_a_relapse_during_recovery_resets_the_streak():
    tracker = FrameHealthTracker(FrameHealthConfig.armed(stall_budget=1))
    _live_stream(tracker, 2)
    for i in range(3):
        tracker.observe(_obs(1100, b"frozen", 11.0 + i * PERIOD_S))

    tracker.observe(_obs(2000, b"live-1", 12.0))
    tracker.observe(_obs(2100, b"live-2", 12.1))
    relapse = tracker.observe(_obs(2100, b"live-2", 12.2))   # non-advancing again
    assert relapse.state == FAIL_CLOSED
    assert "recovery reset" in relapse.detail

    # The streak must restart from zero, not resume at 2.
    stamp, t = 2200, 12.3
    for n in range(1, DEFAULT_RECOVERY_STREAK):
        assert tracker.observe(_obs(stamp, b"r-%d" % n, t)).state == FAIL_CLOSED, n
        stamp += PERIOD_MS
        t += PERIOD_S
    assert tracker.observe(_obs(stamp, b"r-final", t)).state == HEALTHY


def test_recovery_window_expiry_restarts_the_streak():
    # A trickle of good frames spread over minutes must never re-arm motion.
    cfg = FrameHealthConfig.armed(stall_budget=1, recovery_window_s=1.0)
    tracker = FrameHealthTracker(cfg)
    _live_stream(tracker, 2)
    for i in range(3):
        tracker.observe(_obs(1100, b"frozen", 11.0 + i * PERIOD_S))

    stamp, t = 2000, 12.0
    for _ in range(20):                     # one advancing frame every 2 s
        health = tracker.observe(_obs(stamp, b"trickle-%d" % stamp, t))
        assert health.state == FAIL_CLOSED
        stamp += PERIOD_MS
        t += 2.0


# ---------------------------------------------------------------------------
# Disarmed by default — the nominal path is untouched
# ---------------------------------------------------------------------------

def test_disarmed_is_a_total_no_op():
    tracker = FrameHealthTracker(FrameHealthConfig.disarmed())
    for i in range(50):
        health = tracker.observe(_obs(1000, b"frozen", 10.0))  # worst case
        assert health.state == DISARMED
        assert health.reason == REASON_OK


def test_default_config_is_disarmed():
    assert FrameHealthTracker().health.state == DISARMED
    cfg = FrameHealthConfig.disarmed()
    assert cfg.enabled is False
    assert cfg.min_rate_hz == 0.0            # sentinel: rate detector off
    assert cfg.max_invalid_ray_ratio == 1.0  # sentinel: ray-ratio detector off


def test_armed_tracker_reports_no_frames_before_the_first_arrival():
    # "The sensor did not look" is never "clear".
    tracker = FrameHealthTracker(FrameHealthConfig.armed())
    assert tracker.health.reason == REASON_NO_FRAMES


# ---------------------------------------------------------------------------
# Opt-in threshold detectors
# ---------------------------------------------------------------------------

def test_rate_floor_is_off_by_default_and_fires_when_armed():
    off = FrameHealthTracker(FrameHealthConfig.armed())
    off.observe(_obs(1000, b"a", 10.0))
    assert off.observe(_obs(1100, b"b", 20.0)).state == HEALTHY  # 0.1 Hz, ignored

    on = FrameHealthTracker(FrameHealthConfig.armed(min_rate_hz=5.0))
    on.observe(_obs(1000, b"a", 10.0))
    health = on.observe(_obs(1100, b"b", 20.0))
    assert health.state == FAIL_CLOSED
    assert health.reason == REASON_RATE_BELOW_FLOOR


def test_invalid_ray_ratio_is_off_by_default_and_fires_when_armed():
    off = FrameHealthTracker(FrameHealthConfig.armed())
    assert off.observe(_obs(1000, b"a", 10.0, invalid_ratio=1.0)).state == HEALTHY

    on = FrameHealthTracker(FrameHealthConfig.armed(max_invalid_ray_ratio=0.5))
    health = on.observe(_obs(1000, b"a", 10.0, invalid_ratio=0.9))
    assert health.state == FAIL_CLOSED
    assert health.reason == REASON_INVALID_RAY_RATIO


def test_quality_faults_outrank_a_healthy_identity():
    # A scan can advance perfectly while carrying mostly garbage.
    tracker = FrameHealthTracker(FrameHealthConfig.armed(max_invalid_ray_ratio=0.2))
    tracker.observe(_obs(1000, b"a", 10.0))
    health = tracker.observe(_obs(1100, b"b", 10.1, invalid_ratio=0.8))
    assert health.state == FAIL_CLOSED
    assert health.reason == REASON_INVALID_RAY_RATIO


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def test_fingerprint_is_stable_and_discriminating():
    assert scan_fingerprint([1.0, 2.0, 3.0]) == scan_fingerprint([1.0, 2.0, 3.0])
    assert scan_fingerprint([1.0, 2.0, 3.0]) != scan_fingerprint([1.0, 2.0, 3.1])
    assert len(scan_fingerprint([1.0])) == 16
    assert scan_fingerprint([]) == scan_fingerprint([])


def test_fingerprint_treats_nan_as_equal_to_itself():
    # `nan != nan`, but the BYTES match. Without this a dirty sensor's dropout
    # rays would make every frozen frame look new — inverting the detector
    # exactly when the sensor is least trustworthy.
    nan = float("nan")
    assert scan_fingerprint([1.0, nan, 3.0]) == scan_fingerprint([1.0, nan, 3.0])


def test_invalid_ray_ratio_counts_non_finite_and_out_of_bounds():
    inf, nan = float("inf"), float("nan")
    assert invalid_ray_ratio([1.0, 2.0, 3.0], 0.1, 12.0) == 0.0
    assert invalid_ray_ratio([nan, 2.0], 0.1, 12.0) == 0.5
    assert invalid_ray_ratio([inf, 2.0], 0.1, 12.0) == 0.5
    assert invalid_ray_ratio([0.01, 2.0], 0.1, 12.0) == 0.5     # below range_min
    assert invalid_ray_ratio([99.0, 2.0], 0.1, 12.0) == 0.5     # above range_max
    assert invalid_ray_ratio([], 0.1, 12.0) == 0.0              # emptiness is not invalidity
    # Non-finite bounds disable the range comparison rather than condemning
    # every ray — a driver that fails to report its own limits is a separate bug.
    assert invalid_ray_ratio([1.0, 2.0], nan, 12.0) == 0.0


def test_reason_codes_are_distinct():
    reasons = {REASON_OK, REASON_STAMP_FROZEN_CONTENT_REPEATED,
               REASON_STAMP_NOT_ADVANCING, REASON_STAMP_REGRESSED,
               REASON_RATE_BELOW_FLOOR, REASON_INVALID_RAY_RATIO,
               REASON_NO_FRAMES}
    assert len(reasons) == 7, "each fault must name a different physical cause"
