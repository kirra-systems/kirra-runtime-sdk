#!/usr/bin/env python3
"""
Kirra Perception Governor (Taj corridor -> cmd_vel speed cap)

Subscribes to the robot's lidar (/scan), forwards each scan to the Taj perception
service (a thin HTTP wrapper over the real `kirra-taj` geometric corridor), and
publishes the resulting **assured-clear-distance (ACD) speed cap** on
/kirra/perception_speed_cap. The cmd_vel interceptor applies that cap to the doer's
proposed speed BEFORE the KIRRA governor — Taj tightens the envelope, KIRRA bounds.

Topic flow:
  /scan (sensor_msgs/LaserScan) [FROM lidar]
      |
  Taj service  POST /perception (HTTP)   -> corridor health + ACD speed cap
      |
  /kirra/perception_speed_cap (std_msgs/Float64) [TO cmd_vel interceptor]
  /kirra/perception_health    (std_msgs/String)  [FOR monitoring]

Fail-closed: a Taj-service error/timeout, or an unhealthy corridor, publishes a 0.0
cap (the MRC floor). The interceptor additionally fails closed if this topic goes
stale (see perception_cap.apply_perception_cap), so a crashed governor also stops the
robot rather than freezing the last cap.
"""

import rclpy
from rclpy.node import Node
from rclpy.qos import QoSProfile, ReliabilityPolicy, HistoryPolicy
from sensor_msgs.msg import LaserScan
from std_msgs.msg import Float64, String

# Lidar-ingress QoS (hardware finding: the TG30 driver publishes /scan
# BEST_EFFORT; a default RELIABLE subscription silently matches ZERO messages —
# the cap topic then goes stale and the interceptor fail-closes, but the cause
# is invisible). BestEffort + KeepLast(1) mirrors the house sensor-ingress
# discipline (kirra-ros2-adapter ingress_sensor_qos); depth 1 also stops a
# post-stall backlog of stale scans from queueing Taj POSTs.
SCAN_QOS = QoSProfile(
    reliability=ReliabilityPolicy.BEST_EFFORT,
    history=HistoryPolicy.KEEP_LAST,
    depth=1,
)

try:
    import requests
    REQUESTS_AVAILABLE = True
except ImportError:
    REQUESTS_AVAILABLE = False


class PerceptionGovernor(Node):
    def __init__(self):
        super().__init__('perception_governor')

        self.declare_parameter('taj_url', 'http://localhost:8101')
        self.declare_parameter('scan_topic', '/scan')
        self.declare_parameter('cap_topic', '/kirra/perception_speed_cap')
        self.declare_parameter('health_topic', '/kirra/perception_health')
        self.declare_parameter('timeout_ms', 40)
        # ACD parameters (forwarded to the Taj service; conservative R2 defaults).
        self.declare_parameter('forward_extent_m', 8.0)   # Taj geometric horizon
        self.declare_parameter('decel_mps2', 1.5)         # assumed comfortable brake
        self.declare_parameter('margin_m', 0.4)           # standoff (matches the checker margin)
        self.declare_parameter('lane_half_m', 0.6)        # in-lane half-width for object gating
        self.declare_parameter('confidence_floor', 0.5)   # below this → unhealthy → 0.0 cap

        self._taj_url = self.get_parameter('taj_url').value
        self._timeout_s = self.get_parameter('timeout_ms').value / 1000.0
        self._extent = self.get_parameter('forward_extent_m').value
        self._decel = self.get_parameter('decel_mps2').value
        self._margin = self.get_parameter('margin_m').value
        self._lane_half = self.get_parameter('lane_half_m').value
        self._floor = self.get_parameter('confidence_floor').value

        scan_topic = self.get_parameter('scan_topic').value
        self._pub_cap = self.create_publisher(Float64, self.get_parameter('cap_topic').value, 10)
        self._pub_health = self.create_publisher(String, self.get_parameter('health_topic').value, 10)
        self.create_subscription(LaserScan, scan_topic, self._on_scan, SCAN_QOS)

        if not REQUESTS_AVAILABLE:
            self.get_logger().error(
                'python3-requests not installed. Perception cap disabled — publishing 0.0 '
                '(fail-closed): the interceptor will hold until Taj caps arrive.'
            )

        self.get_logger().info(
            f'Kirra perception governor started: {scan_topic} -> Taj({self._taj_url}) '
            f'-> {self.get_parameter("cap_topic").value}'
        )

    def _publish(self, cap_mps: float, health: str):
        self._pub_cap.publish(Float64(data=float(cap_mps)))
        self._pub_health.publish(String(data=health))

    def _on_scan(self, msg: LaserScan):
        if not REQUESTS_AVAILABLE:
            self._publish(0.0, 'NO_REQUESTS_LIB')
            return

        # ROS2 LaserScan stamp -> ms; the Taj service is stateless on time (we process at
        # the scan stamp; wall-clock staleness is enforced downstream on the cap topic).
        stamp = msg.header.stamp
        stamp_ms = int(stamp.sec * 1000 + stamp.nanosec / 1_000_000)
        body = {
            'angle_min_rad': float(msg.angle_min),
            'angle_increment_rad': float(msg.angle_increment),
            'range_min_m': float(msg.range_min),
            'range_max_m': float(msg.range_max),
            'ranges': [float(r) for r in msg.ranges],
            'stamp_ms': stamp_ms,
            'forward_extent_m': self._extent,
            'decel_mps2': self._decel,
            'margin_m': self._margin,
            'lane_half_m': self._lane_half,
            'confidence_floor': self._floor,
        }
        try:
            resp = requests.post(
                f'{self._taj_url}/perception', json=body, timeout=self._timeout_s)
            if resp.status_code != 200:
                self._publish(0.0, f'TAJ_HTTP_{resp.status_code}')
                return
            data = resp.json()
            cap = data.get('speed_cap_mps')
            if cap is None or not isinstance(cap, (int, float)):
                self._publish(0.0, 'TAJ_MALFORMED')
                return
            healthy = bool(data.get('healthy', False))
            clear = data.get('clear_distance_m', float('nan'))
            self._publish(
                cap,
                f'{"OK" if healthy else "UNHEALTHY"}:clear={clear:.1f}m:cap={cap:.2f}',
            )
        except requests.Timeout:
            self._publish(0.0, 'TAJ_TIMEOUT')
        except requests.ConnectionError:
            self._publish(0.0, 'TAJ_UNREACHABLE')
        except Exception as e:  # noqa: BLE001 - fail closed on any fault
            self.get_logger().error(f'perception governor error: {e}')
            self._publish(0.0, 'TAJ_ERROR')


def main(args=None):
    rclpy.init(args=args)
    node = PerceptionGovernor()
    try:
        rclpy.spin(node)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        rclpy.try_shutdown()


if __name__ == '__main__':
    main()
