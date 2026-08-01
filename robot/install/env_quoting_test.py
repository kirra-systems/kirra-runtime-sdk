#!/usr/bin/env python3
"""Shell-semantics gate for /etc/kirra/robot.env value quoting.

THE BUG CLASS THIS PINS (found live on the R2, 2026-08): robot.env is parsed
by BOTH systemd `EnvironmentFile=` AND bash `source` (rabbit_voice.sh, the
doctor, the bring-up docs). The two disagree about an UNQUOTED multi-word
value:

    KIRRA_RECORD_CMD=python3 /opt/kirra/robot/vad_record.py

systemd reads the whole tail as the value; bash reads `VAR=word command` — it
EXECUTES vad_record.py during the source (no WAV argument → startup FATAL) and
leaves the variable EMPTY, so the turn recorder later runs the WAV path itself
("Permission denied"). The only form both parse identically is the
double-quoted value, and grep-level checks are NOT enough — the broken file
looked plausible — so these tests run REAL `bash -c 'set -a; source …'` and
assert actual shell semantics: round-tripped values, no side execution, and
the `$KIRRA_RECORD_CMD "$wav"` word-split invocation contract rabbit_voice.sh
relies on. The linter's new unquoted-multi-word check and its --fix requoting
are exercised the same way.

Runs standalone (`python3 robot/install/env_quoting_test.py`, exit 1 on any
failure); also importable under pytest.
"""
from __future__ import annotations

import os
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent          # robot/install
ENV_EXAMPLE = HERE / "rabbit.env.example"
LINTER = HERE / "lint_robot_env.sh"

VAD_CMD = "python3 /opt/kirra/robot/vad_record.py"


def _source_and_print(env_file: str, var: str) -> subprocess.CompletedProcess:
    """The REAL consumer semantics: bash sources the file (set -a) and prints
    the variable — exactly what rabbit_voice.sh / the doctor do."""
    return subprocess.run(
        ["bash", "-c", 'set -a; source "$1"; set +a; printf "%s" "${'
         + var + ':-}"', "bash", env_file],
        capture_output=True, text=True, timeout=20.0)


def _lint(env_file: str, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["bash", str(LINTER), env_file, *args],
                          capture_output=True, text=True, timeout=20.0)


def _stub(dir_path: Path, name: str, body: str) -> Path:
    p = dir_path / name
    p.write_text("#!/bin/sh\n" + body)
    p.chmod(p.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return p


# --- the shipped example is bash-source-safe ---------------------------------

def test_example_env_round_trips_the_record_cmd_through_bash_source() -> None:
    p = _source_and_print(str(ENV_EXAMPLE), "KIRRA_RECORD_CMD")
    assert p.returncode == 0, f"sourcing rabbit.env.example failed: {p.stderr}"
    assert p.stdout == VAD_CMD, (
        f"KIRRA_RECORD_CMD round-tripped as {p.stdout!r}, want {VAD_CMD!r} — "
        "the example's quoting is broken for bash `source`")


def test_example_env_round_trips_every_multiword_cmd() -> None:
    for var in ("KIRRA_STT_CMD", "KIRRA_VAD_CAPTURE_CMD", "KIRRA_PULSE_SOURCE"):
        p = _source_and_print(str(ENV_EXAMPLE), var)
        assert p.returncode == 0, f"sourcing failed on {var}: {p.stderr}"
        assert p.stdout.strip(), f"{var} sourced to empty — unquoted multi-word line?"
        assert "\n" not in p.stdout


def test_example_env_is_lint_clean() -> None:
    p = _lint(str(ENV_EXAMPLE))
    assert p.returncode == 0, f"linter rejects the shipped example:\n{p.stdout}{p.stderr}"


# --- the bug class itself, reproduced with a sentinel ------------------------

def test_unquoted_multiword_line_executes_the_tail_and_empties_the_var() -> None:
    """Non-vacuity: prove the failure mode is real, exactly as seen on the R2 —
    sourcing the UNQUOTED line runs the recorder (marker created) and the var
    comes back EMPTY. If bash semantics ever change, this tells us."""
    with tempfile.TemporaryDirectory() as d:
        marker = Path(d) / "executed.marker"
        stub = _stub(Path(d), "vad_stub.sh", f'touch "{marker}"\nexit 2\n')
        env = Path(d) / "robot.env"
        env.write_text(f"KIRRA_RECORD_CMD=python3 {stub}\n")
        p = _source_and_print(str(env), "KIRRA_RECORD_CMD")
        assert marker.exists(), "unquoted line did NOT execute the tail — premise broken?"
        assert p.stdout == "", f"var should be empty after the mis-parse, got {p.stdout!r}"


def test_quoted_multiword_line_does_not_execute_and_round_trips() -> None:
    with tempfile.TemporaryDirectory() as d:
        marker = Path(d) / "executed.marker"
        stub = _stub(Path(d), "vad_stub.sh", f'touch "{marker}"\nexit 2\n')
        env = Path(d) / "robot.env"
        env.write_text(f'KIRRA_RECORD_CMD="python3 {stub}"\n')
        p = _source_and_print(str(env), "KIRRA_RECORD_CMD")
        assert not marker.exists(), "QUOTED line executed its value — quoting broken"
        assert p.stdout == f"python3 {stub}"


def test_word_split_invocation_appends_the_wav_as_final_arg() -> None:
    """rabbit_voice.sh's contract: `$KIRRA_RECORD_CMD "$wav"` — word-split
    command, WAV path appended last. Prove it end-to-end with a stub recorder
    reached through a sourced, quoted env line."""
    with tempfile.TemporaryDirectory() as d:
        argv_log = Path(d) / "argv.log"
        stub = _stub(Path(d), "recorder.sh",
                     f'printf "%s\\n" "$@" > "{argv_log}"\nexit 0\n')
        env = Path(d) / "robot.env"
        env.write_text(f'KIRRA_RECORD_CMD="{stub} --flag-a --flag-b"\n')
        wav = str(Path(d) / "turn.wav")
        p = subprocess.run(
            ["bash", "-c",
             'set -a; source "$1"; set +a; $KIRRA_RECORD_CMD "$2"',
             "bash", str(env), wav],
            capture_output=True, text=True, timeout=20.0)
        assert p.returncode == 0, p.stderr
        args = argv_log.read_text().splitlines()
        assert args == ["--flag-a", "--flag-b", wav], (
            f"stub recorder saw argv {args}, want flags then the WAV last")


# --- the linter catches what it previously missed ----------------------------

def _lint_tmp(content: str, *args: str) -> subprocess.CompletedProcess:
    with tempfile.NamedTemporaryFile("w", suffix=".env", delete=False) as f:
        f.write(content)
        name = f.name
    try:
        return _lint(name, *args)
    finally:
        os.unlink(name)


def test_linter_passes_quoted_multiword() -> None:
    p = _lint_tmp(f'KIRRA_RECORD_CMD="{VAD_CMD}"\n')
    assert p.returncode == 0, p.stdout


def test_linter_passes_unquoted_single_token() -> None:
    p = _lint_tmp("KIRRA_RECORD_CMD=python3\n")
    assert p.returncode == 0, p.stdout


def test_linter_fails_unquoted_multiword_with_actionable_message() -> None:
    p = _lint_tmp(f"KIRRA_RECORD_CMD={VAD_CMD}\n")
    assert p.returncode == 1, (
        "the linter previously blessed exactly this file — it must now fail: "
        f"rc={p.returncode}\n{p.stdout}")
    assert "UNQUOTED multi-word" in p.stdout
    assert f'KIRRA_RECORD_CMD="{VAD_CMD}"' in p.stdout, "fix hint must show the quoted form"


def test_linter_still_flags_inline_comments_and_duplicates() -> None:
    p = _lint_tmp("KIRRA_R2_WHEELBASE_M=0.229   # CHOSEN\n")
    assert p.returncode == 1 and "inline comment" in p.stdout
    p = _lint_tmp("KIRRA_A=1\nKIRRA_A=2\n")
    assert p.returncode == 1 and "duplicate key" in p.stdout


def test_linter_fix_requotes_and_result_survives_bash_source() -> None:
    """--fix must repair the broken line into the quoted form, and the PROOF is
    semantic: the fixed file lints clean AND round-trips through a real bash
    source with the full command intact."""
    with tempfile.TemporaryDirectory() as d:
        env = Path(d) / "robot.env"
        env.write_text(f"KIRRA_RECORD_CMD={VAD_CMD}\n"
                       'KIRRA_VAD_CAPTURE_CMD="/opt/kirra/robot/pulse_capture.sh"\n')
        p = _lint(str(env), "--fix")
        assert p.returncode == 0, f"--fix failed: {p.stdout}{p.stderr}"
        text = env.read_text()
        assert f'KIRRA_RECORD_CMD="{VAD_CMD}"' in text, text
        assert 'KIRRA_VAD_CAPTURE_CMD="/opt/kirra/robot/pulse_capture.sh"' in text
        assert _lint(str(env)).returncode == 0, "fixed file must lint clean"
        rt = _source_and_print(str(env), "KIRRA_RECORD_CMD")
        assert rt.stdout == VAD_CMD, f"fixed file sources to {rt.stdout!r}"


# --- standalone runner (house pattern) ----------------------------------------

def _run_all() -> int:
    failures = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"  ok  {name}")
            except AssertionError as e:
                failures += 1
                print(f"FAIL  {name}: {e}")
    print("env_quoting_test:", "ALL OK" if failures == 0 else f"{failures} FAILURE(S)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(_run_all())
