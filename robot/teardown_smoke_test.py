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
            # R2 Path-B recorders (unused by the x3 cases).
            self.motor_writes: list[tuple[int, int, int, int]] = []
            self.steer_writes: list[int] = []
            self.default_angle: "int | None" = None
            self._car_type = 1  # default X3; set_car_type(5) flips it for r2
            self._enc = [0, 0, 0, 0]  # cumulative encoder counts (closed-loop reads)

        def create_receive_threading(self) -> None: ...

        def set_auto_report_state(self, enable: bool, forever: bool = False) -> None:
            # Closed-loop init enables MCU encoder auto-report before reading
            # encoders; the stub records nothing but must accept the call so the
            # closed-loop init path stays runnable under stubs.
            self.auto_report = bool(enable)

        def get_motor_encoder(self) -> "list[int]":
            # Advance each channel a little per read so a closed-loop actuate()
            # sees a non-zero speed (the no-op spin here does not call it, but the
            # method must exist for the closed-loop path to be importable/runnable).
            self._enc = [c + 10 for c in self._enc]
            return list(self._enc)

        def get_car_type_from_machine(self) -> int:
            # Reflects set_car_type — x3 cases never call it, so this stays 1
            # (matches KIRRA_EXPECTED_CAR_TYPE below); r2 sets it to 5.
            return self._car_type

        def set_car_motion(self, vx: float, vy: float, vz: float) -> None:
            self.motions.append((vx, vy, vz))

        def set_car_type(self, car_type: int) -> None:
            self._car_type = int(car_type)

        def set_motor(self, s1: int, s2: int, s3: int, s4: int) -> None:
            self.motor_writes.append((s1, s2, s3, s4))

        def set_akm_steering_angle(self, cmd: int) -> None:
            self.steer_writes.append(int(cmd))

        def set_akm_default_angle(self, angle: int) -> None:
            self.default_angle = int(angle)

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

    # ------------------------------------------------------------------
    # 5. Motor consumer in R2 Path-B mode (off-by-default flag ON). This
    #    harness's spin is a no-op, so it does NOT drive the subscription/
    #    timer actuation callbacks — it covers the INIT + TEARDOWN dispatch:
    #    car-type 5 set (+ centre trim), and the r2 safe stop (set_motor 0 +
    #    centre), with set_car_motion NEVER used. The last-hop actuation
    #    semantics (translate → set_motor/AKM ordering, MRC zeros) are covered
    #    by robot/r2_drive_test.py. KIRRA_EXPECTED_CAR_TYPE is cleared first so
    #    this also enforces that r2 mode does NOT require the x3-only knob.
    # ------------------------------------------------------------------
    ctx.spin_downs_context = False
    ctx.spin_raises = None
    os.environ.pop("KIRRA_EXPECTED_CAR_TYPE", None)  # r2 mode must not need it
    os.environ.update({
        "KIRRA_DRIVE_MODE": "r2_ackermann",
        # Measured-calibration stand-ins (test fixtures, not hardware values).
        "KIRRA_R2_WHEELBASE_M": "0.229",
        "KIRRA_R2_V_PER_PWM": "0.0145",
        "KIRRA_R2_PWM_MAX": "60",
        "KIRRA_R2_STEER_UNITS_PER_RAD": "140",
        "KIRRA_R2_DELTA_MAX_RAD": "0.5",
        "KIRRA_R2_STEER_SIGN": "-1",
        "KIRRA_R2_CENTER_TRIM": "90",
    })
    try:
        rc = kirra_motor_consumer.main()
    except BaseException as e:  # noqa: BLE001
        failures.append(f"(5) consumer r2 mode raised: {e!r}")
        rc = -1
    finally:
        for k in ("KIRRA_DRIVE_MODE", "KIRRA_R2_WHEELBASE_M", "KIRRA_R2_V_PER_PWM",
                  "KIRRA_R2_PWM_MAX", "KIRRA_R2_STEER_UNITS_PER_RAD",
                  "KIRRA_R2_DELTA_MAX_RAD", "KIRRA_R2_STEER_SIGN",
                  "KIRRA_R2_CENTER_TRIM"):
            os.environ.pop(k, None)
    check(rc == 0, f"(5) r2-mode consumer must exit 0, got {rc}")
    bot = sys.modules["Rosmaster_Lib"].Rosmaster.last
    check(bot is not None, "(5) r2-mode consumer must have opened the board")
    if bot is not None:
        check(bot._car_type == 5, "(5) r2 mode must set car-type 5 (AKM servo enable)")
        check(bot.default_angle == 90, "(5) r2 mode must apply the measured centre trim")
        check(len(bot.motions) == 0,
              "(5) r2 mode must NEVER call set_car_motion (type 5 breaks it)")
        check(bot.motor_writes and bot.motor_writes[-1] == (0, 0, 0, 0),
              "(5) r2 safe stop must zero both rear motors via set_motor")
        check(bot.steer_writes and bot.steer_writes[-1] == 0,
              "(5) r2 safe stop must centre the steering")
    print("(5) consumer/r2 mode → exit 0, car-type 5 + trim set, "
          "no set_car_motion, safe stop zeros motors + centre (init + teardown)")

    # ------------------------------------------------------------------
    # 6. R2 closed-loop (§9) init: with KIRRA_R2_CLOSED_LOOP on, the consumer
    #    must build the speed matcher (needs KIRRA_R2_M_PER_TICK +
    #    KIRRA_R2_V_PER_PWM_RIGHT) and start; and fail-closed (rc != 0) when the
    #    flag is on but those params are MISSING — never silently fall back to
    #    open-loop. The no-op spin covers INIT; the loop math is r2_drive_test.py.
    # ------------------------------------------------------------------
    r2_env = {
        "KIRRA_DRIVE_MODE": "r2_ackermann",
        "KIRRA_R2_WHEELBASE_M": "0.229",
        "KIRRA_R2_V_PER_PWM": "0.0145",
        "KIRRA_R2_PWM_MAX": "60",
        "KIRRA_R2_STEER_UNITS_PER_RAD": "140",
        "KIRRA_R2_DELTA_MAX_RAD": "0.5",
        "KIRRA_R2_STEER_SIGN": "-1",
        "KIRRA_R2_CENTER_TRIM": "90",
    }
    cl_keys = list(r2_env) + ["KIRRA_R2_CLOSED_LOOP", "KIRRA_R2_M_PER_TICK", "KIRRA_R2_V_PER_PWM_RIGHT"]

    # 6a. flag ON but params MISSING → fail closed.
    os.environ.update(r2_env)
    os.environ["KIRRA_R2_CLOSED_LOOP"] = "1"
    os.environ.pop("KIRRA_R2_M_PER_TICK", None)
    os.environ.pop("KIRRA_R2_V_PER_PWM_RIGHT", None)
    try:
        rc_missing = kirra_motor_consumer.main()
    except BaseException:  # noqa: BLE001
        rc_missing = -1
    check(rc_missing != 0, "(6a) closed-loop ON with missing params must fail closed (rc != 0)")

    # 6b. flag ON + params present → inits + exits 0 + safe-stops.
    os.environ.update(r2_env)
    os.environ.update({
        "KIRRA_R2_CLOSED_LOOP": "1",
        "KIRRA_R2_M_PER_TICK": "0.00025101",
        "KIRRA_R2_V_PER_PWM_RIGHT": "0.0194",
    })
    try:
        rc_cl = kirra_motor_consumer.main()
    except BaseException as e:  # noqa: BLE001
        failures.append(f"(6b) closed-loop consumer raised: {e!r}")
        rc_cl = -1
    finally:
        for k in cl_keys:
            os.environ.pop(k, None)
    check(rc_cl == 0, f"(6b) closed-loop consumer must exit 0, got {rc_cl}")
    bot_cl = sys.modules["Rosmaster_Lib"].Rosmaster.last
    if bot_cl is not None:
        check(bot_cl._car_type == 5, "(6b) closed-loop r2 mode must still set car-type 5")
        check(bot_cl.motor_writes and bot_cl.motor_writes[-1] == (0, 0, 0, 0),
              "(6b) closed-loop safe stop must zero both rear motors")
    print("(6) consumer/r2 closed-loop → fail-closed on missing params; "
          "inits + exits 0 + safe stop when params present")

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
