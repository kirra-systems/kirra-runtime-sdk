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
    STEER_CMD_LIMIT,
    R2CalibrationError,
    R2DriveCalibration,
    mrc_stop,
    translate,
)


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
