#!/usr/bin/env python3
"""Host tests for the R2 Path-B Ackermann last-hop (`robot/r2_drive.py`).

Pure and hardware-free by construction — the module does no serial I/O and
imports no ROS/vendor library, so its geometry, calibration and fail-closed
behaviour are exercised exhaustively on a plain host (this is why the
translation was kept out of the consumer). Runs as a standalone script like the
other robot smoke tests (`python3 robot/r2_drive_test.py`, exit 1 on any
failure); also importable under pytest (each `test_*` asserts).

Covers: fail-closed calibration validation (no path starts on a bad/guessed
profile), Ackermann geometry + sign, all clamps, equal-PWM v0, reverse, and the
MRC / commanded-stop split.
"""

from __future__ import annotations

import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from r2_drive import (  # noqa: E402
    CLOSED_LOOP_MAX_DT_S,
    STEER_CMD_LIMIT,
    ClosedLoopSpeedMatcher,
    R2Actuation,
    R2CalibrationError,
    R2DriveCalibration,
    SpeedMatchParams,
    SpeedMatchState,
    apply_actuation,
    calibration_from_env,
    closed_loop_enabled,
    mrc_stop,
    r2_safe_stop,
    speed_match_params_from_env,
    speed_match_step,
    translate,
)


class _FakeBot:
    """Records the vendor calls the consumer would make — no hardware."""

    def __init__(self) -> None:
        self.calls: list = []

    def set_motor(self, s1, s2, s3, s4) -> None:
        self.calls.append(("set_motor", s1, s2, s3, s4))

    def set_akm_steering_angle(self, cmd) -> None:
        self.calls.append(("set_akm_steering_angle", cmd))


def _full_env() -> dict:
    return {
        "KIRRA_R2_WHEELBASE_M": "0.229",
        "KIRRA_R2_V_PER_PWM": "0.0145",
        "KIRRA_R2_PWM_MAX": "60",
        "KIRRA_R2_STEER_UNITS_PER_RAD": "140",
        "KIRRA_R2_DELTA_MAX_RAD": "0.5",
        "KIRRA_R2_STEER_SIGN": "-1",
        "KIRRA_R2_CENTER_TRIM": "90",
    }


def _valid_cal(**overrides) -> R2DriveCalibration:
    """A structurally-valid profile for tests. The NUMBERS ARE TEST FIXTURES,
    not measured hardware values — the real profile comes from the bench
    (`r2_drive_calibration_results.txt`); these just exercise the math."""
    base = dict(
        wheelbase_m=1.0,
        v_per_pwm=0.02,
        pwm_max=100.0,
        steer_units_per_rad=40.0,
        delta_max_rad=1.0,  # > pi/4 so the exact case below does not clamp
        steer_sign=1.0,
        center_trim=90.0,
        drive_deadband_pwm=0.0,
    )
    base.update(overrides)
    return R2DriveCalibration(**base)


# --------------------------------------------------------------------------
# Calibration validation — fail-closed on any missing/invalid field.
# --------------------------------------------------------------------------

def test_calibration_accepts_valid() -> None:
    cal = _valid_cal()
    assert cal.wheelbase_m == 1.0


def test_calibration_rejects_bad_fields() -> None:
    bad_cases = [
        dict(wheelbase_m=0.0),
        dict(wheelbase_m=-1.0),
        dict(wheelbase_m=float("nan")),
        dict(v_per_pwm=0.0),
        dict(v_per_pwm=-0.01),
        dict(v_per_pwm=float("inf")),
        dict(pwm_max=0.0),
        dict(pwm_max=100.1),
        dict(pwm_max=-5.0),
        dict(steer_units_per_rad=0.0),
        dict(steer_units_per_rad=-1.0),
        dict(delta_max_rad=0.0),
        dict(delta_max_rad=math.pi / 2.0),  # not strictly less than pi/2
        dict(delta_max_rad=2.0),
        dict(steer_sign=0.0),
        dict(steer_sign=2.0),
        dict(steer_sign=0.5),
        dict(center_trim=59.0),
        dict(center_trim=121.0),
        dict(drive_deadband_pwm=-0.1),
        dict(drive_deadband_pwm=float("nan")),
        # bool subclasses int — a YAML/JSON `true` must NOT pass as 1.0.
        dict(wheelbase_m=True),
        dict(v_per_pwm=True),
        dict(steer_sign=True),
        dict(pwm_max=False),
    ]
    for override in bad_cases:
        try:
            _valid_cal(**override)
        except R2CalibrationError:
            continue
        raise AssertionError(f"calibration accepted invalid field: {override}")


# --------------------------------------------------------------------------
# Geometry + calibration — the happy path.
# --------------------------------------------------------------------------

def test_straight_is_centered_and_equal_pwm() -> None:
    cal = _valid_cal()
    out = translate(0.5, 0.0, cal)
    assert not out.is_mrc and out.reason == "ok"
    assert out.steer_cmd == 0
    assert out.pwm_left == out.pwm_right
    assert out.pwm_left > 0


def test_exact_hand_computed_case() -> None:
    # L=1, v=1, omega=1 -> delta=atan(1)=pi/4; K=40 -> 40*pi/4=31.4159 -> 31.
    # v_per_pwm=0.02 -> pwm=1/0.02=50.
    cal = _valid_cal()
    out = translate(1.0, 1.0, cal)
    assert out.pwm_left == 50 and out.pwm_right == 50
    assert out.steer_cmd == 31, out.steer_cmd


def test_steer_sign_selects_left_direction() -> None:
    # omega>0 (left / CCW). steer_sign flips which command sign is "left".
    left_neg = translate(1.0, 1.0, _valid_cal(steer_sign=-1.0))
    left_pos = translate(1.0, 1.0, _valid_cal(steer_sign=1.0))
    assert left_neg.steer_cmd == -31
    assert left_pos.steer_cmd == 31
    # Right turn (omega<0) is the mirror.
    right_pos = translate(1.0, -1.0, _valid_cal(steer_sign=-1.0))
    assert right_pos.steer_cmd == 31


def test_delta_clamped_to_full_lock() -> None:
    # Huge omega saturates delta to delta_max before the unit conversion.
    cal = _valid_cal(delta_max_rad=0.5, steer_units_per_rad=50.0)  # 50*0.5=25 < 45
    out = translate(0.5, 1000.0, cal)
    assert out.steer_cmd == 25, out.steer_cmd


def test_steer_cmd_clamped_to_envelope() -> None:
    # K*delta_max exceeds the vendor [-45,45] envelope -> clamp.
    cal = _valid_cal(delta_max_rad=0.5, steer_units_per_rad=200.0)  # 200*0.5=100
    out = translate(0.5, 1000.0, cal)
    assert out.steer_cmd == STEER_CMD_LIMIT == 45
    out_r = translate(0.5, -1000.0, cal)
    assert out_r.steer_cmd == -45


def test_pwm_clamped_to_max() -> None:
    cal = _valid_cal(v_per_pwm=0.02, pwm_max=100.0)  # 5.0/0.02 = 250 -> 100
    out = translate(5.0, 0.0, cal)
    assert out.pwm_left == 100 and out.pwm_right == 100


def test_reverse_is_symmetric() -> None:
    cal = _valid_cal()
    fwd = translate(1.0, 0.0, cal)
    rev = translate(-1.0, 0.0, cal)
    assert rev.pwm_left == -fwd.pwm_left
    assert rev.pwm_right == -fwd.pwm_right


def test_deadband_offsets_pwm() -> None:
    # Same speed, a measured deadband adds to |pwm|.
    plain = translate(1.0, 0.0, _valid_cal(drive_deadband_pwm=0.0))
    dead = translate(1.0, 0.0, _valid_cal(drive_deadband_pwm=5.0))
    assert dead.pwm_left == plain.pwm_left + 5


# --------------------------------------------------------------------------
# Fail-closed.
# --------------------------------------------------------------------------

def test_non_finite_is_mrc() -> None:
    cal = _valid_cal()
    for v, w in ((float("nan"), 0.0), (0.5, float("inf")), (float("inf"), float("nan"))):
        out = translate(v, w, cal)
        assert out.is_mrc and out.reason == "non_finite_command"
        assert out.pwm_left == 0 and out.pwm_right == 0 and out.steer_cmd == 0


def test_spin_in_place_is_mrc() -> None:
    cal = _valid_cal()
    out = translate(0.0, 1.0, cal)
    assert out.is_mrc and out.reason == "spin_in_place_not_achievable"
    out2 = translate(0.01, -0.5, cal)  # within STOP_EPS, real yaw commanded
    assert out2.is_mrc and out2.reason == "spin_in_place_not_achievable"


def test_commanded_zero_is_plain_stop_not_mrc() -> None:
    cal = _valid_cal()
    out = translate(0.0, 0.0, cal)
    assert not out.is_mrc and out.reason == "stopped"
    assert out.pwm_left == 0 and out.pwm_right == 0 and out.steer_cmd == 0


def test_mrc_stop_helper() -> None:
    out = mrc_stop("whatever")
    assert out.is_mrc and out.pwm_left == 0 and out.pwm_right == 0 and out.steer_cmd == 0


# --------------------------------------------------------------------------
# Consumer-wiring helpers (env loader + appliers), still hardware-free.
# --------------------------------------------------------------------------

def test_calibration_from_env_builds_valid() -> None:
    cal = calibration_from_env(_full_env())
    assert cal.wheelbase_m == 0.229 and cal.steer_sign == -1.0
    assert cal.drive_deadband_pwm == 0.0  # optional, defaults to proportional


def test_calibration_from_env_optional_deadband() -> None:
    env = _full_env()
    env["KIRRA_R2_DRIVE_DEADBAND_PWM"] = "4.7"
    assert calibration_from_env(env).drive_deadband_pwm == 4.7


def test_calibration_from_env_fails_closed_on_missing() -> None:
    for var in list(_full_env()):
        env = _full_env()
        del env[var]
        try:
            calibration_from_env(env)
        except R2CalibrationError:
            continue
        raise AssertionError(f"env loader accepted a missing {var}")


def test_calibration_from_env_fails_closed_on_blank_and_nonnumeric() -> None:
    for bad in ("", "   ", "nan-ish", "true"):
        env = _full_env()
        env["KIRRA_R2_V_PER_PWM"] = bad
        try:
            calibration_from_env(env)
        except R2CalibrationError:
            continue
        raise AssertionError(f"env loader accepted v_per_pwm={bad!r}")


def test_calibration_from_env_fails_closed_on_out_of_range() -> None:
    # A syntactically-numeric but out-of-domain value must still fail (range
    # validation in the dataclass): steer_sign=2 is not +/-1.
    env = _full_env()
    env["KIRRA_R2_STEER_SIGN"] = "2"
    try:
        calibration_from_env(env)
    except R2CalibrationError:
        return
    raise AssertionError("env loader accepted steer_sign=2")


def test_apply_actuation_order_is_steer_then_drive() -> None:
    bot = _FakeBot()
    apply_actuation(bot, R2Actuation(is_mrc=False, reason="ok", pwm_left=27, pwm_right=27, steer_cmd=-12))
    assert bot.calls == [
        ("set_akm_steering_angle", -12),
        ("set_motor", 27, 0, 0, 27),
    ]


def test_apply_actuation_mrc_writes_zeros() -> None:
    bot = _FakeBot()
    apply_actuation(bot, mrc_stop("non_finite_command"))
    assert bot.calls == [
        ("set_akm_steering_angle", 0),
        ("set_motor", 0, 0, 0, 0),
    ]


def test_r2_safe_stop_zeros_motors_and_centers() -> None:
    bot = _FakeBot()
    r2_safe_stop(bot)
    assert bot.calls == [
        ("set_motor", 0, 0, 0, 0),
        ("set_akm_steering_angle", 0),
    ]


# --------------------------------------------------------------------------
# Closed-loop speed matcher (§9): pure controller core + fail-closed behaviour.
# --------------------------------------------------------------------------

def _valid_params(**overrides) -> SpeedMatchParams:
    """Structurally-valid controller params (TEST FIXTURES, not measured)."""
    base = dict(
        m_per_tick=0.00025,
        v_per_pwm_left=0.0145,
        v_per_pwm_right=0.0194,
        kp_pwm_per_mps=20.0,
        pwm_max=40.0,
        max_pwm_step=100.0,  # wide by default so tests see the raw control law
        stall_cycles=5,
        stall_min_pwm=10.0,
        stall_min_mps=0.02,
    )
    base.update(overrides)
    return SpeedMatchParams(**base)


def test_speed_match_params_reject_bad_fields() -> None:
    for bad in (
        dict(m_per_tick=0.0),
        dict(m_per_tick=-1.0),
        dict(v_per_pwm_left=0.0),
        dict(v_per_pwm_right=-0.1),
        dict(kp_pwm_per_mps=-1.0),
        dict(pwm_max=0.0),
        dict(pwm_max=101.0),
        dict(max_pwm_step=0.0),
        dict(stall_cycles=0),
        dict(stall_cycles=True),  # bool must be rejected (subclasses int)
        dict(stall_min_pwm=-1.0),
        dict(m_per_tick=float("nan")),
        dict(max_pwm_step=float("inf")),
    ):
        try:
            _valid_params(**bad)
        except R2CalibrationError:
            continue
        raise AssertionError(f"expected R2CalibrationError for {bad}")


def test_speed_match_slow_wheel_gets_more_pwm() -> None:
    # Both wheels below target; the SLOWER wheel (bigger error) must get more PWM.
    p = _valid_params()
    st = SpeedMatchState()
    pl, pr, fault = speed_match_step(0.30, 0.10, 0.20, p, st)
    assert fault is None
    assert pl > pr, f"slower left wheel should get more PWM: {pl} vs {pr}"


def test_speed_match_feedforward_when_on_target() -> None:
    # Zero error → command is exactly the per-wheel feedforward (target/v_per_pwm).
    p = _valid_params()
    st = SpeedMatchState()
    target = 0.30
    pl, pr, fault = speed_match_step(target, target, target, p, st)
    assert fault is None
    assert pl == round(target / p.v_per_pwm_left)   # ~21
    assert pr == round(target / p.v_per_pwm_right)  # ~15


def test_speed_match_pwm_capped() -> None:
    # A large error cannot push PWM past pwm_max.
    p = _valid_params(pwm_max=25.0)
    st = SpeedMatchState()
    pl, pr, fault = speed_match_step(0.40, 0.0, 0.0, p, st)
    assert fault is None
    assert abs(pl) <= 25 and abs(pr) <= 25


def test_speed_match_slew_limited() -> None:
    # From a zero prior command, one cycle can move each wheel at most max_pwm_step.
    p = _valid_params(max_pwm_step=3.0)
    st = SpeedMatchState()  # prev pwm 0
    pl, pr, fault = speed_match_step(0.40, 0.0, 0.0, p, st)
    assert fault is None
    assert abs(pl) <= 3 and abs(pr) <= 3


def test_speed_match_non_finite_feedback_faults() -> None:
    p = _valid_params()
    st = SpeedMatchState()
    for tv, ml, mr in ((float("nan"), 0.1, 0.1), (0.3, float("inf"), 0.1), (0.3, 0.1, float("nan"))):
        pl, pr, fault = speed_match_step(tv, ml, mr, p, st)
        assert fault == "non_finite_feedback"
        assert (pl, pr) == (0, 0)


def test_speed_match_stall_faults_after_threshold() -> None:
    # A wheel commanded real PWM but not moving trips a fault at stall_cycles.
    p = _valid_params(stall_cycles=3, max_pwm_step=100.0, pwm_max=40.0)
    st = SpeedMatchState()
    faults = []
    for _ in range(3):
        _, _, f = speed_match_step(0.30, 0.0, 0.30, p, st)  # left stalled, right on target
        faults.append(f)
    assert faults[0] is None and faults[1] is None
    assert faults[2] == "wheel_stall_left", faults


def test_speed_match_motion_resets_stall() -> None:
    p = _valid_params(stall_cycles=2, max_pwm_step=100.0)
    st = SpeedMatchState()
    speed_match_step(0.30, 0.0, 0.30, p, st)      # left stall count -> 1
    assert st.stall_left == 1
    speed_match_step(0.30, 0.30, 0.30, p, st)     # left now moving -> reset
    assert st.stall_left == 0


def test_matcher_first_cycle_is_feedforward_only() -> None:
    p = _valid_params()
    m = ClosedLoopSpeedMatcher(p)
    pl, pr, fault = m.step(0.30, enc_left=1000, enc_right=1000, now_s=10.0)
    assert fault is None
    assert pl == round(0.30 / p.v_per_pwm_left)
    assert pr == round(0.30 / p.v_per_pwm_right)


def test_matcher_second_cycle_uses_measured_speed() -> None:
    # RR spun far more ticks than RL over the same dt → RL (slower) gets more PWM.
    p = _valid_params()
    m = ClosedLoopSpeedMatcher(p)
    m.step(0.30, enc_left=0, enc_right=0, now_s=0.0)          # seed
    pl, pr, fault = m.step(0.30, enc_left=400, enc_right=1200, now_s=0.1)
    assert fault is None
    assert pl > pr, f"slower RL should be trimmed up: {pl} vs {pr}"


def test_matcher_bad_dt_falls_back_to_feedforward() -> None:
    p = _valid_params()
    for now2 in (0.0, -1.0, CLOSED_LOOP_MAX_DT_S + 1.0):  # dt<=0 or too large
        m = ClosedLoopSpeedMatcher(p)
        m.step(0.30, 0, 0, 0.0)
        pl, pr, fault = m.step(0.30, 9999, 9999, now2)
        assert fault is None
        assert pl == round(0.30 / p.v_per_pwm_left)   # feedforward, ignored the huge delta
        assert pr == round(0.30 / p.v_per_pwm_right)


def test_matcher_non_finite_resets_and_faults() -> None:
    p = _valid_params()
    m = ClosedLoopSpeedMatcher(p)
    m.step(0.30, 100, 100, 1.0)
    pl, pr, fault = m.step(float("nan"), 200, 200, 1.1)
    assert fault == "non_finite_feedback" and (pl, pr) == (0, 0)
    assert m._prev is None  # reset so a stale delta can't drive the next cycle


def test_closed_loop_enabled_parsing() -> None:
    for v in ("1", "true", "TRUE", "yes", "on", " On "):
        assert closed_loop_enabled({"KIRRA_R2_CLOSED_LOOP": v}) is True
    for v in ("0", "false", "no", "off", "", None):
        env = {} if v is None else {"KIRRA_R2_CLOSED_LOOP": v}
        assert closed_loop_enabled(env) is False
    assert closed_loop_enabled({}) is False


def test_speed_match_params_from_env_loads_and_defaults() -> None:
    cal = _valid_cal(v_per_pwm=0.0145, pwm_max=40.0)
    env = {"KIRRA_R2_M_PER_TICK": "0.00025", "KIRRA_R2_V_PER_PWM_RIGHT": "0.0194"}
    p = speed_match_params_from_env(env, cal)
    assert p.v_per_pwm_left == 0.0145   # reuses cal.v_per_pwm (the LEFT slope)
    assert p.v_per_pwm_right == 0.0194
    assert p.pwm_max == 40.0            # reuses cal.pwm_max
    assert p.kp_pwm_per_mps == 20.0     # default
    assert p.stall_cycles == 5          # default


def test_speed_match_params_from_env_fail_closed_on_missing() -> None:
    cal = _valid_cal()
    for env in ({}, {"KIRRA_R2_M_PER_TICK": "0.00025"}, {"KIRRA_R2_V_PER_PWM_RIGHT": "0.0194"}):
        try:
            speed_match_params_from_env(env, cal)
        except R2CalibrationError:
            continue
        raise AssertionError(f"expected fail-closed for {env}")


def test_speed_match_params_ema_validation() -> None:
    for bad in (0.0, -0.1, 1.5, float("nan")):
        try:
            _valid_params(ema_alpha=bad)
        except R2CalibrationError:
            continue
        raise AssertionError(f"expected R2CalibrationError for ema_alpha={bad}")
    # 1.0 (no filter) and a mid value are valid.
    _valid_params(ema_alpha=1.0)
    _valid_params(ema_alpha=0.4)


def test_matcher_ema_smooths_alternating_speed() -> None:
    # Feed an ALTERNATING encoder delta (aliasing-like): +100 then +300 ticks per
    # 0.1s. With alpha<1 the filtered speed the matcher acts on must sit BETWEEN
    # the two raw samples (smoothed), not swing fully to each.
    p = _valid_params(ema_alpha=0.3)
    m = ClosedLoopSpeedMatcher(p)
    m.step(0.30, 0, 0, 0.0)  # seed
    enc = 0
    t = 0.0
    filtered = []
    for i in range(8):
        enc += 100 if i % 2 == 0 else 300
        t += 0.1
        m.step(0.30, enc, enc, t)
        filtered.append(m._ema_left)
    raw_low = 100 * p.m_per_tick / 0.1   # 0.25
    raw_high = 300 * p.m_per_tick / 0.1  # 0.75
    tail = filtered[-4:]
    assert all(raw_low < f < raw_high for f in tail), f"EMA should sit between raw extremes: {tail}"
    # And it should be far less jumpy than the raw sawtooth (spread << raw spread).
    assert (max(tail) - min(tail)) < (raw_high - raw_low) * 0.6, f"EMA not smoothing enough: {tail}"


def test_matcher_last_filtered_speeds_accessor() -> None:
    p = _valid_params(ema_alpha=1.0)
    m = ClosedLoopSpeedMatcher(p)
    assert m.last_filtered_speeds() == (None, None)  # before any measured cycle
    m.step(0.30, 0, 0, 0.0)      # feedforward seed — still no measured speed
    assert m.last_filtered_speeds() == (None, None)
    m.step(0.30, 120, 120, 0.1)  # 120 ticks / 0.1s * m_per_tick
    fl, fr = m.last_filtered_speeds()
    expected = 120 * p.m_per_tick / 0.1
    assert fl is not None and abs(fl - expected) < 1e-9 and abs(fr - expected) < 1e-9


def test_speed_match_params_from_env_rejects_nonnumeric() -> None:
    cal = _valid_cal()
    env = {"KIRRA_R2_M_PER_TICK": "abc", "KIRRA_R2_V_PER_PWM_RIGHT": "0.0194"}
    try:
        speed_match_params_from_env(env, cal)
    except R2CalibrationError:
        return
    raise AssertionError("expected R2CalibrationError for non-numeric m_per_tick")


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    failures = 0
    for t in tests:
        try:
            t()
            print(f"  ok   {t.__name__}")
        except AssertionError as e:
            failures += 1
            print(f"  FAIL {t.__name__}: {e}")
        except Exception as e:  # noqa: BLE001
            failures += 1
            print(f"  ERROR {t.__name__}: {type(e).__name__}: {e}")
    print(f"\n{len(tests) - failures}/{len(tests)} passed")
    return 1 if failures else 0


if __name__ == "__main__":
    print("r2_drive host tests (pure, no hardware):")
    sys.exit(_run_all())
