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
from std_msgs.msg import String

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
        self.declare_parameter('wheelbase_m', 0.2)
        self.declare_parameter('max_speed_mps', 1.8)
        self.declare_parameter('input_topic', '/cmd_vel')
        self.declare_parameter('output_topic', '/cmd_vel_safe')
        self.declare_parameter('enforcement_topic', '/kirra/enforcement_action')
        self.declare_parameter('posture_topic', '/kirra/fleet_posture')
        self.declare_parameter('timeout_ms', 50)
        self.declare_parameter('fallback_on_timeout', 'stop')

        self._kirra_url = self.get_parameter('kirra_url').value
        self._kirra_token = self.get_parameter('kirra_token').value
        self._client_id = self.get_parameter('kirra_client_id').value
        self._wheelbase_m = self.get_parameter('wheelbase_m').value
        self._max_speed_mps = self.get_parameter('max_speed_mps').value
        self._timeout_s = self.get_parameter('timeout_ms').value / 1000.0
        self._fallback = self.get_parameter('fallback_on_timeout').value

        input_topic = self.get_parameter('input_topic').value
        output_topic = self.get_parameter('output_topic').value
        enforcement_topic = self.get_parameter('enforcement_topic').value

        # State
        self._current_velocity_mps = 0.0
        self._current_steering_deg = 0.0
        self._last_cmd_time = time.monotonic()
        self._lock = threading.Lock()

        # Publishers
        self._pub_safe = self.create_publisher(Twist, output_topic, 10)
        self._pub_action = self.create_publisher(String, enforcement_topic, 10)

        # Subscriber
        self._sub = self.create_subscription(
            Twist,
            input_topic,
            self._on_cmd_vel,
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

    def _on_cmd_vel(self, msg: Twist):
        if not REQUESTS_AVAILABLE:
            self._publish_stop('NO_REQUESTS_LIB')
            return

        proposed = self._twist_to_proposed_command(msg)

        try:
            resp = requests.post(
                f'{self._kirra_url}/actuator/motion/command',
                headers=self._headers(),
                json=proposed,
                timeout=self._timeout_s,
            )

            if resp.status_code == 200:
                result = resp.json()
                action = result.get('action', 'Allow')
                enforced_v = result.get('enforced_linear_velocity_mps', proposed['linear_velocity_mps'])
                enforced_s = result.get('enforced_steering_angle_deg', proposed['steering_angle_deg'])

                if action == 'DenyBreach':
                    reason = result.get('reason', 'UNKNOWN')
                    self._publish_stop(reason)
                    self._publish_action(f'DENIED:{reason}')
                    self.get_logger().warn(
                        f'Kirra denied cmd_vel: {reason} '
                        f'(requested v={proposed["linear_velocity_mps"]:.2f} m/s)'
                    )
                else:
                    safe_twist = self._build_safe_twist(msg, enforced_v, enforced_s)
                    self._pub_safe.publish(safe_twist)
                    self._publish_action(f'{action}:v={enforced_v:.2f}')

                    with self._lock:
                        self._current_velocity_mps = enforced_v
                        self._current_steering_deg = enforced_s

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
