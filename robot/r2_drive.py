#!/usr/bin/env python3
"""R2 Path-B Ackermann last-hop translation (PURE, no hardware).

This is the R2 *doer* last-hop from the Path-B proposal
(`docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md` §§3-7): it turns an
already-governed twist `(v, omega)` into the raw vendor actuation for the
Yahboom R2 chassis — two rear motors via `set_motor(pwm,0,0,pwm)` plus the
front servo via `set_akm_steering_angle(cmd)` — doing the Ackermann kinematics
(L1) and the measured calibration (L2) in verifiable code instead of the
firmware mixer (which this cross-labeled X3 image does not implement for R2).

WHY THIS MODULE IS PURE. It performs NO serial I/O and imports NO ROS / vendor
library. It takes a twist and a calibration profile and returns a typed
`R2Actuation` decision; the caller (`kirra_motor_consumer.py`, the ADR-0033
verify chokepoint) applies that decision to the motor board AFTER the Rust
verify core has released the command. The verify gate stays ahead of this — the
same place the X3 firmware mixing runs after verify. Keeping the translation
pure is what lets it be host-tested exhaustively (`robot/r2_drive_test.py`)
without a robot.

NO INVENTED CONSTANTS. Every physical value (wheelbase, PWM↔speed slope,
command-units per radian, max road-wheel angle, steering sign, centre trim) is a
field of `R2DriveCalibration`, which the operator populates from bench
measurements (`robot/r2_drive_calibration_results.txt`). The dataclass validates
every field and REFUSES construction (raises `R2CalibrationError`) if any is
missing / non-finite / out of range — so the R2 drive path cannot start on
guessed numbers. Until the §5 calibration gaps are measured, no profile can be
built and the path stays inert. This is the "measure, do not invent" discipline
of the proposal made structural.

FAIL-CLOSED. A non-finite command, or a `v≈0 & |omega|>eps` command (spin in
place — not Ackermann-achievable), yields an MRC stop
(`set_motor(0,0,0,0)` + centre), never an unbounded actuation. A legitimate
commanded zero (`v≈0 & omega≈0`) yields a plain stop (wheels 0, steer centred),
distinct from an MRC fault.

Scope of v0 (per proposal §4/§9, the reviewed defaults):
- Equal-PWM rear drive (no Ackermann rear-speed differential; that needs the
  rear track `t`, a later refinement). Both rear channels get the same PWM.
- Open-loop PWM by default (no encoder speed-matching). The two rear wheels
  differ ~34% in ticks/s at equal PWM (calibration Phase A + RR confirm), so the
  open-loop path does not perfectly track a `m/s` command — acceptable for the
  tethered low-speed v0. Closing the loop (§9) is now available as an OPT-IN
  controller (`speed_match_step` / `ClosedLoopSpeedMatcher`, below): per-wheel
  encoder speed-matching toward the SAME governed target, fail-closed and
  envelope-respecting. It is a separate, gated path — `translate` is unchanged.
- Proportional PWM (`pwm = v / v_per_pwm`); the measured deadband is a later
  refinement (`drive_deadband_pwm`, default 0 → the reviewed §7 form).
"""

from __future__ import annotations

import math
from dataclasses import dataclass

# Stopped-speed epsilon. Mirrors the codebase-wide Degraded STOP_EPSILON_MPS
# (0.05 m/s) used by the decel-to-stop gate — the same "is the vehicle at rest"
# threshold, not a new tuned value.
STOP_EPS_MPS = 0.05

# Rotation-while-stopped detection tolerance (rad/s). A NUMERICAL guard for the
# not-Ackermann-achievable case (v≈0, omega≠0), not a physical calibration: any
# commanded yaw above this while stopped is a fault → MRC. KIRRA also refuses
# such a command upstream (proposal §4); this is the last-hop backstop.
OMEGA_EPS_RPS = 1e-3

# The vendor steering command envelope: set_akm_steering_angle takes [-45, +45]
# command units about the servo centre (proposal §2/§4). Fixed by the vendor
# library, not a calibration.
STEER_CMD_LIMIT = 45


class R2CalibrationError(ValueError):
    """A calibration profile field is missing, non-finite, or out of range.

    Raised by `R2DriveCalibration` construction — fail-closed: the R2 drive
    path must abort rather than run on an incomplete/guessed profile.
    """


def _finite(x: object) -> bool:
    # Reject bool explicitly: it subclasses int, so without this a YAML/JSON
    # mistake like `wheelbase_m: true` would pass as 1.0 and slip through the
    # range checks — weakening the fail-closed calibration validation.
    if isinstance(x, bool):
        return False
    return isinstance(x, (int, float)) and math.isfinite(float(x))


def _iround(x: float) -> int:
    """Round to nearest int, ties away from zero (sign-symmetric for reverse)."""
    return int(math.floor(abs(x) + 0.5)) * (1 if x >= 0.0 else -1)


def _clamp(x: float, lo: float, hi: float) -> float:
    return lo if x < lo else hi if x > hi else x


@dataclass(frozen=True)
class R2DriveCalibration:
    """Measured Path-B calibration (proposal §5). Every field is bench-measured;
    none is invented. Construction fails closed on any invalid field.

    Fields:
        wheelbase_m:          L, rear-to-front axle distance (m). > 0.
        v_per_pwm:            PWM↔speed slope, (m/s) per PWM unit. > 0.
        pwm_max:              max |PWM| this deployment commands. (0, 100].
        steer_units_per_rad:  K, servo command-units per radian of road-wheel
                              angle. > 0.
        delta_max_rad:        max road-wheel angle at full lock (rad). (0, pi/2).
        steer_sign:           +1 or -1 — which command sign steers LEFT (omega>0).
                              Bench-verified (proposal §4 sign check).
        center_trim:          set_akm_default_angle value for physical straight
                              ahead. [60, 120]. Applied ONCE by the consumer at
                              init (not per-command); carried here for the profile.
        drive_deadband_pwm:   optional PWM offset added to |pwm| when moving, to
                              cross the motor deadband. >= 0. Default 0.0 → the
                              reviewed §7 proportional form.
    """

    wheelbase_m: float
    v_per_pwm: float
    pwm_max: float
    steer_units_per_rad: float
    delta_max_rad: float
    steer_sign: float
    center_trim: float
    drive_deadband_pwm: float = 0.0

    def __post_init__(self) -> None:
        for name in (
            "wheelbase_m",
            "v_per_pwm",
            "pwm_max",
            "steer_units_per_rad",
            "delta_max_rad",
            "steer_sign",
            "center_trim",
            "drive_deadband_pwm",
        ):
            if not _finite(getattr(self, name)):
                raise R2CalibrationError(f"{name} must be a finite number")
        if self.wheelbase_m <= 0.0:
            raise R2CalibrationError("wheelbase_m must be > 0")
        if self.v_per_pwm <= 0.0:
            raise R2CalibrationError("v_per_pwm must be > 0")
        if not (0.0 < self.pwm_max <= 100.0):
            raise R2CalibrationError("pwm_max must be in (0, 100]")
        if self.steer_units_per_rad <= 0.0:
            raise R2CalibrationError("steer_units_per_rad must be > 0")
        if not (0.0 < self.delta_max_rad < math.pi / 2.0):
            raise R2CalibrationError("delta_max_rad must be in (0, pi/2)")
        if self.steer_sign not in (-1.0, 1.0):
            raise R2CalibrationError("steer_sign must be exactly +1.0 or -1.0")
        if not (60.0 <= self.center_trim <= 120.0):
            raise R2CalibrationError("center_trim must be in [60, 120]")
        if self.drive_deadband_pwm < 0.0:
            raise R2CalibrationError("drive_deadband_pwm must be >= 0")


@dataclass(frozen=True)
class R2Actuation:
    """The raw actuation decision the consumer applies after verify.

    is_mrc:    True → a fail-closed minimum-risk-condition stop (a FAULT).
               False → a normal actuation (which may itself be a commanded zero).
    reason:    short tag: "ok", "stopped", or the MRC fault cause.
    pwm_left:  set_motor s1 (M1 = rear-left), signed PWM %.
    pwm_right: set_motor s4 (M4 = rear-right), signed PWM %.
    steer_cmd: set_akm_steering_angle argument, [-45, +45] command units.
    """

    is_mrc: bool
    reason: str
    pwm_left: int
    pwm_right: int
    steer_cmd: int


def mrc_stop(reason: str) -> R2Actuation:
    """A fail-closed stop: both rear motors 0, steering centred."""
    return R2Actuation(is_mrc=True, reason=reason, pwm_left=0, pwm_right=0, steer_cmd=0)


def translate(v: float, omega: float, cal: R2DriveCalibration) -> R2Actuation:
    """Turn a governed twist (v m/s, omega rad/s) into R2 raw actuation.

    Inputs are ALREADY bounded by KIRRA upstream. This layer is the exact
    Ackermann geometry + measured calibration + a fail-closed backstop; it does
    NOT re-open the safety envelope. See proposal §§3-7.
    """
    # Fail-closed: reject non-finite before any arithmetic.
    if not (_finite(v) and _finite(omega)):
        return mrc_stop("non_finite_command")

    at_rest = abs(v) <= STOP_EPS_MPS

    # Spin-in-place is not Ackermann-achievable → MRC (a car cannot rotate about
    # its own centre). KIRRA refuses this upstream; this is the last-hop backstop.
    if at_rest and abs(omega) > OMEGA_EPS_RPS:
        return mrc_stop("spin_in_place_not_achievable")

    # A legitimate commanded zero: wheels stopped, steering centred. Distinct
    # from an MRC fault (is_mrc=False) so the caller/telemetry can tell a
    # commanded halt from a fault stop.
    if at_rest:
        return R2Actuation(is_mrc=False, reason="stopped", pwm_left=0, pwm_right=0, steer_cmd=0)

    # L1 — steering geometry. Bicycle model: tan(delta) = L * (omega / v).
    # Using the curvature ratio makes the sign correct for either travel
    # direction. Clamp to the measured full-lock angle.
    delta = math.atan(cal.wheelbase_m * omega / v)
    delta = _clamp(delta, -cal.delta_max_rad, cal.delta_max_rad)

    # L2 — steering calibration: radians → servo command units, apply the
    # bench-verified sign, clamp to the vendor [-45, +45] envelope.
    steer_cmd = _iround(cal.steer_sign * cal.steer_units_per_rad * delta)
    steer_cmd = int(_clamp(float(steer_cmd), -STEER_CMD_LIMIT, STEER_CMD_LIMIT))

    # L2 — drive calibration: m/s → PWM (proportional v0 + optional measured
    # deadband). Equal PWM on both rear channels (no differential in v0).
    pwm_mag = abs(v) / cal.v_per_pwm + cal.drive_deadband_pwm
    pwm = _iround(math.copysign(pwm_mag, v))
    pwm = int(_clamp(float(pwm), -cal.pwm_max, cal.pwm_max))

    return R2Actuation(is_mrc=False, reason="ok", pwm_left=pwm, pwm_right=pwm, steer_cmd=steer_cmd)


# --------------------------------------------------------------------------
# Consumer-wiring helpers (still hardware-free / duck-typed).
#
# These let `kirra_motor_consumer.py` adopt Path B without inlining any logic:
# the env loader is fail-closed, and the actuation/stop appliers take any
# "Rosmaster-like" object (anything exposing set_motor / set_akm_steering_angle)
# so they are unit-testable against a recording fake, no vendor lib required.
# --------------------------------------------------------------------------

# The per-field env var names an operator sets for R2 drive mode. Every value is
# bench-measured (`r2_drive_calibration_results.txt`); there are NO defaults for
# the required ones — a missing/blank var fails the load closed.
_ENV_REQUIRED = {
    "wheelbase_m": "KIRRA_R2_WHEELBASE_M",
    "v_per_pwm": "KIRRA_R2_V_PER_PWM",
    "pwm_max": "KIRRA_R2_PWM_MAX",
    "steer_units_per_rad": "KIRRA_R2_STEER_UNITS_PER_RAD",
    "delta_max_rad": "KIRRA_R2_DELTA_MAX_RAD",
    "steer_sign": "KIRRA_R2_STEER_SIGN",
    "center_trim": "KIRRA_R2_CENTER_TRIM",
}
_ENV_OPTIONAL_DEADBAND = "KIRRA_R2_DRIVE_DEADBAND_PWM"


def calibration_from_env(env) -> R2DriveCalibration:
    """Build an `R2DriveCalibration` from `KIRRA_R2_*` env vars, fail-closed.

    `env` is a mapping (e.g. `os.environ`). A missing/blank required var, or a
    non-numeric value, raises `R2CalibrationError` — so R2 drive mode cannot
    start until every measured value is provided. Range validation is the
    dataclass's (`__post_init__`). The deadband is optional (default 0.0 → the
    reviewed §7 proportional drive).
    """
    kwargs: dict = {}
    for field, var in _ENV_REQUIRED.items():
        raw = env.get(var)
        if raw is None or str(raw).strip() == "":
            raise R2CalibrationError(f"{var} is unset — R2 drive mode requires a measured value")
        try:
            kwargs[field] = float(raw)
        except (TypeError, ValueError):
            raise R2CalibrationError(f"{var} must be a number, got {raw!r}") from None
    dead = env.get(_ENV_OPTIONAL_DEADBAND)
    if dead is not None and str(dead).strip() != "":
        try:
            kwargs["drive_deadband_pwm"] = float(dead)
        except (TypeError, ValueError):
            raise R2CalibrationError(f"{_ENV_OPTIONAL_DEADBAND} must be a number, got {dead!r}") from None
    return R2DriveCalibration(**kwargs)


def apply_actuation(bot, act: R2Actuation) -> None:
    """Apply an `R2Actuation` to a Rosmaster-like `bot`.

    Order: steer, then drive (proposal §7). `bot` must expose
    `set_akm_steering_angle(cmd)` and `set_motor(s1, s2, s3, s4)`. An MRC or
    commanded-stop decision already carries zeros, so this same call stops the
    platform — no special-casing at the call site.
    """
    bot.set_akm_steering_angle(act.steer_cmd)
    bot.set_motor(act.pwm_left, 0, 0, act.pwm_right)


def r2_safe_stop(bot) -> None:
    """The R2 SS-002 safe stop: zero both rear motors + centre the steering.

    Replaces the x3 `set_car_motion(0,0,0)` for Path B (proposal §6).
    """
    bot.set_motor(0, 0, 0, 0)
    bot.set_akm_steering_angle(0)


# ==========================================================================
# Closed-loop per-wheel speed matching (proposal §9, the beyond-tethered path).
#
# WHY. The RR-channel confirm (r2_drive_calibration_results.txt) proved the two
# rear wheels differ ~34% in speed at equal PWM AND that the imbalance is
# session-variable — so equal-PWM open-loop drifts, and a FIXED per-wheel PWM
# trim would not hold. It also proved both encoders are trustworthy and
# IDENTICALLY scaled (RR 825.6 vs RL 834.5 ticks/rev), which is exactly what a
# speed-matching controller needs. This closes the loop: each rear wheel's PWM is
# trimmed toward the SAME governed target ground speed, so the platform tracks
# straight regardless of per-wheel drift.
#
# FAIL-CLOSED + ENVELOPE-RESPECTING (never re-opens the safety envelope):
#   * The controlled SETPOINT is the governed target speed |v| — a ceiling KIRRA
#     already bounds. The loop only redistributes PWM to HIT it, never raises it.
#   * Each wheel's PWM is hard-capped at pwm_max and slew-limited per cycle.
#   * Pure P + per-wheel feedforward (no integral) → no windup / no slow ramp to
#     an unsafe command.
#   * A wheel commanded real PWM but not moving (encoder ~0) for stall_cycles
#     consecutive cycles → a FAULT the caller turns into an MRC stop — never
#     ramp-to-max on a stuck/faulted wheel.
#   * Non-finite feedback (target / encoder / clock) → fault, and the matcher
#     resets so a stale delta can never drive the next cycle.
#
# STILL PURE where it counts: `speed_match_step` is a pure function of
# (target, measured speeds, params, state). `ClosedLoopSpeedMatcher` adds only
# the encoder-delta / dt bookkeeping. Both are host-tested without a robot.
#
# GAINS ARE NOT YET HARDWARE-TUNED. The defaults are conservative and every gain
# is env-tunable; closing the loop on hardware (ELEVATED first) is the validation
# step. This module lands the algorithm + its fail-closed behaviour; the consumer
# encoder-read wiring behind KIRRA_R2_CLOSED_LOOP is the next slice.
# ==========================================================================

# Max control period (s) between encoder samples for a valid speed estimate. A
# longer gap (a paused loop, or the first cycle) → treat as a fresh start:
# command feedforward-only that cycle, never trust a stale/huge delta.
CLOSED_LOOP_MAX_DT_S = 0.5

# Conservative controller defaults (env-overridable). NOT hardware-tuned.
DEFAULT_SPEED_KP = 20.0            # PWM per (m/s of speed error)
DEFAULT_MAX_PWM_STEP = 5.0         # per-cycle slew cap on each wheel's PWM
DEFAULT_STALL_CYCLES = 5           # consecutive under-response cycles → fault
DEFAULT_STALL_MIN_PWM = 10.0       # "commanding real effort" threshold
DEFAULT_STALL_MIN_MPS = 0.02       # "not moving" threshold
# EMA low-pass on the measured wheel speed. The MCU encoder auto-report cadence is
# not phase-locked to the control loop, so raw per-cycle Δticks/dt ALIASES (a
# high/low sawtooth every other cycle — seen on the bench). A first-order EMA
# smooths that so the P-term acts on the real speed, not the sampling artifact.
# alpha in (0, 1]: 1.0 = no filter (raw); smaller = smoother + more lag.
DEFAULT_SPEED_EMA_ALPHA = 0.4


@dataclass(frozen=True)
class SpeedMatchParams:
    """Closed-loop controller calibration + gains. Fail-closed on any bad field.

    m_per_tick:        encoder scale (m/tick), > 0 — converts ticks/s → m/s.
    v_per_pwm_left:    RL feedforward slope, (m/s)/PWM, > 0.
    v_per_pwm_right:   RR feedforward slope, (m/s)/PWM, > 0.
    kp_pwm_per_mps:    proportional gain, PWM per (m/s error), >= 0.
    pwm_max:           per-wheel |PWM| cap, (0, 100].
    max_pwm_step:      per-cycle slew cap on each wheel's PWM, > 0.
    stall_cycles:      consecutive under-response cycles before a fault, int >= 1.
    stall_min_pwm:     |PWM| at/above which a non-moving wheel counts as stalled, >= 0.
    stall_min_mps:     |speed| below which a wheel counts as "not moving", >= 0.
    ema_alpha:         EMA weight on the NEW measured speed, in (0, 1]. 1.0 = no
                       filter. Applied in ClosedLoopSpeedMatcher before the P-term
                       to smooth aliased encoder reads. Default 1.0 (back-compat);
                       the env loader defaults it to DEFAULT_SPEED_EMA_ALPHA.
    """

    m_per_tick: float
    v_per_pwm_left: float
    v_per_pwm_right: float
    kp_pwm_per_mps: float
    pwm_max: float
    max_pwm_step: float
    stall_cycles: int
    stall_min_pwm: float
    stall_min_mps: float
    ema_alpha: float = 1.0

    def __post_init__(self) -> None:
        for name in (
            "m_per_tick",
            "v_per_pwm_left",
            "v_per_pwm_right",
            "kp_pwm_per_mps",
            "pwm_max",
            "max_pwm_step",
            "stall_min_pwm",
            "stall_min_mps",
            "ema_alpha",
        ):
            if not _finite(getattr(self, name)):
                raise R2CalibrationError(f"{name} must be a finite number")
        if self.m_per_tick <= 0.0:
            raise R2CalibrationError("m_per_tick must be > 0")
        if self.v_per_pwm_left <= 0.0 or self.v_per_pwm_right <= 0.0:
            raise R2CalibrationError("v_per_pwm_left/right must be > 0")
        if self.kp_pwm_per_mps < 0.0:
            raise R2CalibrationError("kp_pwm_per_mps must be >= 0")
        if not (0.0 < self.pwm_max <= 100.0):
            raise R2CalibrationError("pwm_max must be in (0, 100]")
        if self.max_pwm_step <= 0.0:
            raise R2CalibrationError("max_pwm_step must be > 0")
        # stall_cycles is a count — reject bool (subclasses int) and < 1.
        if isinstance(self.stall_cycles, bool) or not isinstance(self.stall_cycles, int) or self.stall_cycles < 1:
            raise R2CalibrationError("stall_cycles must be an int >= 1")
        if self.stall_min_pwm < 0.0 or self.stall_min_mps < 0.0:
            raise R2CalibrationError("stall_min_pwm/stall_min_mps must be >= 0")
        if not (0.0 < self.ema_alpha <= 1.0):
            raise R2CalibrationError("ema_alpha must be in (0, 1]")


@dataclass
class SpeedMatchState:
    """Mutable controller state carried between cycles (per-wheel PWM + stall)."""

    pwm_left: float = 0.0
    pwm_right: float = 0.0
    stall_left: int = 0
    stall_right: int = 0


def _match_one(target_v: float, meas_v: float, prev_pwm: float, v_per_pwm_side: float, p: SpeedMatchParams) -> float:
    # Per-wheel feedforward toward the governed target + a proportional trim on
    # the measured error, slew-limited around the previous command, then hard-
    # capped at pwm_max. The setpoint is |target_v| (a ceiling); P has no integral
    # so it cannot wind up past it.
    ff = target_v / v_per_pwm_side
    raw = ff + p.kp_pwm_per_mps * (target_v - meas_v)
    raw = _clamp(raw, prev_pwm - p.max_pwm_step, prev_pwm + p.max_pwm_step)
    return _clamp(raw, -p.pwm_max, p.pwm_max)


def speed_match_step(
    target_v: float,
    meas_v_left: float,
    meas_v_right: float,
    params: SpeedMatchParams,
    state: SpeedMatchState,
):
    """One closed-loop control step (PURE apart from mutating `state`).

    Returns `(pwm_left:int, pwm_right:int, fault:str|None)`. On a fault the PWMs
    are `(0, 0)` — the caller applies an MRC stop. Non-finite feedback and a
    per-wheel stall are the two fault causes. `target_v` is the governed signed
    speed (the caller has already handled the at-rest / MRC cases via `translate`).
    """
    if not (_finite(target_v) and _finite(meas_v_left) and _finite(meas_v_right)):
        return 0, 0, "non_finite_feedback"

    pl = _match_one(target_v, meas_v_left, state.pwm_left, params.v_per_pwm_left, params)
    pr = _match_one(target_v, meas_v_right, state.pwm_right, params.v_per_pwm_right, params)

    def _bump(count: int, pwm: float, meas: float) -> int:
        # A wheel commanding real effort but not moving is stalling — count it;
        # any real motion (or no real command) resets the counter.
        if abs(pwm) >= params.stall_min_pwm and abs(meas) < params.stall_min_mps:
            return count + 1
        return 0

    state.stall_left = _bump(state.stall_left, pl, meas_v_left)
    state.stall_right = _bump(state.stall_right, pr, meas_v_right)
    state.pwm_left = pl
    state.pwm_right = pr

    if state.stall_left >= params.stall_cycles:
        return 0, 0, "wheel_stall_left"
    if state.stall_right >= params.stall_cycles:
        return 0, 0, "wheel_stall_right"
    return _iround(pl), _iround(pr), None


class ClosedLoopSpeedMatcher:
    """Encoder-fed wrapper around `speed_match_step`.

    The consumer feeds RAW cumulative encoder counts + a monotonic timestamp each
    control cycle; this computes per-wheel ground speeds (Δticks · m_per_tick / dt)
    and runs the pure step. The FIRST valid cycle (or any cycle after a stale/
    invalid interval) has no trustworthy speed estimate → it commands
    FEEDFORWARD-ONLY and seeds the state, so the next cycle's slew is relative to
    the feedforward, not 0. Fail-closed: non-finite input resets and faults.
    """

    def __init__(self, params: SpeedMatchParams) -> None:
        self.params = params
        self.state = SpeedMatchState()
        self._prev = None  # (enc_left, enc_right, now_s) or None
        self._ema_left = None  # filtered measured speed (None until first sample)
        self._ema_right = None

    def reset(self) -> None:
        """Drop the prior sample + zero the controller (call on stop / MRC / fault)."""
        self.state = SpeedMatchState()
        self._prev = None
        self._ema_left = None
        self._ema_right = None

    def _filter(self, meas: float, prev_ema) -> float:
        # First-order EMA: seed on the first sample, else blend. Smooths the
        # aliased per-cycle encoder speed (see DEFAULT_SPEED_EMA_ALPHA).
        a = self.params.ema_alpha
        return meas if prev_ema is None else a * meas + (1.0 - a) * prev_ema

    def _feedforward(self, target_v: float):
        p = self.params
        pl = _clamp(target_v / p.v_per_pwm_left, -p.pwm_max, p.pwm_max)
        pr = _clamp(target_v / p.v_per_pwm_right, -p.pwm_max, p.pwm_max)
        self.state.pwm_left = pl
        self.state.pwm_right = pr
        self.state.stall_left = 0
        self.state.stall_right = 0
        return _iround(pl), _iround(pr), None

    def step(self, target_v: float, enc_left: float, enc_right: float, now_s: float):
        """Advance one cycle. Returns `(pwm_left:int, pwm_right:int, fault:str|None)`."""
        if not (_finite(target_v) and _finite(enc_left) and _finite(enc_right) and _finite(now_s)):
            self.reset()
            return 0, 0, "non_finite_feedback"
        prev = self._prev
        self._prev = (enc_left, enc_right, now_s)
        if prev is None:
            return self._feedforward(target_v)
        dt = now_s - prev[2]
        # A non-positive or too-large interval yields no trustworthy speed → treat
        # as a fresh start (feedforward-only), never divide by / trust it.
        if not (dt > 0.0) or dt > CLOSED_LOOP_MAX_DT_S:
            return self._feedforward(target_v)
        meas_v_left = (enc_left - prev[0]) * self.params.m_per_tick / dt
        meas_v_right = (enc_right - prev[1]) * self.params.m_per_tick / dt
        # Low-pass the aliased raw speed before the P-term / stall check.
        self._ema_left = self._filter(meas_v_left, self._ema_left)
        self._ema_right = self._filter(meas_v_right, self._ema_right)
        return speed_match_step(target_v, self._ema_left, self._ema_right, self.params, self.state)


# Closed-loop env vars. The enable flag is read by the consumer; the params come
# from the measured RR confirm (left slope reuses the open-loop KIRRA_R2_V_PER_PWM).
ENV_CLOSED_LOOP_ENABLE = "KIRRA_R2_CLOSED_LOOP"
_ENV_CL_M_PER_TICK = "KIRRA_R2_M_PER_TICK"
_ENV_CL_V_PER_PWM_RIGHT = "KIRRA_R2_V_PER_PWM_RIGHT"
_ENV_CL_KP = "KIRRA_R2_SPEED_KP"
_ENV_CL_MAX_STEP = "KIRRA_R2_SPEED_MAX_PWM_STEP"
_ENV_CL_STALL_CYCLES = "KIRRA_R2_SPEED_STALL_CYCLES"
_ENV_CL_STALL_MIN_PWM = "KIRRA_R2_SPEED_STALL_MIN_PWM"
_ENV_CL_STALL_MIN_MPS = "KIRRA_R2_SPEED_STALL_MIN_MPS"
_ENV_CL_EMA_ALPHA = "KIRRA_R2_SPEED_EMA_ALPHA"


def closed_loop_enabled(env) -> bool:
    """True iff KIRRA_R2_CLOSED_LOOP is set truthy (1/true/yes/on)."""
    raw = env.get(ENV_CLOSED_LOOP_ENABLE)
    return raw is not None and str(raw).strip().lower() in ("1", "true", "yes", "on")


def speed_match_params_from_env(env, cal: R2DriveCalibration) -> SpeedMatchParams:
    """Build `SpeedMatchParams` from env + the loaded `cal`, fail-closed.

    Required (measured): KIRRA_R2_M_PER_TICK, KIRRA_R2_V_PER_PWM_RIGHT. The LEFT
    slope reuses `cal.v_per_pwm` (KIRRA_R2_V_PER_PWM) and pwm_max reuses
    `cal.pwm_max`. Gains are optional (conservative defaults; NOT hardware-tuned).
    """

    def _req_float(var: str) -> float:
        raw = env.get(var)
        if raw is None or str(raw).strip() == "":
            raise R2CalibrationError(f"{var} is unset — closed-loop drive requires a measured value")
        try:
            return float(raw)
        except (TypeError, ValueError):
            raise R2CalibrationError(f"{var} must be a number, got {raw!r}") from None

    def _opt_float(var: str, default: float) -> float:
        raw = env.get(var)
        if raw is None or str(raw).strip() == "":
            return default
        try:
            return float(raw)
        except (TypeError, ValueError):
            raise R2CalibrationError(f"{var} must be a number, got {raw!r}") from None

    def _opt_int(var: str, default: int) -> int:
        raw = env.get(var)
        if raw is None or str(raw).strip() == "":
            return default
        try:
            return int(str(raw).strip())
        except (TypeError, ValueError):
            raise R2CalibrationError(f"{var} must be an integer, got {raw!r}") from None

    return SpeedMatchParams(
        m_per_tick=_req_float(_ENV_CL_M_PER_TICK),
        v_per_pwm_left=cal.v_per_pwm,
        v_per_pwm_right=_req_float(_ENV_CL_V_PER_PWM_RIGHT),
        kp_pwm_per_mps=_opt_float(_ENV_CL_KP, DEFAULT_SPEED_KP),
        pwm_max=cal.pwm_max,
        max_pwm_step=_opt_float(_ENV_CL_MAX_STEP, DEFAULT_MAX_PWM_STEP),
        stall_cycles=_opt_int(_ENV_CL_STALL_CYCLES, DEFAULT_STALL_CYCLES),
        stall_min_pwm=_opt_float(_ENV_CL_STALL_MIN_PWM, DEFAULT_STALL_MIN_PWM),
        stall_min_mps=_opt_float(_ENV_CL_STALL_MIN_MPS, DEFAULT_STALL_MIN_MPS),
        ema_alpha=_opt_float(_ENV_CL_EMA_ALPHA, DEFAULT_SPEED_EMA_ALPHA),
    )
