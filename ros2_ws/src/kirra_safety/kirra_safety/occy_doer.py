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
  4. converts that trajectory to velocities (pure pursuit) and publishes them on
     /cmd_vel_raw as an ATOMIC evidence-bound proposal envelope — the PROPOSAL.

The envelope is canonical JSON in a std_msgs/String (see bound_proposal.py), not a
bare Twist: it carries the proposed velocities AND the `release_binding` naming the
exact Taj evidence frame, platform profile, and Occy proposal digest they were
authored against. That pairing is what lets the verifier mint a signed V2 release the
motor consumer can police — a bare Twist names no evidence, so it could only ever be
signed as "some robot asked for this".

The proposal then flows through the cmd_vel_interceptor (Taj speed cap + the KIRRA
kinematic governor) before reaching the wheels, so Occy only PROPOSES and KIRRA still
DISPOSES — twice (the planner runs the slow-loop checker; the interceptor runs the
fast-loop one). The doer is fail-soft: no goal, a stale scan, an unhealthy scan
stream, a service error, a refused plan, or a plan whose evidence identity cannot be
established all publish an EMPTY envelope (hold) — which the interceptor refuses
fail-closed into a stop. A hold deliberately publishes nothing actuatable rather than
a zero-velocity envelope bound to evidence this tick may not have.

Frame health (opt-in, `frame_health_enabled`): `scan_stale_s` measures ARRIVAL, so it
cannot tell a live lidar from a driver republishing its last buffer at the full rate —
that stream looks perfectly fresh while the world it describes is frozen. The
detectors in `sensor_freshness.py` track frame IDENTITY progression (the producer's
header stamp) and hold the doer with a SPECIFIC reason when it stalls. Content
fingerprinting is corroborating evidence only: a stationary robot facing a wall
legitimately emits identical scans forever, and that is never a fault.

  doer (this node) ─/cmd_vel_raw→ cmd_vel_interceptor [Taj cap + KIRRA] ─/cmd_vel→ wheels

Where Parko fits (Phase 2): when the Parko ML detector is up, its semantic objects feed
the same `objects` list (richer than Taj's geometric clusters) — this node's seam is
unchanged. Mick (the LLM) fits by publishing the goal/intent instead of RViz.
"""

import json
import math
import time

import rclpy
from rclpy.node import Node
from rclpy.qos import QoSProfile, ReliabilityPolicy, HistoryPolicy
from geometry_msgs.msg import PoseStamped
from nav_msgs.msg import Odometry
from sensor_msgs.msg import LaserScan
from std_msgs.msg import String

from kirra_safety.doer_core import (
    yaw_from_quaternion, goal_to_base, goal_reached, decide, extend_corridor_back,
    staleness_budget_valid,
)
from kirra_safety.camera_detect_core import (
    DEFAULT_CAMERA_MAX_AGE_MS, camera_perception_fields, object_goal_reached,
    plan_object_goal_fields,
)
from kirra_safety.bound_proposal import build_release_binding, encode_bound_proposal
from kirra_safety.sensor_freshness import (
    FAIL_CLOSED,
    FrameHealthConfig,
    FrameHealthTracker,
    FrameObservation,
    invalid_ray_ratio,
    scan_fingerprint,
)

try:
    import requests
    REQUESTS_AVAILABLE = True
except ImportError:
    REQUESTS_AVAILABLE = False

# A hold. Empty data is not a parseable envelope, so the interceptor refuses it
# fail-closed and stops — which is exactly what a hold means. It is deliberately
# NOT a zero-velocity envelope: most holds happen before this tick has any
# evidence frame, and fabricating a binding to describe "no motion" would invent
# perception identity the doer never actually observed.
HOLD_ENVELOPE = ''

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
        # --- frame-health detectors (OPT-IN; default off = byte-identical) ---
        # `scan_stale_s` above measures ARRIVAL: it cannot tell a live lidar from
        # a driver republishing its last buffer at the full rate. These track
        # frame IDENTITY progression instead (see sensor_freshness.py). Armed but
        # frozen holds the doer, exactly as an armed-but-silent channel does.
        self.declare_parameter('frame_health_enabled', False)
        # Consecutive non-advancing scans tolerated before holding. One
        # duplicated frame must not stop the robot; a sustained stall must.
        self.declare_parameter('frame_stall_budget', 3)
        # Both thresholds are sentinel-disabled: 0.0 Hz and ratio 1.0 never fire.
        self.declare_parameter('scan_min_rate_hz', 0.0)
        self.declare_parameter('scan_max_invalid_ray_ratio', 1.0)
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
        # --- camera channels (OPT-IN; default off = byte-identical prior behaviour) ---
        # `camera_armed` arms the Taj Phase-B fusion, which is TIGHTEN-ONLY: camera
        # detections may only SHORTEN the lidar corridor, never extend it. Armed but
        # blind (detector down / unregistered depth) relays NO stamp, so Taj faults the
        # channel and floors the speed cap — "the detector did not look" is never "clear".
        self.declare_parameter('camera_armed', False)
        self.declare_parameter('camera_detections_topic', '/camera_detect/detections')
        self.declare_parameter('camera_max_age_ms', DEFAULT_CAMERA_MAX_AGE_MS)
        # The object-goal channel ("drive to the red cup"). A phrase published here is
        # reduced to the one colour term the classical detector can ground and sent as
        # `object_goal`; the planner resolves it against `targets` and fails closed.
        # Empty string clears it. This is a DESTINATION, never a drivability claim.
        self.declare_parameter('object_goal_topic', '/object_goal')

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

        # Frame-health tracker. Disarmed unless explicitly enabled, in which
        # case every resolution is a no-op DISARMED and the tick path below is
        # byte-identical to its prior behaviour.
        self._frame_health_enabled = bool(self.get_parameter('frame_health_enabled').value)
        if self._frame_health_enabled:
            frame_cfg = FrameHealthConfig.armed(
                stall_budget=int(self.get_parameter('frame_stall_budget').value),
                min_rate_hz=float(self.get_parameter('scan_min_rate_hz').value),
                max_invalid_ray_ratio=float(
                    self.get_parameter('scan_max_invalid_ray_ratio').value),
            )
        else:
            frame_cfg = FrameHealthConfig.disarmed()
        self._frame_health = FrameHealthTracker(frame_cfg)
        self._frame_health_reason = None  # latched, so a hold logs once per episode
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

        self._camera_armed = bool(self.get_parameter('camera_armed').value)
        self._camera_max_age_ms = int(self.get_parameter('camera_max_age_ms').value)

        self._pose = None         # (x, y, yaw, speed)
        self._goal = None         # (x, y) in the odom/world frame
        self._scan = None         # (LaserScan, monotonic_recv_time)
        self._camera = None       # the latest detector frame (dict), or None
        self._object_goal = None  # the operator's requested thing, e.g. "red cup"
        self._object_refusal = None   # last refusal reason, logged once
        self._camera_healthy = True   # last reported channel health, logged on change

        # The proposal topic carries the atomic evidence-bound envelope as
        # canonical JSON (std_msgs/String), never a bare Twist.
        self._pub = self.create_publisher(String, self.get_parameter('cmd_topic').value, 10)
        self.create_subscription(Odometry, self.get_parameter('odom_topic').value, self._on_odom, 20)
        self.create_subscription(PoseStamped, self.get_parameter('goal_topic').value, self._on_goal, 10)
        self.create_subscription(LaserScan, self.get_parameter('scan_topic').value, self._on_scan, SCAN_QOS)
        self.create_subscription(
            String, self.get_parameter('camera_detections_topic').value, self._on_camera, 10)
        self.create_subscription(
            String, self.get_parameter('object_goal_topic').value, self._on_object_goal, 10)
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
        # Fold the arrival into the frame-health tracker. This does NOT change
        # the request architecture — the callback still only stores and returns;
        # the HTTP path is untouched (that is PR 3b). The work is skipped
        # entirely when disarmed, so the nominal path costs nothing.
        if not self._frame_health_enabled:
            return
        stamp_ms = int(msg.header.stamp.sec * 1000
                       + msg.header.stamp.nanosec // 1_000_000)
        ranges = msg.ranges
        self._frame_health.observe(FrameObservation(
            stamp_ms=stamp_ms,
            # Supporting evidence only: it distinguishes a replayed buffer from
            # a broken stamp. The stamp is what decides (sensor_freshness.py).
            fingerprint=scan_fingerprint(ranges),
            monotonic_s=self._scan[1],
            invalid_ratio=invalid_ray_ratio(
                ranges, float(msg.range_min), float(msg.range_max)),
        ))

    def _on_camera(self, msg: String):
        """Store the detector frame. Undecodable JSON drops it (armed -> faults)."""
        try:
            frame = json.loads(msg.data)
        except (ValueError, TypeError):
            self._camera = None
            return
        self._camera = frame if isinstance(frame, dict) else None

    def _on_object_goal(self, msg: String):
        phrase = (msg.data or '').strip()
        self._object_goal = phrase or None
        self._object_refusal = None
        self.get_logger().info(
            f'object goal: {self._object_goal!r}' if self._object_goal else 'object goal cleared')

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
    def _publish_envelope(self, data: str):
        msg = String()
        msg.data = data
        self._pub.publish(msg)

    def _hold(self, why: str):
        self._publish_envelope(HOLD_ENVELOPE)
        self.get_logger().debug(f'hold: {why}')

    def _hold_frame_health(self, health):
        """Hold on a frame-health fault, naming the SPECIFIC detector.

        Warn-level and latched on the reason code: a frozen lidar holds every
        tick, so an unlatched log would flood, but the operator must still see
        WHICH fault fired — "the driver is replaying a buffer" and "the stamp is
        broken but data is moving" have different fixes. Re-armed by recovery,
        so a fresh episode logs again.
        """
        if self._frame_health_reason != health.reason:
            self._frame_health_reason = health.reason
            self.get_logger().warn(
                f'frame health {health.reason}: {health.detail} — holding '
                f'(no motion proposed until the scan stream recovers)'
            )
        return self._hold(f'frame-health:{health.reason}')

    def _clear_frame_health(self):
        if self._frame_health_reason is not None:
            self.get_logger().info(
                f'frame health recovered from {self._frame_health_reason}')
            self._frame_health_reason = None

    def _tick(self):
        if not REQUESTS_AVAILABLE:
            return self._hold('no-requests')
        self._poll_mick()
        if self._pose is None:
            return self._hold('awaiting pose')
        if self._goal is None and self._object_goal is None:
            return self._hold('awaiting goal')
        if self._scan is None or (time.monotonic() - self._scan[1]) > self._scan_stale_s:
            return self._hold('stale-scan')  # fail-soft: no fresh perception → hold
        # Frame-identity health. A scan can arrive on cadence and still be the
        # same frame the driver published ten cycles ago — the check above
        # cannot see that, this one can. Disarmed → never FAIL_CLOSED.
        health = self._frame_health.health
        if health.state == FAIL_CLOSED:
            return self._hold_frame_health(health)
        self._clear_frame_health()

        rx, ry, ryaw, speed = self._pose
        # The positional goal (if any). With an object goal set, the planner ignores
        # this in favour of the camera-grounded one — but every numeric field must
        # still be finite, so the ego is the neutral placeholder.
        if self._goal is not None:
            gx, gy = goal_to_base(rx, ry, ryaw, self._goal[0], self._goal[1])
            if self._object_goal is None and goal_reached(gx, gy, self._goal_tol):
                return self._hold('goal-reached')
        else:
            gx, gy = 0.0, 0.0

        scan = self._scan[0]
        # ONE clock domain for every stamp in the request (AOU-TIMESYNC-001): the
        # scan's own header stamp is `now_ms` for the camera-freshness comparison,
        # so a camera stamp from the same ROS clock is judged correctly. Phase A is
        # unaffected (it derives age as now_ms - scan.stamp_ms, i.e. zero either way).
        scan_stamp_ms = int(scan.header.stamp.sec * 1000
                            + scan.header.stamp.nanosec // 1_000_000)

        # The object-goal channel decides BEFORE any request goes out: a refusal is
        # a hold, never a quiet fall-back to some other goal.
        if object_goal_reached(self._object_goal, self._camera, self._goal_tol):
            return self._hold('object-goal-reached')
        goal_fields, refusal = plan_object_goal_fields(
            self._object_goal, self._camera, scan_stamp_ms)
        if refusal:
            if refusal != self._object_refusal:
                self._object_refusal = refusal
                self.get_logger().warn(f'object goal {self._object_goal!r}: {refusal}')
            return self._hold(f'object-goal-refused:{refusal}')
        self._object_refusal = None

        try:
            perception = {
                'angle_min_rad': float(scan.angle_min),
                'angle_increment_rad': float(scan.angle_increment),
                'range_min_m': float(scan.range_min),
                'range_max_m': float(scan.range_max),
                'ranges': [float(r) for r in scan.ranges],
                'stamp_ms': scan_stamp_ms, 'forward_extent_m': self._extent,
            }
            # Camera Phase-B (tighten-only). Disarmed → this is {'camera_armed': False}
            # and the endpoint is byte-identical to the lidar-only path.
            perception.update(camera_perception_fields(
                self._camera_armed, self._camera, self._camera_max_age_ms))
            taj = requests.post(
                f'{self._taj}/perception', timeout=self._timeout_s, json=perception).json()
            if self._camera_armed:
                # Taj already floored the speed cap; say so once per transition so an
                # operator sees WHY the robot crawled rather than guessing — and once
                # per transition rather than every tick, so the log stays readable.
                healthy = taj.get('camera_healthy') is not False
                if healthy != self._camera_healthy:
                    self._camera_healthy = healthy
                    if healthy:
                        self.get_logger().info('camera channel healthy again')
                    else:
                        self.get_logger().warn(
                            'camera channel unhealthy — Taj floored the speed cap '
                            '(no usable frame: detector down, stale, or blind)')

            # Extend the Taj corridor behind the robot (footprint containment) and tell the
            # checker the robot's real size, so KIRRA judges a robot — not a 4.8 m car.
            left, right = extend_corridor_back(taj.get('left', []), taj.get('right', []), self._back_m)
            plan_req = {
                'ego': {'x': 0.0, 'y': 0.0, 'heading': 0.0, 'speed': float(speed)},
                'goal': {'x': gx, 'y': gy},
                'cruise': self._cruise,
                'left': left,
                'right': right,
                'objects': taj.get('objects', []),
                'pedestrians': taj.get('pedestrians', []),
                'predicted_vrus': taj.get('predicted_vrus', []),
                'perception_frame_id': taj.get('frame_id'),
                'vehicle': self._vehicle,
            }
            if goal_fields:
                plan_req.update(goal_fields)
            plan = requests.post(
                f'{self._planner}/plan', timeout=self._timeout_s, json=plan_req).json()
        except Exception as e:  # noqa: BLE001 — any fault holds (fail-soft)
            return self._hold(f'service-error:{e}')

        v, w, reason = decide(plan, self._lookahead, self._max_v, self._max_w)

        # Bind the velocities to the evidence they were authored against. Both
        # halves come from the SAME plan response: `perception_frame_id` is the
        # planner's own statement of the Taj frame it planned against, and
        # `proposal_digest` is computed over exactly that frame plus the
        # authored trajectory. Taking both from one response is what makes the
        # published envelope atomic.
        binding = build_release_binding(
            plan.get('perception_frame_id'), plan.get('proposal_digest'))
        if binding is None:
            # No usable evidence identity — e.g. Taj returned its invalid-frame
            # sentinel, or the planner answered without a digest. Hold rather
            # than propose motion the verifier could not bind.
            return self._hold('unbound-plan')

        envelope = encode_bound_proposal(v, 0.0, w, binding)
        if envelope is None:
            return self._hold('unencodable-proposal')

        self._publish_envelope(envelope)
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
