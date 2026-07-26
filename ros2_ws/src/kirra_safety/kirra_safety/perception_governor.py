#!/usr/bin/env python3
"""
Kirra Perception Governor.

Receives the latest lidar scan, requests a fused safety envelope from Taj, and
publishes an asymmetrically stabilized speed cap.

Safety behavior:
- Restrictive cap changes apply immediately.
- Permissive changes require consecutive confirmation and rate-limited release.
- Taj errors, unhealthy perception, malformed responses, and timing faults
  publish an immediate zero cap and reset release history.
- The downstream interceptor independently stops if this topic becomes stale.
"""

import math
import time

import rclpy
from rclpy.node import Node
from rclpy.qos import HistoryPolicy, QoSProfile, ReliabilityPolicy
from sensor_msgs.msg import LaserScan
from std_msgs.msg import Float64, String

from kirra_safety.perception_cap import (
    SpeedCapGovernor,
    SpeedCapGovernorConfig,
)

SCAN_QOS = QoSProfile(
    reliability=ReliabilityPolicy.BEST_EFFORT,
    history=HistoryPolicy.KEEP_LAST,
    depth=1,
)

try:
    import requests

    REQUESTS_AVAILABLE = True
except ImportError:
    requests = None
    REQUESTS_AVAILABLE = False


class PerceptionGovernor(Node):
    def __init__(self):
        super().__init__("perception_governor")

        self.declare_parameter("taj_url", "http://localhost:8101")
        self.declare_parameter("scan_topic", "/scan")
        self.declare_parameter(
            "cap_topic",
            "/kirra/perception_speed_cap",
        )
        self.declare_parameter(
            "health_topic",
            "/kirra/perception_health",
        )
        self.declare_parameter("timeout_ms", 40)

        # Taj request and R2 platform geometry.
        self.declare_parameter("forward_extent_m", 8.0)
        self.declare_parameter("decel_mps2", 1.5)
        self.declare_parameter("margin_m", 0.4)
        self.declare_parameter("lane_half_m", 0.26)
        self.declare_parameter("vehicle_width_m", 0.203)
        self.declare_parameter("lateral_clearance_m", 0.15)
        self.declare_parameter("confidence_floor", 0.5)

        # Asymmetric cap-release policy.
        self.declare_parameter(
            "cap_rise_rate_mps_per_s",
            0.50,
        )
        self.declare_parameter(
            "cap_clear_confirmations",
            5,
        )
        self.declare_parameter(
            "cap_maximum_dt_ms",
            250,
        )

        self._taj_url = str(
            self.get_parameter("taj_url").value
        ).rstrip("/")
        self._timeout_s = (
            float(self.get_parameter("timeout_ms").value)
            / 1000.0
        )

        self._extent = float(
            self.get_parameter("forward_extent_m").value
        )
        self._decel = float(
            self.get_parameter("decel_mps2").value
        )
        self._margin = float(
            self.get_parameter("margin_m").value
        )
        self._lane_half = float(
            self.get_parameter("lane_half_m").value
        )
        self._vehicle_width = float(
            self.get_parameter("vehicle_width_m").value
        )
        self._lateral_clearance = float(
            self.get_parameter(
                "lateral_clearance_m"
            ).value
        )
        self._floor = float(
            self.get_parameter("confidence_floor").value
        )

        self._cap_governor = SpeedCapGovernor(
            SpeedCapGovernorConfig(
                rise_rate_mps_per_s=float(
                    self.get_parameter(
                        "cap_rise_rate_mps_per_s"
                    ).value
                ),
                clear_confirmations=int(
                    self.get_parameter(
                        "cap_clear_confirmations"
                    ).value
                ),
                maximum_dt_s=float(
                    self.get_parameter(
                        "cap_maximum_dt_ms"
                    ).value
                )
                / 1000.0,
                initial_cap_mps=0.0,
            )
        )

        self._session = (
            requests.Session()
            if REQUESTS_AVAILABLE
            else None
        )

        cap_topic = str(
            self.get_parameter("cap_topic").value
        )
        health_topic = str(
            self.get_parameter("health_topic").value
        )
        scan_topic = str(
            self.get_parameter("scan_topic").value
        )

        self._pub_cap = self.create_publisher(
            Float64,
            cap_topic,
            10,
        )
        self._pub_health = self.create_publisher(
            String,
            health_topic,
            10,
        )
        self._scan_subscription = self.create_subscription(
            LaserScan,
            scan_topic,
            self._on_scan,
            SCAN_QOS,
        )

        if not REQUESTS_AVAILABLE:
            self.get_logger().error(
                "python3-requests is unavailable; "
                "perception remains fail-closed."
            )

        self.get_logger().info(
            "Kirra perception governor started: "
            f"{scan_topic} -> Taj({self._taj_url}) -> "
            f"{cap_topic}; width={self._vehicle_width:.3f}m, "
            f"lane_half={self._lane_half:.3f}m, "
            f"clearance={self._lateral_clearance:.3f}m"
        )

    def _publish(
        self,
        cap_mps: float,
        health: str,
    ) -> None:
        self._pub_cap.publish(
            Float64(data=float(cap_mps))
        )
        self._pub_health.publish(
            String(data=health)
        )

    def _publish_fault(self, reason: str) -> None:
        # A fault invalidates all accumulated release evidence.
        self._cap_governor.reset()
        self._publish(0.0, reason)

    @staticmethod
    def _finite_number(value) -> bool:
        return (
            not isinstance(value, bool)
            and isinstance(value, (int, float))
            and math.isfinite(float(value))
        )

    @staticmethod
    def _format_optional_number(value) -> str:
        if PerceptionGovernor._finite_number(value):
            return f"{float(value):.3f}"
        return "none"

    def _publish_stabilized(
        self,
        *,
        raw_cap_mps: float,
        healthy: bool,
        clear_distance_m: float,
        minimum_corridor_width_m,
        required_corridor_width_m,
    ) -> None:
        # Never accept a nonzero cap from a response marked unhealthy.
        effective_raw_cap = (
            float(raw_cap_mps)
            if healthy
            else 0.0
        )

        decision = self._cap_governor.update(
            effective_raw_cap,
            time.monotonic_ns(),
        )

        min_width = self._format_optional_number(
            minimum_corridor_width_m
        )
        required_width = self._format_optional_number(
            required_corridor_width_m
        )

        health = (
            f'{"OK" if healthy else "UNHEALTHY"}:'
            f"clear={clear_distance_m:.2f}m:"
            f"raw={effective_raw_cap:.2f}:"
            f"governed={decision.governed_cap_mps:.2f}:"
            f"state={decision.reason}:"
            f"streak={decision.clear_streak}:"
            f"width={min_width}/{required_width}m"
        )

        self._publish(
            decision.governed_cap_mps,
            health,
        )

    def _request_body(self, msg: LaserScan) -> dict:
        stamp = msg.header.stamp
        stamp_ms = int(
            stamp.sec * 1000
            + stamp.nanosec / 1_000_000
        )

        return {
            "angle_min_rad": float(msg.angle_min),
            "angle_increment_rad": float(
                msg.angle_increment
            ),
            "range_min_m": float(msg.range_min),
            "range_max_m": float(msg.range_max),
            "ranges": [
                float(value)
                for value in msg.ranges
            ],
            "stamp_ms": stamp_ms,
            "forward_extent_m": self._extent,
            "decel_mps2": self._decel,
            "margin_m": self._margin,
            "lane_half_m": self._lane_half,
            "vehicle_width_m": self._vehicle_width,
            "lateral_clearance_m": (
                self._lateral_clearance
            ),
            "confidence_floor": self._floor,
        }

    def _on_scan(self, msg: LaserScan) -> None:
        if self._session is None:
            self._publish_fault("NO_REQUESTS_LIB")
            return

        try:
            response = self._session.post(
                f"{self._taj_url}/perception",
                json=self._request_body(msg),
                timeout=self._timeout_s,
            )

            if response.status_code != 200:
                self._publish_fault(
                    f"TAJ_HTTP_{response.status_code}"
                )
                return

            data = response.json()

            cap = data.get("speed_cap_mps")
            clear = data.get("clear_distance_m")
            healthy_value = data.get("healthy")

            if (
                not self._finite_number(cap)
                or float(cap) < 0.0
                or not self._finite_number(clear)
                or not isinstance(healthy_value, bool)
            ):
                self._publish_fault("TAJ_MALFORMED")
                return

            self._publish_stabilized(
                raw_cap_mps=float(cap),
                healthy=healthy_value,
                clear_distance_m=float(clear),
                minimum_corridor_width_m=data.get(
                    "minimum_corridor_width_m"
                ),
                required_corridor_width_m=data.get(
                    "required_corridor_width_m"
                ),
            )

        except requests.Timeout:
            self._publish_fault("TAJ_TIMEOUT")
        except requests.ConnectionError:
            self._publish_fault("TAJ_UNREACHABLE")
        except (ValueError, TypeError):
            self._publish_fault("TAJ_MALFORMED")
        except Exception as error:  # noqa: BLE001
            self.get_logger().error(
                f"perception governor error: {error}"
            )
            self._publish_fault("TAJ_ERROR")

    def destroy_node(self):
        if self._session is not None:
            self._session.close()
        return super().destroy_node()


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


if __name__ == "__main__":
    main()
