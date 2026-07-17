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
    KIRRA_EXPECTED_CAR_TYPE    (x3 mode only) board drive-model register value
                               the platform mapping expects (X3=1); mismatch →
                               refuse. Not read in r2_ackermann mode.
Optional:
    KIRRA_RELEASE_TOPIC        (default /kirra/release)
    KIRRA_CONSUMER_LIB         explicit path to libkirra_consumer_ffi.so
    KIRRA_DRIVE_MODE           actuation last-hop: "x3_set_car_motion" (default)
                               or "r2_ackermann" (Path B). Off by default →
                               existing behaviour byte-identical.

r2_ackermann mode ADDITIONALLY requires the measured R2 calibration (fail-closed
via r2_drive.calibration_from_env; a missing value aborts startup):
    KIRRA_R2_WHEELBASE_M, KIRRA_R2_V_PER_PWM, KIRRA_R2_PWM_MAX,
    KIRRA_R2_STEER_UNITS_PER_RAD, KIRRA_R2_DELTA_MAX_RAD, KIRRA_R2_STEER_SIGN,
    KIRRA_R2_CENTER_TRIM  (+ optional KIRRA_R2_DRIVE_DEADBAND_PWM).
In this mode the consumer sets car-type 5 at init (enables the AKM steering
servo, §2a) and NEVER calls set_car_motion.
"""

from __future__ import annotations

import os
import signal
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from kirra_ffi import KirraConsumer, REFUSAL_NAMES, split_frame  # noqa: E402
from r2_drive import (  # noqa: E402
    ClosedLoopSpeedMatcher,
    R2CalibrationError,
    apply_actuation,
    calibration_from_env,
    closed_loop_enabled,
    r2_safe_stop,
    speed_match_params_from_env,
    translate,
)

# Actuation last-hop selector (R2 Path B). Default = the existing X3 path
# (set_car_motion), byte-identical. r2_ackermann swaps in the KIRRA-governed
# Ackermann last-hop (set_motor + AKM steering) — see
# docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md §6. Off by default.
DRIVE_MODE_X3 = "x3_set_car_motion"
DRIVE_MODE_R2 = "r2_ackermann"
R2_CAR_TYPE = 5  # Ackermann drive model; RAM-volatile, re-asserted every start.


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
    drive_mode = (os.environ.get("KIRRA_DRIVE_MODE") or DRIVE_MODE_X3).strip() or DRIVE_MODE_X3
    if drive_mode not in (DRIVE_MODE_X3, DRIVE_MODE_R2):
        print(f"FATAL: KIRRA_DRIVE_MODE must be {DRIVE_MODE_X3!r} or "
              f"{DRIVE_MODE_R2!r}, got {drive_mode!r}", file=sys.stderr)
        return 2
    # x3 mode asserts the board's car-type register against the platform
    # mapping; r2 mode SETS car-type 5 (below) and loads the measured
    # calibration instead — fail-closed if any measured value is missing.
    expected_car_type = _req_int("KIRRA_EXPECTED_CAR_TYPE") if drive_mode == DRIVE_MODE_X3 else None
    r2_cal = None
    r2_matcher = None  # closed-loop speed matcher (r2 mode + KIRRA_R2_CLOSED_LOOP)
    if drive_mode == DRIVE_MODE_R2:
        try:
            r2_cal = calibration_from_env(os.environ)
        except R2CalibrationError as e:
            print(f"FATAL: R2 drive mode requires a measured calibration "
                  f"profile: {e}", file=sys.stderr)
            return 2
        # §9 closed-loop per-wheel speed matching — OPT-IN (default off → open-loop
        # equal-PWM, byte-identical). Fail-closed: if the flag is on but the
        # controller params are incomplete, refuse to start (never silently fall
        # back to open-loop when closed-loop was requested).
        if closed_loop_enabled(os.environ):
            try:
                r2_matcher = ClosedLoopSpeedMatcher(speed_match_params_from_env(os.environ, r2_cal))
            except R2CalibrationError as e:
                print(f"FATAL: KIRRA_R2_CLOSED_LOOP is on but its params are "
                      f"incomplete: {e}", file=sys.stderr)
                return 2
    # Opt-in closed-loop diagnostics: throttled per-cycle log of target + filtered
    # L/R speed + commanded PWMs, so the loop is OBSERVABLE over SSH (accepted
    # frames are otherwise silent). Off by default; log-only, no actuation change.
    r2_cl_debug = (
        r2_matcher is not None
        and (os.environ.get("KIRRA_R2_CLOSED_LOOP_DEBUG") or "").strip().lower() in ("1", "true", "yes", "on")
    )
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

    # Closed-loop speed matching reads the wheel encoders (get_motor_encoder),
    # which ONLY update from the MCU's auto-report frames. Enable auto-report once
    # here, fail-closed — without it every encoder read is a stale 0, so the
    # matcher would see a dead wheel and immediately trip a stall→MRC fault (or
    # never converge). Open-loop paths never read encoders, so this is scoped to
    # the closed-loop consumer only (byte-identical otherwise).
    if r2_matcher is not None:
        try:
            bot.set_auto_report_state(True)
        except Exception as e:  # noqa: BLE001 — no encoder feed → refuse to start
            print(f"FATAL: closed-loop drive needs encoder auto-report, but "
                  f"set_auto_report_state(True) failed: {e}", file=sys.stderr)
            return 2

    def _settle_car_type() -> "int | None":
        # Read the board's car-type register with settle retries (~3 s total; the
        # register needs receive-thread settle after a car-type write). The valid
        # range is 1..6, so the vendor lib's -1 is a "not-yet-reported" SENTINEL,
        # NOT a reading — treat it (and None) as unread and keep polling; return
        # the first NON-NEGATIVE value. Never-readable → None (caller fail-closes).
        for _ in range(12):
            time.sleep(0.25)
            try:
                raw = bot.get_car_type_from_machine()
                # Guard the int() too: a non-numeric sentinel (str/float/None)
                # must keep the poll going, never raise past the settle loop
                # (fail-closed lives in the caller on a None return).
                t = int(raw) if raw is not None else None
            except Exception:  # noqa: BLE001 — unreadable/unconvertible → retry
                t = None
            if t is not None and t >= 0:
                return t
        return None

    if drive_mode == DRIVE_MODE_R2:
        # 🔴 R2 Path-B init: enable the AKM steering servo by setting car-type 5
        # (§2a — RAM-volatile, re-asserted every start). set_motor drive is
        # car-type independent; set_car_motion is NEVER called in this mode
        # (type 5 breaks its drive, which is fine — Path B does not use it).
        # Verify the board accepted 5 (fail-closed: an inert servo must not
        # silently drive), then apply the measured steering centre trim.
        bot.set_car_type(R2_CAR_TYPE)
        observed_type = _settle_car_type()
        if observed_type != R2_CAR_TYPE:
            print(
                f"FATAL: set_car_type({R2_CAR_TYPE}) did not take — board reads "
                f"{observed_type!r}. The AKM steering servo would be inert; "
                f"refusing to start (fail-closed).",
                file=sys.stderr,
            )
            return 2
        bot.set_akm_default_angle(int(round(r2_cal.center_trim)))
    else:
        # 🔴 Drive-model assertion (hardware finding, HARDWARE_FINDINGS_R2X3.md):
        # the board's car-type register selects the DRIVE MODEL the same
        # set_car_motion bytes execute under — mecanum mixing (1) vs Ackermann
        # (5) — it is RAM-volatile, and R2 hardware shipped reporting 1
        # (cross-labeled image). A consumer validated against one model must
        # never drive a board configured for another: read the register and
        # REFUSE on mismatch/unreadable. KIRRA_EXPECTED_CAR_TYPE comes from the
        # platform mapping (kirra-install), never guessed here.
        observed_type = _settle_car_type()
        if observed_type != expected_car_type:
            print(
                f"FATAL: board car-type register reads {observed_type!r} but this "
                f"deployment expects {expected_car_type} (platform mapping). The "
                f"drive model does not match what governed commands were validated "
                f"against — refusing to start (fail-closed). Fix: flash/configure "
                f"the correct vendor base image for this platform.",
                file=sys.stderr,
            )
            return 2

    def safe_stop() -> None:
        # SS-002 shutdown guarantee: command zero, best-effort, idempotent.
        try:
            if drive_mode == DRIVE_MODE_R2:
                r2_safe_stop(bot)  # set_motor(0,0,0,0) + centre steering
            else:
                bot.set_car_motion(0.0, 0.0, 0.0)
        except Exception as e:  # noqa: BLE001 — shutdown must not raise past here.
            # Keep the primitive identifiable in the field: drive_mode selects
            # r2_safe_stop (set_motor+centre) vs set_car_motion(0,0,0).
            print(f"safe_stop raised (drive_mode={drive_mode}): {e}", file=sys.stderr)

    rclpy.init()
    node = Node("kirra_motor_consumer")
    node.get_logger().info(
        f"KIRRA consumer OWNS {motor_port} (sole writer). topic={topic} "
        f"envelope: vx_max={vx_max} m/s vz_max={vz_max} rad/s (DEMO backstop; "
        f"Kirra's checker is the authority). Vendor base node must NOT be running."
    )
    if drive_mode == DRIVE_MODE_R2:
        node.get_logger().info(
            f"r2_ackermann drive: {'CLOSED-LOOP speed matching' if r2_matcher else 'open-loop equal-PWM'}"
        )

    alarm_announced = False
    cl_debug_ctr = 0  # throttle counter for the closed-loop debug log

    def actuate(linear: float, angular: float) -> None:
        nonlocal cl_debug_ctr
        if drive_mode == DRIVE_MODE_R2:
            # Path B: the Ackermann last-hop runs AFTER verify (the same place
            # the x3 firmware mixing runs after verify). translate() is
            # fail-closed; an MRC/stop decision carries zeros, so this same call
            # stops the platform.
            act = translate(linear, angular, r2_cal)
            if r2_matcher is None:
                apply_actuation(bot, act)  # open-loop equal-PWM (default)
                return
            # §9 closed loop: translate() stays the safety front (non-finite /
            # spin-in-place / at-rest all yield is_mrc or reason=="stopped" with
            # zeros). Only when translate says "ok" do we KEEP its steer command
            # and REPLACE the two drive PWMs with the per-wheel speed-matched ones.
            if act.is_mrc or act.reason == "stopped":
                apply_actuation(bot, act)  # zeros → stop; drop the loop's history
                r2_matcher.reset()
                return
            enc = bot.get_motor_encoder()  # [m1(RL), m2, m3, m4(RR)]
            pwm_left, pwm_right, fault = r2_matcher.step(
                linear, enc[0], enc[3], time.monotonic()
            )
            if fault is not None:
                # A stalled wheel / non-finite feedback → MRC stop (never keep
                # driving a faulted loop). Reset so a stale delta can't resume it.
                r2_safe_stop(bot)
                r2_matcher.reset()
                node.get_logger().warn(f"closed-loop MRC ({fault}) — motors stopped")
                return
            bot.set_akm_steering_angle(act.steer_cmd)
            bot.set_motor(pwm_left, 0, 0, pwm_right)
            if r2_cl_debug:
                # Throttled (~every 5th cycle) so the loop is observable over SSH
                # without flooding: target, filtered L/R speed, commanded PWMs, steer.
                cl_debug_ctr += 1
                if cl_debug_ctr % 5 == 0:
                    fl, fr = r2_matcher.last_filtered_speeds()
                    # NaN (not 0.0) before the first measured cycle — 0.0 would read
                    # as a real "wheel stopped" sample and mislead debugging.
                    fl = float("nan") if fl is None else fl
                    fr = float("nan") if fr is None else fr
                    node.get_logger().info(
                        f"cl tgt={linear:.3f} ema_L={fl:.3f} ema_R={fr:.3f} "
                        f"pwm_L={pwm_left} pwm_R={pwm_right} steer={act.steer_cmd}"
                    )
        else:
            # x3: v_y = 0 (skid-steer demo; no lateral). linear→v_x, angular→v_z,
            # both already clamped by the Rust capture seam.
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
    # Same double-shutdown class as the publisher's hardware finding: shutdown
    # must be GUARDED (a second signal, or a race with teardown, must not call
    # rcl_shutdown twice), and spin's ExternalShutdownException — which rclpy
    # raises once the context is shut down from a signal handler — must be
    # caught, not tracebacked. Teardown-only; the safe_stop ordering (stop the
    # wheels BEFORE shutting down) is unchanged.
    from rclpy.executors import ExternalShutdownException

    def handle_signal(signum, _frame):  # noqa: ANN001
        node.get_logger().warn(f"signal {signum} → safe stop + shutdown")
        safe_stop()
        if rclpy.ok():
            rclpy.shutdown()

    signal.signal(signal.SIGINT, handle_signal)
    signal.signal(signal.SIGTERM, handle_signal)

    try:
        rclpy.spin(node)
    except (KeyboardInterrupt, ExternalShutdownException):
        pass  # clean signal-driven exit; the finally below stops the wheels
    finally:
        # Belt-and-braces: even a panic/spin-exit stops the wheels.
        safe_stop()
        try:
            node.destroy_node()
        except Exception:  # noqa: BLE001
            pass
        consumer.close()
        if rclpy.ok():
            rclpy.shutdown()
    return 0


if __name__ == "__main__":
    sys.exit(main())
