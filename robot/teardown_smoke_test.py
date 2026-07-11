#!/usr/bin/env python3
"""Host teardown test for the robot scripts' shutdown paths (no ROS needed).

The hardware first-run test surfaced a double-shutdown crash: `rclpy.init()`
installs its own SIGINT/SIGTERM handlers, so on Ctrl-C / `timeout`'s SIGTERM
the context is ALREADY shut down when the script's `finally` runs — an
unconditional `rclpy.shutdown()` there was a guaranteed second call
(`RCLError: rcl_shutdown already called`) after every publish window.

This harness injects a stub `rclpy` whose `shutdown()` raises on a second call
— exactly like real rcl — then drives the REAL `main()` of the publisher and
the motor consumer through the signal-shutdown scenarios. Remove the
`if rclpy.ok():` guards and this test goes red (non-vacuity). The motor
consumer runs against the REAL libkirra_consumer_ffi and a recording Rosmaster
stub, so "safe_stop still writes (0,0,0) before teardown" is asserted too.

Prereqs (same as ffi_smoke_test.py):
    cargo build -p kirra-consumer-ffi
    cargo build -p kirra-release-token --bin kirra_ros_release_mint

Exit 0 = teardown exactly-once everywhere; exit 1 = a double-shutdown or a
behavior change (frames not published / safe stop not written).
"""

from __future__ import annotations

import os
import subprocess
import sys
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))

failures: list[str] = []


def check(cond: bool, msg: str) -> None:
    if not cond:
        failures.append(msg)


# ---------------------------------------------------------------------------
# Stub rclpy — mimics the real teardown semantics that produced the bug:
#   * ok() reflects context liveness;
#   * a "signal" (real rclpy's own handler) shuts the context down out-of-band;
#   * shutdown() on an already-down context RAISES, like rcl_shutdown.
# ---------------------------------------------------------------------------

class StubContext:
    def __init__(self) -> None:
        self.up = False
        self.shutdown_calls = 0
        self.ok_calls_until_signal: int | None = None
        self._ok_calls = 0
        self.spin_raises: BaseException | None = None
        self.spin_downs_context = False

    def init(self, *a, **k) -> None:
        self.up = True
        self._ok_calls = 0
        self.shutdown_calls = 0

    def ok(self) -> bool:
        self._ok_calls += 1
        if (self.ok_calls_until_signal is not None
                and self._ok_calls > self.ok_calls_until_signal):
            self.up = False  # rclpy's own signal handler shut the context down
        return self.up

    def shutdown(self, *a, **k) -> None:
        if not self.up:
            # Real rcl behavior — the exact crash from the hardware run.
            raise RuntimeError("failed to shutdown: rcl_shutdown already called")
        self.up = False
        self.shutdown_calls += 1

    def spin(self, _node) -> None:
        if self.spin_downs_context:
            self.up = False
        if self.spin_raises is not None:
            raise self.spin_raises


class StubLogger:
    def info(self, *_a) -> None: ...
    def warn(self, *_a) -> None: ...
    def error(self, *_a) -> None: ...


class StubPublisher:
    def __init__(self) -> None:
        self.published: list[bytes] = []

    def publish(self, msg) -> None:
        self.published.append(bytes(msg.data))


class StubNode:
    last: "StubNode | None" = None

    def __init__(self, _name: str) -> None:
        StubNode.last = self
        self.pub = StubPublisher()
        self.destroyed = 0

    def create_publisher(self, _t, _topic, _qos):
        return self.pub

    def create_subscription(self, _t, _topic, _cb, _qos):
        return object()

    def create_timer(self, _period, _cb):
        return object()

    def get_logger(self):
        return StubLogger()

    def destroy_node(self) -> None:
        self.destroyed += 1


class ExternalShutdownException(Exception):
    pass


class UInt8MultiArray:
    def __init__(self) -> None:
        self.data: list[int] = []


def install_stubs(ctx: StubContext) -> None:
    rclpy = types.ModuleType("rclpy")
    rclpy.init = ctx.init
    rclpy.ok = ctx.ok
    rclpy.shutdown = ctx.shutdown
    rclpy.spin = ctx.spin

    rclpy_node = types.ModuleType("rclpy.node")
    rclpy_node.Node = StubNode
    rclpy_execs = types.ModuleType("rclpy.executors")
    rclpy_execs.ExternalShutdownException = ExternalShutdownException
    rclpy.node = rclpy_node
    rclpy.executors = rclpy_execs

    std_msgs = types.ModuleType("std_msgs")
    std_msgs_msg = types.ModuleType("std_msgs.msg")
    std_msgs_msg.UInt8MultiArray = UInt8MultiArray
    std_msgs.msg = std_msgs_msg

    rosmaster_lib = types.ModuleType("Rosmaster_Lib")

    class Rosmaster:
        last: "Rosmaster | None" = None

        def __init__(self, com: str = "") -> None:
            Rosmaster.last = self
            self.com = com
            self.motions: list[tuple[float, float, float]] = []

        def create_receive_threading(self) -> None: ...

        def get_car_type_from_machine(self) -> int:
            return 1  # matches KIRRA_EXPECTED_CAR_TYPE below

        def set_car_motion(self, vx: float, vy: float, vz: float) -> None:
            self.motions.append((vx, vy, vz))

    rosmaster_lib.Rosmaster = Rosmaster

    for name, mod in {
        "rclpy": rclpy,
        "rclpy.node": rclpy_node,
        "rclpy.executors": rclpy_execs,
        "std_msgs": std_msgs,
        "std_msgs.msg": std_msgs_msg,
        "Rosmaster_Lib": rosmaster_lib,
    }.items():
        sys.modules[name] = mod


def find_mint() -> str:
    for prof in ("release", "debug"):
        cand = REPO / "target" / prof / "kirra_ros_release_mint"
        if cand.is_file():
            return str(cand)
    print("FAIL: kirra_ros_release_mint not built")
    sys.exit(1)


def main() -> int:
    mint = find_mint()
    ctx = StubContext()
    install_stubs(ctx)

    import kirra_release_publisher
    import kirra_motor_consumer

    # ------------------------------------------------------------------
    # 1. Publisher, signal path (the hardware crash): rclpy's own handler
    #    downs the context mid-loop; the finally must NOT shutdown again.
    # ------------------------------------------------------------------
    os.environ.update({
        "KIRRA_MINT_BIN": mint,
        "KIRRA_PUB_RATE_HZ": "500",  # fast loop; content/timing logic unchanged
    })
    ctx.ok_calls_until_signal = 3  # 3 loop iterations, then "SIGTERM arrived"
    sys.argv = ["kirra_release_publisher.py", "--valid"]
    try:
        rc = kirra_release_publisher.main()
    except BaseException as e:  # noqa: BLE001 — any traceback is THE bug
        failures.append(f"(1) publisher signal path raised: {e!r}")
        rc = -1
    check(rc == 0, f"(1) publisher must exit 0 on the signal path, got {rc}")
    node = StubNode.last
    # Guard the dereferences (Copilot #903): if main() bailed before creating
    # the node, this must report a clean check failure, not an AttributeError.
    check(node is not None, "(1) publisher must have created its node")
    if node is not None:
        check(len(node.pub.published) == 3,
              "(1) publisher must still publish its frames (behavior unchanged)")
        check(node.destroyed == 1, "(1) node destroyed exactly once")
        check(all(len(f) == 128 for f in node.pub.published),
              "(1) valid mode still emits 128-byte signed frames")
        print(f"(1) publisher/SIGTERM → exit 0, {len(node.pub.published)} frames, "
              f"no double shutdown")
    check(ctx.shutdown_calls == 0,
          "(1) context was signal-shutdown; the guard must SKIP the second call")

    # ------------------------------------------------------------------
    # 2. Publisher, KeyboardInterrupt with the context still up: shutdown
    #    must run EXACTLY once (the guard must not skip a needed teardown).
    # ------------------------------------------------------------------
    ctx.ok_calls_until_signal = None
    calls = {"n": 0}
    real_mint_frame = kirra_release_publisher.mint_frame

    def interrupting_mint(*a, **k):
        calls["n"] += 1
        if calls["n"] >= 2:
            raise KeyboardInterrupt  # Ctrl-C before rclpy's handler ran
        return real_mint_frame(*a, **k)

    kirra_release_publisher.mint_frame = interrupting_mint
    try:
        rc = kirra_release_publisher.main()
    except BaseException as e:  # noqa: BLE001
        failures.append(f"(2) publisher KeyboardInterrupt path raised: {e!r}")
        rc = -1
    finally:
        kirra_release_publisher.mint_frame = real_mint_frame
    check(rc == 0, f"(2) publisher must exit 0 on Ctrl-C, got {rc}")
    check(ctx.shutdown_calls == 1,
          f"(2) context still up → shutdown must run exactly once, got {ctx.shutdown_calls}")
    print("(2) publisher/Ctrl-C (context up) → exit 0, shutdown exactly once")

    # ------------------------------------------------------------------
    # 3. Motor consumer: spin raises ExternalShutdownException after the
    #    signal handler downed the context (the Humble behavior). Must exit
    #    cleanly, write the (0,0,0) safe stop, and not double-shutdown.
    # ------------------------------------------------------------------
    # subprocess (not os.popen — Copilot #903): no shell, and check=True makes
    # a mint failure fail THIS test loudly instead of feeding an empty key on.
    vk = subprocess.run(
        [mint, "--seed", "2a" * 32, "pubkey"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    os.environ.update({
        "KIRRA_GOVERNOR_VK_HEX": vk,
        "KIRRA_FRESHNESS_WINDOW_MS": "200",
        "KIRRA_CONTROL_PERIOD_MS": "100",
        "KIRRA_MISSED_PERIODS": "3",
        "KIRRA_STOP_DECEL_MPS2": "0.5",
        "KIRRA_DEMO_VX_MAX": "0.15",
        "KIRRA_DEMO_VZ_MAX": "0.4",
        "KIRRA_MOTOR_PORT": "/dev/stub-serial",
        "KIRRA_EXPECTED_CAR_TYPE": "1",
    })
    ctx.spin_downs_context = True  # our handler already shut it down...
    ctx.spin_raises = ExternalShutdownException()  # ...and spin surfaces it
    try:
        rc = kirra_motor_consumer.main()
    except BaseException as e:  # noqa: BLE001 — the pre-fix behavior tracebacked here
        failures.append(f"(3) consumer external-shutdown path raised: {e!r}")
        rc = -1
    check(rc == 0, f"(3) consumer must exit 0 on external shutdown, got {rc}")
    bot = sys.modules["Rosmaster_Lib"].Rosmaster.last
    check(bot is not None and bot.motions and bot.motions[-1] == (0.0, 0.0, 0.0),
          "(3) the shutdown path must still command the (0,0,0) safe stop")
    check(ctx.shutdown_calls == 0,
          "(3) context already down → the guarded finally must not re-shutdown")
    print("(3) consumer/ExternalShutdown → exit 0, safe stop written, no double shutdown")

    # ------------------------------------------------------------------
    # 4. Motor consumer, spin returns normally (context up): teardown must
    #    still safe-stop AND shut down exactly once.
    # ------------------------------------------------------------------
    ctx.spin_downs_context = False
    ctx.spin_raises = None
    try:
        rc = kirra_motor_consumer.main()
    except BaseException as e:  # noqa: BLE001
        failures.append(f"(4) consumer normal-exit path raised: {e!r}")
        rc = -1
    check(rc == 0, f"(4) consumer must exit 0 on normal spin exit, got {rc}")
    bot = sys.modules["Rosmaster_Lib"].Rosmaster.last
    check(bot is not None and bot.motions and bot.motions[-1] == (0.0, 0.0, 0.0),
          "(4) normal exit still commands the safe stop")
    check(ctx.shutdown_calls == 1,
          f"(4) context up → shutdown exactly once, got {ctx.shutdown_calls}")
    print("(4) consumer/normal exit → exit 0, safe stop written, shutdown exactly once")

    print()
    if failures:
        for f in failures:
            print(f"FAIL {f}")
        print(f"\nteardown smoke test FAILED ({len(failures)} mismatch(es))")
        return 1
    print("teardown smoke test: OK — shutdown is exactly-once on every exit path, "
          "behavior (frames / safe stop) unchanged.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
