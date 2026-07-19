#!/usr/bin/env python3
"""autoware_stub_publisher.py — a stand-in Autoware DOER for the isolation
scaffold (ADR-0036).

Publishes minimal, structurally-valid messages on the 5 curated boundary topics
so the KIRRA checker+adapter (Jazzy) can be validated **end-to-end without a real
Autoware** — proving the wire (topic discovery, type match, message flow across
the Humble↔Jazzy DDS boundary) independent of whether the Autoware doer is fully
built out. It is NOT Autoware: it drives nothing meaningful, it exercises the
seam.

Boundary (subscribed by kirra-ros2-adapter/src/node.rs), published here:
  <prefix>/trajectory         autoware_planning_msgs/Trajectory
  <prefix>/objects            autoware_perception_msgs/PredictedObjects
  <prefix>/objects_secondary  autoware_perception_msgs/PredictedObjects  (channel-B)
  <prefix>/map                autoware_map_msgs/LaneletMapBin
  <prefix>/odometry           nav_msgs/Odometry
It also *subscribes* the governed control topic to show the checker's bounded
output comes back (logs on receipt).

Robust by construction: each message type is imported LAZILY; a type that isn't
built (or whose field names differ on a distro) degrades to a logged warning and
that one topic is skipped — the rest keep flowing. Empty-but-valid payloads are
the point (wire proof), not behaviour.

Env:
  KIRRA_BOUNDARY_PREFIX   topic prefix (default /input) — remap to your adapter ns
  KIRRA_CONTROL_TOPIC     governed control topic to listen on (default /output/control_cmd)
  KIRRA_STUB_HZ           publish rate (default 10)
Run (inside a Jazzy/Humble container with the curated msgs built + sourced):
  python3 autoware_stub_publisher.py
"""
import os
import sys

try:
    import rclpy
    from rclpy.node import Node
except Exception as e:  # noqa: BLE001
    sys.exit(f"autoware_stub_publisher: rclpy unavailable ({e}). Source a ROS 2 setup.bash first.")

PREFIX = os.environ.get("KIRRA_BOUNDARY_PREFIX", "/input").rstrip("/")
CONTROL_TOPIC = os.environ.get("KIRRA_CONTROL_TOPIC", "/output/control_cmd")
HZ = float(os.environ.get("KIRRA_STUB_HZ", "10"))


def _try_import(modpath, name):
    """Import a message class, or return None with a warning (never crash)."""
    try:
        mod = __import__(modpath, fromlist=[name])
        return getattr(mod, name)
    except Exception as e:  # noqa: BLE001
        print(f"  [skip] {modpath}.{name} unavailable ({type(e).__name__}) — topic disabled", file=sys.stderr)
        return None


class AutowareStub(Node):
    def __init__(self):
        super().__init__("autoware_stub_publisher")
        # Lazy type imports — each independent.
        self.Trajectory = _try_import("autoware_planning_msgs.msg", "Trajectory")
        self.PredictedObjects = _try_import("autoware_perception_msgs.msg", "PredictedObjects")
        self.LaneletMapBin = _try_import("autoware_map_msgs.msg", "LaneletMapBin")
        self.Odometry = _try_import("nav_msgs.msg", "Odometry")
        self.Control = _try_import("autoware_control_msgs.msg", "Control")

        self.pubs = {}
        if self.Trajectory:
            self.pubs["trajectory"] = self.create_publisher(self.Trajectory, f"{PREFIX}/trajectory", 1)
        if self.PredictedObjects:
            self.pubs["objects"] = self.create_publisher(self.PredictedObjects, f"{PREFIX}/objects", 1)
            self.pubs["objects_secondary"] = self.create_publisher(
                self.PredictedObjects, f"{PREFIX}/objects_secondary", 1)
        if self.LaneletMapBin:
            self.pubs["map"] = self.create_publisher(self.LaneletMapBin, f"{PREFIX}/map", 1)
        if self.Odometry:
            self.pubs["odometry"] = self.create_publisher(self.Odometry, f"{PREFIX}/odometry", 1)

        # Round-trip: hear the checker's governed output come back.
        if self.Control:
            self.create_subscription(self.Control, CONTROL_TOPIC, self._on_control, 1)

        self._control_seen = 0
        self._ticks = 0
        self.create_timer(1.0 / max(HZ, 0.1), self._tick)
        self.get_logger().info(
            f"stub doer up — publishing {sorted(self.pubs)} on '{PREFIX}/*' at {HZ} Hz; "
            f"listening for governed control on '{CONTROL_TOPIC}'")

    def _stamp(self, msg):
        """Set header.stamp/frame_id when the message has a header."""
        try:
            msg.header.stamp = self.get_clock().now().to_msg()
            msg.header.frame_id = "map"
        except Exception:  # noqa: BLE001 — not all msgs have a header
            pass
        return msg

    def _tick(self):
        self._ticks += 1
        for key, pub in self.pubs.items():
            try:
                cls = pub.msg_type
                pub.publish(self._stamp(cls()))   # empty-but-valid: exercises the wire
            except Exception as e:  # noqa: BLE001
                self.get_logger().warn(f"publish {key} failed ({type(e).__name__}: {e})")
        if self._ticks % int(max(HZ, 1)) == 0:  # ~once/sec
            self.get_logger().info(
                f"heartbeat: published {sorted(self.pubs)}; governed-control msgs seen={self._control_seen}")

    def _on_control(self, _msg):
        self._control_seen += 1
        if self._control_seen == 1:
            self.get_logger().info("✓ received a governed control_cmd back from the checker — boundary round-trips")


def main():
    rclpy.init()
    node = AutowareStub()
    if not node.pubs:
        node.get_logger().error("no boundary types available — build the curated autoware_*_msgs first")
    try:
        rclpy.spin(node)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        try:
            rclpy.shutdown()
        except Exception:  # noqa: BLE001
            pass


if __name__ == "__main__":
    main()
