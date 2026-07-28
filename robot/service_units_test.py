#!/usr/bin/env python3
"""Structural tests for the shipped systemd units.

These parse the CHECKED-IN unit files (they are installed verbatim — the robot
installer only sed-renders `__KIRRA_ROBOT_USER__`, and deploy/systemd/install.sh
copies its units byte-for-byte), so what is asserted here is what lands in
/etc/systemd/system.

What they defend is one distinction that is easy to get wrong and expensive when
you do: **ordering is not a dependency.**

  After=  — "start me later in the boot ordering". Costs nothing at runtime.
  Wants=  — "also pull that unit in". Still no runtime coupling.
  Requires= / BindsTo= / PartOf= — a LIFECYCLE coupling: stop or restart the
            other unit and this one goes down too.

Rabbit talks to Ollama and to mick, and both calls are fail-soft per turn. If
those relationships were hard dependencies, restarting Ollama for thirty seconds
to swap a model would STOP THE WAKE LISTENER — the robot would go deaf and the
operator would have no way to ask it what was wrong. Losing the ears is worse
than losing the answers, so the coupling must stay soft.

No systemd needed; pure text. Runs standalone, also importable under pytest.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MICK = REPO / "deploy" / "systemd" / "kirra-mick.service"
VOICE = REPO / "robot" / "install" / "systemd" / "kirra-rabbit-voice.service"
INSTALLER = REPO / "robot" / "install" / "install_robot_units.sh"

# Directives that couple LIFECYCLES rather than just ordering.
HARD_DEPS = ("Requires", "Requisite", "BindsTo", "PartOf")

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


def directive(text: str, key: str) -> list[str]:
    """All values of `key=` across the unit, comment lines excluded, split on
    whitespace (systemd allows several units per directive line)."""
    out: list[str] = []
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("#") or "=" not in line:
            continue
        k, _, v = line.partition("=")
        if k.strip() == key:
            out.extend(v.split())
    return out


def directive_lines(text: str, key: str) -> list[str]:
    """One entry per `key=` LINE, value unsplit — for directives whose value is a
    single string (ExecStart, Restart) rather than a unit list."""
    out: list[str] = []
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("#") or "=" not in line:
            continue
        k, _, v = line.partition("=")
        if k.strip() == key:
            out.append(v.strip())
    return out


def _unit(path: Path) -> str:
    check(path.is_file(), f"missing unit: {path}")
    return path.read_text() if path.is_file() else ""


# ── Mick: ordered after Ollama, not chained to it ────────────────────────────

def test_mick_is_ordered_after_ollama():
    text = _unit(MICK)
    check("ollama.service" in directive(text, "After"),
          "kirra-mick.service must declare After=ollama.service")
    check("ollama.service" in directive(text, "Wants"),
          "kirra-mick.service must declare Wants=ollama.service")


def test_mick_does_not_hard_depend_on_ollama():
    text = _unit(MICK)
    for key in HARD_DEPS:
        vals = directive(text, key)
        check("ollama.service" not in vals,
              f"kirra-mick.service must not {key}=ollama.service — "
              "stopping Ollama would take the intent door down with it")


def test_mick_keeps_its_restart_policy():
    text = _unit(MICK)
    check(directive(text, "Restart") == ["always"],
          f"mick's restart policy changed: {directive(text, 'Restart')}")


# ── Rabbit voice: ordered after both, coupled to neither ─────────────────────

def test_rabbit_voice_is_ordered_after_ollama_and_mick():
    text = _unit(VOICE)
    after = directive(text, "After")
    for unit in ("ollama.service", "kirra-mick.service"):
        check(unit in after, f"kirra-rabbit-voice.service must declare After={unit}")


def test_rabbit_voice_never_hard_depends_on_a_downstream_service():
    """The load-bearing one. A hard dependency on either backend would make a
    momentary Ollama or mick outage STOP THE WAKE LISTENER."""
    text = _unit(VOICE)
    for key in HARD_DEPS:
        for unit in directive(text, key):
            check(unit not in ("ollama.service", "kirra-mick.service", "kirra.target"),
                  f"kirra-rabbit-voice.service has {key}={unit} — a downstream "
                  "outage would stop wake listening")


def test_rabbit_voice_keeps_its_existing_restart_policy():
    """`on-failure`, NOT `always`, and deliberately so: with the wake word
    disabled wake_word.py exits 0 and the unit must rest inactive rather than
    restart-loop. Changing it to `always` would spin a disabled listener."""
    text = _unit(VOICE)
    check(directive(text, "Restart") == ["on-failure"],
          f"restart policy changed: {directive(text, 'Restart')} (expected on-failure)")
    check(directive(text, "RestartSec"), "RestartSec must stay set (no tight respawn)")


def test_downstream_unavailability_is_handled_in_process_not_by_restarting():
    """A restart policy cannot fix an unreachable HTTP endpoint — it would just
    respawn into the same failure. So the handling must live in the code, and it
    does: both backends have a spoken, fail-soft arm."""
    converse = (REPO / "robot" / "rabbit_converse.py").read_text()
    check("voice module is offline" in converse,
          "the Ollama-unreachable arm must still SAY something truthful")
    check("can't reach my driving control" in converse,
          "the mick-unreachable arm must still SAY something truthful")
    # And neither arm exits the process — the listener keeps listening.
    check("sys.exit" not in converse.split("def handle_turn", 1)[-1],
          "a downstream outage must never exit the turn loop")


# ── one ExecStart, and the installer preserves what we wrote ─────────────────

def test_each_unit_declares_exactly_one_execstart():
    """A second ExecStart= in a Type=simple unit is a systemd ERROR (only
    oneshot may repeat it), and a stale drop-in that re-declares it silently
    replaces the command. Catch a duplicate in the shipped file at least."""
    for path in (MICK, VOICE):
        n = len(directive_lines(_unit(path), "ExecStart"))
        check(n == 1, f"{path.name} declares {n} ExecStart lines (want exactly 1)")


def test_the_installer_copies_the_voice_unit_without_rewriting_its_unit_section():
    """The robot installer renders ONLY the user placeholder. If it grew a
    broader sed the ordering lines could be silently dropped on install."""
    sh = _unit(INSTALLER)
    check("kirra-rabbit-voice.service" in sh,
          "the installer must still install the voice unit")
    seds = re.findall(r'sed\s+"([^"]+)"', sh) + re.findall(r"sed\s+'([^']+)'", sh)
    for expr in seds:
        check("__KIRRA_ROBOT_USER__" in expr,
              f"the installer rewrites more than the user placeholder: sed {expr!r}")
    check("__KIRRA_ROBOT_USER__" in _unit(VOICE),
          "the voice unit must still carry the placeholder the installer renders")


def test_no_stale_dropin_shipped_for_these_units():
    """A checked-in `<unit>.service.d/*.conf` would override the file above
    without showing up in it. There is one legitimate drop-in in the repo
    (Ollama server tuning); it must not target our units."""
    for d in REPO.rglob("*.service.d"):
        check(d.name not in (f"{MICK.name}.d", f"{VOICE.name}.d"),
              f"unexpected drop-in directory shipped: {d}")
    tuning = REPO / "robot" / "install" / "kirra-ollama-tuning.conf"
    if tuning.is_file():
        text = tuning.read_text()
        check("ExecStart" not in text,
              "the Ollama tuning drop-in must set Environment only, never ExecStart")


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items())
             if k.startswith("test_") and callable(v)]
    print("service_units_test:")
    for t in tests:
        before = len(_FAILURES)
        t()
        print(f"  {'ok  ' if len(_FAILURES) == before else 'FAIL'} {t.__name__}")
    if _FAILURES:
        print(f"\n{len(_FAILURES)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"\nall {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(_run_all())
