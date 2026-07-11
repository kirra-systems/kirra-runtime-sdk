#!/usr/bin/env python3
"""KIRRA verifying motor consumer — Yahboom Rosmaster X3 (ADR-0033 chokepoint, physical).

This node IS the motor bringup. Per ADR-0033 we do NOT stand up the vendor
`/cmd_vel` driver and retrofit a fence — the verifying consumer is the ONLY
thing that opens the motor board (`/dev/myserial`) and the ONLY thing that calls
`Rosmaster.set_car_motion`. A command is actuated ONLY if the Rust verify core
(libkirra_consumer_ffi, ADR-0033 decision (c)) releases it: token → Ed25519 over
the exact bytes → freshness → strictly-advancing sequence. No token / stale /
replayed / bad-signature → refused, no motor write.

🔴 Nothing here re-implements verification. Every gate/watermark/freshness/
liveness/decel/alarm decision is made in Rust and returned across the FFI; this
node presents wire bytes and actuates whatever twist the core decides.

🔴 No vendor base node. Do NOT launch `yahboomcar_bringup` alongside this — it
would be a second, UNFENCED writer to the motor board. This node owning
`/dev/myserial` (exclusive) is the structural guarantee.

Wire input: topic KIRRA_RELEASE_TOPIC (default /kirra/release),
`std_msgs/UInt8MultiArray`, data = payload(32) || token(96) for a governed
command, or payload(32) alone for an unsigned one (→ refused).

Config — ALL required, NO defaults (fail-closed; a missing var aborts):
    KIRRA_GOVERNOR_VK_HEX      64-hex Ed25519 public key this consumer pins
    KIRRA_FRESHNESS_WINDOW_MS  freshness window (ADR-0033 decision 3; e.g. 200)
    KIRRA_CONTROL_PERIOD_MS    control period (e.g. 100 at 10 Hz)
    KIRRA_MISSED_PERIODS       liveness deadline in periods (ADR-0033: 3)
    KIRRA_STOP_DECEL_MPS2      MRC decel for the safe stop (class MRC profile)
    KIRRA_DEMO_VX_MAX          demo linear cap (m/s) — Step 3 backstop
    KIRRA_DEMO_VZ_MAX          demo angular cap (rad/s) — Step 3 backstop
    KIRRA_MOTOR_PORT           motor serial device (e.g. /dev/myserial)
Optional:
    KIRRA_RELEASE_TOPIC        (default /kirra/release)
    KIRRA_CONSUMER_LIB         explicit path to libkirra_consumer_ffi.so
"""

from __future__ import annotations

import os
import signal
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from kirra_ffi import KirraConsumer, REFUSAL_NAMES, split_frame  # noqa: E402


def _req(name: str) -> str:
    v = os.environ.get(name)
    if v is None or v == "":
        # Fail-closed: no invented "safe" numbers. A missing knob is an abort,
        # not a default.
        print(f"FATAL: required env var {name} is unset — refusing to start "
              f"(no defaults for physical/safety numbers).", file=sys.stderr)
        sys.exit(2)
    return v


def _req_float(name: str) -> float:
    try:
        return float(_req(name))
    except ValueError:
        print(f"FATAL: {name} must be a number", file=sys.stderr)
        sys.exit(2)


def _req_int(name: str) -> int:
    try:
        return int(_req(name))
    except ValueError:
        print(f"FATAL: {name} must be an integer", file=sys.stderr)
        sys.exit(2)


def now_ms() -> int:
    # UNIX epoch ms — MUST share a synchronized clock with the signer
    # (AOU-TIMESYNC-001): freshness compares this to the token's issued_at_ms.
    return int(time.time() * 1000)


def main() -> int:
    # ROS + vendor lib imported inside main so the file parses/syntax-checks on a
    # host without them (CI py_compile); they are required on the robot.
    import rclpy
    from rclpy.node import Node
    from std_msgs.msg import UInt8MultiArray
    from Rosmaster_Lib import Rosmaster

    vk_hex = _req("KIRRA_GOVERNOR_VK_HEX").strip()
    try:
        governor_vk = bytes.fromhex(vk_hex)
    except ValueError:
        print("FATAL: KIRRA_GOVERNOR_VK_HEX is not valid hex", file=sys.stderr)
        return 2
    if len(governor_vk) != 32:
        print("FATAL: KIRRA_GOVERNOR_VK_HEX must be 32 bytes (64 hex)", file=sys.stderr)
        return 2

    freshness_window_ms = _req_int("KIRRA_FRESHNESS_WINDOW_MS")
    control_period_ms = _req_int("KIRRA_CONTROL_PERIOD_MS")
    missed_periods = _req_int("KIRRA_MISSED_PERIODS")
    stop_decel_mps2 = _req_float("KIRRA_STOP_DECEL_MPS2")
    vx_max = _req_float("KIRRA_DEMO_VX_MAX")
    vz_max = _req_float("KIRRA_DEMO_VZ_MAX")
    motor_port = _req("KIRRA_MOTOR_PORT")
    topic = os.environ.get("KIRRA_RELEASE_TOPIC", "/kirra/release")

    # The verify core (fail-closed: raises on a NULL handle).
    consumer = KirraConsumer(
        governor_vk,
        freshness_window_ms=freshness_window_ms,
        control_period_ms=control_period_ms,
        missed_periods=missed_periods,
        stop_decel_mps2=stop_decel_mps2,
        vx_max=vx_max,
        vz_max=vz_max,
    )

    # 🔴 OWN the motor board. This is the sole opener/writer of /dev/myserial.
    bot = Rosmaster(com=motor_port)
    bot.create_receive_threading()

    def safe_stop() -> None:
        # SS-002 shutdown guarantee: command zero, best-effort, idempotent.
        try:
            bot.set_car_motion(0.0, 0.0, 0.0)
        except Exception as e:  # noqa: BLE001 — shutdown must not raise past here.
            print(f"safe_stop: set_car_motion(0,0,0) raised: {e}", file=sys.stderr)

    rclpy.init()
    node = Node("kirra_motor_consumer")
    node.get_logger().info(
        f"KIRRA consumer OWNS {motor_port} (sole writer). topic={topic} "
        f"envelope: vx_max={vx_max} m/s vz_max={vz_max} rad/s (DEMO backstop; "
        f"Kirra's checker is the authority). Vendor base node must NOT be running."
    )

    alarm_announced = False

    def actuate(linear: float, angular: float) -> None:
        # v_y = 0 (skid-steer demo; no lateral). linear→v_x, angular→v_z, both
        # already clamped by the Rust capture seam.
        bot.set_car_motion(linear, 0.0, angular)

    def on_msg(msg: UInt8MultiArray) -> None:
        nonlocal alarm_announced
        data = bytes(msg.data)
        # STRICT wire parse (Copilot #901): exactly 32 (unsigned) or exactly
        # 128 (signed). Anything else is malformed — ignored with a warn, never
        # sliced into an oversized token (which raised ValueError in the
        # callback and let hostile input take down the consumer).
        parsed = split_frame(data)
        if parsed is None:
            node.get_logger().warn(
                f"malformed release frame ({len(data)} bytes; expected 32 or 128) — ignored"
            )
            return
        payload, token = parsed
        res = consumer.on_frame(payload, token, now_ms())
        if res.write == 1:
            actuate(res.linear, res.angular)
        else:
            name = REFUSAL_NAMES.get(res.refusal_code, f"code{res.refusal_code}")
            node.get_logger().warn(f"REFUSED ({name}) — no motor write")
        # 🔴 #892 loud, DISTINCT key-mismatch diagnostic (latched once).
        h = consumer.health()
        if h.key_mismatch_alarm == 1 and not alarm_announced:
            alarm_announced = True
            node.get_logger().error("🔴 " + consumer.alarm_explanation())
        elif h.key_mismatch_alarm == 0:
            alarm_announced = False

    node.create_subscription(UInt8MultiArray, topic, on_msg, 10)

    # Liveness clock: on_tick every control period drives the SS-002 decel-to-zero
    # ramp when releases stop arriving (never hold-last). A refusal does NOT feed
    # liveness — a flood starves into the stop exactly as silence does.
    def on_timer() -> None:
        t = consumer.on_tick(now_ms())
        if t.write == 1:
            actuate(t.linear, t.angular)

    node.create_timer(control_period_ms / 1000.0, on_timer)

    # Guaranteed stop on any exit path (SIGINT / SIGTERM / exception / normal).
    def handle_signal(signum, _frame):  # noqa: ANN001
        node.get_logger().warn(f"signal {signum} → safe stop + shutdown")
        safe_stop()
        rclpy.shutdown()

    signal.signal(signal.SIGINT, handle_signal)
    signal.signal(signal.SIGTERM, handle_signal)

    try:
        rclpy.spin(node)
    finally:
        # Belt-and-braces: even a panic/spin-exit stops the wheels.
        safe_stop()
        try:
            node.destroy_node()
        except Exception:  # noqa: BLE001
            pass
        consumer.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
