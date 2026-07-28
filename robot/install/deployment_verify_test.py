#!/usr/bin/env python3
"""Host tests for deployment verification + the env migration report.

Both exist because the live R2 ran stale /opt/kirra artifacts against a newer
checkout and everything LOOKED fine: units active, /health ok, consumer started.
"""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
import env_migration_report as mig  # noqa: E402
import verify_deployment as vd  # noqa: E402

_F: list[str] = []


def check(cond, msg):
    if not cond:
        _F.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


VK = "aa11bb22cc33dd44ee55ff6677889900aa11bb22cc33dd44ee55ff6677889900"
LIVE_R2 = f"""
KIRRA_GOVERNOR_VK_HEX={VK}
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
KIRRA_CONSUMER_LIB=/opt/kirra/libkirra_consumer_ffi.so
"""


def _report(text):
    with tempfile.NamedTemporaryFile("w", suffix=".env", delete=False) as f:
        f.write(text)
        p = f.name
    try:
        d = subprocess.run([sys.executable, str(HERE / "env_migration_report.py"), p],
                           capture_output=True, text=True, timeout=30)
        return d.returncode, d.stdout, p
    finally:
        Path(p).unlink()


# ── the three-state capability classification (mirrors the Rust side) ────────

def test_a_healthy_legacy_service_is_legacy_not_current():
    """THE live failure: /health said ok, the binary was old."""
    state, v = vd.classify_contract(True, '{"status":"ok"}', 2)
    check(state == "legacy" and v == 1, f"pre-capability build → legacy, got {state} v{v}")


def test_a_current_service_is_current():
    state, v = vd.classify_contract(True, '{"status":"ok","contract":2}', 2)
    check(state == "current" and v == 2, f"got {state} v{v}")


def test_unreachable_is_its_own_state():
    state, _ = vd.classify_contract(False, "", 2)
    check(state == "unavailable", f"got {state}")


def test_a_service_with_no_declared_requirement_is_not_failed_on_contract():
    state, _ = vd.classify_contract(True, '{"status":"ok"}', None)
    check(state == "current", "a service with no contract requirement must not FAIL")


def test_ffi_is_probed_explicitly_not_inferred_from_startup():
    src = (HERE / "verify_deployment.py").read_text()
    check("ctypes.CDLL" in src, "the FFI must be dlopen'd, not inferred")
    check("systemctl start" not in src and "systemctl restart" not in src,
          "verification must never start a service to prove anything")


def test_verification_is_read_only():
    """It must OBSERVE the installed system, never change it.

    Parsed, not grepped: a `fix=` hint may legitimately TELL the operator to run
    `sudo chmod +x`, which is advice, not an action. What matters is what the
    tool itself executes and writes."""
    import ast
    src = (HERE / "verify_deployment.py").read_text()
    tree = ast.parse(src)
    mutating = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        name = ast.unparse(node.func)
        if name in ("os.remove", "os.rename", "os.replace", "shutil.copy",
                    "shutil.move", "os.chmod", "os.unlink"):
            mutating.append(f"{name} at line {node.lineno}")
        if name == "open":
            mode = node.args[1].value if len(node.args) > 1 and \
                isinstance(node.args[1], ast.Constant) else "r"
            if "w" in str(mode) or "a" in str(mode):
                mutating.append(f"open(mode={mode}) at line {node.lineno}")
        # what it SPAWNS — sudo/systemctl-mutation must never be executed
        if name in ("subprocess.run", "subprocess.Popen", "subprocess.call"):
            argv = ast.unparse(node.args[0]) if node.args else ""
            for verb in ("sudo", "systemctl start", "systemctl stop",
                         "systemctl disable", "systemctl enable", "pkill"):
                if verb in argv:
                    mutating.append(f"spawns {verb!r} at line {node.lineno}")
    check(not mutating, f"verification must be read-only, found: {mutating}")


# ── the migration report ─────────────────────────────────────────────────────

def test_the_report_never_prints_a_secret():
    """A migration report pasted into a ticket must not disclose the governor
    verifying key."""
    rc, out, _ = _report(LIVE_R2)
    check(VK not in out, "the governor key LEAKED into the report")
    check("fp=" in out and "64 chars" in out,
          f"it must report presence + a fingerprint instead: {out}")
    check(VK not in json.dumps(mig.redact("KIRRA_GOVERNOR_VK_HEX", VK)),
          "redact() must not return the value")
    check(mig.redact("KIRRA_MOTOR_PORT", "/dev/myserial") == "/dev/myserial",
          "a non-secret must be shown verbatim")


def test_all_six_sections_are_always_present_and_ordered():
    rc, out, _ = _report(LIVE_R2)
    order = ["REQUIRED MISSING", "INVALID VALUES", "MODE-INAPPLICABLE",
             "OBSOLETE", "PRESERVED", "OPERATOR ACTION REQUIRED"]
    pos = [out.find(s) for s in order]
    check(all(p >= 0 for p in pos), f"a section is missing: {list(zip(order, pos))}")
    check(pos == sorted(pos), "sections must appear in the documented order")
    check("(none)" in out, "an empty section must say (none), not vanish")


def test_a_sufficient_env_needs_no_operator_action():
    rc, out, _ = _report(LIVE_R2)
    check(rc == 0, f"a complete env must exit 0: {out}")
    check("none — this file is sufficient" in out, out)


def test_missing_keys_are_listed_and_require_action():
    rc, out, _ = _report("KIRRA_STT_CMD=x\nKIRRA_TTS_CMD=y\n")
    check(rc != 0, "an insufficient env must exit non-zero")
    check("KIRRA_GOVERNOR_VK_HEX" in out and "KIRRA_MOTOR_PORT" in out,
          "the missing keys must be listed")


def test_invalid_values_are_caught_separately_from_missing():
    rc, out, _ = _report(LIVE_R2.replace("KIRRA_CONTROL_PERIOD_MS=100",
                                         "KIRRA_CONTROL_PERIOD_MS=fast"))
    check(rc != 0 and "not a number" in out, f"a non-numeric value must be INVALID: {out}")
    bad_vk = LIVE_R2.replace(VK, "abcd")
    rc, out, _ = _report(bad_vk)
    check("64" in out, f"a short verifying key must be flagged: {out}")


def test_obsolete_keys_are_reported_but_are_not_a_failure():
    rc, out, _ = _report(LIVE_R2 + "\nKIRRA_MOTOR_BAUD=115200\n")
    check("KIRRA_MOTOR_BAUD" in out, "an obsolete key must be reported")
    check(rc == 0, "a stale key is inert — reporting it must not fail the run")


def test_the_report_never_writes():
    src = (HERE / "env_migration_report.py").read_text()
    for bad in ('open(path, "w"', "'w')", "shutil", "os.replace", "os.rename"):
        check(bad not in src, f"the migration report must not {bad!r}")
    # And the file is byte-identical after a run.
    import hashlib
    with tempfile.NamedTemporaryFile("w", suffix=".env", delete=False) as f:
        f.write(LIVE_R2)
        p = f.name
    try:
        before = hashlib.sha256(Path(p).read_bytes()).hexdigest()
        subprocess.run([sys.executable, str(HERE / "env_migration_report.py"), p],
                       capture_output=True, timeout=30)
        check(hashlib.sha256(Path(p).read_bytes()).hexdigest() == before,
              "the env file was MODIFIED by a read-only report")
    finally:
        Path(p).unlink()


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    print("deployment_verify_test:")
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
