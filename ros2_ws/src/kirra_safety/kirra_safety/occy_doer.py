#!/usr/bin/env python3
"""
Occy doer bridge — the planner that decides where to go (the DOER).

This is the missing piece that makes real Occy drive the robot to a goal, governed by
KIRRA. Each tick it:
  1. reads the robot pose + speed (/odom) and the current goal (/goal_pose, e.g. RViz
     "2D Goal Pose", or an LLM/Mick publisher),
  2. POSTs the latest lidar scan (/scan) to the Taj sidecar → the geometric corridor
     (left/right polylines) + objects,
  3. POSTs {ego, goal-in-base, Taj corridor, objects} to the Occy planner sidecar
     (/plan) → a KIRRA-validated trajectory,
  4. converts that trajectory to a Twist (pure pursuit) and publishes it on
     /cmd_vel_raw — the PROPOSAL.

The proposal then flows through the cmd_vel_interceptor (Taj speed cap + the KIRRA
kinematic governor) before reaching the wheels, so Occy only PROPOSES and KIRRA still
DISPOSES — twice (the planner runs the slow-loop checker; the interceptor runs the
fast-loop one). The doer is fail-soft: no goal, a stale scan, a service error, or a
refused plan all publish a zero Twist (hold).

  doer (this node) ─/cmd_vel_raw→ cmd_vel_interceptor [Taj cap + KIRRA] ─/cmd_vel→ wheels

Where Parko fits (Phase 2): when the Parko ML detector is up, its semantic objects feed
the same `objects` list (richer than Taj's geometric clusters) — this node's seam is
unchanged. Mick (the LLM) fits by publishing the goal/intent instead of RViz.
"""

import math
import time

import rclpy
from rclpy.node import Node
from rclpy.qos import QoSProfile, ReliabilityPolicy, HistoryPolicy
from geometry_msgs.msg import Twist, PoseStamped
from nav_msgs.msg import Odometry
from sensor_msgs.msg import LaserScan

from kirra_safety.doer_core import (
    yaw_from_quaternion, goal_to_base, goal_reached, decide, extend_corridor_back,
    staleness_budget_valid,
)

try:
    import requests
    REQUESTS_AVAILABLE = True
except ImportError:
    REQUESTS_AVAILABLE = False

ZERO = Twist()

# Lidar-ingress QoS (hardware finding: the TG30 driver publishes /scan
# BEST_EFFORT; a default RELIABLE subscription silently matches ZERO messages —
# no error, just an eternally stale scan). BestEffort + KeepLast(1) is the
# house sensor-ingress discipline (kirra-ros2-adapter ingress_sensor_qos,
# node.rs: freshness over buffering — no stale backlog after a stall), and a
# BestEffort subscription is compatible with BOTH BestEffort and Reliable
# publishers, so this never regresses a Reliable lidar.
SCAN_QOS = QoSProfile(
    reliability=ReliabilityPolicy.BEST_EFFORT,
    history=HistoryPolicy.KEEP_LAST,
    depth=1,
)

# Mick intents this bridge grounds. Positional intents become the goal;
# `hold` clears it. Anything else is ignored (logged once) — the doer stays
# the ONLY consumer of intents, and an unknown/partial intent NEVER becomes
# motion (fail-closed, mirrors MickIntent::from_llm_json's posture).
MICK_POSITIONAL_INTENTS = ('go_to', 'route_to', 'yield', 'cross_when_clear', 'creep_through')


class OccyDoer(Node):
    def __init__(self):
        super().__init__('occy_doer')
        self.declare_parameter('taj_url', 'http://localhost:8101')
        self.declare_parameter('planner_url', 'http://localhost:8100')
        # The Mick typed-intent sidecar (kirra-sidecars mick_service). Empty =
        # off (goals come from /goal_pose only). When set, the doer polls
        # GET /intent/last each tick and grounds NEW intents: a positional
        # intent (go_to / route_to / ...) becomes the goal (ego frame at
        # receipt → odom), `hold` clears it. Mick publishes INTENTS, never
        # commands — this bridge is the only consumer.
        self.declare_parameter('mick_url', '')
        self.declare_parameter('odom_topic', '/odom')
        self.declare_parameter('goal_topic', '/goal_pose')
        self.declare_parameter('scan_topic', '/scan')
        self.declare_parameter('cmd_topic', '/cmd_vel_raw')
        self.declare_parameter('plan_hz', 5.0)
        self.declare_parameter('cruise_speed_mps', 1.2)
        self.declare_parameter('max_speed_mps', 1.2)
        self.declare_parameter('max_yaw_rate_rps', 1.5)
        self.declare_parameter('lookahead_m', 0.8)
        self.declare_parameter('goal_tolerance_m', 0.25)
        self.declare_parameter('forward_extent_m', 8.0)
        # REQUIRED, no default — 0.0 is the unset sentinel. How old the newest
        # scan may be before this node stops proposing motion (holds). A
        # safety number: it bounds how blind the doer can be while still
        # moving, so it is operator-set per deployment (lidar rate dependent —
        # e.g. ~0.25 s for a 10 Hz TG30), never silently defaulted. Mirrors
        # the ros2-adapter's KIRRA_SUBSCRIPTION_STALENESS_MS discipline and
        # the interceptor's required wheelbase_m.
        self.declare_parameter('scan_stale_s', 0.0)
        self.declare_parameter('http_timeout_ms', 60)
        # The robot's footprint/kinematics for the CHECKER. A small differential robot MUST
        # pass these, or the planner's default urban-car (4.8 m) footprint can't fit a
        # robot-scale corridor and KIRRA MRCs every plan. Defaults: a Rosmaster-class robot.
        self.declare_parameter('vehicle_class', 'courier')  # per-class checker profile (CONTRACT_PROFILES.md)
        self.declare_parameter('wheelbase_m', 0.2)
        self.declare_parameter('half_length_m', 0.18)
        self.declare_parameter('half_width_m', 0.15)
        self.declare_parameter('max_steering_deg', 30.0)
        # Per-class RSS band (checker) + the doer's lateral-clearance target. Robot-scale so
        # the small robot is judged as a robot, not a 4.8 m car. See CONTRACT_PROFILES.md.
        self.declare_parameter('rss_lateral_alignment_tolerance_m', 0.6)
        self.declare_parameter('lateral_clearance_target_m', 0.6)
        # Extend the corridor behind the robot so its footprint (which sits behind the lidar
        # at the origin) is contained — Taj only reports forward free space.
        self.declare_parameter('corridor_back_m', 0.5)

        self._taj = self.get_parameter('taj_url').value.rstrip('/')
        self._planner = self.get_parameter('planner_url').value.rstrip('/')
        self._mick = self.get_parameter('mick_url').value.rstrip('/')
        self._mick_seq = 0          # last consumed intent seq (apply-once)
        self._mick_ignored = set()  # unknown tags already logged
        self._cruise = self.get_parameter('cruise_speed_mps').value
        self._max_v = self.get_parameter('max_speed_mps').value
        self._max_w = self.get_parameter('max_yaw_rate_rps').value
        self._lookahead = self.get_parameter('lookahead_m').value
        self._goal_tol = self.get_parameter('goal_tolerance_m').value
        self._extent = self.get_parameter('forward_extent_m').value
        self._scan_stale_s = self.get_parameter('scan_stale_s').value
        if not staleness_budget_valid(self._scan_stale_s):
            # Fail-closed: an unset/invalid staleness budget would either let
            # the doer plan on arbitrarily old perception or come from a typo
            # the operator believes is in effect. Refuse to start.
            self.get_logger().fatal(
                'scan_stale_s parameter is REQUIRED (finite, > 0 seconds) — '
                f'refusing to start (got {self._scan_stale_s!r}). Set it to the '
                'deployment lidar staleness budget (e.g. 0.25 for a 10 Hz scan).'
            )
            raise SystemExit(2)
        self._timeout_s = self.get_parameter('http_timeout_ms').value / 1000.0
        self._back_m = self.get_parameter('corridor_back_m').value
        self._vehicle = {
            'class': self.get_parameter('vehicle_class').value,
            'wheelbase_m': self.get_parameter('wheelbase_m').value,
            'half_length_m': self.get_parameter('half_length_m').value,
            'half_width_m': self.get_parameter('half_width_m').value,
            'max_speed_mps': self.get_parameter('max_speed_mps').value,
            'max_steering_deg': self.get_parameter('max_steering_deg').value,
            'rss_lateral_alignment_tolerance_m':
                self.get_parameter('rss_lateral_alignment_tolerance_m').value,
            'lateral_clearance_target_m': self.get_parameter('lateral_clearance_target_m').value,
        }

        self._pose = None         # (x, y, yaw, speed)
        self._goal = None         # (x, y) in the odom/world frame
        self._scan = None         # (LaserScan, monotonic_recv_time)

        self._pub = self.create_publisher(Twist, self.get_parameter('cmd_topic').value, 10)
        self.create_subscription(Odometry, self.get_parameter('odom_topic').value, self._on_odom, 20)
        self.create_subscription(PoseStamped, self.get_parameter('goal_topic').value, self._on_goal, 10)
        self.create_subscription(LaserScan, self.get_parameter('scan_topic').value, self._on_scan, SCAN_QOS)
        self.create_timer(1.0 / self.get_parameter('plan_hz').value, self._tick)

        if not REQUESTS_AVAILABLE:
            self.get_logger().error('python3-requests missing — doer holds (publishes zero).')
        self.get_logger().info(
            f'occy_doer: Taj({self._taj}) + Occy({self._planner}) -> '
            f'{self.get_parameter("cmd_topic").value}. Send a goal on '
            f'{self.get_parameter("goal_topic").value} (RViz "2D Goal Pose").'
        )

    # --- subscriptions ------------------------------------------------------
    def _on_odom(self, msg: Odometry):
        p, q = msg.pose.pose.position, msg.pose.pose.orientation
        yaw = yaw_from_quaternion(q.x, q.y, q.z, q.w)
        speed = msg.twist.twist.linear.x
        self._pose = (p.x, p.y, yaw, speed)

    def _on_goal(self, msg: PoseStamped):
        self._goal = (msg.pose.position.x, msg.pose.position.y)
        self.get_logger().info(f'new goal: ({self._goal[0]:.2f}, {self._goal[1]:.2f})')

    def _on_scan(self, msg: LaserScan):
        self._scan = (msg, time.monotonic())

    # --- Mick intent consumption (intents, never commands) -------------------
    def _poll_mick(self):
        """Ground a NEW Mick intent, fail-closed at every step.

        Any fault — Mick unreachable, malformed JSON, an unknown tag, a
        non-finite coordinate — leaves the current goal untouched (the same
        outcome as no /goal_pose arriving). A rejected intent NEVER becomes a
        default goal or motion.
        """
        if not self._mick or self._pose is None:
            return
        try:
            wire = requests.get(f'{self._mick}/intent/last', timeout=self._timeout_s).json()
            intent = wire.get('intent')
            seq = int(wire.get('seq', 0))
            if not isinstance(intent, dict) or seq <= self._mick_seq:
                return  # nothing new (apply-once by seq)
            self._mick_seq = seq
            tag = intent.get('intent')
            if tag == 'hold':
                self._goal = None
                self.get_logger().info(f'mick intent #{seq}: hold — goal cleared')
            elif tag in MICK_POSITIONAL_INTENTS:
                x, y = float(intent['x_m']), float(intent['y_m'])
                if not (math.isfinite(x) and math.isfinite(y)):
                    raise ValueError('non-finite intent target')
                # Ego-frame (+ahead, +left) at receipt → odom frame.
                rx, ry, ryaw, _ = self._pose
                gx = rx + x * math.cos(ryaw) - y * math.sin(ryaw)
                gy = ry + x * math.sin(ryaw) + y * math.cos(ryaw)
                self._goal = (gx, gy)
                self.get_logger().info(
                    f'mick intent #{seq}: {tag} ego({x:.1f},{y:.1f}) -> goal ({gx:.2f},{gy:.2f})')
            elif tag not in self._mick_ignored:
                self._mick_ignored.add(tag)
                self.get_logger().info(
                    f'mick intent #{seq}: `{tag}` carries no goal for this bridge — ignored')
        except Exception as e:  # noqa: BLE001 — any fault keeps the current goal (fail-soft)
            self.get_logger().debug(f'mick poll: {e}')

    # --- the doer loop ------------------------------------------------------
    def _hold(self, why: str):
        self._pub.publish(ZERO)
        self.get_logger().debug(f'hold: {why}')

    def _tick(self):
        if not REQUESTS_AVAILABLE:
            return self._hold('no-requests')
        self._poll_mick()
        if self._pose is None or self._goal is None:
            return self._hold('awaiting pose/goal')
        if self._scan is None or (time.monotonic() - self._scan[1]) > self._scan_stale_s:
            return self._hold('stale-scan')  # fail-soft: no fresh perception → hold

        rx, ry, ryaw, speed = self._pose
        gx, gy = goal_to_base(rx, ry, ryaw, self._goal[0], self._goal[1])
        if goal_reached(gx, gy, self._goal_tol):
            return self._hold('goal-reached')

        scan = self._scan[0]
        try:
            taj = requests.post(f'{self._taj}/perception', timeout=self._timeout_s, json={
                'angle_min_rad': float(scan.angle_min),
                'angle_increment_rad': float(scan.angle_increment),
                'range_min_m': float(scan.range_min),
                'range_max_m': float(scan.range_max),
                'ranges': [float(r) for r in scan.ranges],
                'stamp_ms': 0, 'forward_extent_m': self._extent,
            }).json()

            # Extend the Taj corridor behind the robot (footprint containment) and tell the
            # checker the robot's real size, so KIRRA judges a robot — not a 4.8 m car.
            left, right = extend_corridor_back(taj.get('left', []), taj.get('right', []), self._back_m)
            plan = requests.post(f'{self._planner}/plan', timeout=self._timeout_s, json={
                'ego': {'x': 0.0, 'y': 0.0, 'heading': 0.0, 'speed': float(speed)},
                'goal': {'x': gx, 'y': gy},
                'cruise': self._cruise,
                'left': left,
                'right': right,
                'objects': taj.get('objects', []),
                'vehicle': self._vehicle,
            }).json()
        except Exception as e:  # noqa: BLE001 — any fault holds (fail-soft)
            return self._hold(f'service-error:{e}')

        v, w, reason = decide(plan, self._lookahead, self._max_v, self._max_w)
        twist = Twist()
        twist.linear.x = v
        twist.angular.z = w
        self._pub.publish(twist)
        self.get_logger().debug(f'{reason}  v={v:.2f} w={w:.2f}  goal_base=({gx:.1f},{gy:.1f})')


def main(args=None):
    rclpy.init(args=args)
    node = OccyDoer()
    try:
        rclpy.spin(node)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        rclpy.try_shutdown()


if __name__ == '__main__':
    main()
