#!/usr/bin/env python3
"""
Pure geometry/decision helpers for the Occy doer bridge (occy_doer).

No ROS, no HTTP — just the math that turns a goal + Occy's KIRRA-validated trajectory
into a `/cmd_vel_raw` Twist. Kept pure so the safety-relevant conversion is unit
testable standalone (the same discipline as perception_cap / enforcement_decision).

Frames: the robot base frame has +X forward, +Y left (ROS REP-103). The planner runs in
this base frame (ego at the origin, heading 0); the goal is transformed into it here.
"""

import math


def yaw_from_quaternion(x, y, z, w):
    """Planar yaw (rad) from a quaternion (full formula; robust for non-planar tilt)."""
    siny_cosp = 2.0 * (w * z + x * y)
    cosy_cosp = 1.0 - 2.0 * (y * y + z * z)
    return math.atan2(siny_cosp, cosy_cosp)


def goal_to_base(robot_x, robot_y, robot_yaw, goal_x, goal_y):
    """Transform a goal from the odom/world frame into the robot base frame.

    Returns (gx, gy): the goal expressed with the robot at the origin facing +X.
    """
    dx = goal_x - robot_x
    dy = goal_y - robot_y
    c, s = math.cos(-robot_yaw), math.sin(-robot_yaw)
    return c * dx - s * dy, s * dx + c * dy


def goal_reached(goal_base_x, goal_base_y, tolerance_m):
    """True once the goal (in base frame) is within tolerance of the robot."""
    return math.hypot(goal_base_x, goal_base_y) <= tolerance_m


def extend_corridor_back(left, right, back_m):
    """Prepend a straight back-extension to each corridor polyline.

    Taj reports only FORWARD free space (x >= 0 from the lidar), but the robot's footprint
    extends behind the sensor at the origin. Without this, the checker sees the rear of the
    footprint outside the corridor and MRCs every plan. The area just behind the robot —
    where it already sits — is assumed clear, so we extend each boundary straight back.
    """
    def ext(poly):
        if not poly:
            return poly
        x0, y0 = poly[0][0], poly[0][1]
        return [[x0 - back_m, y0]] + [list(p) for p in poly]
    return ext(left), ext(right)


def _lookahead_point(traj, lookahead_m):
    """The first trajectory point at/after `lookahead_m` of straight-line distance from
    the ego (base-frame origin); falls back to the last point. `traj` is a list of dicts
    with at least x, y (and optionally v)."""
    for p in traj:
        if math.hypot(p["x"], p["y"]) >= lookahead_m:
            return p
    return traj[-1] if traj else None


def trajectory_to_twist(traj, lookahead_m, max_v, max_w):
    """Pure-pursuit conversion of a base-frame trajectory to (v, w).

    Curvature to the lookahead point (lx, ly): kappa = 2*ly / (lx^2 + ly^2); w = v*kappa.
    +Y (left) → +w (CCW), matching ROS. v is the planned speed at the lookahead point,
    capped at max_v; w is clamped to +/- max_w. Empty trajectory → (0, 0).
    """
    pt = _lookahead_point(traj, lookahead_m)
    if pt is None:
        return 0.0, 0.0
    lx, ly = pt["x"], pt["y"]
    dist2 = lx * lx + ly * ly
    v = min(abs(pt.get("v", max_v)), max_v)
    if dist2 < 1e-6:
        return 0.0, 0.0
    if lx <= 0.0:
        # Lookahead is at/behind the axle — rotate in place toward it rather than lunging.
        w = math.copysign(max_w, ly if ly != 0.0 else 1.0)
        return 0.0, w
    kappa = 2.0 * ly / dist2
    w = max(-max_w, min(max_w, v * kappa))
    return v, w


def decide(plan_json, lookahead_m, max_v, max_w):
    """Turn an Occy /plan response into (v, w, reason).

    HOLD (0, 0) unless Occy proposed motion AND KIRRA's slow-loop verdict admits it
    (Accept|Clamp). A SafeStop proposal, an MRC/refused verdict, or an empty trajectory
    all hold — the doer never commands motion the checker already refused. The downstream
    cmd_vel_interceptor re-checks every command anyway (fast-loop KIRRA + Taj cap).
    """
    kind = plan_json.get("kind")
    verdict = plan_json.get("verdict")
    traj = plan_json.get("trajectory") or []
    if kind == "SafeStop" or verdict not in ("Accept", "Clamp") or not traj:
        return 0.0, 0.0, f"HOLD:{kind}/{verdict}"
    v, w = trajectory_to_twist(traj, lookahead_m, max_v, max_w)
    return v, w, f"DRIVE:{verdict}"
