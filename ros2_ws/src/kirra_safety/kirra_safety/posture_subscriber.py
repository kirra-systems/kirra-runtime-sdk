#!/usr/bin/env python3
"""
Kirra Posture SSE Bridge

Connects to the Kirra SSE posture stream and republishes posture transitions
as ROS2 topics. Also emits an emergency stop when posture transitions to LockedOut.

SSE source: GET /system/posture/stream
ROS2 outputs:
  /kirra/fleet_posture   (std_msgs/String) — "Nominal" | "Degraded" | "LockedOut"
  /kirra/posture_events  (std_msgs/String) — JSON event stream
"""

import json
import threading
import time

import rclpy
from rclpy.node import Node
from std_msgs.msg import String
from geometry_msgs.msg import Twist

try:
    import requests
    REQUESTS_AVAILABLE = True
except ImportError:
    REQUESTS_AVAILABLE = False

ZERO_TWIST = Twist()
RECONNECT_DELAY_S = 5.0


class PostureSubscriber(Node):
    def __init__(self):
        super().__init__('posture_subscriber')

        self.declare_parameter('kirra_url', 'http://localhost:8090')
        self.declare_parameter('kirra_token', '')
        self.declare_parameter('kirra_client_id', 'ros2-posture-bridge-01')
        self.declare_parameter('cmd_vel_safe_topic', '/cmd_vel_safe')

        self._kirra_url = self.get_parameter('kirra_url').value
        self._kirra_token = self.get_parameter('kirra_token').value
        self._client_id = self.get_parameter('kirra_client_id').value
        cmd_vel_safe = self.get_parameter('cmd_vel_safe_topic').value

        self._pub_posture = self.create_publisher(String, '/kirra/fleet_posture', 10)
        self._pub_events = self.create_publisher(String, '/kirra/posture_events', 10)
        self._pub_estop = self.create_publisher(Twist, cmd_vel_safe, 10)

        self._current_posture = 'Unknown'
        self._running = True

        if not REQUESTS_AVAILABLE:
            self.get_logger().error('python3-requests not installed. SSE bridge disabled.')
        else:
            self._sse_thread = threading.Thread(
                target=self._sse_loop,
                daemon=True,
                name='kirra_sse_bridge',
            )
            self._sse_thread.start()

        # Publish current posture on a timer as a heartbeat
        self.create_timer(1.0, self._publish_posture_heartbeat)

        self.get_logger().info(
            f'Kirra posture bridge started (SSE: {self._kirra_url}/system/posture/stream)'
        )

    def _headers(self):
        return {
            'Authorization': f'Bearer {self._kirra_token}',
            'x-kirra-client-id': self._client_id,
            'Accept': 'text/event-stream',
            'Cache-Control': 'no-cache',
        }

    def _sse_loop(self):
        """Long-running SSE reader thread. Reconnects automatically on failure."""
        url = f'{self._kirra_url}/system/posture/stream'
        while self._running:
            try:
                self.get_logger().info(f'Connecting to Kirra SSE stream: {url}')
                with requests.get(url, headers=self._headers(), stream=True, timeout=30) as resp:
                    if resp.status_code != 200:
                        self.get_logger().error(
                            f'SSE stream returned {resp.status_code}. Retry in {RECONNECT_DELAY_S}s.'
                        )
                        time.sleep(RECONNECT_DELAY_S)
                        continue

                    self.get_logger().info('Connected to Kirra SSE stream.')
                    buffer = ''
                    for chunk in resp.iter_content(chunk_size=None, decode_unicode=True):
                        if not self._running:
                            break
                        buffer += chunk
                        while '\n\n' in buffer:
                            event_block, buffer = buffer.split('\n\n', 1)
                            self._process_sse_block(event_block)

            except requests.Timeout:
                self.get_logger().warn(f'SSE connection timed out. Reconnecting in {RECONNECT_DELAY_S}s.')
            except requests.ConnectionError:
                self.get_logger().error(
                    f'Cannot reach Kirra SSE stream at {url}. '
                    f'Retry in {RECONNECT_DELAY_S}s.'
                )
            except Exception as e:
                self.get_logger().error(f'SSE error: {e}. Reconnecting in {RECONNECT_DELAY_S}s.')

            if self._running:
                time.sleep(RECONNECT_DELAY_S)

    def _process_sse_block(self, block: str):
        """Parse an SSE event block and dispatch to ROS2 topics."""
        data_lines = [
            line[len('data:'):].strip()
            for line in block.splitlines()
            if line.startswith('data:')
        ]
        if not data_lines:
            return

        raw = '\n'.join(data_lines)

        # Publish raw event JSON
        event_msg = String()
        event_msg.data = raw
        self._pub_events.publish(event_msg)

        # Parse posture
        try:
            event = json.loads(raw)
            posture = event.get('posture') or event.get('fleet_posture', '')
            if posture in ('Nominal', 'Degraded', 'LockedOut'):
                self._on_posture_change(posture, event)
        except (json.JSONDecodeError, AttributeError):
            pass

    def _on_posture_change(self, new_posture: str, event: dict):
        old_posture = self._current_posture
        self._current_posture = new_posture

        posture_msg = String()
        posture_msg.data = new_posture
        self._pub_posture.publish(posture_msg)

        if new_posture == 'LockedOut' and old_posture != 'LockedOut':
            # Emergency stop: publish zero velocity immediately
            self._pub_estop.publish(ZERO_TWIST)
            self.get_logger().error(
                f'FLEET LOCKED OUT -- emergency stop issued. '
                f'Reason: {event.get("reason", "unknown")}. '
                f'Previous posture: {old_posture}.'
            )
        elif new_posture != old_posture:
            self.get_logger().warn(
                f'Fleet posture transition: {old_posture} -> {new_posture}'
            )

    def _publish_posture_heartbeat(self):
        msg = String()
        msg.data = self._current_posture
        self._pub_posture.publish(msg)

    def destroy_node(self):
        self._running = False
        super().destroy_node()


def main(args=None):
    rclpy.init(args=args)
    node = PostureSubscriber()
    try:
        rclpy.spin(node)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        rclpy.try_shutdown()


if __name__ == '__main__':
    main()
