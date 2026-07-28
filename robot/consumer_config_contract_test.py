#!/usr/bin/env python3
"""Host tests for the shared consumer config contract.

The two live failures, as regressions:
  * r2_ackermann must NOT demand KIRRA_EXPECTED_CAR_TYPE (the doctor warned
    about it on a correctly-configured R2);
  * a lint-clean Rabbit-only env must be reported INSUFFICIENT in ONE pass,
    not discovered one restart at a time.
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import consumer_config_contract as c  # noqa: E402

_F: list[str] = []


def check(cond, msg):
    if not cond:
        _F.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


# The live, working R2 env from bring-up.
LIVE_R2 = {
    "KIRRA_GOVERNOR_VK_HEX": "ab", "KIRRA_PROFILE_DIGEST": "9c70",
    "KIRRA_FRESHNESS_WINDOW_MS": "200", "KIRRA_CONTROL_PERIOD_MS": "100",
    "KIRRA_MISSED_PERIODS": "3", "KIRRA_STOP_DECEL_MPS2": "0.5",
    "KIRRA_DEMO_VX_MAX": "0.15", "KIRRA_DEMO_VZ_MAX": "0.4",
    "KIRRA_MOTOR_PORT": "/dev/myserial", "KIRRA_DRIVE_MODE": "r2_ackermann",
    "KIRRA_R2_WHEELBASE_M": "0.229", "KIRRA_R2_V_PER_PWM": "0.0145",
    "KIRRA_R2_PWM_MAX": "40", "KIRRA_R2_STEER_UNITS_PER_RAD": "66",
    "KIRRA_R2_DELTA_MAX_RAD": "0.68", "KIRRA_R2_STEER_SIGN": "-1",
    "KIRRA_R2_CENTER_TRIM": "90",
    "KIRRA_CONSUMER_LIB": "/opt/kirra/libkirra_consumer_ffi.so",
}

RABBIT_ONLY = {
    "KIRRA_STT_CMD": "whisper-cli -m m.bin", "KIRRA_TTS_CMD": "./speak.sh",
    "KIRRA_RECORD_CMD": "arecord -d 4", "KIRRA_RABBIT_MODEL": "gemma3:4b",
    "KIRRA_OLLAMA_URL": "http://localhost:11434",
}


def test_the_live_r2_env_is_sufficient():
    check(c.sufficient_for_actuation(LIVE_R2),
          f"the working live env must pass, missing={c.missing_keys(LIVE_R2)}")


def test_r2_does_not_require_expected_car_type():
    """Live failure #4."""
    check("KIRRA_EXPECTED_CAR_TYPE" not in c.required_keys(LIVE_R2),
          "r2_ackermann must NOT require the X3-only car type")
    check("KIRRA_EXPECTED_CAR_TYPE" in c.inapplicable_keys(LIVE_R2),
          "it should be reported as inapplicable in R2 mode")
    env = dict(LIVE_R2); env.pop("KIRRA_DRIVE_MODE")
    check("KIRRA_EXPECTED_CAR_TYPE" in c.required_keys(env),
          "X3 mode must still require it")


def test_rabbit_only_env_is_insufficient_and_reported_in_one_pass():
    """Live failure #6: lint-clean, actuation-useless."""
    check(not c.sufficient_for_actuation(RABBIT_ONLY),
          "a Rabbit-only env must NOT be sufficient for actuation")
    missing = c.missing_keys(RABBIT_ONLY)
    for key in ("KIRRA_GOVERNOR_VK_HEX", "KIRRA_FRESHNESS_WINDOW_MS",
                "KIRRA_CONTROL_PERIOD_MS", "KIRRA_MISSED_PERIODS",
                "KIRRA_STOP_DECEL_MPS2", "KIRRA_DEMO_VX_MAX",
                "KIRRA_DEMO_VZ_MAX", "KIRRA_MOTOR_PORT", "KIRRA_CONSUMER_LIB"):
        check(key in missing, f"{key} must be reported missing")
    check(len(missing) >= 9,
          f"the WHOLE set must come back at once, got {len(missing)}: {missing}")


def test_odom_and_closed_loop_are_conditional():
    check("KIRRA_R2_M_PER_TICK" not in c.required_keys(LIVE_R2),
          "odom values must not be required while odom is off")
    on = dict(LIVE_R2, KIRRA_R2_ODOM_ENABLED="1")
    check("KIRRA_R2_M_PER_TICK" in c.required_keys(on),
          "odom values must be required when odom is on")
    check(not c.sufficient_for_actuation(on), "and missing → insufficient")
    cl = dict(LIVE_R2, KIRRA_R2_CLOSED_LOOP="1")
    check("KIRRA_R2_V_PER_PWM_RIGHT" in c.required_keys(cl),
          "closed-loop values required when enabled")


def test_r2_calibration_is_required_not_defaulted():
    for key in c.R2_REQUIRED:
        env = dict(LIVE_R2); env.pop(key)
        check(key in c.missing_keys(env),
              f"{key} is a per-robot MEASUREMENT and must be required")


def test_blank_counts_as_missing():
    env = dict(LIVE_R2, KIRRA_MOTOR_PORT="   ")
    check("KIRRA_MOTOR_PORT" in c.missing_keys(env), "whitespace is not a value")


def test_report_shape():
    r = c.report(RABBIT_ONLY)
    check(r["sufficient"] is False and r["missing"], "report must flag it")
    check(r["mode"] == c.MODE_X3, f"unset mode reports the default: {r['mode']}")
    r2 = c.report(LIVE_R2)
    check(r2["sufficient"] is True and r2["mode"] == c.MODE_R2, "live env ok")


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    print("consumer_config_contract_test:")
    for t in tests:
        b = len(_F); t()
        print(f"  {'ok  ' if len(_F) == b else 'FAIL'} {t.__name__}")
    if _F:
        print(f"\n{len(_F)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"\nall {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(_run_all())
