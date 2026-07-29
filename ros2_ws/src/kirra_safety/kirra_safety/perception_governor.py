#!/usr/bin/env python3
"""
Kirra Perception Governor.

Receives the latest lidar scan, requests a fused safety envelope from Taj, and
publishes an asymmetrically stabilized speed cap.

#1218: the scan callback is NON-BLOCKING. It stamps and copies the scan into a
one-entry latest-frame slot and returns; a single bounded worker performs the
HTTP round trip; a publish timer decides what reaches the wire. The callback no
longer waits on a socket, so a slow sidecar cannot back up sensor ingest, and a
reply can no longer be attributed to a scan other than the one it was issued
for. Decisions live in `async_perception`; this module is the shell.

Safety behavior:
- Restrictive cap changes apply immediately.
- Permissive changes require consecutive confirmation and rate-limited release.
- Taj errors, unhealthy perception, malformed responses, and timing faults
  publish an immediate zero cap and reset release history.
- The downstream interceptor independently stops if this topic becomes stale.
"""

import math
import threading
import time

import rclpy
from rclpy.node import Node
from rclpy.qos import HistoryPolicy, QoSProfile, ReliabilityPolicy
from sensor_msgs.msg import LaserScan
from std_msgs.msg import Float64, String

from kirra_safety.async_perception import (
    ISSUE,
    LatestFrameSlot,
    PerceptionRequest,
    PerceptionResult,
    STALE,
    SUPERSEDED,
    USE,
    deadline_breached,
    decide_issue,
    resolve_result,
)
from kirra_safety.perception_cap import (
    STABILIZED_OK,
    STABILIZED_UNSTABILIZED,
    resolve_stabilized_cap,
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

        # #1212: the asymmetric cap-release parameters (rise rate, restriction
        # epsilon, clear confirmations, maximum dt) are NOT declared here. They
        # configure the stabilizer, the stabilizer lives in the Rust checker, and a
        # parameter this node accepts but cannot act on is worse than no parameter:
        # an operator who tunes it sees the value take effect in `ros2 param get` and
        # concludes the policy changed. The checker owns them; this node relays.

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

        # #1218 non-blocking cycle. The publish period is what bounds how long a
        # wedged sidecar can leave a stale cap on the wire, and the deadline is
        # what floors it — deliberately separate from `timeout_ms`, which is a
        # PER-SOCKET-OPERATION budget and cannot bound wall-clock latency.
        self.declare_parameter("publish_period_ms", 100)
        self.declare_parameter("deadline_ms", 250)
        self.declare_parameter("scan_stale_ms", 500)
        publish_period_s = (
            float(self.get_parameter("publish_period_ms").value) / 1000.0
        )
        self._deadline_s = float(self.get_parameter("deadline_ms").value) / 1000.0
        self._scan_stale_s = (
            float(self.get_parameter("scan_stale_ms").value) / 1000.0
        )

        # One lock over everything the worker and the executor thread both
        # touch. The in-flight flag and the slot MUST be read together — "is a
        # job running" and "what would it run on" is one question, and splitting
        # it is a race that starts two workers.
        self._lock = threading.Lock()
        self._slot = LatestFrameSlot()
        self._request_sequence = 0
        self._last_published_sequence = 0
        self._in_flight_since_s = None
        self._result = None
        self._discarded_replies = 0
        self._deadline_breaches = 0

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
        # The only publisher. Runs whether or not scans are arriving, which is
        # the point: a sidecar that stops answering, or a lidar that stops
        # producing, must still floor the cap on a schedule this node controls.
        self._publish_timer = self.create_timer(
            publish_period_s, self._on_publish_timer
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

    def _counters(self) -> str:
        """#1218 counters, appended to every health message.

        On the health topic rather than a new one: an operator diagnosing "why
        is the cap zero" is already reading this string, and a counter they have
        to go and find separately is a counter they will not find. Read without
        the lock — these are diagnostic totals, and a torn read costs nothing
        that matters.
        """
        return (
            f"discarded={self._discarded_replies}:"
            f"deadline={self._deadline_breaches}:"
            f"overwritten={self._slot.overwrites}"
        )

    def _publish_fault(self, reason: str) -> None:
        # A fault invalidates all accumulated release evidence.
        self._publish(0.0, f"{reason}:{self._counters()}")

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
        data=None,
    ) -> None:
        # Never accept a nonzero cap from a response marked unhealthy.
        effective_raw_cap = (
            float(raw_cap_mps)
            if healthy
            else 0.0
        )

        # #1212: the CHECKER stabilizes the cap. This node relays the number Taj
        # served and does not compute one. The local state machine used to decide
        # here, in the doer layer, outside the safety authority — which looked done
        # while being unenforceable.
        #
        # Fail closed if the field is absent or malformed: an older Taj that does
        # not serve a stabilized cap is not a licence to fall back on the raw one.
        governed_cap_mps, relay_reason = resolve_stabilized_cap(
            data,
            effective_raw_cap,
        )
        if relay_reason != STABILIZED_OK:
            # The two failures are surfaced separately because the operator action
            # differs: UNSTABILIZED means this sidecar never stabilized anything and
            # needs upgrading or configuring, MALFORMED means it claimed to and the
            # value is unusable. Both stop the robot; only one is fixed by a restart.
            self._publish_fault(
                "TAJ_UNSTABILIZED"
                if relay_reason == STABILIZED_UNSTABILIZED
                else "TAJ_NO_STABILIZED_CAP"
            )
            return
        cap_reason = data.get("cap_reason") or "NONE"
        cap_streak = data.get("cap_clear_streak")
        cap_limiting = data.get("cap_limiting_object")

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
            f"governed={governed_cap_mps:.2f}:"
            f"state={cap_reason}:"
            f"streak={cap_streak}:"
            f"limiting={cap_limiting}:"
            f"width={min_width}/{required_width}m:"
            f"{self._counters()}"
        )

        self._publish(
            governed_cap_mps,
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
            # #1211 scan liveness. Publisher count can ONLY be obtained by
            # inspecting the ROS graph, which the Taj sidecar (plain HTTP) cannot
            # do — so this node, which holds the subscription, observes it and
            # carries it as data. Taj reports `publisher_count_observed` so an
            # omission is visible rather than silently read as zero.
            "publisher_count": self._scan_publisher_count(),
        }

    def _scan_publisher_count(self):
        """Publishers currently advertising the scan topic, or None.

        Zero publishers is a real, distinct fault (the driver died or never
        started) and must reach Taj as 0. None means this rclpy build could not
        tell us — reported as unobserved so the check is skipped rather than
        firing on an unknown.
        """
        subscription = self._scan_subscription
        if subscription is None:
            return None
        try:
            return int(subscription.get_publisher_count())
        except (AttributeError, TypeError, ValueError):
            return None

    # ------------------------------------------------------------------
    # #1218 non-blocking scan cycle.
    #
    # The callback COPIES and RETURNS; a worker does the network; a timer
    # publishes. Nothing here waits on a socket, so a slow or wedged sidecar can
    # no longer back up the sensor ingest it is being asked to judge.
    #
    # The decisions live in `async_perception` so they are testable without a
    # ROS graph. This node is the shell: it owns the lock, the thread and the
    # publishers, and asks that module what to do.
    # ------------------------------------------------------------------

    def _on_scan(self, msg: LaserScan) -> None:
        """Stamp, copy, offer, return. No network, no publishing, no blocking.

        Publishing from here is deliberately avoided even for the
        NO_REQUESTS_LIB case: the timer already floors the cap when nothing
        resolves, so a second publisher on the scan callback would only add a
        path that could disagree with it.
        """
        received_s = time.monotonic()
        # Built BEFORE the lock. It reads the live ROS message — which the
        # worker must never touch, hence copying here — but that copy is pure
        # computation over a few hundred floats and holding the lock across it
        # would stall the worker's deposit for no reason.
        body = self._request_body(msg)
        with self._lock:
            self._request_sequence += 1
            self._slot.offer(
                PerceptionRequest(
                    request_sequence=self._request_sequence,
                    scan_received_s=received_s,
                    taj_url=self._taj_url,
                    timeout_s=self._timeout_s,
                    body=body,
                )
            )
        self._maybe_issue()

    def _maybe_issue(self) -> None:
        """Start a worker if one may run. Returns immediately either way."""
        if self._session is None:
            return
        with self._lock:
            if decide_issue(self._in_flight_since_s is not None,
                            self._slot.has_pending) != ISSUE:
                return
            pending = self._slot.take()
            self._in_flight_since_s = time.monotonic()

        worker = threading.Thread(
            target=self._run_request, args=(pending,), daemon=True
        )
        worker.start()

    def _run_request(self, pending) -> None:
        """Worker body. Computes; deposits; touches nothing else.

        Every exit path deposits a result, including the failures. A worker that
        returned without depositing would leave the node unable to distinguish
        "still working" from "died", and the deadline would be the only thing
        that ever cleared it.
        """
        outcome = self._fetch(pending)
        with self._lock:
            self._result = outcome
            self._in_flight_since_s = None
        # A scan may have arrived while this ran; start its request now rather
        # than waiting for the next one.
        self._maybe_issue()

    def _fetch(self, pending) -> PerceptionResult:
        def faulted(reason):
            return PerceptionResult(
                request_sequence=pending.request_sequence,
                scan_received_s=pending.scan_received_s,
                response=None,
                fault=reason,
            )

        try:
            response = self._session.post(
                f"{pending.taj_url}/perception",
                json=pending.body,
                timeout=pending.timeout_s,
            )
            if response.status_code != 200:
                return faulted(f"TAJ_HTTP_{response.status_code}")

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
                return faulted("TAJ_MALFORMED")

            return PerceptionResult(
                request_sequence=pending.request_sequence,
                scan_received_s=pending.scan_received_s,
                response=data,
                fault=None,
            )
        except requests.Timeout:
            return faulted("TAJ_TIMEOUT")
        except requests.ConnectionError:
            return faulted("TAJ_UNREACHABLE")
        except (ValueError, TypeError):
            return faulted("TAJ_MALFORMED")
        except Exception as error:  # noqa: BLE001
            self.get_logger().error(f"perception governor error: {error}")
            return faulted("TAJ_ERROR")

    def _on_publish_timer(self) -> None:
        """The only publisher. Decides once per period and always publishes.

        Always, not only on success: a cap that stops being republished is a cap
        a consumer will age out on its own staleness rule, which is fail-closed
        but silent. Publishing the floor with a named reason says WHY the robot
        is holding.
        """
        now_s = time.monotonic()

        # Decide under the lock; publish outside it. Holding a lock across a ROS
        # publish would couple the worker's deposit to publisher latency, which
        # is exactly the kind of coupling this issue exists to remove.
        accepted = None
        fault = None
        with self._lock:
            if self._session is None:
                fault = "NO_REQUESTS_LIB"
            elif deadline_breached(self._in_flight_since_s, now_s, self._deadline_s):
                self._deadline_breaches += 1
                # The request is left running — it cannot be cancelled, and the
                # one-in-flight bound is what keeps a sick sidecar from spawning
                # threads. It will deposit late and be refused as superseded.
                fault = "TAJ_DEADLINE"
            else:
                verdict = resolve_result(
                    self._result,
                    newest_request_sequence=self._request_sequence,
                    last_published_sequence=self._last_published_sequence,
                    now_s=now_s,
                    scan_stale_s=self._scan_stale_s,
                )
                if verdict.state == USE:
                    accepted = self._result
                    self._last_published_sequence = accepted.request_sequence
                else:
                    if verdict.state in (SUPERSEDED, STALE):
                        self._discarded_replies += 1
                    fault = f"TAJ_{verdict.state}"

        if accepted is None:
            self._publish_fault(fault)
            return

        data = accepted.response
        self._publish_stabilized(
            raw_cap_mps=float(data.get("speed_cap_mps")),
            healthy=data.get("healthy"),
            clear_distance_m=float(data.get("clear_distance_m")),
            minimum_corridor_width_m=data.get("minimum_corridor_width_m"),
            required_corridor_width_m=data.get("required_corridor_width_m"),
            data=data,
        )

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
