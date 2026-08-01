#!/usr/bin/env python3
"""Host tests for the microphone CAPTURE-BACKEND selection: the fail-closed
`wake_word.resolve_record_cmd` resolver, the `robot/pulse_capture.sh` wrapper's
contract (--check / --print-cmd under stub pactl/parec binaries via the
KIRRA_PULSE_PACTL / KIRRA_PULSE_PAREC seams — no audio, no PulseAudio needed),
and the deployment sources actually emitting the Pulse-based configuration
(rabbit.env.example / installer / the user unit).

WHY (2026-07 field failure): with the desktop session's sound server active,
direct `arecord -D plughw:…` capture fails "Device or resource busy" even with
the PCM closed (device reservation) — the recorder dies instantly and the wake
listener loops "capture stream ended". Capture must go THROUGH the server with
an EXPLICIT source; a wrong microphone is worse than a visible failure, so
every misconfiguration here must be a refusal, never a fallback.

Runs standalone (`python3 robot/wake_capture_test.py`, exit 1 on any failure);
also importable under pytest.
"""
from __future__ import annotations

import os
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

ROBOT = Path(__file__).resolve().parent
REPO = ROBOT.parent
WRAPPER = ROBOT / "pulse_capture.sh"

sys.path.insert(0, str(ROBOT))
from wake_word import (  # noqa: E402
    BYTES_PER_SAMPLE, PULSE_CAPTURE_BASENAME, SAMPLE_RATE, resolve_record_cmd,
)

C_MEDIA = "alsa_input.usb-C-Media_Electronics_Inc._USB_PnP_Sound_Device-00.analog-mono"


def _run_wrapper(args, env_extra):
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("KIRRA_PULSE")}
    env.update(env_extra)
    return subprocess.run([str(WRAPPER)] + args, capture_output=True,
                          text=True, timeout=20.0, env=env)


def _stub(dir_path: Path, name: str, body: str) -> str:
    p = dir_path / name
    p.write_text("#!/bin/sh\n" + body)
    p.chmod(p.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return str(p)


PACTL_TWO_SOURCES = (
    'if [ "$1 $2 $3" != "list short sources" ]; then '
    'echo "unexpected: $*" >&2; exit 2; fi\n'
    "printf '0\\talsa_input.stub-one.analog-mono\\tmodule.c\\ts16le 1ch 16000Hz\\tSUSPENDED\\n'\n"
    f"printf '1\\t{C_MEDIA}\\tmodule.c\\ts16le 1ch 16000Hz\\tSUSPENDED\\n'\n")


# --- the resolver (wake_word.py) ---------------------------------------------

def test_default_backend_is_the_pulse_wrapper() -> None:
    argv, err = resolve_record_cmd(None, C_MEDIA, str(ROBOT))
    assert err is None
    assert argv == [str(WRAPPER)]
    assert os.path.basename(argv[0]) == PULSE_CAPTURE_BASENAME


def test_default_without_a_source_is_a_fatal_not_a_guess() -> None:
    for src in (None, "", "   "):
        argv, err = resolve_record_cmd(None, src, str(ROBOT))
        assert argv is None
        assert err and "KIRRA_PULSE_SOURCE" in err
        assert "Refusing to guess" in err


def test_pulse_backends_are_accepted_verbatim() -> None:
    for cmd in (f"parec --device={C_MEDIA} --format=s16le --rate=16000 --channels=1",
                "/opt/kirra/robot/pulse_capture.sh"):
        argv, err = resolve_record_cmd(cmd, None, str(ROBOT))
        assert err is None, f"{cmd!r} must be accepted: {err}"
        assert argv[0] in ("parec", "/opt/kirra/robot/pulse_capture.sh")


def test_bare_arecord_is_rejected_never_silently_replaced() -> None:
    # No -D → refused, even when a Pulse source IS configured (an explicit but
    # invalid command must ERROR, not be silently swapped for the default).
    argv, err = resolve_record_cmd(
        "arecord -f S16_LE -r 16000 -c 1 -t raw", C_MEDIA, str(ROBOT))
    assert argv is None
    assert err and "-D" in err and "arecord" in err


def test_arecord_with_a_pinned_device_remains_accepted() -> None:
    # Back-compat where it does NOT weaken safety: an explicitly pinned ALSA
    # device stays valid (the headless/no-session topology).
    cmd = "arecord -D plughw:CARD=Device,DEV=0 -f S16_LE -r 16000 -c 1 -t raw"
    argv, err = resolve_record_cmd(cmd, None, str(ROBOT))
    assert err is None
    assert argv[0] == "arecord" and "-D" in argv


# --- the wrapper (pulse_capture.sh) ------------------------------------------

def test_wrapper_cmd_requests_the_wake_stream_contract() -> None:
    p = _run_wrapper(["--print-cmd"], {"KIRRA_PULSE_SOURCE": C_MEDIA})
    assert p.returncode == 0, p.stderr
    args = p.stdout.splitlines()
    assert os.path.basename(args[0]) == "parec"
    assert f"--device={C_MEDIA}" in args, "the source must be EXPLICIT"
    assert "--format=s16le" in args
    assert f"--rate={SAMPLE_RATE}" in args      # 16000 — wake_word.py parity
    assert "--channels=1" in args
    assert SAMPLE_RATE == 16_000 and BYTES_PER_SAMPLE == 2


def test_wrapper_requires_an_explicit_source() -> None:
    p = _run_wrapper(["--print-cmd"], {})
    assert p.returncode != 0
    assert "KIRRA_PULSE_SOURCE" in p.stderr
    assert "pactl list short sources" in p.stderr


def test_wrapper_check_passes_when_source_is_present() -> None:
    with tempfile.TemporaryDirectory() as d:
        env = {
            "KIRRA_PULSE_SOURCE": C_MEDIA,
            "KIRRA_PULSE_PACTL": _stub(Path(d), "pactl", PACTL_TWO_SOURCES),
            "KIRRA_PULSE_PAREC": _stub(Path(d), "parec", "exit 0\n"),
        }
        p = _run_wrapper(["--check"], env)
        assert p.returncode == 0, p.stderr
        assert C_MEDIA in p.stderr  # the ok line names the validated source


def test_wrapper_missing_source_fails_with_a_useful_message() -> None:
    with tempfile.TemporaryDirectory() as d:
        env = {
            "KIRRA_PULSE_SOURCE": "alsa_input.usb-Not_Plugged_In-00.analog-mono",
            "KIRRA_PULSE_PACTL": _stub(Path(d), "pactl", PACTL_TWO_SOURCES),
            "KIRRA_PULSE_PAREC": _stub(Path(d), "parec", "exit 0\n"),
        }
        p = _run_wrapper(["--check"], env)
        assert p.returncode != 0
        assert "Not_Plugged_In" in p.stderr, "must name the missing source"
        assert "available sources" in p.stderr and C_MEDIA in p.stderr
        assert "NOT falling back" in p.stderr


def test_wrapper_missing_parec_is_actionable() -> None:
    with tempfile.TemporaryDirectory() as d:
        env = {
            "KIRRA_PULSE_SOURCE": C_MEDIA,
            "KIRRA_PULSE_PACTL": _stub(Path(d), "pactl", PACTL_TWO_SOURCES),
            "KIRRA_PULSE_PAREC": str(Path(d) / "no-such-parec"),
        }
        p = _run_wrapper(["--check"], env)
        assert p.returncode != 0
        assert "parec not found" in p.stderr
        assert "pulseaudio-utils" in p.stderr


def test_wrapper_unreachable_sound_server_names_the_session_problem() -> None:
    with tempfile.TemporaryDirectory() as d:
        env = {
            "KIRRA_PULSE_SOURCE": C_MEDIA,
            "KIRRA_PULSE_PACTL": _stub(Path(d), "pactl",
                                       'echo "Connection failure: Connection refused" >&2; exit 1\n'),
            "KIRRA_PULSE_PAREC": _stub(Path(d), "parec", "exit 0\n"),
        }
        p = _run_wrapper(["--check"], env)
        assert p.returncode != 0
        assert "user session" in p.stderr  # points at the user-unit topology


# --- deployment sources emit the corrected configuration ---------------------

def _active_lines(path: Path) -> dict[str, str]:
    out: dict[str, str] = {}
    for line in path.read_text().splitlines():
        s = line.strip()
        if s and not s.startswith("#") and "=" in s:
            k, v = s.split("=", 1)
            out[k.strip()] = v.strip().strip('"')
    return out

def test_env_example_emits_pulse_capture_not_direct_alsa() -> None:
    env = _active_lines(REPO / "robot/install/rabbit.env.example")
    src = env.get("KIRRA_PULSE_SOURCE", "")
    assert src.startswith("alsa_input."), "explicit Pulse source must be ACTIVE"
    assert "pulse_capture.sh" in env.get("KIRRA_VAD_CAPTURE_CMD", "")
    rec = env.get("KIRRA_RECORD_CMD", "")
    assert "arecord" not in rec and "plughw" not in rec, \
        f"turn recorder must not be direct ALSA by default: {rec!r}"
    wake = env.get("KIRRA_WAKE_RECORD_CMD", "")
    assert wake == "", "wake capture override must default to unset (Pulse)"


def test_installer_stages_the_wrapper_and_the_user_unit() -> None:
    installer = (REPO / "robot/install/install_robot_units.sh").read_text()
    assert "pulse_capture.sh" in installer
    assert "user/rabbit-voice.service" in installer
    unit = REPO / "robot/install/systemd/user/rabbit-voice.service"
    assert unit.is_file(), "user unit missing"
    text = unit.read_text()
    assert "wake_word.py" in text and "rabbit_voice.sh" in text
    assert "EnvironmentFile=/etc/kirra/robot.env" in text


def test_wrapper_is_executable() -> None:
    assert os.access(WRAPPER, os.X_OK), "pulse_capture.sh must be executable"


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
    print("wake_capture_test:", "ALL OK" if failures == 0 else f"{failures} FAILURE(S)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(_run_all())
