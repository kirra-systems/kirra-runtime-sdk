#!/usr/bin/env python3
"""KIRRA verifying motor consumer — Yahboom Rosmaster X3 (ADR-0033 chokepoint, physical).

This node IS the motor bringup. Per ADR-0033 we do NOT stand up the vendor
`/cmd_vel` driver and retrofit a fence — the verifying consumer is the ONLY
thing that opens the motor board (`/dev/myserial`) and the ONLY thing that calls
`Rosmaster.set_car_motion`. A command is actuated ONLY if the Rust verify core
(libkirra_consumer_ffi, ADR-0033 decision (c)) releases it.

🔴 EVIDENCE-BOUND V2 ONLY. This node consumes the 272-byte V2 frame
(payload(176) || token(96)) whose signed payload carries not just the enforced
twist but the perception scan/camera/tracker identity, the platform profile
digest and the Occy proposal digest it was authored against. The core verifies
the Ed25519 signature over exactly those bytes AND that the evidence identity
is acceptable: the pinned profile digest matches, the perception frame has not
been superseded, sequence and nonce strictly advance, and the signed expiry has
not passed. The 128-byte V1 frame is REFUSED here — it names no evidence, so
accepting it would strip the binding while still producing motion. V1 remains
available in the Rust library and in `kirra_ffi.KirraConsumer` for the bench
publisher and compatibility, but never on this live path.

🔴 Nothing here re-implements verification. Decoding, signature, expiry,
lifetime, replay, nonce, profile digest, evidence supersession, proposal digest
and liveness are ALL decided in Rust and returned across the FFI; this node
validates its own config, splits fixed-size wire bytes, and actuates whatever
twist the core releases. It never decodes or compares an evidence field itself.

🔴 No vendor base node. Do NOT launch `yahboomcar_bringup` alongside this — it
would be a second, UNFENCED writer to the motor board. This node owning
`/dev/myserial` (exclusive) is the structural guarantee.

Wire input: topic KIRRA_RELEASE_TOPIC (default /kirra/release),
`std_msgs/UInt8MultiArray`, data = payload(176) || token(96) — exactly 272
bytes. Any other length is malformed and ignored (no motor write).

Config — ALL required, NO defaults (fail-closed; a missing var aborts):
    KIRRA_GOVERNOR_VK_HEX      64-hex Ed25519 public key this consumer pins
    KIRRA_PROFILE_DIGEST       64 LOWERCASE hex (32 bytes) — the platform
                               profile digest this robot is pinned to. TRUSTED
                               CONFIGURATION: it is never read from an incoming
                               payload, the ROS proposal, or a verifier
                               response. The core refuses any release whose
                               signed profile digest differs, so a robot cannot
                               be driven under another platform's envelope.
                               Missing/uppercase/non-hex/wrong-length → abort.
    KIRRA_FRESHNESS_WINDOW_MS  MAXIMUM token lifetime (ms) the core will accept.
                               V2 enforces the signed `expires_at_ms`; this is
                               the upper bound on how long a mint may be valid
                               (a signer cannot hand itself an eternal token).
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
    KIRRA_ALLOW_SHARED_SERIAL  "1" acknowledges a bring-up run on a port that
                               FAILS the ADR-0033 Tier-3 exclusivity sentinel
                               (wrong owner / loose mode / another holder).
                               Default: violations REFUSE startup (fail-closed;
                               see robot/serial_exclusivity.py + the tightened
                               udev rule 99-kirra-serial-exclusivity.rules)
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

import math
import os
import signal
import sys
import time
from collections import namedtuple
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from clock_step_guard import ClockStepGuard  # noqa: E402
from kirra_ffi import (  # noqa: E402
    BOUND_FRAME_LEN,
    BoundKirraConsumer,
    bound_refusal_name,
    parse_profile_digest_hex,
    split_bound_frame,
)
from r2_drive import (  # noqa: E402
    ClosedLoopSpeedMatcher,
    R2CalibrationError,
    _wrap_angle,
    apply_actuation,
    calibration_from_env,
    closed_loop_enabled,
    odom_step,
    odom_zero,
    r2_safe_stop,
    speed_match_params_from_env,
    translate,
    yaw_to_quaternion_zw,
)

# Actuation last-hop selector (R2 Path B). Default = the existing X3 path
# (set_car_motion), byte-identical. r2_ackermann swaps in the KIRRA-governed
# Ackermann last-hop (set_motor + AKM steering) — see
# docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md §6. Off by default.
DRIVE_MODE_X3 = "x3_set_car_motion"
DRIVE_MODE_R2 = "r2_ackermann"
# 🔴 R2CP: the last hop is the Kirra MCU protocol, NOT the vendor board.
# In this mode the vendor library is never imported, never constructed, and
# never called — see the r2cp branch in main(). There is deliberately NO
# fallback to r2_ackermann or x3: if the R2CP peer cannot be identified,
# motion stays unavailable. A fallback would silently return actuation to the
# unverified vendor path precisely when the verified one failed.
DRIVE_MODE_R2CP = "r2cp"


def _now_us() -> int:
    """Monotonic microseconds for the R2CP source-time field.

    Monotonic, never wall clock: a clock step backwards would make fresh
    commands look stale and stale ones fresh, which is AOU-TIMESYNC-001 and
    exactly the property the peer's freshness rule depends on.
    """
    return time.monotonic_ns() // 1000


_COMMAND_ID = 0


def _next_command_id() -> int:
    """A strictly advancing correlation id for the ACK to echo.

    Not a security property — the peer's replay watermark is on the frame
    SEQUENCE, which the Rust link owns. This is only so a log can pair a
    command with its acknowledgement.
    """
    global _COMMAND_ID
    _COMMAND_ID = (_COMMAND_ID + 1) & 0xFFFF_FFFF
    return _COMMAND_ID
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
    # (AOU-TIMESYNC-001): the core compares this to the token's signed
    # issued_at_ms / expires_at_ms.
    return int(time.time() * 1000)


def read_profile_digest(environ) -> "bytes | None":
    """The pinned platform profile digest, from TRUSTED configuration only.

    Returns 32 bytes, or None if `KIRRA_PROFILE_DIGEST` is absent/empty or is
    not exactly 64 lowercase hex characters (the caller aborts startup).

    🔴 This value must NEVER be derived from the incoming 176-byte payload, the
    ROS proposal, a verifier response, or any other command-carried field. The
    whole point of pinning is that the robot states independently which
    platform profile it is, so the core can refuse a release minted under a
    different one. Reading it from the command would let the command authorize
    itself.
    """
    return parse_profile_digest_hex((environ.get("KIRRA_PROFILE_DIGEST") or "").strip())


# One decision for one inbound release message. `reason` is None on a release
# and a short stable tag otherwise (used for latched logging).
FrameDecision = namedtuple("FrameDecision", ["write", "linear", "angular", "reason"])


def decide_bound_frame(consumer, data: bytes, now_ms_val: int) -> FrameDecision:
    """Split one V2 wire frame and ask the core what to do with it.

    Python decides NOTHING about legitimacy here: a well-formed 272-byte frame
    is handed to Rust verbatim and the core's `write` flag is the only thing
    consulted. Python's own refusal is limited to the one thing it must do
    itself — refusing bytes that are not a V2 frame at all, so the FFI is never
    handed a short buffer.
    """
    parsed = split_bound_frame(data)
    if parsed is None:
        # Includes a 128-byte V1 frame: refused, never downgraded.
        return FrameDecision(False, 0.0, 0.0, f"MALFORMED_FRAME_{len(data)}B")

    payload, token = parsed
    res = consumer.on_frame(payload, token, now_ms_val)
    if res.write == 1:
        return FrameDecision(True, res.linear, res.angular, None)
    return FrameDecision(False, 0.0, 0.0, bound_refusal_name(res.refusal_code))


# Health counters worth naming when a refusal episode starts/changes. Ordered
# so the most diagnostic ones read first.
_BOUND_HEALTH_FIELDS = (
    "signature_invalid",
    "digest_mismatch",
    "profile_digest_mismatch",
    "perception_frame_superseded",
    "sequence_not_advanced",
    "nonce_not_advanced",
    "expired",
    "future_issued",
    "lifetime_too_long",
    "undecodable",
    "no_token",
)


def format_bound_health(health) -> str:
    """A compact one-line diagnostic of the non-zero refusal counters."""
    parts = [
        f"{name}={getattr(health, name)}"
        for name in _BOUND_HEALTH_FIELDS
        if getattr(health, name, 0)
    ]
    if getattr(health, "silent", 0) == 1:
        parts.append("silent=1")
    if getattr(health, "key_mismatch_alarm", 0) == 1:
        parts.append("key_mismatch_alarm=1")
    return " ".join(parts) if parts else "no refusals recorded"


def make_frame_handler(
    consumer,
    actuate,
    warn,
    error,
    now_fn=now_ms,
    mono_fn=_now_us,
    clock_guard=None,
):
    """Build the `/kirra/release` callback.

    Factored out of `main` so the release path is testable without ROS, a motor
    board, or a real .so: the caller injects the consumer, the single serial
    chokepoint (`actuate`) and the log sinks.

    `clock_guard` (#1214) is the wall-clock step detector. The token's signed
    `expires_at_ms` is compared against `now_fn()`, so a step in that clock
    changes what "expired" means without changing anything observable — the
    check goes on returning confident answers about a quantity that has stopped
    meaning what it asserts. When the guard reports a step, the frame is refused
    and the core is NOT consulted, so no watermark advances and liveness starves
    into the SS-002 stop exactly as silence does. `None` disables the guard.

    Latched logging (the project's existing style, cf. the interceptor's relay
    latch): a refusal logs when the reason CHANGES, not every cycle — releases
    arrive at the control rate, so an unfixed fault would otherwise flood the
    log. The suppressed count is reported when the state changes, so a storm is
    still visible without being noisy. A successful release re-arms the latch,
    so a fresh episode always logs again.
    """
    state = {"reason": None, "suppressed": 0, "alarm": False}

    def _flush_suppressed() -> None:
        if state["suppressed"]:
            warn(f"(+{state['suppressed']} further {state['reason']} refusals suppressed)")
        state["suppressed"] = 0

    def handle(data: bytes) -> None:
        wall_ms = now_fn()

        # Clock check FIRST, before the core sees the frame. A token whose
        # freshness cannot be judged must not reach the gate at all: releasing it
        # would advance the sequence, nonce and perception watermarks on the
        # strength of a timestamp comparison that is no longer meaningful.
        if clock_guard is not None:
            verdict = clock_guard.observe(wall_ms, mono_fn() // 1000)
            if not verdict.ok:
                reason = verdict.reason
                if reason == state["reason"]:
                    state["suppressed"] += 1
                else:
                    _flush_suppressed()
                    state["reason"] = reason
                    warn(
                        f"REFUSED ({reason}) — no motor write; the wall clock "
                        f"used for token freshness diverged from the monotonic "
                        f"clock by {verdict.divergence_ms} ms. Holding for "
                        f"{verdict.hold_remaining_ms} ms so every token minted "
                        f"before the step expires under any offset."
                    )
                return

        decision = decide_bound_frame(consumer, data, wall_ms)
        # One health snapshot per message: it feeds both the refusal diagnostic
        # and the alarm latch below, and each call is an FFI crossing.
        health = consumer.health()

        if decision.write:
            # 🔴 The single serial chokepoint. Reached only when the Rust core
            # released this exact frame.
            actuate(decision.linear, decision.angular)
            if state["reason"] is not None:
                _flush_suppressed()
                state["reason"] = None
        elif decision.reason == state["reason"]:
            state["suppressed"] += 1
        else:
            _flush_suppressed()
            state["reason"] = decision.reason
            warn(f"REFUSED ({decision.reason}) — no motor write; "
                 f"{format_bound_health(health)}")

        # 🔴 #892 loud, DISTINCT key-mismatch diagnostic (latched once).
        if health.key_mismatch_alarm == 1 and not state["alarm"]:
            state["alarm"] = True
            error("🔴 " + consumer.alarm_explanation())
        elif health.key_mismatch_alarm == 0:
            state["alarm"] = False

    return handle


def make_tick_handler(consumer, actuate, now_fn=now_ms):
    """Build the liveness-clock callback (SS-002 decel-to-zero ramp).

    A refusal does NOT feed liveness — the core's watermark only advances on a
    release — so a refusal flood starves into the same stop as silence.
    """

    def on_tick() -> None:
        tick = consumer.on_tick(now_fn())
        if tick.write == 1:
            actuate(tick.linear, tick.angular)

    return on_tick


def main() -> int:
    # ROS + vendor lib imported inside main so the file parses/syntax-checks on a
    # host without them (CI py_compile); they are required on the robot.
    import rclpy
    from rclpy.node import Node
    from std_msgs.msg import UInt8MultiArray

    # The mode is resolved BEFORE the vendor import so an r2cp deployment does
    # not need Rosmaster_Lib installed at all. "No vendor library" is then a
    # property of the image, not a promise about which code path runs.
    drive_mode = (os.environ.get("KIRRA_DRIVE_MODE") or DRIVE_MODE_X3).strip() or DRIVE_MODE_X3
    if drive_mode not in (DRIVE_MODE_X3, DRIVE_MODE_R2, DRIVE_MODE_R2CP):
        print(f"FATAL: KIRRA_DRIVE_MODE must be {DRIVE_MODE_X3!r}, "
              f"{DRIVE_MODE_R2!r} or {DRIVE_MODE_R2CP!r}, got {drive_mode!r}",
              file=sys.stderr)
        return 2
    if drive_mode != DRIVE_MODE_R2CP:
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

    # 🔴 The pinned platform profile digest — trusted configuration, never a
    # command-carried field. Without it the core could not tell a release
    # minted for THIS platform from one minted for another envelope.
    profile_digest = read_profile_digest(os.environ)
    if profile_digest is None:
        raw = os.environ.get("KIRRA_PROFILE_DIGEST")
        print(
            "FATAL: KIRRA_PROFILE_DIGEST must be exactly 64 LOWERCASE hexadecimal "
            "characters (the 32-byte platform profile digest this robot is pinned "
            f"to) — refusing to start. Got {raw!r}. It comes from trusted robot "
            "configuration only; it is never taken from an incoming release "
            "payload or a verifier response.",
            file=sys.stderr,
        )
        return 2

    maximum_token_lifetime_ms = _req_int("KIRRA_FRESHNESS_WINDOW_MS")
    control_period_ms = _req_int("KIRRA_CONTROL_PERIOD_MS")
    missed_periods = _req_int("KIRRA_MISSED_PERIODS")
    stop_decel_mps2 = _req_float("KIRRA_STOP_DECEL_MPS2")
    vx_max = _req_float("KIRRA_DEMO_VX_MAX")
    vz_max = _req_float("KIRRA_DEMO_VZ_MAX")
    motor_port = _req("KIRRA_MOTOR_PORT")
    # x3 mode asserts the board's car-type register against the platform
    # mapping; r2 mode SETS car-type 5 (below) and loads the measured
    # calibration instead — fail-closed if any measured value is missing.
    expected_car_type = _req_int("KIRRA_EXPECTED_CAR_TYPE") if drive_mode == DRIVE_MODE_X3 else None
    # r2cp link settings. Required in that mode only; the MCU owns everything
    # physical (PWM scaling, steering geometry, limits, watchdog policy), so
    # none of the R2 measured calibration is read here.
    r2cp_handshake_timeout_ms = (
        _req_int("KIRRA_R2CP_HANDSHAKE_TIMEOUT_MS") if drive_mode == DRIVE_MODE_R2CP else 0
    )
    r2cp_command_timeout_ms = (
        _req_int("KIRRA_R2CP_COMMAND_TIMEOUT_MS") if drive_mode == DRIVE_MODE_R2CP else 0
    )
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

    # R2 wheel-odometry — OPT-IN (default OFF → byte-identical, no /odom publish).
    # Publishes nav_msgs/Odometry from the rear-wheel encoders so the PLANNER sees
    # the ego advance and STOPS at the goal. Without it the doer runs on a static-
    # origin /odom crutch (never sees the goal approach) and a floor drive would
    # command forward indefinitely. Ackermann dead-reckoning: forward travel from
    # the mean rear-wheel distance, heading from the STEERING angle (never the
    # per-wheel encoder difference — the R2's independent rear motors differ ~34%
    # at equal PWM and would fabricate phantom yaw). r2 mode only.
    odom_enabled = (
        drive_mode == DRIVE_MODE_R2
        and (os.environ.get("KIRRA_R2_ODOM_ENABLED") or "").strip().lower() in ("1", "true", "yes", "on")
    )
    odom_m_per_tick = _req_float("KIRRA_R2_M_PER_TICK") if odom_enabled else 0.0
    # Parse the period ONLY when odom is enabled — a stray/invalid
    # KIRRA_R2_ODOM_PERIOD_MS must never fail-close a run that isn't using odom.
    odom_period_ms = (
        _req_int("KIRRA_R2_ODOM_PERIOD_MS")
        if (odom_enabled and os.environ.get("KIRRA_R2_ODOM_PERIOD_MS"))
        else 50
    )
    odom_topic = os.environ.get("KIRRA_R2_ODOM_TOPIC", "/odom")
    odom_frame = os.environ.get("KIRRA_R2_ODOM_FRAME", "odom")
    odom_child_frame = os.environ.get("KIRRA_R2_ODOM_CHILD_FRAME", "base_link")
    if odom_enabled and odom_m_per_tick <= 0.0:
        # A 0/negative scale silently yields a never-advancing (or reversed) odom
        # → the planner never sees the goal approach → the robot drives forever.
        # It is a measured physical constant like the others: fail closed.
        print("FATAL: KIRRA_R2_M_PER_TICK must be > 0 (odom)", file=sys.stderr)
        return 2
    if odom_enabled and odom_period_ms <= 0:
        print("FATAL: KIRRA_R2_ODOM_PERIOD_MS must be > 0", file=sys.stderr)
        return 2

    topic = os.environ.get("KIRRA_RELEASE_TOPIC", "/kirra/release")

    # The evidence-bound V2 verify core (fail-closed: raises on a NULL handle).
    consumer = BoundKirraConsumer(
        governor_vk,
        profile_digest,
        maximum_token_lifetime_ms=maximum_token_lifetime_ms,
        control_period_ms=control_period_ms,
        missed_periods=missed_periods,
        stop_decel_mps2=stop_decel_mps2,
        vx_max=vx_max,
        vz_max=vz_max,
    )

    # 🔴 OWN the motor board — and PROVE it (ADR-0033 Tier-3, #887). Sole
    # ownership of /dev/myserial was a docstring convention; the stock vendor
    # udev rule ships the port MODE 0777, so any process could actuate below
    # the checker. The sentinel makes the claim fail-closed at boot:
    # owner==this uid, mode 0600, no other holder — violations REFUSE startup
    # unless KIRRA_ALLOW_SHARED_SERIAL=1 acknowledges a bring-up run.
    import serial_exclusivity
    verdict, sentinel_msg = serial_exclusivity.startup_verdict(
        serial_exclusivity.preflight(motor_port),
        serial_exclusivity.ack_given(os.environ.get(serial_exclusivity.ACK_ENV)),
    )
    if verdict == "refuse":
        print(f"FATAL: {sentinel_msg}", file=sys.stderr)
        return 2
    print(("WARNING: " if verdict == "acknowledged" else "") + sentinel_msg,
          file=sys.stderr)

    r2cp_link = None
    if drive_mode == DRIVE_MODE_R2CP:
        # 🔴 No Rosmaster construction, no car-type read or write, no
        # set_motor, no set_car_motion. The vendor library was not even
        # imported. `bot` stays None and every use of it below is behind a
        # mode branch.
        #
        # ⚠ TIOCEXCL is NOT claimed on this path: the file descriptor lives
        # inside the Rust link, and reaching into it from Python would defeat
        # the point of the opaque handle. The boot sentinel above (owner,
        # 0600, no other holder) and the udev rule still hold, so the port is
        # still not shareable — but kernel-enforced session exclusivity is a
        # recorded gap for the r2cp path, not something this claims to have.
        bot = None
        import kirra_r2cp as r2cp_mod

        # Same .so the verify core already loaded — one dlopen, one library.
        r2cp_lib = r2cp_mod.bind(consumer.lib)
        try:
            r2cp_link = r2cp_mod.open(r2cp_lib, motor_port, r2cp_handshake_timeout_ms)
        except r2cp_mod.R2cpError as e:
            # Exit CLASS matters: kirra-consumer.service carries
            # RestartPreventExitStatus=2, so returning 2 here for a peer that
            # is merely absent would leave the unit permanently dead after
            # someone reconnects the MCU. Only deterministic configuration
            # earns 2.
            print(f"FATAL: R2CP peer unavailable: {e}", file=sys.stderr)
            print("motion output remains unavailable", file=sys.stderr)
            return 2 if e.is_config else 1
        info = r2cp_link.peer_info()
        print(f"R2CP peer identified: {info.describe()}", file=sys.stderr)
        # Only claimed when the KERNEL confirms it (TIOCGEXCL read back in
        # Rust). Saying "exclusivity claimed" because a call returned 0 is the
        # kind of unearned assurance ADR-0033 Tier-3 exists to remove.
        if info.exclusive:
            print(f"serial exclusivity: TIOCEXCL confirmed on {motor_port} "
                  "(further non-root opens refused by the kernel)", file=sys.stderr)
        else:
            print(f"WARNING: kernel exclusivity NOT confirmed on {motor_port} — "
                  "the boot sentinel and udev mode still apply, but this session "
                  "is not kernel-locked", file=sys.stderr)
        try:
            r2cp_link.activate(r2cp_command_timeout_ms)
        except r2cp_mod.R2cpError as e:
            print(f"FATAL: R2CP peer would not arm: {e}", file=sys.stderr)
            print("motion output remains unavailable", file=sys.stderr)
            r2cp_link.send_stop(1_000_000, 0)
            r2cp_link.close()
            return 2 if e.is_config else 1
        print("R2CP drive mode armed", file=sys.stderr)
    else:
        bot = Rosmaster(com=motor_port)
    # Layer 2 — kernel-enforced exclusivity for the session: TIOCEXCL makes
    # every further non-root open of the port fail EBUSY while we hold it.
    # Best-effort by design: a vendor-lib layout change (fd not found) or a
    # non-tty degrades to a WARN — the boot sentinel + udev rule still hold.
    #
    # Skipped in r2cp mode: the descriptor lives inside the Rust link and there
    # is no vendor object to reach into. See the note in that branch.
    _ser_fd = None if bot is None else serial_exclusivity.serial_fd_of(bot)
    if _ser_fd is not None and serial_exclusivity.claim_exclusive(_ser_fd):
        print(f"serial exclusivity: TIOCEXCL claimed on {motor_port} "
              "(further opens refused by the kernel)", file=sys.stderr)
    elif bot is not None:
        print("WARNING: could not set TIOCEXCL on the motor port (vendor lib "
              "layout changed?) — boot sentinel + udev mode still enforce "
              "exclusivity", file=sys.stderr)
    if bot is not None:
        bot.create_receive_threading()

    # Closed-loop speed matching AND wheel-odometry read the encoders
    # (get_motor_encoder), which ONLY update from the MCU's auto-report frames.
    # Enable auto-report once here, fail-closed — without it every encoder read is
    # a stale 0, so the matcher would trip a stall→MRC fault (or never converge)
    # and odom would never advance. Open-loop-without-odom paths never read
    # encoders, so this is scoped to those consumers only (byte-identical otherwise).
    if bot is not None and (r2_matcher is not None or odom_enabled):
        try:
            bot.set_auto_report_state(True)
        except Exception as e:  # noqa: BLE001 — no encoder feed → refuse to start
            print(f"FATAL: encoder auto-report is required (closed-loop or odom) but "
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

    if drive_mode == DRIVE_MODE_R2CP:
        # No car-type register exists on this path: the peer is a Kirra MCU
        # reached over R2CP, already identified and armed above.
        pass
    elif drive_mode == DRIVE_MODE_R2:
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
            if drive_mode == DRIVE_MODE_R2CP:
                # Best-effort: True means the bytes were written, NOT that
                # anything stopped. The peer's own watchdog is what actually
                # halts a governed robot when commands cease.
                sent = r2cp_link is not None and r2cp_link.send_stop(
                    max(r2cp_command_timeout_ms, 1) * 1000, _now_us()
                )
                print(f"safe_stop: R2CP stop {'sent' if sent else 'NOT sent'}",
                      file=sys.stderr)
            elif drive_mode == DRIVE_MODE_R2:
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
        f"evidence-bound V2 only: {BOUND_FRAME_LEN}-byte frames "
        f"(payload 176 + token 96); V1 128-byte frames are REFUSED. "
        f"profile_digest pinned to {profile_digest.hex()[:16]}… "
        f"envelope: vx_max={vx_max} m/s vz_max={vz_max} rad/s (DEMO backstop; "
        f"Kirra's checker is the authority). Vendor base node must NOT be running."
    )
    if drive_mode == DRIVE_MODE_R2:
        node.get_logger().info(
            f"r2_ackermann drive: {'CLOSED-LOOP speed matching' if r2_matcher else 'open-loop equal-PWM'}"
        )

    cl_debug_ctr = 0  # throttle counter for the closed-loop debug log
    last_delta_rad = 0.0  # steering angle of the last actuation (odom heading source)

    def actuate(linear: float, angular: float) -> None:
        nonlocal cl_debug_ctr, last_delta_rad
        if drive_mode == DRIVE_MODE_R2CP:
            # 🔴 TRANSPORT ONLY. `linear`/`angular` are what the verify core
            # already released; nothing here decides, clamps or authorizes.
            # Exactly one MOTION_COMMAND per call — the release boundary is
            # upstream, and a refused release never reaches this function.
            #
            # Curvature from angular/linear: at a standstill there is no
            # kinematic curvature to express, so a stop carries zero rather
            # than an infinity the peer would refuse as non-finite.
            curvature = (angular / linear) if abs(linear) > 1e-6 else 0.0
            ack = r2cp_link.command(
                velocity_mps=linear,
                curvature_per_m=curvature,
                acceleration_limit_mps2=stop_decel_mps2,
                jerk_limit_mps3=0.0,
                valid_for_us=max(control_period_ms * missed_periods, 20) * 1000,
                command_id=_next_command_id(),
                source_time_us=_now_us(),
                timeout_ms=r2cp_command_timeout_ms,
            )
            if not ack.accepted:
                # A refusal is an expected operating condition (disarmed,
                # stale, replayed), not a link fault — surfaced, never
                # retried, and never worked around.
                print(f"R2CP command refused: result={ack.result} "
                      f"safety_state={ack.safety_state} "
                      f"timed_out={ack.timed_out}", file=sys.stderr)
            return
        if drive_mode == DRIVE_MODE_R2:
            # Path B: the Ackermann last-hop runs AFTER verify (the same place
            # the x3 firmware mixing runs after verify). translate() is
            # fail-closed; an MRC/stop decision carries zeros, so this same call
            # stops the platform.
            act = translate(linear, angular, r2_cal)
            # The odom integrator's heading source: the kinematic road-wheel angle
            # this actuation applied (0.0 for a stop/MRC). Latched for the odom timer.
            last_delta_rad = act.delta_rad
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

    # The release path. STRICT V2 wire parse (exactly 272 bytes) → the Rust
    # core → `actuate` only on a release. Built by a factory so the whole
    # decision path is host-testable without ROS or a motor (see
    # robot/bound_consumer_test.py); `actuate` stays the single serial
    # chokepoint.
    # #1214: the hold after a detected step is exactly the maximum token
    # lifetime the core will accept, so it outlasts every token that could have
    # been minted before the step and is still inside its window. Sharing the
    # one configured value means the two can never drift apart.
    frame_handler = make_frame_handler(
        consumer,
        actuate,
        warn=node.get_logger().warn,
        error=node.get_logger().error,
        clock_guard=ClockStepGuard(maximum_token_lifetime_ms),
    )

    def on_msg(msg: UInt8MultiArray) -> None:
        frame_handler(bytes(msg.data))

    node.create_subscription(UInt8MultiArray, topic, on_msg, 10)

    # Liveness clock: on_tick every control period drives the SS-002 decel-to-zero
    # ramp when releases stop arriving (never hold-last). A refusal does NOT feed
    # liveness — a flood starves into the stop exactly as silence does.
    node.create_timer(control_period_ms / 1000.0, make_tick_handler(consumer, actuate))

    # R2 wheel-odometry publisher (opt-in). Reads the rear-wheel encoders each
    # period and publishes an Ackermann dead-reckoning pose so the planner sees
    # the ego advance and stops at the goal. Read-only w.r.t. actuation — never
    # writes a motor; a transient encoder-read fault skips the tick (fail-soft).
    if odom_enabled:
        from nav_msgs.msg import Odometry  # robot-only dep; imported under the gate

        odom_pub = node.create_publisher(Odometry, odom_topic, 10)
        odom_state = odom_zero()
        odom_debug = (os.environ.get("KIRRA_R2_ODOM_DEBUG") or "").strip().lower() in (
            "1", "true", "yes", "on"
        )
        odom_dbg_ctr = 0

        def on_odom_timer() -> None:
            nonlocal odom_state, odom_dbg_ctr
            try:
                enc = bot.get_motor_encoder()  # [m1(RL), m2, m3, m4(RR)]
                left, right = int(enc[0]), int(enc[3])
            except Exception as e:  # noqa: BLE001 — transient read fault → skip this tick
                if odom_debug:
                    node.get_logger().warn(f"odom: get_motor_encoder failed: {e}")
                return
            if odom_debug:
                # ~1 Hz: raw rear encoder counts + integrated x, so a hand-spin
                # (wheels up) shows whether the MCU auto-report is delivering.
                odom_dbg_ctr += 1
                if odom_dbg_ctr % max(1, int(round(1000.0 / odom_period_ms))) == 0:
                    node.get_logger().info(
                        f"odom raw: encL={left} encR={right} x={odom_state.pose.x_m:.3f} "
                        f"yaw={odom_state.pose.yaw_rad:.3f} delta={last_delta_rad:.3f}"
                    )
            prev = odom_state.pose
            odom_state = odom_step(
                odom_state, left, right, last_delta_rad, odom_m_per_tick, r2_cal.wheelbase_m
            )
            cur = odom_state.pose
            dt = odom_period_ms / 1000.0
            dx, dy = cur.x_m - prev.x_m, cur.y_m - prev.y_m
            dist = math.hypot(dx, dy)
            # Signed forward speed: project the step onto the entry heading.
            forward = dist if (dx * math.cos(prev.yaw_rad) + dy * math.sin(prev.yaw_rad)) >= 0.0 else -dist
            dyaw = _wrap_angle(cur.yaw_rad - prev.yaw_rad)  # correct across the ±pi seam
            msg = Odometry()
            msg.header.stamp = node.get_clock().now().to_msg()
            msg.header.frame_id = odom_frame
            msg.child_frame_id = odom_child_frame
            msg.pose.pose.position.x = cur.x_m
            msg.pose.pose.position.y = cur.y_m
            qz, qw = yaw_to_quaternion_zw(cur.yaw_rad)
            msg.pose.pose.orientation.z = qz
            msg.pose.pose.orientation.w = qw
            msg.twist.twist.linear.x = forward / dt
            msg.twist.twist.angular.z = dyaw / dt
            # Honest covariance: this is UN-fused wheel dead-reckoning, loosely
            # trusted. A modest diagonal (x,y,yaw / vx,wz) tells a downstream
            # estimator not to treat it as ground truth; occy ignores it. The
            # unset DOF rows stay 0 (planar robot). Indices: 6x6 row-major.
            msg.pose.covariance[0] = 0.02   # x
            msg.pose.covariance[7] = 0.02   # y
            msg.pose.covariance[35] = 0.05  # yaw
            msg.twist.covariance[0] = 0.05  # vx
            msg.twist.covariance[35] = 0.10  # wz
            odom_pub.publish(msg)

        node.create_timer(odom_period_ms / 1000.0, on_odom_timer)
        node.get_logger().info(
            f"R2 wheel-odometry ON → {odom_topic} @ {1000.0 / odom_period_ms:.0f} Hz "
            f"(m/tick={odom_m_per_tick}, wheelbase={r2_cal.wheelbase_m} m; Ackermann "
            f"dead-reckoning: heading from steering, distance from mean rear encoder)."
        )

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
