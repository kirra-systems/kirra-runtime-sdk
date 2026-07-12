#!/usr/bin/env python3
"""
Kirra /cmd_vel Safety Interceptor

Subscribes to /cmd_vel (from nav stack / AI planner).
Sends each command through Kirra kinematic enforcement.
Publishes enforced command to /cmd_vel_safe (to motor controllers).

Topic flow:
  /cmd_vel (geometry_msgs/Twist) [FROM planner]
      |
  Kirra POST /actuator/motion/command (HTTP)
      | enforce kinematic contract
      | check fleet posture
  /cmd_vel_safe (geometry_msgs/Twist) [TO motors]
  /kirra/enforcement_action (std_msgs/String) [FOR monitoring]
"""

import json
import time
import threading

import rclpy
from rclpy.node import Node
from geometry_msgs.msg import Twist
from std_msgs.msg import String, Float64, UInt8MultiArray

from kirra_safety.enforcement_decision import (
    decide_enforcement, Forward, wheelbase_consistent, release_frame,
)
from kirra_safety.perception_cap import apply_perception_cap, DISABLED

try:
    import requests
    REQUESTS_AVAILABLE = True
except ImportError:
    REQUESTS_AVAILABLE = False


ZERO_TWIST = Twist()


class CmdVelInterceptor(Node):
    def __init__(self):
        super().__init__('cmd_vel_interceptor')

        # Parameters
        self.declare_parameter('kirra_url', 'http://localhost:8090')
        self.declare_parameter('kirra_token', '')
        self.declare_parameter('kirra_client_id', 'ros2-interceptor-01')
        # Track-A A3: REQUIRED, no default. The wheelbase must be the active
        # vehicle class's contract wheelbase (the same L the P6 lateral-accel
        # check uses); the verifier reports its value on every release and this
        # node fail-closes on mismatch. 0.0 is the unset sentinel — refused.
        self.declare_parameter('wheelbase_m', 0.0)
        self.declare_parameter('max_speed_mps', 1.8)
        self.declare_parameter('input_topic', '/cmd_vel')
        self.declare_parameter('output_topic', '/cmd_vel_safe')
        self.declare_parameter('enforcement_topic', '/kirra/enforcement_action')
        self.declare_parameter('posture_topic', '/kirra/fleet_posture')
        self.declare_parameter('timeout_ms', 50)
        self.declare_parameter('fallback_on_timeout', 'stop')
        # Taj perception derate (opt-in). When enabled, Taj's assured-clear-distance cap
        # (published by the perception_governor node) tightens the proposed forward speed
        # BEFORE the governor — Taj tightens, KIRRA bounds. Default OFF → byte-identical path.
        self.declare_parameter('use_perception_cap', False)
        self.declare_parameter('perception_cap_topic', '/kirra/perception_speed_cap')
        self.declare_parameter('perception_cap_stale_ms', 300)
        # ADR-0033 live-loop relay: when the verifier's 200 carries a `release`
        # object (signer provisioned), its payload_hex||token_hex bytes are
        # republished as one 128-byte frame on this topic for the verifying
        # motor consumer (robot/kirra_motor_consumer.py). Pure carriage — the
        # consumer's Ed25519 verify over exactly these bytes is the trust path.
        self.declare_parameter('release_topic', '/kirra/release')

        self._kirra_url = self.get_parameter('kirra_url').value
        self._kirra_token = self.get_parameter('kirra_token').value
        self._client_id = self.get_parameter('kirra_client_id').value
        self._wheelbase_m = self.get_parameter('wheelbase_m').value
        import math as _math
        if (isinstance(self._wheelbase_m, bool)
                or not isinstance(self._wheelbase_m, (int, float))
                or not _math.isfinite(self._wheelbase_m)
                or self._wheelbase_m <= 0.0):
            # bool is an int subclass: `wheelbase_m: true` would otherwise
            # pass as an effective 1.0 m wheelbase (review #904) — rejected,
            # matching _is_finite_number's explicit bool refusal.
            # Fail-closed: an unset/invalid wheelbase would silently scale every
            # commanded yaw by L_i/L_v (what Kirra approves != what executes).
            self.get_logger().fatal(
                'wheelbase_m parameter is REQUIRED (finite, > 0) and must equal the '
                'active vehicle class contract wheelbase — refusing to start '
                f'(got {self._wheelbase_m!r}).'
            )
            raise SystemExit(2)
        # Latched by the per-release cross-check: once a mismatch between this
        # parameter and the verifier-reported conversion wheelbase is seen, the
        # node publishes stop for every subsequent command until fixed+restarted.
        self._wheelbase_mismatch = False
        self._max_speed_mps = self.get_parameter('max_speed_mps').value
        self._timeout_s = self.get_parameter('timeout_ms').value / 1000.0
        self._fallback = self.get_parameter('fallback_on_timeout').value
        self._use_perception_cap = self.get_parameter('use_perception_cap').value
        self._cap_stale_s = self.get_parameter('perception_cap_stale_ms').value / 1000.0

        input_topic = self.get_parameter('input_topic').value
        output_topic = self.get_parameter('output_topic').value
        enforcement_topic = self.get_parameter('enforcement_topic').value

        # State
        self._current_velocity_mps = 0.0
        self._current_steering_deg = 0.0
        self._last_cmd_time = time.monotonic()
        self._lock = threading.Lock()
        # Latest Taj perception cap (m/s) and the monotonic time it arrived.
        self._perception_cap = None
        self._perception_cap_time = None

        # Publishers
        self._pub_safe = self.create_publisher(Twist, output_topic, 10)
        self._pub_action = self.create_publisher(String, enforcement_topic, 10)
        # The governed-frame relay to the verifying motor consumer. Reliable +
        # depth 1 (the output-side freshness discipline: a gated command must
        # not be silently dropped, but an OLDER queued frame must never reach
        # the consumer after a newer verdict — mirrors the ros2-adapter's
        # actuator_output_qos, node.rs). Logged-once when the first frame
        # flows / when a release is malformed.
        from rclpy.qos import QoSProfile, ReliabilityPolicy, HistoryPolicy
        self._pub_release = self.create_publisher(
            UInt8MultiArray,
            self.get_parameter('release_topic').value,
            QoSProfile(reliability=ReliabilityPolicy.RELIABLE,
                       history=HistoryPolicy.KEEP_LAST, depth=1),
        )
        self._release_relay_announced = False

        # Subscriber
        self._sub = self.create_subscription(
            Twist,
            input_topic,
            self._on_cmd_vel,
            10,
        )
        if self._use_perception_cap:
            self._sub_cap = self.create_subscription(
                Float64,
                self.get_parameter('perception_cap_topic').value,
                self._on_perception_cap,
                10,
            )

        if not REQUESTS_AVAILABLE:
            self.get_logger().error(
                'python3-requests not installed. HTTP enforcement disabled. '
                'All commands will be blocked (fail-closed).'
            )

        self.get_logger().info(
            f'Kirra cmd_vel interceptor started: '
            f'{input_topic} -> Kirra -> {output_topic} '
            f'(fallback={self._fallback}, timeout={self._timeout_s*1000:.0f}ms)'
        )

    def _headers(self):
        return {
            'Authorization': f'Bearer {self._kirra_token}',
            'x-kirra-client-id': self._client_id,
            'Content-Type': 'application/json',
        }

    def _twist_to_proposed_command(self, twist: Twist) -> dict:
        """
        Convert a ROS2 Twist message to a ProposedVehicleCommand for Kirra.

        For mecanum wheels, the primary safety-critical axis is linear.x (forward
        velocity). linear.y (lateral) is clamped independently. angular.z is
        converted to an equivalent steering angle using the bicycle model:
          steering_deg = atan2(angular.z * wheelbase_m, |linear.x|)
        """
        import math
        vx = twist.linear.x
        vy = twist.linear.y
        wz = twist.angular.z

        # Primary speed for kinematic contract: forward + lateral magnitude
        speed_mps = math.sqrt(vx ** 2 + vy ** 2)
        # Preserve direction sign from forward component
        if vx < 0:
            speed_mps = -speed_mps

        # Bicycle model approximation: convert angular rate to steering angle
        if abs(vx) > 0.01:
            steering_deg = math.degrees(math.atan2(wz * self._wheelbase_m, abs(vx)))
        else:
            # Near-zero forward velocity: treat rotation as in-place turn
            steering_deg = math.degrees(math.atan(wz * self._wheelbase_m)) if wz != 0 else 0.0

        with self._lock:
            current_v = self._current_velocity_mps
            current_s = self._current_steering_deg

        return {
            'linear_velocity_mps': speed_mps,
            'current_velocity_mps': current_v,
            'delta_time_s': 0.1,
            'steering_angle_deg': steering_deg,
            'current_steering_angle_deg': current_s,
        }

    def _on_perception_cap(self, msg: Float64):
        with self._lock:
            self._perception_cap = float(msg.data)
            self._perception_cap_time = time.monotonic()

    def _apply_perception_cap(self, proposed: dict) -> str:
        """Tighten the proposed forward speed to Taj's ACD cap, fail-closed. Returns a
        short reason suffix for the action log ('' when the derate is off)."""
        if not self._use_perception_cap:
            return ''
        with self._lock:
            cap = self._perception_cap
            cap_time = self._perception_cap_time
        cap_age = (time.monotonic() - cap_time) if cap_time is not None else None
        capped_v, reason = apply_perception_cap(
            proposed['linear_velocity_mps'], cap, cap_age,
            enabled=True, stale_s=self._cap_stale_s,
        )
        proposed['linear_velocity_mps'] = capped_v
        return '' if reason == DISABLED else f'|{reason}'

    def _on_cmd_vel(self, msg: Twist):
        if not REQUESTS_AVAILABLE:
            self._publish_stop('NO_REQUESTS_LIB')
            return

        # Track-A A3: a detected wheelbase mismatch is LATCHED — every
        # subsequent command stops until the config is fixed and the node
        # restarted. Motion under a wrong wheelbase would execute a yaw the
        # checker never approved (scaled by L_i/L_v).
        if self._wheelbase_mismatch:
            self._publish_stop('WHEELBASE_MISMATCH_LATCHED')
            return

        proposed = self._twist_to_proposed_command(msg)
        # Taj tightens the proposed speed BEFORE the governor (perception derate); the
        # governor then bounds whatever survives. Fail-closed on a stale/absent cap.
        cap_suffix = self._apply_perception_cap(proposed)

        try:
            resp = requests.post(
                f'{self._kirra_url}/actuator/motion/command',
                headers=self._headers(),
                json=proposed,
                timeout=self._timeout_s,
            )

            if resp.status_code == 200:
                # Parse defensively — a 200 with a non-JSON body is a fault.
                try:
                    parsed = resp.json()
                except ValueError:
                    parsed = None

                # Track-A A3 — single wheelbase source. When the verifier minted a
                # release, it reports the wheelbase its steering→angular conversion
                # used (the active class contract's L, the same L the P6 check ran
                # against). It must equal THIS node's Twist→steering wheelbase, or
                # executed yaw = commanded yaw × L_i/L_v — what Kirra approved is
                # not what the motors would do. Mismatch → stop + LATCH (fatal
                # config error, never a warn-and-continue). No release object (no
                # signer provisioned) → nothing to check; behavior unchanged.
                release = parsed.get('release') if isinstance(parsed, dict) else None
                if isinstance(release, dict):
                    reported_wb = release.get('wheelbase_m')
                    if not wheelbase_consistent(self._wheelbase_m, reported_wb):
                        self._wheelbase_mismatch = True
                        self._publish_stop('WHEELBASE_MISMATCH')
                        self._publish_action('BLOCKED:WHEELBASE_MISMATCH')
                        self.get_logger().fatal(
                            '\U0001f534 WHEELBASE MISMATCH: this node converts Twist→steering '
                            f'with wheelbase_m={self._wheelbase_m!r} but the verifier checked/'
                            f'minted with {reported_wb!r} (the active class contract). What '
                            'Kirra approves would not be what the motors execute. LATCHED to '
                            'stop: set this node\'s wheelbase_m to the class contract value '
                            'and restart.'
                        )
                        return

                # Pure, fail-closed decision (see enforcement_decision.py). A 200
                # is only ever Allow / ClampLinear / ClampSteering (denials are
                # 400, lockouts 403, handled below). The canonical keys are now
                # REQUIRED: a 200 missing them — or with a non-finite enforced
                # value — STOPS the robot. It is NEVER forwarded as the original
                # (unclamped) command; that removes the last fail-OPEN path.
                decision = decide_enforcement(200, parsed, proposed)
                if isinstance(decision, Forward):
                    safe_twist = self._build_safe_twist(
                        msg, decision.enforced_v, decision.enforced_s)
                    self._pub_safe.publish(safe_twist)
                    self._publish_action(f'{decision.action}:v={decision.enforced_v:.2f}{cap_suffix}')

                    # ADR-0033 live-loop relay: republish the verifier-minted
                    # signed frame (payload||token, 128 bytes) for the verifying
                    # motor consumer. ONLY on a Forward decision — a wheelbase
                    # mismatch or contract-violating 200 returned above and
                    # relays nothing. No/malformed release → nothing published;
                    # the consumer starves into its decel-to-zero (fail-closed).
                    self._relay_release(release)

                    with self._lock:
                        self._current_velocity_mps = decision.enforced_v
                        self._current_steering_deg = decision.enforced_s
                else:
                    # Contract-violating 200 → fail closed, like every other
                    # anomalous branch of this node.
                    self._publish_stop(decision.reason)
                    self._publish_action(f'BLOCKED:{decision.reason}')
                    self.get_logger().error(
                        'Kirra 200 missing canonical enforcement keys — stopping, '
                        f'fail-closed (reason={decision.reason})'
                    )

            elif resp.status_code in (403, 503):
                # Fleet locked out or Kirra down -- fail closed
                posture = resp.json().get('posture', 'unknown')
                self._publish_stop(f'POSTURE_{posture.upper()}')
                self._publish_action(f'BLOCKED:HTTP_{resp.status_code}')
                self.get_logger().warn(
                    f'Kirra blocked command: HTTP {resp.status_code} posture={posture}'
                )
            else:
                self._publish_stop(f'HTTP_{resp.status_code}')
                self.get_logger().error(f'Kirra returned unexpected status: {resp.status_code}')

        except requests.Timeout:
            self._handle_timeout(msg, proposed)
        except requests.ConnectionError:
            self._handle_connection_error(msg)
        except Exception as e:
            self.get_logger().error(f'Unexpected error in Kirra enforcement: {e}')
            self._publish_stop('UNEXPECTED_ERROR')

    def _handle_timeout(self, original_msg: Twist, proposed: dict):
        self.get_logger().warn(
            f'Kirra enforcement timeout ({self._timeout_s*1000:.0f}ms). '
            f'Fallback: {self._fallback}. '
            f'v_requested={proposed["linear_velocity_mps"]:.2f} m/s'
        )
        if self._fallback == 'passthrough':
            self._pub_safe.publish(original_msg)
            self._publish_action('TIMEOUT:PASSTHROUGH')
        else:
            self._publish_stop('TIMEOUT')
            self._publish_action('TIMEOUT:STOP')

    def _handle_connection_error(self, original_msg: Twist):
        self.get_logger().error(
            f'Cannot reach Kirra at {self._kirra_url}. '
            f'Fallback: {self._fallback}.'
        )
        if self._fallback == 'passthrough':
            self._pub_safe.publish(original_msg)
            self._publish_action('CONNECTION_ERROR:PASSTHROUGH')
        else:
            self._publish_stop('CONNECTION_ERROR')
            self._publish_action('CONNECTION_ERROR:STOP')

    def _relay_release(self, release):
        """Republish the gateway 200's release object as the 128-byte wire
        frame the verifying motor consumer parses (`split_frame`). Strictness
        lives in the pure `release_frame`; a None (absent OR malformed release)
        publishes nothing — starving the consumer into decel is the fail-closed
        outcome, never a guessed frame."""
        if release is None:
            return  # no signer provisioned upstream — nothing to relay
        frame = release_frame(release)
        if frame is None:
            self.get_logger().warn(
                'release object present but malformed — no frame relayed '
                '(consumer starves into decel-to-zero, fail-closed)'
            )
            return
        out = UInt8MultiArray()
        out.data = list(frame)
        self._pub_release.publish(out)
        if not self._release_relay_announced:
            self._release_relay_announced = True
            self.get_logger().info(
                f'release relay ACTIVE: signed frames flowing to '
                f'{self.get_parameter("release_topic").value} '
                f'(key_id={release.get("key_id", "?")!r})'
            )

    def _publish_stop(self, reason: str):
        self._pub_safe.publish(ZERO_TWIST)
        with self._lock:
            self._current_velocity_mps = 0.0

    def _publish_action(self, action: str):
        msg = String()
        msg.data = action
        self._pub_action.publish(msg)

    def _build_safe_twist(self, original: Twist, enforced_v: float, enforced_s_deg: float) -> Twist:
        """
        Reconstruct a safe Twist from the Kirra-enforced speed and steering.
        Preserves the original direction ratios for mecanum lateral motion.
        """
        import math
        safe = Twist()

        orig_speed = math.sqrt(original.linear.x ** 2 + original.linear.y ** 2)
        if orig_speed > 1e-6:
            ratio = abs(enforced_v) / orig_speed
            direction = 1.0 if enforced_v >= 0 else -1.0
            safe.linear.x = original.linear.x * ratio * direction
            safe.linear.y = original.linear.y * ratio * direction
        else:
            safe.linear.x = 0.0
            safe.linear.y = 0.0

        # Convert enforced steering angle back to angular rate
        if abs(safe.linear.x) > 0.01:
            safe.angular.z = (math.tan(math.radians(enforced_s_deg)) * abs(safe.linear.x)) / self._wheelbase_m
        else:
            safe.angular.z = original.angular.z

        return safe


def main(args=None):
    rclpy.init(args=args)
    node = CmdVelInterceptor()
    try:
        rclpy.spin(node)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        rclpy.try_shutdown()


if __name__ == '__main__':
    main()
