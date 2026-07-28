#!/usr/bin/env python3
"""Host tests for the consumer-config preflight entry point.

The regression it exists for: a lint-clean, Rabbit-only /etc/kirra/robot.env
that the consumer could not start from, and that surfaced ONE missing key per
restart (200+ restarts to learn eight facts). One command must now report the
whole set, before the service is enabled.
"""
from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "preflight_consumer_env.py"
sys.path.insert(0, str(HERE.parent))

_F: list[str] = []


def check(cond, msg):
    if not cond:
        _F.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


RABBIT_ONLY = """
# voice setup copied this in — syntactically valid, actuation-useless
KIRRA_STT_CMD="whisper-cli -m models/ggml-base.en.bin -np -nt -f"
KIRRA_TTS_CMD="./speak.sh"
KIRRA_RECORD_CMD="arecord -d 4 -f S16_LE -r 16000 -c 1"
KIRRA_RABBIT_MODEL="gemma3:4b"
"""

LIVE_R2 = """
KIRRA_GOVERNOR_VK_HEX=abcd
KIRRA_PROFILE_DIGEST=9c70086efe
KIRRA_FRESHNESS_WINDOW_MS=200
KIRRA_CONTROL_PERIOD_MS=100
KIRRA_MISSED_PERIODS=3
KIRRA_STOP_DECEL_MPS2=0.5
KIRRA_DEMO_VX_MAX=0.15
KIRRA_DEMO_VZ_MAX=0.4
KIRRA_MOTOR_PORT=/dev/myserial
KIRRA_DRIVE_MODE=r2_ackermann
KIRRA_R2_WHEELBASE_M=0.229
KIRRA_R2_V_PER_PWM=0.0145
KIRRA_R2_PWM_MAX=40
KIRRA_R2_STEER_UNITS_PER_RAD=66
KIRRA_R2_DELTA_MAX_RAD=0.68
KIRRA_R2_STEER_SIGN=-1
KIRRA_R2_CENTER_TRIM=90
export KIRRA_CONSUMER_LIB=/opt/kirra/libkirra_consumer_ffi.so
"""


def _run(text, *args):
    with tempfile.NamedTemporaryFile("w", suffix=".env", delete=False) as f:
        f.write(text)
        path = f.name
    try:
        d = subprocess.run([sys.executable, str(SCRIPT), path, *args],
                           capture_output=True, text=True, timeout=30)
        return d.returncode, d.stdout
    finally:
        Path(path).unlink()


def test_rabbit_only_env_is_not_ready_and_lists_every_key_at_once():
    rc, out = _run(RABBIT_ONLY)
    check(rc != 0, "a Rabbit-only env must exit non-zero")
    check("NOT READY" in out, f"and say NOT READY: {out}")
    for key in ("KIRRA_GOVERNOR_VK_HEX", "KIRRA_FRESHNESS_WINDOW_MS",
                "KIRRA_CONTROL_PERIOD_MS", "KIRRA_MISSED_PERIODS",
                "KIRRA_STOP_DECEL_MPS2", "KIRRA_DEMO_VX_MAX", "KIRRA_DEMO_VZ_MAX",
                "KIRRA_MOTOR_PORT", "KIRRA_CONSUMER_LIB"):
        check(key in out, f"{key} must appear in the ONE report")


def test_live_r2_env_is_ready():
    rc, out = _run(LIVE_R2)
    check(rc == 0, f"the live working env must be READY (rc={rc}):\n{out}")
    check("READY" in out and "NOT READY" not in out, f"verdict: {out}")
    check("r2_ackermann" in out, "the mode must be reported")


def test_r2_marks_expected_car_type_not_applicable():
    rc, out = _run(LIVE_R2 + "\nKIRRA_EXPECTED_CAR_TYPE=1\n")
    check(rc == 0, "an X3-only key set in R2 mode is not a failure")
    check("KIRRA_EXPECTED_CAR_TYPE" in out and "X3-only" in out,
          f"it must be marked not-applicable, not demanded: {out}")
    check("REQUIRED key(s) missing" not in out,
          "and must never appear in the missing list in R2 mode")


def test_odom_and_closed_loop_only_required_when_enabled():
    rc, out = _run(LIVE_R2)
    check("KIRRA_R2_M_PER_TICK" not in out.split("VERDICT")[0].split("NOT USED")[0],
          "odom keys must not be demanded while odom is off")
    rc, out = _run(LIVE_R2 + "\nKIRRA_R2_ODOM_ENABLED=1\n")
    check(rc != 0 and "KIRRA_R2_M_PER_TICK" in out,
          f"enabling odom must require its key: {out}")
    rc, out = _run(LIVE_R2 + "\nKIRRA_R2_CLOSED_LOOP=1\n")
    check(rc != 0 and "KIRRA_R2_V_PER_PWM_RIGHT" in out,
          f"enabling closed loop must require its key: {out}")


def test_quoting_and_export_are_handled():
    rc, out = _run(LIVE_R2.replace("KIRRA_MOTOR_PORT=/dev/myserial",
                                   'export KIRRA_MOTOR_PORT="/dev/myserial"'))
    check(rc == 0, f"export + quotes must parse: {out}")


def test_unreadable_file_fails_closed():
    d = subprocess.run([sys.executable, str(SCRIPT), "/nope/robot.env"],
                       capture_output=True, text=True, timeout=30)
    check(d.returncode != 0, "a missing env file must fail closed")
    check("not readable" in d.stdout, f"and say so: {d.stdout}")


def test_json_form_is_machine_readable():
    import json
    rc, out = _run(RABBIT_ONLY, "--format=json")
    check(rc != 0, "json form keeps the exit code")
    j = json.loads(out)
    check(j["sufficient"] is False and len(j["missing"]) >= 9,
          f"json must carry the whole missing set: {j}")


def test_it_is_read_only():
    """It gates STARTING the service, so it must be safe to run any time."""
    src = SCRIPT.read_text()
    for forbidden in ("systemctl", "subprocess", "open(.*'w'", "os.remove", "pkill"):
        check(forbidden not in src or forbidden == "subprocess",
              f"the preflight must not {forbidden}")
    check("subprocess" not in src, "the preflight must spawn nothing")


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    print("preflight_consumer_env_test:")
    for t in tests:
        b = len(_F)
        t()
        print(f"  {'ok  ' if len(_F) == b else 'FAIL'} {t.__name__}")
    if _F:
        print(f"\n{len(_F)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"\nall {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(_run_all())
