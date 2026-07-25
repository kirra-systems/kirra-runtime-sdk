#!/usr/bin/env python3
"""lidar_orient_check.py — verify the lidar points FORWARD and left/right isn't
mirrored, from one live /scan. A bring-up tool: it needs nothing but rclpy +
sensor_msgs (NOT the kirra_safety workspace, unlike inspect_corridor.py), so it
runs the moment the ydlidar driver is publishing.

WHY it matters (safety, not cosmetics): the checker + Taj read /scan RAW and
treat the ray at angle 0 as the ego +X axis (forward), +90° as +Y (left). There
is no tf/offset layer. So if the sensor's 0° is not physically forward, the
robot's whole notion of "ahead" is wrong (it brakes for phantom side obstacles,
misses real front ones); if left/right are mirrored, lateral RSS sees hazards on
the wrong side. This tool confirms both.

USAGE:
  1. Put a single narrow object (box/pole) ~1 m directly in FRONT of the robot's
     centerline, nothing else close. Run it — the nearest return should read
     FRONT at ~0°.
  2. Move the object to the robot's LEFT and re-run — LEFT should go short (the
     object), RIGHT stay long. If RIGHT goes short instead, left/right are
     mirrored → flip `inverted` in the ydlidar params and relaunch.

  python3 robot/lidar_orient_check.py

Read-only, zero actuation. The pure geometry (angle math, direction labelling)
is host-tested in robot/lidar_orient_check_test.py; only the /scan grab is live.
"""
from __future__ import annotations

import math

# A return within this many degrees of a cardinal target counts toward it.
DIR_TOLERANCE_DEG = 10.0
# A nearest return within this many degrees of 0 is "aligned forward".
FORWARD_ALIGN_DEG = 5.0


# ---- pure geometry (host-tested) --------------------------------------------

def wrap180(a_deg: float) -> float:
    """Normalize an angle to (-180, 180]."""
    return ((a_deg + 180.0) % 360.0) - 180.0


def valid_returns(angle_min_rad, angle_increment_rad, ranges, range_min, range_max):
    """[(angle_deg, range_m)] for finite in-band returns, in scan order."""
    out = []
    for i, r in enumerate(ranges):
        if math.isfinite(r) and range_min < r < range_max:
            out.append((math.degrees(angle_min_rad + i * angle_increment_rad), float(r)))
    return out


def direction_label(a_deg: float) -> str:
    """FRONT / LEFT / RIGHT / BEHIND for an angle, or 'off the main axes'."""
    a = wrap180(a_deg)
    if abs(a) <= 20.0:
        return "FRONT"
    if abs(a - 90.0) <= 20.0:
        return "LEFT"
    if abs(a + 90.0) <= 20.0:
        return "RIGHT"
    if abs(abs(a) - 180.0) <= 20.0:
        return "BEHIND"
    return "off the main axes"


def direction_min(valid, target_deg: float, tol_deg: float = DIR_TOLERANCE_DEG) -> float:
    """Nearest range within ±tol of target_deg, or +inf if nothing there."""
    xs = [r for a, r in valid if abs(wrap180(a - target_deg)) <= tol_deg]
    return min(xs) if xs else float("inf")


def summarize(valid):
    """A report dict from the valid returns (or None if there are none)."""
    if not valid:
        return None
    ang, rng = min(valid, key=lambda ar: ar[1])
    return {
        "count": len(valid),
        "nearest_deg": ang,
        "nearest_m": rng,
        "nearest_label": direction_label(ang),
        "front_m": direction_min(valid, 0.0),
        "left_m": direction_min(valid, 90.0),
        "right_m": direction_min(valid, -90.0),
        "behind_m": direction_min(valid, 180.0),
        "forward_aligned": abs(wrap180(ang)) <= FORWARD_ALIGN_DEG,
    }


def render(report) -> str:
    if report is None:
        return ("valid returns=0 — every range is 0/out-of-band. The lidar is NOT\n"
                "producing data (motor not spinning / wrong baud / blocked view).\n"
                "That is a DATA problem, not orientation — fix it first.")
    lines = [
        f"valid returns={report['count']}",
        f"NEAREST return overall: {report['nearest_m']:.2f} m at "
        f"{report['nearest_deg']:+.1f} deg  ->  {report['nearest_label']}",
        f"   FRONT  (0): {report['front_m']:.2f} m",
        f"   LEFT (+90): {report['left_m']:.2f} m",
        f"   RIGHT(-90): {report['right_m']:.2f} m",
        f"   BEHIND(180): {report['behind_m']:.2f} m",
    ]
    return "\n".join(lines)


# ---- live /scan grab --------------------------------------------------------

def _grab_scan(timeout_s: float = 5.0):
    import rclpy
    from rclpy.node import Node
    from rclpy.qos import QoSProfile, ReliabilityPolicy, HistoryPolicy
    from sensor_msgs.msg import LaserScan
    import time

    class _Grab(Node):
        def __init__(self):
            super().__init__("lidar_orient_check")
            q = QoSProfile(reliability=ReliabilityPolicy.BEST_EFFORT,
                           history=HistoryPolicy.KEEP_LAST, depth=1)
            self.scan = None
            self.create_subscription(LaserScan, "/scan", self._cb, q)

        def _cb(self, m):
            self.scan = m

    rclpy.init()
    try:
        node = _Grab()
        t0 = time.monotonic()
        while node.scan is None and time.monotonic() - t0 < timeout_s:
            rclpy.spin_once(node, timeout_sec=0.2)
        scan = node.scan
        node.destroy_node()
        return scan
    finally:
        rclpy.shutdown()


def main() -> int:
    scan = _grab_scan()
    if scan is None:
        print("NO /scan received (is the ydlidar driver up and on ROS_DOMAIN_ID 28?)")
        return 1
    valid = valid_returns(scan.angle_min, scan.angle_increment,
                          list(scan.ranges), scan.range_min, scan.range_max)
    print(f"points={len(scan.ranges)}")
    print(render(summarize(valid)))
    return 0


if __name__ == "__main__":
    import sys
    sys.exit(main())
