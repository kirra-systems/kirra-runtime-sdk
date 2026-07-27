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

Steps 2-3 do NOT run inside the timer callback. rclpy's executor is
single-threaded, so two sequential blocking POSTs (up to 2x http_timeout_ms of a
1/plan_hz period) stalled every other callback on this node — including the scan
subscription, so a slow sidecar froze perception ingest as well as planning. They
now run in ONE bounded background job: the worker COMPUTES and the timer
PUBLISHES. A tick therefore publishes the PREVIOUS tick's result — a pipeline,
one tick of latency — and a result is used at most once, only for a scan the
doer has not already acted on, and only while the perception behind it is inside
`scan_stale_s`. Anything else holds. The decision algebra is pure and lives in
`async_plan.py`; this module is the ROS shim around it.

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

Every hold is routed through ONE monitor (`async_plan.HoldMonitor`), because exactly
one hold cause fires per tick. Safe is not the same as visible: a wedged sidecar or a
dead lidar holds forever, which looks identical to a robot standing at a delivered
goal. A sustained hold therefore warns once, naming its cause, and warns again when
the cause changes. Holds that mean "nothing has been asked of me" (awaiting/reached a
goal) are never announced, and holds that already carry their own more specific
warning (frame health, a refused object goal) are counted but not spoken for twice.

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
import threading
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
from kirra_safety.async_plan import (
    ANNOUNCE,
    AWAITING_GOAL,
    AWAITING_POSE,
    FRAME_HEALTH,
    GOAL_REACHED,
    ISSUE,
    NO_REQUESTS,
    OBJECT_GOAL_REACHED,
    OBJECT_GOAL_REFUSED,
    RECOVERED,
    STALE_SCAN,
    UNBOUND,
    UNENCODABLE,
    USE,
    HoldMonitor,
    JobRequest,
    JobResult,
    decide_issue,
    evidence_sequences,
    job_overdue_s,
    resolve_result,
)
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

        # The plan cycle is a PIPELINE: a tick publishes the PREVIOUS tick's
        # result, so the perception behind a proposal is always at least one
        # tick period plus one round trip old. If that does not fit inside
        # `scan_stale_s`, results age out and the doer holds intermittently
        # even with a perfectly healthy lidar — a liveness fault that looks
        # like a sensor fault. WARN rather than abort: the real round-trip time
        # is a measured deployment number and a fast sidecar pair can make a
        # marginal configuration work; refusing to start would be worse.
        self._plan_hz = float(self.get_parameter('plan_hz').value)
        pipeline_s = (1.0 / self._plan_hz) + 2.0 * self._timeout_s
        if pipeline_s > self._scan_stale_s:
            self.get_logger().warn(
                f'plan pipeline budget {pipeline_s * 1000:.0f} ms '
                f'(1/plan_hz={1000.0 / self._plan_hz:.0f} ms + 2x http_timeout_ms) '
                f'exceeds scan_stale_s ({self._scan_stale_s * 1000:.0f} ms): '
                'plans will age out and the doer will hold intermittently. '
                'Raise plan_hz, lower http_timeout_ms, or review the staleness '
                'budget against the deployment lidar rate.'
            )

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

        # --- non-blocking plan cycle -------------------------------------
        # The Taj + planner POSTs run in ONE bounded background job so the
        # timer callback never blocks. Thread discipline (see async_plan.py):
        # the worker COMPUTES, the timer PUBLISHES. The worker touches only
        # `_pending_result` / `_job_in_flight`, both under `_job_lock`; it never
        # publishes, never calls _hold, and never reads or mutates goal, pose,
        # scan or frame-health state.
        #
        # The lock is held ONLY for the slot swap — never across network I/O,
        # so a hung sidecar cannot block the executor thread on acquisition.
        self._job_lock = threading.Lock()
        self._job_in_flight = False
        self._pending_result = None
        # Monotonic time the in-flight job started, or None. Written under the
        # lock alongside `_job_in_flight` so the pair can never disagree.
        self._job_started_s = None
        # Node-local monotonic scan id. ROS 2 removed `header.seq`, so this is
        # the only "which lidar scan" identity available. Written in _on_scan
        # and read in _tick — both on the executor thread, so it needs no lock.
        self._scan_seq = 0
        # Strictly-advancing watermark: a result is consumed at most once and
        # never goes backwards (the V2 release gate's discipline).
        self._last_used_scan_sequence = 0
        # The scan the last job was issued for. Re-issuing against the same
        # scan would spend a sidecar round-trip on a result the watermark is
        # guaranteed to discard, so a tick with no new perception simply does
        # not start a job (NO_FRESH_INPUT). Executor thread only.
        self._last_issued_scan_sequence = 0
        # Hold observability. Every plan-cycle hold is fail-closed and safe,
        # but a hold that repeats forever because a sidecar wedged looks
        # exactly like a robot with nothing to do. These two make the
        # difference audible; neither can change a verdict. Executor-thread
        # state, touched only from _tick's call chain.
        self._hold_monitor = HoldMonitor()
        self._job_overdue_warned = False

        # The Mick intent fetch gets the same treatment: it is HTTP reachable
        # from the timer, so it cannot run inline either. Its own slot, because
        # it is a different endpoint on a different failure budget — a Mick
        # outage must not stall or fault the plan cycle.
        self._mick_lock = threading.Lock()
        self._mick_in_flight = False
        self._mick_result = None
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
        self.create_timer(1.0 / self._plan_hz, self._tick)

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
        # Node-local scan identity. Incremented BEFORE the slot is replaced so
        # `_scan_seq` and `_scan` are always consistent when _tick reads them
        # (same thread, so this is ordering discipline rather than a race fix).
        self._scan_seq += 1
        self._scan = (msg, time.monotonic())
        # Fold the arrival into the frame-health tracker. The callback still
        # only stores and returns — it makes no request and takes no lock. The
        # work is skipped entirely when disarmed, so the nominal path costs
        # nothing.
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
        """Apply a NEW Mick intent, fail-closed at every step.

        The FETCH is a background job (see `_maybe_issue_mick`) so the timer
        callback never blocks on Mick either; this half runs on the executor
        thread and is the ONLY place the goal is mutated — the worker deposits
        the raw wire dict and nothing else.

        Any fault — Mick unreachable, malformed JSON, an unknown tag, a
        non-finite coordinate — leaves the current goal untouched (the same
        outcome as no /goal_pose arriving). A rejected intent NEVER becomes a
        default goal or motion.
        """
        if not self._mick or self._pose is None:
            return
        self._maybe_issue_mick()
        with self._mick_lock:
            wire = self._mick_result
            self._mick_result = None
        if wire is None:
            return  # nothing fetched yet, or the fetch faulted
        try:
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

    def _maybe_issue_mick(self):
        """Start one background Mick fetch if none is running. Never publishes,
        never touches the goal — it deposits the raw wire dict only."""
        with self._mick_lock:
            if self._mick_in_flight:
                return
            self._mick_in_flight = True
        threading.Thread(
            target=self._run_mick_fetch, name='occy-doer-mick', daemon=True,
        ).start()

    def _run_mick_fetch(self):
        """WORKER THREAD. One bounded GET; the wire dict or nothing."""
        wire = None
        try:
            payload = requests.get(
                f'{self._mick}/intent/last', timeout=self._timeout_s).json()
            if isinstance(payload, dict):
                wire = payload
        except Exception:  # noqa: BLE001 — a fault is simply "no new intent"
            wire = None
        with self._mick_lock:
            self._mick_result = wire
            self._mick_in_flight = False

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
        # Counted by the hold monitor, which stays quiet for this state: the
        # warning above already names a more specific cause than it could.
        return self._note_hold(FRAME_HEALTH, f'frame-health:{health.reason}')

    def _clear_frame_health(self):
        if self._frame_health_reason is not None:
            self.get_logger().info(
                f'frame health recovered from {self._frame_health_reason}')
            self._frame_health_reason = None

    def _tick(self):
        if not REQUESTS_AVAILABLE:
            return self._note_hold(NO_REQUESTS, 'no-requests')
        # Before the guards below, so a wedged worker is still reported when the
        # tick is also holding for another reason (a dead lidar and a dead
        # sidecar are two faults, and the operator needs to hear about both).
        self._check_overdue_job()
        self._poll_mick()
        if self._pose is None:
            return self._note_hold(AWAITING_POSE, 'awaiting pose')
        if self._goal is None and self._object_goal is None:
            return self._note_hold(AWAITING_GOAL, 'awaiting goal')
        if self._scan is None or (time.monotonic() - self._scan[1]) > self._scan_stale_s:
            # fail-soft: no fresh perception → hold
            return self._note_hold(STALE_SCAN, 'stale-scan')
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
                return self._note_hold(GOAL_REACHED, 'goal-reached')
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
            return self._note_hold(OBJECT_GOAL_REACHED, 'object-goal-reached')
        goal_fields, refusal = plan_object_goal_fields(
            self._object_goal, self._camera, scan_stamp_ms)
        if refusal:
            if refusal != self._object_refusal:
                self._object_refusal = refusal
                self.get_logger().warn(f'object goal {self._object_goal!r}: {refusal}')
            return self._note_hold(
                OBJECT_GOAL_REFUSED, f'object-goal-refused:{refusal}')
        self._object_refusal = None

        # Snapshot every input the job needs, as PLAIN PYTHON VALUES. The scan's
        # ranges are copied here, on the executor thread, so the worker can
        # never read a ROS message the executor is replacing underneath it.
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
        request = JobRequest(
            scan_sequence=self._scan_seq,
            scan_received_s=self._scan[1],
            taj_url=self._taj,
            planner_url=self._planner,
            timeout_s=self._timeout_s,
            perception=perception,
            plan_fields={
                'speed': float(speed), 'gx': gx, 'gy': gy,
                'cruise': self._cruise, 'back_m': self._back_m,
                'vehicle': self._vehicle, 'goal_fields': goal_fields,
            },
        )

        # Exactly one publish per tick — a proposal built from the LAST job's
        # result, or a hold. Then start the next job. The consume-then-issue
        # order is what makes this a pipeline: this tick acts on the previous
        # tick's perception while the next round-trip is already in flight.
        self._publish_from_pending(gx, gy)
        self._maybe_issue(request)

    # --- the non-blocking plan cycle ---------------------------------------

    def _publish_from_pending(self, gx: float, gy: float):
        """Publish a proposal from the last completed job, or hold.

        Runs on the EXECUTOR thread and is the only place a proposal is built.
        The result is TAKEN from the slot (not peeked), so every result gets
        exactly one verdict and reuse is structurally impossible.
        """
        with self._job_lock:
            result = self._pending_result
            self._pending_result = None
        # Resolution is pure and runs OUTSIDE the lock.
        resolution = resolve_result(
            result,
            newest_scan_sequence=self._scan_seq,
            last_used_scan_sequence=self._last_used_scan_sequence,
            now_s=time.monotonic(),
            scan_stale_s=self._scan_stale_s,
        )
        if resolution.state != USE:
            return self._note_hold(
                resolution.state,
                f'plan-{resolution.state.lower()}:{resolution.reason}')

        # Camera-channel health is NODE state, so the transition log lives here
        # rather than in the worker. Once per transition, not per tick.
        if self._camera_armed:
            healthy = result.camera_healthy is not False
            if healthy != self._camera_healthy:
                self._camera_healthy = healthy
                if healthy:
                    self.get_logger().info('camera channel healthy again')
                else:
                    self.get_logger().warn(
                        'camera channel unhealthy — Taj floored the speed cap '
                        '(no usable frame: detector down, stale, or blind)')

        plan = result.plan
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
            return self._note_hold(UNBOUND, 'plan-unbound:no bindable evidence')

        envelope = encode_bound_proposal(v, 0.0, w, binding)
        if envelope is None:
            return self._note_hold(
                UNENCODABLE, 'plan-unencodable:proposal could not be encoded')

        # USE is folded in only HERE, once a proposal is actually going out —
        # so a plan that resolved usable but could not be bound or encoded
        # counts as the hold it is, not as a recovery.
        notice = self._hold_monitor.observe(USE)
        if notice.action == RECOVERED:
            self.get_logger().info(
                f'doer recovered from {notice.state} after '
                f'{notice.count} consecutive holds — proposing motion again')

        # Advance the watermark only once the proposal is actually published.
        self._last_used_scan_sequence = result.requested_scan_sequence
        self._publish_envelope(envelope)
        self.get_logger().debug(f'{reason}  v={v:.2f} w={w:.2f}  goal_base=({gx:.1f},{gy:.1f})')

    def _note_hold(self, state: str, tag: str):
        """Fold one hold into the monitor, then hold.

        EVERY hold the doer takes comes through here — the tick-stage guards
        as well as the plan cycle — because exactly one of them fires per tick.
        The hold itself is unconditional and unchanged; this only decides
        whether to say so out loud. A sustained episode warns ONCE, and warns
        again if the cause changes; motion resuming re-arms it. `tag` is the
        existing debug string, kept verbatim so the debug trail is unchanged.
        """
        notice = self._hold_monitor.observe(state)
        if notice.action == ANNOUNCE:
            self.get_logger().warn(
                f'doer holding on {notice.state} ({tag}) for {notice.count} '
                'consecutive ticks — no motion is being proposed and the '
                'robot will remain stopped'
            )
        return self._hold(tag)

    def _check_overdue_job(self):
        """WARN once when a job has been in flight far past its budget.

        A `requests` timeout is per SOCKET OPERATION, not wall-clock, so a
        sidecar that dribbles bytes can hold a worker open indefinitely
        without ever tripping its own timeout. Since exactly one job may run,
        that ends planning for good — and, before this, silently.

        Detecting it deliberately does NOT start a replacement job: the
        one-job bound is what keeps thread creation bounded against a sick
        sidecar, so the doer stays held (fail-closed) and says so instead.
        """
        with self._job_lock:
            started_s = self._job_started_s if self._job_in_flight else None
        # The nominal budget is the two bounded round trips the job makes.
        overdue_s = job_overdue_s(
            started_s, time.monotonic(), 2.0 * self._timeout_s)
        if overdue_s is None:
            self._job_overdue_warned = False
            return
        if self._job_overdue_warned:
            return
        self._job_overdue_warned = True
        self.get_logger().warn(
            f'plan job overdue by {overdue_s * 1000:.0f} ms past its '
            f'{2.0 * self._timeout_s * 1000:.0f} ms budget — a sidecar is not '
            'closing the connection (an HTTP timeout bounds each socket read, '
            'not the whole request). No further plan jobs will start until it '
            'returns; the doer stays held.'
        )

    def _maybe_issue(self, request):
        """Start one plan job if none is running. Never publishes."""
        fresh = request is not None and (
            request.scan_sequence > self._last_issued_scan_sequence)
        with self._job_lock:
            decision = decide_issue(self._job_in_flight, fresh)
            if decision != ISSUE:
                return decision
            self._job_in_flight = True
            self._job_started_s = time.monotonic()
        self._last_issued_scan_sequence = request.scan_sequence
        # Thread creation happens OUTSIDE the lock. `daemon=True` is the
        # teardown contract: a worker blocked on a hung sidecar can never keep
        # the process alive at shutdown, and nothing joins it.
        threading.Thread(
            target=self._run_job, args=(request,),
            name='occy-doer-plan', daemon=True,
        ).start()
        return ISSUE

    def _run_job(self, request):
        """WORKER THREAD. Computes, deposits, and touches nothing else."""
        result = self._execute_job(request)
        with self._job_lock:
            self._pending_result = result
            self._job_in_flight = False
            self._job_started_s = None

    def _execute_job(self, request):
        """WORKER THREAD. The two bounded POSTs, as a pure request→result map.

        No lock is held here: all network I/O happens outside `_job_lock`.
        Every failure becomes a JobResult carrying a fault tag, so a fault can
        never be mistaken for "no result yet".
        """
        def faulted(reason):
            return JobResult(
                requested_scan_sequence=request.scan_sequence,
                scan_received_s=request.scan_received_s,
                taj_scan_sequence=None, plan_scan_sequence=None,
                plan=None, camera_healthy=None, fault=reason,
            )

        fields = request.plan_fields
        try:
            taj = requests.post(
                f'{request.taj_url}/perception',
                timeout=request.timeout_s, json=request.perception).json()
            if not isinstance(taj, dict):
                # Named explicitly rather than left to blow up on `.get` below,
                # so the operator sees WHICH sidecar answered wrongly.
                return faulted('taj-response-not-an-object')
            # Extend the Taj corridor behind the robot (footprint containment) and
            # tell the checker the robot's real size, so KIRRA judges a robot — not
            # a 4.8 m car.
            left, right = extend_corridor_back(
                taj.get('left', []), taj.get('right', []), fields['back_m'])
            plan_req = {
                'ego': {'x': 0.0, 'y': 0.0, 'heading': 0.0, 'speed': fields['speed']},
                'goal': {'x': fields['gx'], 'y': fields['gy']},
                'cruise': fields['cruise'],
                'left': left,
                'right': right,
                'objects': taj.get('objects', []),
                'pedestrians': taj.get('pedestrians', []),
                'predicted_vrus': taj.get('predicted_vrus', []),
                'perception_frame_id': taj.get('frame_id'),
                'vehicle': fields['vehicle'],
            }
            if fields['goal_fields']:
                plan_req.update(fields['goal_fields'])
            plan = requests.post(
                f'{request.planner_url}/plan',
                timeout=request.timeout_s, json=plan_req).json()
        except Exception as e:  # noqa: BLE001 — any fault holds (fail-soft)
            return faulted(f'service-error:{type(e).__name__}')

        if not isinstance(plan, dict):
            return faulted('planner-response-not-an-object')

        taj_seq, plan_seq = evidence_sequences(taj, plan)
        return JobResult(
            requested_scan_sequence=request.scan_sequence,
            scan_received_s=request.scan_received_s,
            taj_scan_sequence=taj_seq,
            plan_scan_sequence=plan_seq,
            plan=plan,
            camera_healthy=taj.get('camera_healthy'),
            fault=None,
        )


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
