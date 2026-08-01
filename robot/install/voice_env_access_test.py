#!/usr/bin/env python3
"""Host tests for the user voice unit's EnvironmentFile access chain.

The live failure: rabbit-voice.service (USER unit) died before ExecStart with
Result=resources / pid=0 / no log, because /etc/kirra had been re-tightened to
0750 kirra:kirra (deploy/systemd/install.sh protects kirra.env's governor
secrets on EVERY run) and the operator's user manager — unlike PID 1, which
opens the system units' EnvironmentFile as root — could not traverse the
directory. The fix is a narrow ACL (traverse-only on the dir, read-only on
robot.env) applied by robot/install/ensure_voice_env_access.sh.

Layers tested here:
  1. the pure classifier in verify_deployment.diagnose_env_access (all states);
  2. the helper's ACL behavior on tmpdir fixtures (grants are narrow, idempotent,
     survive re-runs, restorable after inode replacement; unrelated files and
     base modes untouched) — ACL entries name `root` as the subject so the test
     needs NO test user and NO root to *apply* (owners may set ACLs on their own
     files); actually-denied assertions are root-only and skip loudly;
  3. lint_robot_env.sh --fix preserves ACLs across its inode-replacing rewrite;
  4. fail-closed paths: unknown user, missing acl tools, missing file;
  5. source-level wiring: the installer grants access after staging the user
     unit and nothing in the chain broadens /etc/kirra.

Runs standalone (`python3 robot/install/voice_env_access_test.py`, exit 1 on
any failure); ACL-unsupported filesystems skip the ACL layer loudly.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
HELPER = HERE / "ensure_voice_env_access.sh"
LINTER = HERE / "lint_robot_env.sh"
INSTALLER = HERE / "install_robot_units.sh"

_F: list[str] = []


def check(cond, msg):
    if not cond:
        _F.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


def sh(args, **kw):
    return subprocess.run(args, capture_output=True, text=True, timeout=60, **kw)


def getfacl(path) -> str:
    return sh(["getfacl", "--omit-header", "-p", str(path)]).stdout


def acl_supported() -> bool:
    if not shutil.which("setfacl"):
        return False
    with tempfile.NamedTemporaryFile() as f:
        return sh(["setfacl", "-m", "u:root:r--", f.name]).returncode == 0


# --- 1. pure classifier -------------------------------------------------------

def test_classifier_states():
    sys.path.insert(0, str(HERE))
    import verify_deployment as vd

    def probes(**kw):
        base = {"unit_user": "jetson", "verified_as_user": True,
                "dir_traversable": True, "file_exists": True,
                "file_readable": True}
        base.update(kw)
        return base

    st, detail, fix = vd.diagnose_env_access(probes())
    check(st == vd.PASS, f"all-good must PASS, got {st}")
    check(fix is None, "PASS carries no fix")

    st, detail, fix = vd.diagnose_env_access(probes(dir_traversable=False))
    check(st == vd.FAIL, "non-traversable parent must FAIL")
    check("traverse" in detail and "Result=resources" in detail,
          f"traverse failure must name the symptom: {detail}")
    check("ensure_voice_env_access.sh" in (fix or ""),
          "traverse failure must name the repair helper")

    st, detail, fix = vd.diagnose_env_access(probes(file_exists=False))
    check(st == vd.FAIL, "missing file must FAIL")
    check("install_kirra.sh" in (fix or ""), "missing file fix renders the env first")

    st, detail, fix = vd.diagnose_env_access(probes(file_readable=False))
    check(st == vd.FAIL, "unreadable file must FAIL")
    check("READ" in detail, f"unreadable is distinct from untraversable: {detail}")

    st, detail, fix = vd.diagnose_env_access(probes(verified_as_user=False))
    check(st == vd.WARN, "unverifiable must WARN, never PASS")
    check("--check" in (fix or ""), "unverifiable points at the read-only check")

    st, detail, fix = vd.diagnose_env_access(probes(unit_user=""))
    check(st == vd.WARN, "unknown unit user must WARN, never PASS")

    # 'deployment verified' must be unreachable on a FAIL row.
    rep = vd.Report()
    rep.add("user voice unit can read robot.env", vd.FAIL, "x", fix="y")
    check(rep.failed == 1, "a FAIL row must count as failed")


# --- 2. helper ACL behavior (no root, no test user needed) --------------------

def _fixture(tmp: Path):
    d = tmp / "etc-kirra"
    d.mkdir(mode=0o750)
    env = d / "robot.env"
    env.write_text("KIRRA_WAKE_ENABLED=1\n")
    env.chmod(0o644)
    secret = d / "kirra.env"
    secret.write_text("KIRRA_ADMIN_TOKEN=sekrit\n")
    secret.chmod(0o600)
    return d, env, secret


def _run_helper(d, env, extra=()):
    return sh(["/bin/bash", str(HELPER), "--user", "root",
               "--dir", str(d), "--file", str(env), *extra])


def test_helper_grants_are_narrow_and_idempotent():
    if not acl_supported():
        print("  SKIP: filesystem/tools do not support POSIX ACLs — helper ACL tests skipped")
        return
    with tempfile.TemporaryDirectory() as t:
        d, env, secret = _fixture(Path(t))
        mode_before = (d.stat().st_mode, env.stat().st_mode, secret.stat().st_mode)

        r = _run_helper(d, env)
        check(r.returncode == 0, f"helper apply failed: {r.stdout}{r.stderr}")
        check("user:root:--x" in getfacl(d),
              f"dir must get traverse-ONLY for the user: {getfacl(d)}")
        check("user:root:r--" in getfacl(env),
              f"env file must get read-ONLY for the user: {getfacl(env)}")
        check("user:root" not in getfacl(secret),
              "the helper must NEVER touch other files in the directory")
        check((d.stat().st_mode, env.stat().st_mode, secret.stat().st_mode)
              == mode_before, "base permission bits must be unchanged")

        first = (getfacl(d), getfacl(env))
        r2 = _run_helper(d, env)
        check(r2.returncode == 0, "second run must succeed")
        check((getfacl(d), getfacl(env)) == first,
              "repeated runs must not accumulate or alter entries")


def test_replacement_drops_acl_and_helper_restores():
    if not acl_supported():
        print("  SKIP: no ACL support — replacement test skipped")
        return
    with tempfile.TemporaryDirectory() as t:
        d, env, _ = _fixture(Path(t))
        _run_helper(d, env)
        check("user:root:r--" in getfacl(env), "precondition: entry present")
        # Simulate sed -i / install / tmp+rename: a NEW inode replaces the file.
        tmp = d / "robot.env.tmp"
        tmp.write_text(env.read_text())
        tmp.chmod(0o644)
        tmp.rename(env)
        check("user:root:r--" not in getfacl(env),
              "inode replacement is expected to drop the file ACL (the hazard)")
        check("user:root:--x" in getfacl(d),
              "the directory ACL must survive file replacement")
        r = _run_helper(d, env)
        check(r.returncode == 0 and "user:root:r--" in getfacl(env),
              "re-running the helper must restore file access")


def test_lint_fix_preserves_acl():
    if not acl_supported():
        print("  SKIP: no ACL support — lint --fix ACL test skipped")
        return
    with tempfile.TemporaryDirectory() as t:
        d, env, _ = _fixture(Path(t))
        env.write_text("KIRRA_R2_WHEELBASE_M=0.229   # inline comment trap\n")
        _run_helper(d, env)
        check("user:root:r--" in getfacl(env), "precondition: entry present")
        r = sh(["/bin/bash", str(LINTER), str(env), "--fix"])
        check(r.returncode == 0, f"lint --fix failed: {r.stdout}{r.stderr}")
        check("# inline" not in env.read_text(), "--fix must still normalize")
        check("user:root:r--" in getfacl(env),
              f"lint --fix must preserve the voice-unit ACL: {getfacl(env)}")


def test_fail_closed_paths():
    with tempfile.TemporaryDirectory() as t:
        d, env, _ = _fixture(Path(t))

        r = sh(["/bin/bash", str(HELPER), "--user", "zz_no_such_user_9",
                "--dir", str(d), "--file", str(env)])
        check(r.returncode != 0, "unknown user must fail closed")
        check("zz_no_such_user_9" in r.stderr + r.stdout,
              "unknown-user error must name the account")

        empty = Path(t) / "empty-kirra"
        empty.mkdir()
        r = _run_helper(empty, empty / "robot.env")
        check(r.returncode != 0, "missing env file must fail closed")
        check("install_kirra.sh" in r.stderr + r.stdout,
              "missing-file error must say what renders it")

        # Guardrails (the helper runs under sudo — a typo must not grant
        # access to a secret): only a file NAMED robot.env, directly under
        # the granted directory, is ever a valid target.
        r = _run_helper(d, d / "kirra.env")
        check(r.returncode != 0, "a non-robot.env target must be refused")
        check("robot.env ONLY" in r.stderr + r.stdout,
              f"secret-target refusal must state the rule: {r.stderr}")
        outside = Path(t) / "robot.env"
        outside.write_text("X=1\n")
        r = _run_helper(d, outside)
        check(r.returncode != 0,
              "a robot.env OUTSIDE the granted directory must be refused")
        check("not directly inside" in r.stderr + r.stdout,
              f"mismatched-parent refusal must say so: {r.stderr}")

        # setfacl absent: a stub PATH with the tools the script needs BEFORE
        # the acl check (id), but no setfacl/getfacl.
        stub = Path(t) / "stubbin"
        stub.mkdir()
        for tool in ("id",):
            real = shutil.which(tool)
            if real:
                (stub / tool).symlink_to(real)
        r = sh(["/bin/bash", str(HELPER), "--user", "root",
                "--dir", str(d), "--file", str(env)],
               env={"PATH": str(stub)})
        check(r.returncode != 0, "missing setfacl must fail, not silently skip")
        check("acl" in (r.stderr + r.stdout).lower(),
              f"missing-tool error must name the acl package: {r.stderr}")


# --- 3. root-only integration: actual denial + grant for a real other user ----

def test_root_integration_denial_and_grant():
    if os.geteuid() != 0:
        print("  SKIP: not root — real-denial integration test skipped "
              "(the ACL layer above still ran)")
        return
    if not acl_supported():
        print("  SKIP: no ACL support — root integration skipped")
        return
    probe = sh(["id", "-u", "nobody"])
    if probe.returncode != 0:
        print("  SKIP: no 'nobody' account — root integration skipped")
        return
    with tempfile.TemporaryDirectory() as t:
        os.chmod(t, 0o755)  # 'nobody' must reach the fixture parent
        d, env, secret = _fixture(Path(t))

        def nobody_can(flag, path):
            return sh(["runuser", "-u", "nobody", "--",
                       "test", flag, str(path)]).returncode == 0

        check(not nobody_can("-r", env),
              "precondition: 0750 parent blocks the service user")
        r = sh(["/bin/bash", str(HELPER), "--user", "nobody",
                "--dir", str(d), "--file", str(env)])
        check(r.returncode == 0, f"grant for nobody failed: {r.stdout}{r.stderr}")
        check(nobody_can("-x", d), "after grant: traverse works")
        check(nobody_can("-r", env), "after grant: env file readable")
        check(not sh(["runuser", "-u", "nobody", "--", "ls", str(d)]).returncode == 0,
              "traverse-only: the user must NOT be able to LIST the directory")
        check(not nobody_can("-r", secret),
              "the user must NOT be able to read unrelated secrets")

        r = sh(["/bin/bash", str(HELPER), "--user", "nobody",
                "--dir", str(d), "--file", str(env), "--remove"])
        check(r.returncode == 0 and not nobody_can("-r", env),
              "--remove must roll the grant back")


# --- 4. source-level wiring ---------------------------------------------------

def test_installer_wires_the_helper():
    src = INSTALLER.read_text()
    check("ensure_voice_env_access.sh" in src,
          "install_robot_units.sh must grant user-unit env access when staging "
          "the user unit")
    check("/opt/kirra" in src.split("ensure_voice_env_access.sh")[1][:200]
          or "${OPT}/robot/ensure_voice_env_access.sh" in src,
          "the helper must be STAGED so repair hints work without a checkout")
    check("if ! sudo" in src,
          "a helper failure must be a loud warning, not abort the whole "
          "staging (set -e)")
    check("chmod 755 /etc/kirra" not in src and "chmod -R" not in src,
          "the installer must not broaden /etc/kirra")
    helper_src = HELPER.read_text()
    check("--default" not in helper_src
          and "setfacl -d" not in helper_src
          and "setfacl -R" not in helper_src,
          "the helper must not use default/recursive ACLs (future files must "
          "not inherit access)")
    helper_code = "\n".join(line for line in helper_src.splitlines()
                            if not line.lstrip().startswith("#"))
    check("chmod" not in helper_code,
          "the helper must not change permission bits at all — ACL entries only")
    # Both unit families must keep loading the SAME env file, or the ACL
    # target and the units drift apart.
    sys_unit = (HERE / "systemd" / "kirra-rabbit-voice.service").read_text()
    user_unit = (HERE / "systemd" / "user" / "rabbit-voice.service").read_text()
    for name, src_ in (("system", sys_unit), ("user", user_unit)):
        check("EnvironmentFile=/etc/kirra/robot.env" in src_,
              f"{name} unit must load /etc/kirra/robot.env")


def test_shell_syntax():
    for f in (HELPER, LINTER, INSTALLER):
        r = sh(["bash", "-n", str(f)])
        check(r.returncode == 0, f"bash -n {f.name}: {r.stderr}")


# --- standalone runner (house pattern) ----------------------------------------

def main() -> int:
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            print(f"-- {name}")
            fn()
    if _F:
        print(f"voice_env_access_test: {len(_F)} FAILURE(S)")
        return 1
    print("voice_env_access_test: ALL OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
