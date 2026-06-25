"""
Unit tests for the pure Occy-doer geometry/decision helpers (no ROS, no HTTP).

Run:  pytest ros2_ws/src/kirra_safety/test/test_doer_core.py
"""

import math
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "kirra_safety"))
from doer_core import (  # noqa: E402
    yaw_from_quaternion, goal_to_base, goal_reached, trajectory_to_twist, decide,
    extend_corridor_back,
)


def test_extend_corridor_back_prepends_behind_origin():
    left, right = [[0.0, 1.0], [5.0, 1.0]], [[0.0, -1.0], [5.0, -1.0]]
    l2, r2 = extend_corridor_back(left, right, 0.5)
    assert l2[0] == [-0.5, 1.0] and r2[0] == [-0.5, -1.0]   # prepended back point
    assert l2[1:] == left and r2[1:] == right               # original points preserved
    assert extend_corridor_back([], [], 0.5) == ([], [])    # empty stays empty


def test_yaw_from_quaternion_basic():
    assert abs(yaw_from_quaternion(0, 0, 0, 1)) < 1e-9          # identity → 0
    assert abs(yaw_from_quaternion(0, 0, math.sin(math.pi/4), math.cos(math.pi/4)) - math.pi/2) < 1e-6


def test_goal_to_base_translation_only():
    # Robot at (1,0) facing +X, goal at (4,0) → 3 m straight ahead.
    gx, gy = goal_to_base(1.0, 0.0, 0.0, 4.0, 0.0)
    assert abs(gx - 3.0) < 1e-9 and abs(gy) < 1e-9


def test_goal_to_base_rotation():
    # Robot at origin facing +Y (yaw=pi/2); a goal due east (3,0) is 3 m to the robot's RIGHT.
    gx, gy = goal_to_base(0.0, 0.0, math.pi/2, 3.0, 0.0)
    assert abs(gx) < 1e-6          # nothing ahead
    assert abs(gy + 3.0) < 1e-6    # 3 m to the right → -Y in base frame


def test_goal_reached():
    assert goal_reached(0.1, 0.05, 0.2)
    assert not goal_reached(1.0, 0.0, 0.2)


def test_trajectory_to_twist_straight_ahead_no_turn():
    traj = [{"x": x, "y": 0.0, "v": 1.0} for x in (0.0, 0.5, 1.0, 1.5)]
    v, w = trajectory_to_twist(traj, lookahead_m=1.0, max_v=1.2, max_w=2.0)
    assert abs(v - 1.0) < 1e-9 and abs(w) < 1e-9


def test_trajectory_to_twist_left_curve_turns_left():
    # Lookahead point off to the left (+Y) → positive (CCW) angular velocity.
    traj = [{"x": 1.0, "y": 0.0, "v": 1.0}, {"x": 1.4, "y": 0.5, "v": 1.0}]
    v, w = trajectory_to_twist(traj, lookahead_m=1.2, max_v=1.2, max_w=2.0)
    assert v > 0 and w > 0


def test_trajectory_to_twist_caps_speed_and_turn():
    traj = [{"x": 0.2, "y": 1.0, "v": 5.0}]  # demands a hard left at high speed
    v, w = trajectory_to_twist(traj, lookahead_m=0.1, max_v=1.2, max_w=1.5)
    assert v <= 1.2 + 1e-9 and abs(w) <= 1.5 + 1e-9


def test_trajectory_to_twist_empty_is_stop():
    assert trajectory_to_twist([], 1.0, 1.2, 2.0) == (0.0, 0.0)


def test_decide_drives_on_accept():
    plan = {"kind": "Motion", "verdict": "Accept",
            "trajectory": [{"x": 1.0, "y": 0.0, "v": 1.0}, {"x": 1.5, "y": 0.0, "v": 1.0}]}
    v, w, reason = decide(plan, 1.0, 1.2, 2.0)
    assert v > 0 and reason.startswith("DRIVE")


def test_decide_holds_on_safe_stop():
    plan = {"kind": "SafeStop", "verdict": "Accept", "trajectory": []}
    assert decide(plan, 1.0, 1.2, 2.0) == (0.0, 0.0, "HOLD:SafeStop/Accept")


def test_decide_holds_on_refused_verdict():
    # Occy proposed motion but KIRRA's slow loop refused it → the doer holds.
    plan = {"kind": "Motion", "verdict": "MRCFallback",
            "trajectory": [{"x": 1.0, "y": 0.0, "v": 1.0}]}
    v, w, reason = decide(plan, 1.0, 1.2, 2.0)
    assert (v, w) == (0.0, 0.0) and reason.startswith("HOLD")
