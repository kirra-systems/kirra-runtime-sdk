#!/usr/bin/env python3
"""
Kirra Sensor Health Monitor

Subscribes to robot sensor topics and posts health reports to Kirra.
Maps ROS2 sensor data freshness and quality to Kirra fleet node confidence scores.

Reports to: POST /fleet/diagnostics/report
"""

import time
import threading
import json

import rclpy
from rclpy.node import Node
from sensor_msgs.msg import LaserScan, Imu, Image
from nav_msgs.msg import Odometry
from diagnostic_msgs.msg import DiagnosticArray
from std_msgs.msg import String

try:
    import requests
    REQUESTS_AVAILABLE = True
except ImportError:
    REQUESTS_AVAILABLE = False


class SensorState:
    def __init__(self, node_id: str, sensor_type: str):
        self.node_id = node_id
        self.sensor_type = sensor_type
        self.last_msg_time: float = 0.0
        self.message_count: int = 0
        self.window_start: float = time.monotonic()
        self.hz_estimate: float = 0.0
        self.last_covariance: float = 0.0
        self.hardware_fault: bool = False
        self.lock = threading.Lock()

    def record_message(self):
        now = time.monotonic()
        with self.lock:
            self.message_count += 1
            elapsed = now - self.window_start
            if elapsed >= 1.0:
                self.hz_estimate = self.message_count / elapsed
                self.message_count = 0
                self.window_start = now
            self.last_msg_time = now

    def staleness_s(self) -> float:
        return time.monotonic() - self.last_msg_time if self.last_msg_time > 0 else float('inf')


class SensorMonitor(Node):
    def __init__(self):
        super().__init__('sensor_monitor')

        # Parameters
        self.declare_parameter('kirra_url', 'http://localhost:8090')
        self.declare_parameter('kirra_token', '')
        self.declare_parameter('kirra_client_id', 'ros2-sensor-monitor-01')
        self.declare_parameter('report_interval_ms', 200)
        self.declare_parameter('lidar_min_hz', 5.0)
        self.declare_parameter('imu_covariance_threshold', 0.1)
        self.declare_parameter('odometry_stale_ms', 500)

        self._kirra_url = self.get_parameter('kirra_url').value
        self._kirra_token = self.get_parameter('kirra_token').value
        self._client_id = self.get_parameter('kirra_client_id').value
        self._report_interval_s = self.get_parameter('report_interval_ms').value / 1000.0
        self._lidar_min_hz = self.get_parameter('lidar_min_hz').value
        self._imu_cov_threshold = self.get_parameter('imu_covariance_threshold').value
        self._odom_stale_s = self.get_parameter('odometry_stale_ms').value / 1000.0

        # Sensor states
        self._states = {
            'lidar_front': SensorState('lidar_front', 'lidar'),
            'depth_camera': SensorState('depth_camera', 'camera'),
            'imu_primary': SensorState('imu_primary', 'imu'),
            'wheel_encoders': SensorState('wheel_encoders', 'odometry'),
        }

        # Subscriptions
        self.create_subscription(LaserScan, '/scan', self._on_scan, 10)
        self.create_subscription(Image, '/camera/depth/image_raw', self._on_depth, 10)
        self.create_subscription(Imu, '/imu/data', self._on_imu, 10)
        self.create_subscription(Odometry, '/odom', self._on_odom, 10)
        self.create_subscription(DiagnosticArray, '/diagnostics', self._on_diagnostics, 10)

        # Reporting timer
        self.create_timer(self._report_interval_s, self._report_all)

        if not REQUESTS_AVAILABLE:
            self.get_logger().error('python3-requests not installed. Health reporting disabled.')

        self.get_logger().info(
            f'Kirra sensor monitor started (report_interval={self._report_interval_s*1000:.0f}ms)'
        )

    def _headers(self):
        return {
            'Authorization': f'Bearer {self._kirra_token}',
            'Content-Type': 'application/json',
        }

    # --- Sensor callbacks ---------------------------------------------------

    def _on_scan(self, msg: LaserScan):
        self._states['lidar_front'].record_message()

    def _on_depth(self, msg: Image):
        self._states['depth_camera'].record_message()

    def _on_imu(self, msg: Imu):
        state = self._states['imu_primary']
        state.record_message()
        # Store diagonal covariance mean for quality scoring
        cov = msg.angular_velocity_covariance
        diag = (cov[0] + cov[4] + cov[8]) / 3.0 if len(cov) >= 9 else 0.0
        with state.lock:
            state.last_covariance = diag

    def _on_odom(self, msg: Odometry):
        self._states['wheel_encoders'].record_message()

    def _on_diagnostics(self, msg: DiagnosticArray):
        for status in msg.status:
            # Map ROS2 diagnostic level to hardware_fault flag
            # DiagnosticStatus.ERROR = 2, WARN = 1, OK = 0
            if status.level >= 2:
                # Try to find the matching sensor
                for node_id, state in self._states.items():
                    if node_id in status.name.lower() or state.sensor_type in status.name.lower():
                        with state.lock:
                            state.hardware_fault = True

    # --- Confidence scoring -------------------------------------------------

    def _lidar_confidence(self, state: SensorState) -> float:
        """1.0 if >lidar_min_hz, degrades linearly to 0 at 0 Hz."""
        if state.last_msg_time == 0:
            return 0.0
        if state.staleness_s() > 2.0:
            return 0.0
        hz = state.hz_estimate
        return min(1.0, hz / self._lidar_min_hz)

    def _camera_confidence(self, state: SensorState) -> float:
        """1.0 if receiving frames, 0.5 if at reduced rate, 0.0 if stale."""
        if state.last_msg_time == 0 or state.staleness_s() > 2.0:
            return 0.0
        if state.hz_estimate >= 10.0:
            return 1.0
        if state.hz_estimate >= 5.0:
            return 0.5
        return 0.2

    def _imu_confidence(self, state: SensorState) -> float:
        """1.0 if covariance below threshold and fresh, 0.5 if covariance high."""
        if state.last_msg_time == 0 or state.staleness_s() > 0.5:
            return 0.0
        with state.lock:
            cov = state.last_covariance
        if cov < self._imu_cov_threshold:
            return 1.0
        return 0.5

    def _odometry_confidence(self, state: SensorState) -> float:
        """1.0 if fresh, 0.0 if stale >odometry_stale_ms."""
        if state.last_msg_time == 0:
            return 0.0
        return 1.0 if state.staleness_s() < self._odom_stale_s else 0.0

    def _compute_confidence(self, state: SensorState) -> float:
        if state.sensor_type == 'lidar':
            return self._lidar_confidence(state)
        if state.sensor_type == 'camera':
            return self._camera_confidence(state)
        if state.sensor_type == 'imu':
            return self._imu_confidence(state)
        if state.sensor_type == 'odometry':
            return self._odometry_confidence(state)
        return 0.0

    # --- Reporting ----------------------------------------------------------

    def _report_all(self):
        if not REQUESTS_AVAILABLE:
            return
        now_ms = int(time.time() * 1000)
        for node_id, state in self._states.items():
            confidence = self._compute_confidence(state)
            with state.lock:
                hw_fault = state.hardware_fault
            self._post_report(node_id, confidence, hw_fault, now_ms)

    def _post_report(self, node_id: str, confidence: float, hardware_fault: bool, timestamp_ms: int):
        payload = {
            'node_id': node_id,
            'confidence_score': round(confidence, 4),
            'hardware_fault': hardware_fault,
            'timestamp_ms': timestamp_ms,
        }
        try:
            resp = requests.post(
                f'{self._kirra_url}/fleet/diagnostics/report',
                headers=self._headers(),
                json=payload,
                timeout=0.5,
            )
            if resp.status_code not in (200, 201):
                self.get_logger().debug(
                    f'Kirra diagnostics report for {node_id} returned {resp.status_code}'
                )
        except requests.Timeout:
            self.get_logger().debug(f'Kirra diagnostics timeout for {node_id}')
        except requests.ConnectionError:
            pass  # Kirra unreachable -- silently skip, will retry next interval
        except Exception as e:
            self.get_logger().error(f'Unexpected error posting diagnostics: {e}')


def main(args=None):
    rclpy.init(args=args)
    node = SensorMonitor()
    try:
        rclpy.spin(node)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        rclpy.try_shutdown()


if __name__ == '__main__':
    main()
