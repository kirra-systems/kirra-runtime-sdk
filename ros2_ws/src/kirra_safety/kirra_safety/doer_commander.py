#!/usr/bin/env python3
"""
Kirra demo "doer" — the untrusted proposer.

Publishes a constant forward Twist on /cmd_vel_raw (the planner's PROPOSAL). It is
deliberately naive: it keeps commanding the robot forward into the wall and never brakes.
KIRRA (the governor) plus Taj (the corridor speed cap) are what stop the robot — the doer
is never trusted to. This is the doer-checker thesis made watchable in Gazebo.

Topic flow:
  doer_commander → /cmd_vel_raw → cmd_vel_interceptor [Taj cap + KIRRA] → /cmd_vel → robot
"""

import rclpy
from rclpy.node import Node
from geometry_msgs.msg import Twist


class DoerCommander(Node):
    def __init__(self):
        super().__init__('doer_commander')
        self.declare_parameter('cmd_topic', '/cmd_vel_raw')
        self.declare_parameter('forward_speed_mps', 1.2)  # naive constant forward command
        self.declare_parameter('publish_hz', 10.0)

        self._speed = self.get_parameter('forward_speed_mps').value
        topic = self.get_parameter('cmd_topic').value
        hz = self.get_parameter('publish_hz').value

        self._pub = self.create_publisher(Twist, topic, 10)
        self.create_timer(1.0 / hz, self._tick)
        self.get_logger().info(
            f'doer_commander: proposing {self._speed:.2f} m/s forward on {topic} '
            f'(never brakes — KIRRA + Taj stop the robot)'
        )

    def _tick(self):
        msg = Twist()
        msg.linear.x = float(self._speed)
        self._pub.publish(msg)


def main(args=None):
    rclpy.init(args=args)
    node = DoerCommander()
    try:
        rclpy.spin(node)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        rclpy.try_shutdown()


if __name__ == '__main__':
    main()
