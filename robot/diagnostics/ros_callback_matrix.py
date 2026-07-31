#!/usr/bin/env python3
"""Controlled test matrix: why does a node service timers but not subscriptions?

Isolates the failure boundary for the fault where `kirra_motor_consumer.py`
executes its liveness timer but never its `/kirra/release` subscription, even
though DDS discovery shows a matched pair and a standalone subscriber in another
process receives the same messages.

Each mode changes exactly ONE variable relative to mode `a`, so the first mode
that stops delivering subscription callbacks names the cause:

    a  pure ROS                      — node + sub + timer, SingleThreadedExecutor
    b  + import Rosmaster_Lib        — imported, NOT instantiated
    c  + instantiate Rosmaster       — serial opened, receive thread NOT started
    d  + create_receive_threading()  — the vendor blocking-read thread is live
    e  separate callback groups      — sub and timer in distinct MutuallyExclusive groups
    f  MultiThreadedExecutor         — one clean try/finally, num_threads=2
    g  executor readiness probe      — logs what the wait set reports ready
    h  alternate RMW                 — run under a different RMW_IMPLEMENTATION

`a` through `d` are the important ones: they walk the vendor object in one step
at a time. If `d` is the first failure, the vendor receive thread is the cause.
If `a` already fails, the cause is environmental (RMW/domain), not the vendor.

SAFETY
    This script NEVER writes a motor. Modes c/d construct `Rosmaster` (which
    opens the serial port) but issue no motion primitive and no stop primitive —
    it commands nothing at all, so it cannot move the robot. Even so, run it
    with the robot on blocks and with the consumer service stopped, because
    opening the port while the consumer holds TIOCEXCL will fail by design.

    The probe topic carries a 3-byte payload, deliberately NOT the 272-byte
    governed frame, so no governed actuation path can be reached from here.

USAGE
    source /opt/kirra/robot/ros_env.sh && kirra_source_ros
    python3 robot/diagnostics/ros_callback_matrix.py a
    python3 robot/diagnostics/ros_callback_matrix.py d --seconds 15

    # in a second shell, the matching publisher:
    python3 robot/diagnostics/probe_publisher.py

Exit status is 0 when subscription callbacks were observed, 1 when they were
not — so the matrix can be swept non-interactively.
"""

from __future__ import annotations

import argparse
import os
import sys
import threading
import time

PROBE_TOPIC = "/kirra/probe"
PROBE_PAYLOAD_LEN = 3  # deliberately not 272 — cannot reach governed actuation

MODES = ("a", "b", "c", "d", "e", "f", "g", "h")


def _stderr(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def _report_environment() -> None:
    """Record the DDS-relevant environment before anything else runs.

    Hypothesis 6 (an RMW/DDS mismatch invisible to graph discovery) can only be
    argued from the environment the process actually had, so it is captured in
    every run rather than reconstructed afterwards.
    """
    keys = (
        "RMW_IMPLEMENTATION",
        "ROS_DOMAIN_ID",
        "ROS_LOCALHOST_ONLY",
        "ROS_AUTOMATIC_DISCOVERY_RANGE",
        "ROS_STATIC_PEERS",
        "FASTRTPS_DEFAULT_PROFILES_FILE",
        "CYCLONEDDS_URI",
        "RMW_FASTRTPS_USE_QOS_FROM_XML",
    )
    print("== environment ==", flush=True)
    for k in keys:
        print(f"   {k}={os.environ.get(k, '<unset>')}", flush=True)
    try:
        from rmw_implementation import get_rmw_implementation_identifier

        print(f"   active rmw={get_rmw_implementation_identifier()}", flush=True)
    except Exception as e:  # noqa: BLE001 — diagnostic only
        print(f"   active rmw=<unavailable: {e}>", flush=True)
    print(f"   pid={os.getpid()} threads={threading.active_count()}", flush=True)


def _open_vendor(mode: str, port: str):
    """Return (bot, note) for the vendor-object modes; (None, note) otherwise.

    Mode b imports the library without constructing it, so an import-time side
    effect is separated from a construction-time one. Mode c constructs it
    (opening the serial port) without the receive thread. Mode d additionally
    starts the vendor's blocking-read thread — the one variable under suspicion.
    """
    if mode == "a":
        return None, "no vendor import"

    from Rosmaster_Lib import Rosmaster  # noqa: F401 — mode b measures the import alone

    if mode == "b":
        return None, "Rosmaster_Lib imported, not instantiated"

    if mode not in ("c", "d"):
        return None, "no vendor object for this mode"

    bot = Rosmaster(com=port)
    if mode == "c":
        return bot, f"Rosmaster({port}) constructed, receive thread NOT started"

    bot.create_receive_threading()
    # The vendor reads car type only to prove the thread is genuinely running:
    # without the receive thread this returns -1, with it, 5.
    time.sleep(0.5)
    try:
        car_type = bot.get_car_type_from_machine()
    except Exception as e:  # noqa: BLE001 — diagnostic only
        car_type = f"<raised {e}>"
    return bot, (
        f"Rosmaster({port}) constructed + create_receive_threading(); "
        f"car_type={car_type} (expect 5 when the thread is live, -1 when not)"
    )


def run(mode: str, seconds: float, port: str) -> int:
    import rclpy
    from rclpy.node import Node
    from std_msgs.msg import UInt8MultiArray

    _report_environment()

    bot, note = _open_vendor(mode, port)
    print(f"== mode {mode}: {note} ==", flush=True)
    print(f"   threads after vendor setup={threading.active_count()}", flush=True)

    rclpy.init()
    node = Node(f"kirra_callback_matrix_{mode}")

    counts = {"sub": 0, "timer": 0}
    first_sub_at: list[float] = []
    started = time.monotonic()

    def on_probe(msg: UInt8MultiArray) -> None:
        counts["sub"] += 1
        if not first_sub_at:
            first_sub_at.append(time.monotonic() - started)
            print(
                f"SUB CALLBACK #1 after {first_sub_at[0]:.2f}s "
                f"bytes={len(msg.data)} data={list(msg.data)[:8]}",
                flush=True,
            )

    def on_timer() -> None:
        counts["timer"] += 1

    # Callback groups: only mode e separates them. Everywhere else both entities
    # land in the node's default MutuallyExclusive group, which is what the
    # production consumer uses.
    if mode == "e":
        from rclpy.callback_groups import MutuallyExclusiveCallbackGroup

        sub_group = MutuallyExclusiveCallbackGroup()
        timer_group = MutuallyExclusiveCallbackGroup()
        print("   sub and timer in SEPARATE MutuallyExclusive groups", flush=True)
    else:
        sub_group = None
        timer_group = None

    # The return values are retained deliberately. rclpy's Node already holds a
    # reference, so this does not change lifetime — it only makes hypothesis 10
    # (an entity being collected or invalidated) inspectable rather than assumed.
    sub = node.create_subscription(
        UInt8MultiArray, PROBE_TOPIC, on_probe, 10, callback_group=sub_group
    )
    timer = node.create_timer(0.1, on_timer, callback_group=timer_group)

    print(
        f"   subscription={sub.topic_name} timer_period={timer.timer_period_ns / 1e9:.3f}s",
        flush=True,
    )

    executor = None
    try:
        if mode in ("f", "h"):
            from rclpy.executors import MultiThreadedExecutor

            executor = MultiThreadedExecutor(num_threads=2)
        elif mode == "g":
            executor = _readiness_probe_executor()
        else:
            from rclpy.executors import SingleThreadedExecutor

            executor = SingleThreadedExecutor()

        executor.add_node(node)
        print(f"   executor={type(executor).__name__} spinning {seconds:.0f}s", flush=True)

        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline and rclpy.ok():
            executor.spin_once(timeout_sec=0.1)
    except KeyboardInterrupt:
        pass
    finally:
        # Exactly one finally for the one try. Teardown order: detach the node
        # from the executor, shut the executor down, then destroy the node.
        if executor is not None:
            try:
                executor.remove_node(node)
                executor.shutdown()
            except Exception:  # noqa: BLE001 — teardown must not mask the result
                pass
        try:
            node.destroy_node()
        except Exception:  # noqa: BLE001
            pass
        if rclpy.ok():
            rclpy.shutdown()
        # The vendor object is released without commanding anything. No motion
        # primitive and no stop primitive is issued, because none was issued
        # during the run either — the port is simply closed.
        if bot is not None:
            try:
                if hasattr(bot, "ser") and bot.ser is not None:
                    bot.ser.close()
            except Exception:  # noqa: BLE001
                pass

    elapsed = time.monotonic() - started
    print(
        f"== mode {mode} RESULT: sub_callbacks={counts['sub']} "
        f"timer_callbacks={counts['timer']} over {elapsed:.1f}s ==",
        flush=True,
    )
    if counts["timer"] == 0:
        _stderr("   INCONCLUSIVE: the timer never fired either — the executor "
                "never span. This mode says nothing about subscriptions.")
        return 2
    if counts["sub"] == 0:
        _stderr("   FAILURE BOUNDARY: timer fired but subscription did not. "
                "The variable introduced by this mode is implicated.")
        return 1
    print("   OK: both entity types dispatched.", flush=True)
    return 0


def _readiness_probe_executor():
    """Mode g: a SingleThreadedExecutor that logs what the wait set reports ready.

    Hypothesis 12 asks whether the subscriber is considered ready but never
    dispatched. That distinction is only visible between `wait_for_ready_callbacks`
    and the handler call, so this subclass reports the ready entity types — rate
    limited to one line per second, because an unthrottled log here would itself
    change the timing it is trying to measure.
    """
    from rclpy.executors import SingleThreadedExecutor

    class ReadinessProbeExecutor(SingleThreadedExecutor):
        def __init__(self) -> None:
            super().__init__()
            self._last_log = 0.0
            self._seen: dict[str, int] = {}

        def spin_once(self, timeout_sec=None):  # noqa: ANN001
            try:
                handler, entity, _node = self.wait_for_ready_callbacks(
                    timeout_sec=timeout_sec
                )
            except StopIteration:
                return
            except TimeoutError:
                return
            kind = type(entity).__name__
            self._seen[kind] = self._seen.get(kind, 0) + 1
            now = time.monotonic()
            if now - self._last_log >= 1.0:
                self._last_log = now
                print(f"   [ready] {dict(sorted(self._seen.items()))}", flush=True)
            handler()

    return ReadinessProbeExecutor()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=MODES, help="which matrix row to run")
    parser.add_argument("--seconds", type=float, default=10.0,
                        help="how long to spin (default 10)")
    parser.add_argument("--port", default=os.environ.get("KIRRA_MOTOR_PORT", "/dev/myserial"),
                        help="serial port for the vendor modes (c, d)")
    args = parser.parse_args(argv)

    if args.mode == "h" and not os.environ.get("RMW_IMPLEMENTATION"):
        _stderr("mode h requires RMW_IMPLEMENTATION to be set explicitly, e.g.\n"
                "  RMW_IMPLEMENTATION=rmw_cyclonedds_cpp python3 "
                "robot/diagnostics/ros_callback_matrix.py h\n"
                "Set it per-process only — do not change system configuration.")
        return 2

    return run(args.mode, args.seconds, args.port)


if __name__ == "__main__":
    raise SystemExit(main())
