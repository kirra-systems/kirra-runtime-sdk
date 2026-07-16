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
- Open-loop PWM (no encoder speed-matching). The two rear wheels differ ~28% in
  ticks/s at equal PWM (calibration Phase A), so v0 does not perfectly track a
  `m/s` command — acceptable for low-speed v0; closing the loop is §9.
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
