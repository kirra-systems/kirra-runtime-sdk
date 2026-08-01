#!/usr/bin/env python3
"""verify_deployment.py — is the INSTALLED system what we think it is?

The live R2 ran stale `/opt/kirra` artifacts against a newer checkout for an
entire bring-up session. Everything looked fine: the units were active, `/health`
answered `ok`, and the consumer started. The installed `taj_service` was simply
an older build returning the legacy perception shape, and nothing checked.

`cargo test` proves the CHECKOUT is good. This proves the ROBOT is, which is a
different claim and the one that was wrong:

  1. expected installed paths exist and are executable
  2. installed hashes / build IDs recorded (so drift is detectable next time)
  3. Taj reports a CURRENT capability contract, not merely 200 OK
  4. other sidecars report compatible contracts where they advertise one
  5. the consumer FFI LOADS — probed explicitly, not inferred from startup
  6. systemd units reference the expected installed paths
  7. the consumer environment satisfies consumer_config_contract
  8. services reach the expected healthy/current state

Read-only. Nothing is installed, started, stopped or written.

    robot/install/verify_deployment.py            # human report
    robot/install/verify_deployment.py --json     # machine-readable

Exit 0 iff every check passes.
"""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys

# Both the repo layout and the installed one. In the repo this file sits at
# robot/install/, so its GRANDparent (robot/) holds consumer_config_contract;
# installed it sits flat in /opt/kirra/robot/ beside its imports, so its OWN
# directory is the one that matters. Inserting both makes the same file run
# unmodified from either location — which is the point of deploying it: a
# verifier that only runs from a checkout reports whatever that checkout
# believes, and on 2026-07-31 a stale one reported a stale verdict against a
# correctly-deployed robot.
_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(_HERE))
sys.path.insert(0, _HERE)
import consumer_config_contract as contract  # noqa: E402

OPT = os.environ.get("KIRRA_OPT_DIR", "/opt/kirra")
# The path install_kirra.sh actually writes the cdylib to. KIRRA_CONSUMER_LIB
# must resolve here; a robot.env left on the pre-lib/ layout silently pins an
# older core that every subsequent rebuild fails to replace.
INSTALLED_CONSUMER_LIB = f"{OPT}/lib/libkirra_consumer_ffi.so"
# Digests recorded by install_kirra.sh for everything it installs. Re-hashing
# against this is the only way to see a file that was edited AFTER install —
# the shape that put a hand-patched consumer into production for a day.
INSTALLED_MANIFEST = f"{OPT}/installed.sha256"
ROBOT_ENV = os.environ.get("KIRRA_ROBOT_ENV", "/etc/kirra/robot.env")

# (path, must_be_executable) — the artifacts a governed robot actually runs.
EXPECTED_ARTIFACTS = [
    (f"{OPT}/robot/kirra_motor_consumer.py", True),
    (f"{OPT}/robot/serial_exclusivity.py", False),
    (f"{OPT}/robot/motor_authority.py", False),
    # The verifier verifies its own deployment. A robot carrying no copy of this
    # tool can only be checked from a checkout, and a stale checkout reports a
    # stale verdict — the failure mode that made the first post-fix run read as
    # a deployment fault when the deployment was already correct.
    (f"{OPT}/robot/verify_deployment.py", True),
    (INSTALLED_CONSUMER_LIB, False),
    (f"{OPT}/taj_service", True),
    (f"{OPT}/mick_service", True),
]

# service → (url, minimum contract). A service with no advertised contract yet
# is probed for liveness only; `None` means "no contract requirement declared".
EXPECTED_SERVICES = [
    ("taj", os.environ.get("KIRRA_TAJ_URL", "http://127.0.0.1:8101"), 2),
    ("mick", os.environ.get("KIRRA_MICK_URL", "http://127.0.0.1:8102"), None),
]

PASS, FAIL, WARN = "PASS", "FAIL", "WARN"


class Report:
    def __init__(self):
        self.rows: list[dict] = []

    def add(self, check, status, detail, fix=None):
        self.rows.append({"check": check, "status": status,
                          "detail": detail, "fix": fix})

    @property
    def failed(self) -> int:
        return sum(1 for r in self.rows if r["status"] == FAIL)


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    try:
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(65536), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError:
        return ""


def http_get(url: str, timeout: float = 3.0):
    """(reachable, body). No requests dependency — urllib is stdlib."""
    import urllib.error
    import urllib.request
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return True, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return True, f"__HTTP_{e.code}__"      # answered, just not with a body
    except Exception:  # noqa: BLE001
        return False, ""


def classify_contract(reachable, body, required):
    """Mirrors kirra_sidecars::capabilities::classify — three distinct states.
    A reachable peer with no parseable capabilities is a PRE-CAPABILITY build
    (legacy), not 'unavailable': the operator actions differ."""
    if not reachable:
        return "unavailable", None
    contract_v = 1
    try:
        j = json.loads(body)
        if isinstance(j.get("contract"), int):
            contract_v = j["contract"]
    except Exception:  # noqa: BLE001
        pass
    if required is None:
        return "current", contract_v
    return ("current" if contract_v >= required else "legacy"), contract_v


def check_artifacts(rep: Report) -> dict:
    digests = {}
    for path, must_exec in EXPECTED_ARTIFACTS:
        if not os.path.exists(path):
            rep.add(f"artifact {os.path.basename(path)}", FAIL, f"{path} missing",
                    fix="re-run the installer (install_robot_units.sh / install_kirra.sh)")
            continue
        if must_exec and not os.access(path, os.X_OK):
            rep.add(f"artifact {os.path.basename(path)}", FAIL,
                    f"{path} not executable", fix=f"sudo chmod +x {path}")
            continue
        d = sha256_file(path)
        digests[path] = d
        rep.add(f"artifact {os.path.basename(path)}", PASS,
                f"{path} sha256={d[:12]}…")
    return digests


def check_deployment_identity(rep: Report, env: dict, unit_user: str,
                             port_owner: str, configured_user: str) -> None:
    """The four facts that must name the same account / the same library.

    Both 2026-07-31 faults were invisible disagreements between these values,
    and every component reported healthy in isolation. A consumer running as a
    different user than the publisher cannot exchange Fast DDS shared-memory
    data — discovery still matches over UDP and local timers still fire, so the
    node looks alive while no message is ever delivered. A KIRRA_CONSUMER_LIB
    pointing anywhere but the installer's destination loads a core that no
    rebuild replaces, and it dlopen's perfectly.

    Pure decision over already-collected values so it is host-testable: the
    caller does the reading, this decides.
    """
    lib = (env.get("KIRRA_CONSUMER_LIB") or "").strip() or INSTALLED_CONSUMER_LIB

    if unit_user and configured_user and unit_user != configured_user:
        rep.add("deployment identity: unit user", FAIL,
                f"systemd User={unit_user}, robot user is {configured_user}",
                fix=f"set User={configured_user} in kirra-consumer.service; a "
                    f"mismatched user breaks DDS shared-memory delivery while "
                    f"discovery and timers still look healthy")
    else:
        rep.add("deployment identity: unit user", PASS,
                f"systemd User={unit_user or '<unset>'}")

    # An absent port just means the board is unplugged — not a mismatch.
    if port_owner and port_owner != "<absent>" and port_owner != configured_user:
        rep.add("deployment identity: serial owner", FAIL,
                f"/dev/myserial owned by {port_owner}, robot user is {configured_user}",
                fix="re-trigger udev or replug the board; the consumer's boot "
                    "sentinel refuses to start on an owner mismatch")
    else:
        rep.add("deployment identity: serial owner", PASS,
                f"/dev/myserial owner={port_owner or '<absent>'}")

    if os.path.realpath(lib) != os.path.realpath(INSTALLED_CONSUMER_LIB):
        rep.add("deployment identity: consumer library", FAIL,
                f"KIRRA_CONSUMER_LIB={lib}, installer writes {INSTALLED_CONSUMER_LIB}",
                fix=f"point KIRRA_CONSUMER_LIB at {INSTALLED_CONSUMER_LIB} and "
                    f"remove the stale copy — otherwise rebuilds never take effect")
    elif not os.path.exists(lib):
        rep.add("deployment identity: consumer library", FAIL,
                f"{lib} does not exist", fix="build + install the cdylib")
    else:
        rep.add("deployment identity: consumer library", PASS, f"{lib} present")


def parse_manifest(text: str) -> dict:
    """`sha256sum` output → {path: digest}. Unparseable lines are skipped.

    Tolerant on purpose: a manifest that cannot be read must not fail the whole
    verification, because it is an observability aid, not a gate.
    """
    out = {}
    for line in text.splitlines():
        parts = line.split(None, 1)
        if len(parts) == 2 and len(parts[0]) == 64:
            out[parts[1].strip().lstrip("*")] = parts[0].strip()
    return out


def check_installed_artifacts_unchanged(rep: Report, recorded: dict,
                                        current: dict) -> None:
    """Has anything been edited since the installer put it there?

    WARN, never FAIL. Editing a deployed file is legitimate during bring-up;
    what is NOT legitimate is doing it invisibly. On 2026-07-31 a consumer was
    hand-patched in place while diagnosing, the deployed file silently stopped
    matching its source, and later reasoning was done against a file that was no
    longer the one running. Nothing reported it because nothing recorded what
    had been installed.

    Pure decision over two digest maps so it is host-testable — the caller
    hashes, this compares.
    """
    if not recorded:
        rep.add("installed artifacts unchanged", WARN,
                f"no manifest at {INSTALLED_MANIFEST}",
                fix="re-run install_kirra.sh to record installed digests; "
                    "without it, post-install edits are undetectable")
        return

    changed, missing = [], []
    for path, digest in sorted(recorded.items()):
        now = current.get(path)
        if now is None:
            missing.append(path)
        elif now != digest:
            changed.append((path, digest, now))

    for path in missing:
        rep.add("installed artifacts unchanged", WARN, f"{path} is gone",
                fix="re-run the installer")
    for path, was, now in changed:
        rep.add("installed artifacts unchanged", WARN,
                f"{path} EDITED since install: recorded {was[:12]}… now {now[:12]}…",
                fix=f"re-install from source, or re-run install_kirra.sh to "
                    f"re-record if the edit is intended: sudo install -m 0755 "
                    f"<repo>/robot/{os.path.basename(path)} {path}")
    if not changed and not missing:
        rep.add("installed artifacts unchanged", PASS,
                f"{len(recorded)} artifact(s) match their recorded digests")


def probe_ffi(rep: Report, env: dict) -> None:
    """Load the consumer FFI EXPLICITLY.

    Deliberately not "the consumer started, so the library must be fine": that
    conflates a config abort, a missing ROS environment and a broken .so into
    one restart, which is exactly how the live FFI failure hid inside a restart
    loop. A dlopen either works or names its own error.
    """
    lib = (env.get("KIRRA_CONSUMER_LIB") or "").strip() or INSTALLED_CONSUMER_LIB
    if not os.path.exists(lib):
        rep.add("consumer FFI loads", FAIL, f"{lib} missing",
                fix="build + install libkirra_consumer_ffi.so")
        return
    # A live robot ran an OLD core for hours because robot.env still pointed at
    # the pre-lib/ layout: every rebuild landed in INSTALLED_CONSUMER_LIB while
    # the consumer dlopen'd a stale file one directory up. Both existed, both
    # loaded, and nothing compared them — so the fix "worked" and changed
    # nothing. Loading the wrong core is not a load failure, so this is its own
    # row rather than part of the dlopen check.
    if os.path.realpath(lib) != os.path.realpath(INSTALLED_CONSUMER_LIB):
        rep.add(
            "consumer FFI is the installed one", WARN,
            f"loads {lib}, installer writes {INSTALLED_CONSUMER_LIB}",
            fix=f"point KIRRA_CONSUMER_LIB at {INSTALLED_CONSUMER_LIB} in "
                f"/etc/kirra/robot.env, then remove the stale copy",
        )
    code = ("import ctypes,sys\n"
            "try:\n"
            "    ctypes.CDLL(sys.argv[1])\n"
            "    print('OK')\n"
            "except OSError as e:\n"
            "    print('ERR', e); sys.exit(1)\n")
    try:
        d = subprocess.run([sys.executable, "-c", code, lib],
                           capture_output=True, text=True, timeout=20)
        if d.returncode == 0:
            rep.add("consumer FFI loads", PASS, f"dlopen({lib}) OK")
        else:
            rep.add("consumer FFI loads", FAIL,
                    f"dlopen({lib}) failed: {d.stdout.strip()[:160]}",
                    fix="rebuild the FFI for THIS architecture; check ldd for "
                        "missing shared objects")
    except Exception as e:  # noqa: BLE001
        rep.add("consumer FFI loads", FAIL, f"probe errored: {e}")


def check_services(rep: Report) -> None:
    for name, base, required in EXPECTED_SERVICES:
        reachable, body = http_get(f"{base}/capabilities")
        if not reachable:                      # fall back to /health for liveness
            reachable, body = http_get(f"{base}/health")
        state, contract_v = classify_contract(reachable, body, required)
        if state == "unavailable":
            rep.add(f"{name} service", FAIL, f"{base} unavailable",
                    fix=f"systemctl status kirra-{name}")
        elif state == "legacy":
            rep.add(f"{name} capability contract", FAIL,
                    f"healthy but LEGACY: contract v{contract_v}, need v{required} "
                    "— an OLD binary is installed",
                    fix=f"rebuild and reinstall {OPT}/{name}_service, then restart it")
        else:
            v = f" (contract v{contract_v})" if contract_v else ""
            rep.add(f"{name} service", PASS, f"{base} healthy and current{v}")


def check_units(rep: Report) -> None:
    for unit, expect in (("kirra-consumer.service", f"{OPT}/robot/kirra_motor_consumer.py"),):
        try:
            out = subprocess.run(["systemctl", "cat", unit],
                                 capture_output=True, text=True, timeout=5).stdout
        except Exception:  # noqa: BLE001
            rep.add(f"unit {unit}", WARN, "systemctl unavailable (not the robot?)")
            continue
        if not out:
            rep.add(f"unit {unit}", FAIL, "not installed",
                    fix="robot/install/install_robot_units.sh")
        elif expect not in out:
            rep.add(f"unit {unit}", FAIL,
                    f"does not reference the installed path {expect}",
                    fix="re-run the installer — the unit points somewhere else")
        else:
            rep.add(f"unit {unit}", PASS, f"references {expect}")


def check_env(rep: Report, env: dict) -> None:
    r = contract.report(env)
    if r["missing"]:
        rep.add("consumer env complete", FAIL,
                f"{len(r['missing'])} required key(s) missing for mode {r['mode']}: "
                + ", ".join(r["missing"]),
                fix="robot/install/preflight_consumer_env.py")
    else:
        rep.add("consumer env complete", PASS,
                f"all {r['required_count']} required key(s) set for mode {r['mode']}")


#: Voice-layer flags that are OPT-IN in code and enabled by DEPLOYMENT. Reported
#: because their failure mode is silence: the assistant answers nothing, and
#: `classify()` still returns a correct typed request, so the box looks like a
#: code fault when it is a missing line in robot.env. WARN, never FAIL — a robot
#: installed without the voice layer is a valid deployment, it just should not be
#: mistaken for a broken one.
VOICE_FLAGS = (
    ("KIRRA_ASSIST_ENABLED", "Engineering Assistant",
     "docs/KIRRA_ENGINEERING_ASSISTANT.md"),
)


def check_voice_env(rep: Report, env: dict) -> None:
    """Is the voice layer actually switched on in the file the units load?"""
    for key, label, doc in VOICE_FLAGS:
        raw = (env.get(key) or "").strip()
        if raw.lower() in ("1", "true", "yes", "on"):
            rep.add(f"{label} enabled", PASS, f"{key}={raw}")
        elif raw:
            rep.add(f"{label} enabled", WARN,
                    f"{key}={raw!r} is not a truthy value, so it stays OFF "
                    "(fail-closed)",
                    fix=f"set {key}=1 in {ROBOT_ENV}, then restart the unit")
        else:
            rep.add(f"{label} enabled", WARN,
                    f"{key} is not set in {ROBOT_ENV} — the feature is OFF and "
                    "will answer nothing (see " + doc + ")",
                    # Phrased without the literal service-control command: a
                    # sibling test forbids that string anywhere in this file, so
                    # verification can never be edited into starting something
                    # to prove a point. The operator still gets both steps.
                    fix=f"append {key}=1 to {ROBOT_ENV}, then restart "
                        "kirra-rabbit-voice (see install_robot_units.sh)")


def diagnose_env_access(probes: dict) -> tuple:
    """Pure classifier for the user-unit EnvironmentFile access states.

    The user voice unit (rabbit-voice.service) is the one unit whose
    EnvironmentFile= is opened by the OPERATOR's user manager, not PID 1 —
    root opens the system units' copy and bypasses permissions entirely, so
    every other check here can pass while the user unit dies before ExecStart
    with Result=resources, pid=0 and no service log (live R2 incident:
    /etc/kirra re-tightened to 0750 kirra:kirra by deploy/systemd/install.sh).

    `probes` keys (all set by probe_env_access, injected in tests):
      unit_user        str  — account the deployment runs as ('' if unknown)
      verified_as_user bool — the checks below were run AS unit_user
      dir_traversable  bool
      file_exists      bool
      file_readable    bool
    Returns (status, detail, fix) for Report.add.
    """
    user = probes.get("unit_user") or ""
    helper = f"sudo {acl_helper_path()}"
    if not user:
        return (WARN, "cannot determine the deployment user (unit User= "
                "unreadable) — user-unit env access not verified",
                f"{helper} --check --user <robot-user>")
    fix = f"{helper} --user {user}"
    if not probes.get("verified_as_user"):
        return (WARN, f"cannot verify as '{user}' from this account — run the "
                "verifier as root or as the deployment user",
                f"{helper} --check --user {user}")
    if not probes.get("dir_traversable"):
        return (FAIL, f"'{user}' cannot traverse {os.path.dirname(ROBOT_ENV)} "
                "— the user voice unit fails before ExecStart "
                "(Result=resources, no log)", fix)
    if not probes.get("file_exists"):
        return (FAIL, f"{ROBOT_ENV} does not exist for '{user}'",
                "robot/install/install_kirra.sh renders robot.env if absent, "
                f"then {fix}")
    if not probes.get("file_readable"):
        return (FAIL, f"'{user}' can traverse the directory but cannot READ "
                f"{ROBOT_ENV} (file mode/ACL)", fix)
    return (PASS, f"'{user}' can read {ROBOT_ENV} (traverse + read verified "
            "as that user)", None)


def acl_helper_path() -> str:
    """The repair helper's path FROM WHERE THIS VERIFIER RUNS.

    The verifier runs from the installed /opt/kirra/robot tree on a robot
    with no checkout — a repo-relative hint would be a dead end there. The
    helper is a sibling in the staged tree and lives in this same directory
    in a checkout; fall back to the repo-relative form only when neither
    copy is present (fresh clone before install).
    """
    here = os.path.dirname(os.path.abspath(__file__))
    for cand in (os.path.join(here, "ensure_voice_env_access.sh"),
                 "/opt/kirra/robot/ensure_voice_env_access.sh"):
        if os.access(cand, os.X_OK):
            return cand
    return "robot/install/ensure_voice_env_access.sh"


def probe_env_access(user: str) -> dict:
    """Live probes for diagnose_env_access, dropping privileges honestly.

    As root: run `test` AS the unit user (runuser) — the only proof that
    counts. As the unit user: direct os.access. As anyone else: report
    unverified rather than guessing (root's own os.access always says yes).
    """
    probes = {"unit_user": user, "verified_as_user": False,
              "dir_traversable": False, "file_exists": False,
              "file_readable": False}
    if not user:
        return probes
    d = os.path.dirname(ROBOT_ENV)

    def as_user(flag: str, path: str) -> bool:
        r = subprocess.run(["runuser", "-u", user, "--", "test", flag, path],
                           capture_output=True, timeout=10)
        return r.returncode == 0

    try:
        if os.geteuid() == 0:
            probes["verified_as_user"] = True
            probes["dir_traversable"] = as_user("-x", d)
            probes["file_exists"] = as_user("-e", ROBOT_ENV)
            probes["file_readable"] = as_user("-r", ROBOT_ENV)
        elif pwd_name_of_euid() == user:
            probes["verified_as_user"] = True
            probes["dir_traversable"] = os.access(d, os.X_OK)
            probes["file_exists"] = os.path.exists(ROBOT_ENV)
            probes["file_readable"] = os.access(ROBOT_ENV, os.R_OK)
    except Exception:  # noqa: BLE001 — a broken probe is "unverified", not a crash
        probes["verified_as_user"] = False
    return probes


def pwd_name_of_euid() -> str:
    import pwd
    try:
        return pwd.getpwuid(os.geteuid()).pw_name
    except KeyError:
        return ""


def check_user_env_access(rep: Report) -> None:
    user = configured_robot_user()
    status, detail, fix = diagnose_env_access(probe_env_access(user))
    rep.add("user voice unit can read robot.env", status, detail, fix=fix)


def run() -> Report:
    rep = Report()
    from preflight_consumer_env import parse_env_file
    env = parse_env_file(ROBOT_ENV) if os.path.isfile(ROBOT_ENV) else {}
    if not env:
        rep.add("robot.env readable", FAIL, f"{ROBOT_ENV} missing or empty")
    check_artifacts(rep)
    check_env(rep, env)
    check_voice_env(rep, env)
    check_user_env_access(rep)
    probe_ffi(rep, env)
    check_deployment_identity(
        rep, env,
        unit_user=read_unit_user(),
        port_owner=read_port_owner(env.get("KIRRA_MOTOR_PORT") or "/dev/myserial"),
        configured_user=configured_robot_user(),
    )
    check_installed_artifacts_unchanged(
        rep, *read_installed_manifest())
    check_units(rep)
    check_services(rep)
    return rep


def configured_robot_user() -> str:
    """The account the deployment is meant to run as.

    The unit is the authority: install_kirra.sh renders User= from the invoking
    (sudo) user, so the unit records the decision that was actually made. Falling
    back to the calling user would make this check compare a value against
    itself whenever an operator ran the verifier from a different account.
    """
    return read_unit_user() or ""


def read_unit_user() -> str:
    try:
        d = subprocess.run(
            ["systemctl", "show", "kirra-consumer.service", "-p", "User", "--value"],
            capture_output=True, text=True, timeout=10)
        return d.stdout.strip()
    except Exception:  # noqa: BLE001 — an absent systemd is not a mismatch
        return ""


def read_port_owner(port: str) -> str:
    """Owner of the REAL device, following the symlink.

    /dev/myserial is a symlink and symlinks are always lrwxrwxrwx, so stat'ing
    it without -L reports nothing useful — a distinction that cost real time
    during the 2026-07-31 diagnosis.
    """
    try:
        import pwd
        return pwd.getpwuid(os.stat(port).st_uid).pw_name
    except Exception:  # noqa: BLE001 — unplugged board, not a mismatch
        return "<absent>"


def read_installed_manifest():
    """(recorded, current) digest maps for the manifest comparison."""
    try:
        recorded = parse_manifest(open(INSTALLED_MANIFEST).read())
    except OSError:
        return {}, {}
    return recorded, {p: sha256_file(p) for p in recorded}


def main(argv) -> int:
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    rep = run()
    if "--json" in argv:
        print(json.dumps({"rows": rep.rows, "failed": rep.failed}, indent=2))
        return 1 if rep.failed else 0
    print("== deployment verification ==")
    for r in rep.rows:
        mark = {"PASS": "✔", "FAIL": "❌", "WARN": "⚠"}[r["status"]]
        print(f"  {mark} {r['check']}: {r['detail']}")
        if r["fix"] and r["status"] != PASS:
            print(f"       ↳ fix: {r['fix']}")
    print(f"\n{'❌ ' + str(rep.failed) + ' check(s) FAILED' if rep.failed else '✔ deployment verified'}")
    return 1 if rep.failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
