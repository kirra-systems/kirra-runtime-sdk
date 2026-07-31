#!/usr/bin/env python3
"""Host tests for deployment verification + the env migration report.

Both exist because the live R2 ran stale /opt/kirra artifacts against a newer
checkout and everything LOOKED fine: units active, /health ok, consumer started.
"""
from __future__ import annotations

import json
import re
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


# ── the voice layer is actually deployed ─────────────────────────────────────
#
# The defect these guard against: `assistant.classify()` returned a correct
# typed request, `assistant.handle()` returned None, and the code was fine —
# `KIRRA_ASSIST_ENABLED` was simply absent from the file the units load. It was
# documented in `rabbit.env.example`, which the installer never renders, so no
# fresh install ever had it and nothing reported its absence.

def test_env_template_enables_the_assistant():
    """`env.template` is what becomes /etc/kirra/robot.env. If the flag is not
    there — or is there but commented out — a fresh install ships the assistant
    silently disabled, which reads as a code fault rather than a config gap."""
    lines = (HERE / "env.template").read_text().splitlines()
    active = [ln for ln in lines
              if ln.strip().startswith("KIRRA_ASSIST_ENABLED=")]
    check(active, "env.template must set KIRRA_ASSIST_ENABLED (uncommented)")
    check(any(ln.split("=", 1)[1].strip() in ("1", "true", "yes", "on")
              for ln in active),
          f"KIRRA_ASSIST_ENABLED must be truthy in env.template, got {active}")


def test_the_units_load_the_file_the_template_renders():
    """The chain has to close: template -> robot.env -> EnvironmentFile. A unit
    reading some OTHER env file would make the template edit useless."""
    unit = (HERE / "systemd" / "kirra-rabbit-voice.service").read_text()
    check("EnvironmentFile=/etc/kirra/robot.env" in unit,
          "kirra-rabbit-voice must load /etc/kirra/robot.env")


def test_enablement_is_deployment_not_code():
    """`assistant.enabled()` must stay fail-closed. Turning the assistant on is
    a deployment decision; a code default would make every environment — tests,
    CI, a developer's laptop — silently authoritative."""
    src = (HERE.parent / "assistant.py").read_text()
    body = src.split("def enabled():", 1)[1].split("\n\n", 1)[0]
    check("KIRRA_ASSIST_ENABLED" in body,
          "enabled() must read the env var")
    for default in ('or "1"', "or '1'", 'or "true"', "return True"):
        check(default not in body,
              f"enabled() must not default-enable ({default!r})")


def test_verification_reports_a_disabled_assistant():
    """An absent flag must be REPORTED, not silent — that is the whole reason
    this went unnoticed. WARN, not FAIL: a robot without the voice layer is a
    valid deployment."""
    rep = vd.Report()
    vd.check_voice_env(rep, {})
    check(rep.rows, "an empty env must produce a row, not silence")
    check(rep.rows[0]["status"] == vd.WARN,
          f"absent flag should WARN, got {rep.rows[0]['status']}")
    check(rep.rows[0]["fix"], "…and must tell the operator how to fix it")
    check(rep.failed == 0, "a missing OPT-IN flag must not FAIL the deployment")

    rep = vd.Report()
    vd.check_voice_env(rep, {"KIRRA_ASSIST_ENABLED": "1"})
    check(rep.rows[0]["status"] == vd.PASS, "an enabled assistant passes")

    # The exact trap that makes this fail-closed: a value that LOOKS set but is
    # not truthy leaves the feature off, so it must not report as enabled.
    rep = vd.Report()
    vd.check_voice_env(rep, {"KIRRA_ASSIST_ENABLED": "0"})
    check(rep.rows[0]["status"] == vd.WARN,
          "a non-truthy value must not report as enabled")


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


def test_the_installer_derives_one_robot_user_everywhere():
    """The unit's User= and the udev rule's OWNER must name the SAME account.

    install_kirra.sh is documented to run under sudo. `id -un` therefore returns
    root, while `${SUDO_USER:-${USER}}` returns the real robot user — so mixing
    the two idioms rendered `User=root` into kirra-consumer.service while the
    udev rule kept OWNER=<robot user>. Root bypasses the 0600 owner check on
    /dev/myserial, so serial kept working and the split stayed invisible; Fast
    DDS shared memory does not bypass, so release frames stopped being delivered
    to a root-owned reader and the consumer serviced its timer but never its
    subscription.

    Asserting the two derivations agree is what makes that split impossible to
    reintroduce silently.
    """
    src = (HERE / "install_kirra.sh").read_text()
    assignments = re.findall(
        r"^(?:KIRRA_USER|ROBOT_USER)=(.+)$", src, flags=re.MULTILINE
    )
    check(len(assignments) >= 2,
          f"expected both KIRRA_USER and ROBOT_USER assignments, found {assignments}")
    for rhs in assignments:
        check("id -un" not in rhs,
              f"robot-user derivation uses `id -un`, which is root under sudo: {rhs.strip()}")
        check("SUDO_USER" in rhs,
              f"robot-user derivation must honour SUDO_USER: {rhs.strip()}")
    check(len(set(a.strip() for a in assignments)) == 1,
          f"the robot user is derived inconsistently across the installer: {assignments}")


def test_the_consumer_unit_never_ships_a_hardcoded_root_user():
    """A rendered `User=root` is the failure this whole class produces."""
    unit = (HERE / "systemd" / "kirra-consumer.service").read_text()
    check("User=__KIRRA_ROBOT_USER__" in unit,
          "the consumer unit template must keep the __KIRRA_ROBOT_USER__ placeholder")
    check("User=root" not in unit,
          "the consumer unit must never hardcode User=root — root-owned DDS "
          "shared-memory segments are unwritable by the robot-user publisher")


def test_the_installer_writes_the_consumer_lib_where_the_verifier_expects_it():
    """The .so path in EXPECTED_ARTIFACTS must be a path install_kirra.sh writes.

    A live robot ran an OLD verify core for hours: robot.env pinned
    KIRRA_CONSUMER_LIB to the pre-lib/ layout, so every rebuild landed in
    /opt/kirra/lib/ while the consumer dlopen'd a stale file one directory up.
    Both files existed and both loaded, so nothing failed — the SS-002 stop ramp
    simply never ran and the wheels kept turning after releases stopped.

    The artifact-contract test above only pairs robot/-relative files, which is
    exactly why this one slipped through it.
    """
    installer = (HERE / "install_kirra.sh").read_text()
    # The destinations the installer writes the cdylib to, ${OPT} expanded.
    installed = [
        ln.split()[-1].strip('"').replace("${OPT}", "/opt/kirra")
        for ln in installer.splitlines()
        if "libkirra_consumer_ffi.so" in ln and ln.strip().startswith("sudo install")
    ]
    check(installed, "install_kirra.sh must install the consumer cdylib")
    check(any(d == vd.INSTALLED_CONSUMER_LIB for d in installed),
          f"the installer writes {installed}, the verifier expects "
          f"{vd.INSTALLED_CONSUMER_LIB}")
    expected = [p for p, _ in vd.EXPECTED_ARTIFACTS if p.endswith("libkirra_consumer_ffi.so")]
    check(expected, "EXPECTED_ARTIFACTS must name the consumer cdylib")
    for p in expected:
        check(p == vd.INSTALLED_CONSUMER_LIB,
              f"the verifier expects {p}, the installer writes "
              f"{vd.INSTALLED_CONSUMER_LIB} — a robot pinned to the other one "
              f"runs a core no rebuild can replace")


def test_the_env_template_points_at_the_installed_consumer_lib():
    """robot.env is rendered from env.template; if it names the other path the
    consumer loads a library the installer never updates."""
    tmpl = (HERE / "env.template").read_text()
    vals = [ln.split("=", 1)[1].strip() for ln in tmpl.splitlines()
            if ln.strip().startswith("KIRRA_CONSUMER_LIB=")]
    check(vals, "env.template must set KIRRA_CONSUMER_LIB")
    for v in vals:
        check(v == vd.INSTALLED_CONSUMER_LIB,
              f"env.template points KIRRA_CONSUMER_LIB at {v}, installer writes "
              f"{vd.INSTALLED_CONSUMER_LIB}")


def test_deployment_identity_flags_a_user_mismatch():
    """The 2026-07-31 fault-1 shape: unit runs as root, port owned by the robot
    user. Every component healthy, no message ever delivered."""
    rep = vd.Report()
    vd.check_deployment_identity(rep, {"KIRRA_CONSUMER_LIB": vd.INSTALLED_CONSUMER_LIB},
                                 unit_user="root", port_owner="jetson",
                                 configured_user="jetson")
    bad = [r for r in rep.rows if r["status"] == vd.FAIL]
    check(bad, "a root unit against a robot-user port must FAIL, not pass quietly")
    check(any("unit user" in r["check"] for r in bad),
          f"the unit-user row must be the failing one, got {[r['check'] for r in bad]}")
    check(all(r["fix"] for r in bad), "a failure must tell the operator what to do")


def test_deployment_identity_flags_a_library_pointing_elsewhere():
    """Fault-2 shape: the consumer loads a library the installer never writes,
    so it dlopen's fine and no rebuild ever reaches it."""
    rep = vd.Report()
    vd.check_deployment_identity(rep, {"KIRRA_CONSUMER_LIB": "/opt/kirra/libkirra_consumer_ffi.so"},
                                 unit_user="jetson", port_owner="jetson",
                                 configured_user="jetson")
    bad = [r for r in rep.rows if r["status"] == vd.FAIL]
    check(any("consumer library" in r["check"] for r in bad),
          f"a library outside the installer's destination must FAIL, got "
          f"{[(r['check'], r['status']) for r in rep.rows]}")


def test_deployment_identity_accepts_a_consistent_deployment():
    """…and must not cry wolf on a correct one, including an unplugged board."""
    rep = vd.Report()
    vd.check_deployment_identity(rep, {"KIRRA_CONSUMER_LIB": vd.INSTALLED_CONSUMER_LIB},
                                 unit_user="jetson", port_owner="<absent>",
                                 configured_user="jetson")
    lib_rows = [r for r in rep.rows if "consumer library" in r["check"]]
    check(not [r for r in rep.rows if r["status"] == vd.FAIL and "library" not in r["check"]],
          "a consistent deployment with the board unplugged must not FAIL on user/owner")
    check(lib_rows, "the library row must always be reported")


def test_the_installer_prints_the_deployment_identity():
    """The four lines must actually be emitted at install time — that is the
    whole point: making the state explicit rather than inferable."""
    src = (HERE / "install_kirra.sh").read_text()
    for needle in ("configured robot user", "systemd User=",
                   "/dev/myserial owner", "KIRRA_CONSUMER_LIB"):
        check(needle in src, f"install_kirra.sh must print {needle!r}")


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
