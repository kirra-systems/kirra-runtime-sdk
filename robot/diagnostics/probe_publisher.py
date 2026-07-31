#!/usr/bin/env python3
"""Matching publisher for `ros_callback_matrix.py`.

Publishes a 3-byte UInt8MultiArray on /kirra/probe at 10 Hz with the same QoS
the production consumer subscribes with (reliable, volatile, depth 10).

SAFETY
    The payload is 3 bytes, deliberately NOT the 272-byte governed frame, and it
    carries no release token. It cannot pass the verify core and therefore cannot
    reach actuation — it exercises DDS delivery only.

USAGE
    source /opt/kirra/robot/ros_env.sh && kirra_source_ros
    python3 robot/diagnostics/probe_publisher.py
"""

from __future__ import annotations

import argparse
import sys

PROBE_TOPIC = "/kirra/probe"
PROBE_PAYLOAD = [9, 8, 7]  # 3 bytes — never 272


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--hz", type=float, default=10.0)
    parser.add_argument("--topic", default=PROBE_TOPIC)
    args = parser.parse_args(argv)

    import rclpy
    from rclpy.node import Node
    from std_msgs.msg import UInt8MultiArray

    rclpy.init()
    node = Node("kirra_probe_publisher")
    pub = node.create_publisher(UInt8MultiArray, args.topic, 10)
    sent = 0

    def tick() -> None:
        nonlocal sent
        msg = UInt8MultiArray()
        msg.data = PROBE_PAYLOAD
        pub.publish(msg)
        sent += 1
        if sent % int(max(1.0, args.hz)) == 0:
            print(f"published {sent} probe frames ({len(PROBE_PAYLOAD)} bytes)", flush=True)

    node.create_timer(1.0 / args.hz, tick)
    print(f"publishing {len(PROBE_PAYLOAD)}-byte probes on {args.topic} @ {args.hz:.0f} Hz",
          flush=True)
    try:
        rclpy.spin(node)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        if rclpy.ok():
            rclpy.shutdown()
    return 0


if __name__ == "__main__":
    sys.exit(main())
