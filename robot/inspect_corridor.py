#!/usr/bin/env python3
"""inspect_corridor.py — snapshot WHY occy_doer holds, from the live lidar.

Replicates occy_doer's exact per-tick pipeline ONCE — grab one /scan, POST it to
the Taj sidecar (/perception), extend the corridor back like occy does, then POST
{ego, goal, corridor, objects, vehicle} to the Occy planner (/plan) — and prints:

  * the nearest lidar return overall + in the forward ±15° cone (is the robot
    boxed in? → a real obstacle within ~1 m explains a correct MRC refusal),
  * the Taj corridor (point counts + half-width ahead) + detected objects,
  * the planner's kind / verdict / #893 NARRATION reason — the operator sentence
    that says exactly why the checker refused.

Read-only, zero actuation. Run while the Stage-1 stack is up (taj 8101, planner
8100) and the lidar is publishing /scan. Domain-28 shell, ws sourced.

  ros2 run is not needed — just:  python3 robot/inspect_corridor.py
"""
import math
import sys
import time

import rclpy
from rclpy.node import Node
from rclpy.qos import QoSProfile, ReliabilityPolicy, HistoryPolicy
from sensor_msgs.msg import LaserScan

try:
    import requests
except ImportError:
    sys.exit("python3-requests missing (pip3 install requests)")

# Faithful to occy_doer: same back-extension of the Taj corridor.
from kirra_safety.doer_core import extend_corridor_back

TAJ = "http://localhost:8101"
PLANNER = "http://localhost:8100"
FORWARD_EXTENT_M = 8.0     # occy_doer forward_extent_m default
BACK_M = 0.5               # occy_doer corridor_back_m default
CRUISE = 1.2               # occy_doer cruise_speed_mps default
GOAL = (1.0, 0.0)          # 1 m dead ahead (the go_to (1,0) intent)
FORWARD_CONE_RAD = math.radians(15.0)
# Robot-scale footprint occy sends (kirra_params.yaml / CONTRACT_PROFILES courier).
VEHICLE = {
    "class": "courier", "wheelbase_m": 0.2, "half_length_m": 0.18,
    "half_width_m": 0.15, "max_speed_mps": 1.2, "max_steering_deg": 30.0,
    "rss_lateral_alignment_tolerance_m": 0.6, "lateral_clearance_target_m": 0.6,
}

SCAN_QOS = QoSProfile(
    reliability=ReliabilityPolicy.BEST_EFFORT,
    history=HistoryPolicy.KEEP_LAST, depth=1,
)


class Grab(Node):
    def __init__(self):
        super().__init__("inspect_corridor")
        self.scan = None
        self.create_subscription(LaserScan, "/scan", self._on, SCAN_QOS)

    def _on(self, msg):
        self.scan = msg


def _half_width_at(left, right, x):
    """Corridor half-widths (|y|) at the boundary point nearest x, for a sense of
    how wide Taj thinks the free lane is ahead."""
    def near(poly):
        if not poly:
            return None
        best = min(poly, key=lambda p: abs(p[0] - x))
        return best[1]
    return near(left), near(right)


def main():
    rclpy.init()
    node = Grab()
    print("waiting for one /scan (BEST_EFFORT)...")
    t0 = time.monotonic()
    while node.scan is None and time.monotonic() - t0 < 5.0:
        rclpy.spin_once(node, timeout_sec=0.2)
    scan = node.scan
    if scan is None:
        node.destroy_node(); rclpy.shutdown()
        sys.exit("NO /scan in 5 s — is the lidar publishing on domain 28?")

    ranges = [float(r) for r in scan.ranges]
    n = len(ranges)
    finite = [(i, r) for i, r in enumerate(ranges)
              if math.isfinite(r) and r > scan.range_min]
    nearest = min((r for _, r in finite), default=float("inf"))
    # forward cone: angle = angle_min + i*inc, |angle| < cone
    fwd = [r for i, r in finite
           if abs(scan.angle_min + i * scan.angle_increment) < FORWARD_CONE_RAD]
    nearest_fwd = min(fwd, default=float("inf"))

    print("\n=== LIDAR /scan ===")
    print(f"  points={n}  angle=[{scan.angle_min:.2f},{scan.angle_max:.2f}]rad  "
          f"range=[{scan.range_min:.2f},{scan.range_max:.2f}]m")
    print(f"  nearest return (any dir): {nearest:.3f} m")
    print(f"  nearest in FORWARD ±15°:  {nearest_fwd:.3f} m   "
          f"{'<-- obstacle within 1 m ahead (MRC is CORRECT)' if nearest_fwd < 1.0 else ''}")

    # --- Taj ---
    try:
        taj = requests.post(f"{TAJ}/perception", timeout=2.0, json={
            "angle_min_rad": float(scan.angle_min),
            "angle_increment_rad": float(scan.angle_increment),
            "range_min_m": float(scan.range_min),
            "range_max_m": float(scan.range_max),
            "ranges": ranges, "stamp_ms": 0, "forward_extent_m": FORWARD_EXTENT_M,
        }).json()
    except Exception as e:
        node.destroy_node(); rclpy.shutdown()
        sys.exit(f"Taj /perception failed: {e}")

    left, right = taj.get("left", []), taj.get("right", [])
    objects = taj.get("objects", [])
    print("\n=== TAJ corridor ===")
    print(f"  left pts={len(left)}  right pts={len(right)}  objects={len(objects)}")
    for x in (0.3, 0.5, 1.0):
        lw, rw = _half_width_at(left, right, x)
        print(f"  at x~{x:.1f} m:  left y={lw}  right y={rw}")
    if objects:
        near_obj = min(objects, key=lambda o: math.hypot(o.get("x", 9e9), o.get("y", 9e9)))
        print(f"  nearest object: x={near_obj.get('x')} y={near_obj.get('y')} "
              f"(dist {math.hypot(near_obj.get('x', 0), near_obj.get('y', 0)):.2f} m)")
    if not left or not right:
        print("  ** corridor is EMPTY on a side → planner cannot fit → MRC "
              "(Taj found no forward free space)")

    # --- planner (occy's exact call) ---
    lext, rext = extend_corridor_back(left, right, BACK_M)
    try:
        plan = requests.post(f"{PLANNER}/plan", timeout=2.0, json={
            "ego": {"x": 0.0, "y": 0.0, "heading": 0.0, "speed": 0.0},
            "goal": {"x": GOAL[0], "y": GOAL[1]}, "cruise": CRUISE,
            "left": lext, "right": rext, "objects": objects, "vehicle": VEHICLE,
        }).json()
    except Exception as e:
        node.destroy_node(); rclpy.shutdown()
        sys.exit(f"planner /plan failed: {e}")

    print("\n=== OCCY planner verdict ===")
    print(f"  kind={plan.get('kind')}  verdict={plan.get('verdict')}  "
          f"traj_pts={len(plan.get('trajectory', []))}")
    for k in ("narration", "reason", "deny_code", "explanation", "detail"):
        if k in plan:
            print(f"  {k}: {plan[k]}")
    verdict = plan.get("verdict")
    if plan.get("kind") == "Motion" and verdict in ("Accept", "Clamp"):
        print("  => WOULD DRIVE. If the live occy still holds, its corridor differs "
              "(the robot moved / scan changed since).")
    else:
        print("  => REFUSED. Cause is above: a forward obstacle, an empty/"
              "one-sided corridor, or a footprint that won't fit.")

    node.destroy_node()
    rclpy.shutdown()


if __name__ == "__main__":
    main()
