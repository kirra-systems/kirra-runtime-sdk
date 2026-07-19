#!/usr/bin/env python3
"""Host tests for the pure KITT voice logic (`kitt_persona.py`, `kitt_boot.py`,
`kitt_ota.py` matcher).

Pure and dependency-light by construction: `kitt_persona` is stdlib-only and
`kitt_boot` imports `requests` LAZILY (inside `_read_posture`), so the boot
greeting decision and the `{name}` slot are exercised on a plain host with no
network, no ROS, and no TTS. Runs standalone (`python3 robot/kitt_voice_test.py`,
exit 1 on any failure); also importable under pytest.

Covers: the `{name}` slot (known/unknown), the posture-GATED boot greeting
(nominal vs degraded vs not-ready-yet, and stale-is-not-ready), the shutdown
line, and the OTA voice-command matcher precedence.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from kitt_boot import greeting_line, shutdown_line  # noqa: E402
from kitt_ota import match_command  # noqa: E402
from kitt_persona import name_slot  # noqa: E402


def _with_operator(value):
    """Context helper: set/clear KIRRA_KITT_OPERATOR, restoring the prior value.
    Single-process test env twiddling (not the Rust INV-13 set_var concern)."""
    class _Ctx:
        def __enter__(self):
            self.prev = os.environ.get("KIRRA_KITT_OPERATOR")
            if value is None:
                os.environ.pop("KIRRA_KITT_OPERATOR", None)
            else:
                os.environ["KIRRA_KITT_OPERATOR"] = value
            return self

        def __exit__(self, *a):
            if self.prev is None:
                os.environ.pop("KIRRA_KITT_OPERATOR", None)
            else:
                os.environ["KIRRA_KITT_OPERATOR"] = self.prev
    return _Ctx()


# --- {name} slot ------------------------------------------------------------

def test_name_slot_known_renders_comma_name() -> None:
    with _with_operator("Justin"):
        assert name_slot() == ", Justin"


def test_name_slot_unknown_is_empty() -> None:
    with _with_operator(None):
        assert name_slot() == ""


def test_name_slot_blank_is_treated_as_unknown() -> None:
    with _with_operator("   "):
        assert name_slot() == ""


# --- boot greeting: HONEST, posture-gated -----------------------------------

def test_greeting_nominal_fresh_claims_governor_nominal() -> None:
    with _with_operator(None):
        line = greeting_line(0, True)
    assert "governor nominal" in line and "online" in line


def test_greeting_degraded_fresh_says_degraded_not_nominal() -> None:
    with _with_operator(None):
        line = greeting_line(1, True)
    assert "degraded" in line and "governor nominal" not in line


def test_greeting_lockedout_is_not_ready() -> None:
    with _with_operator(None):
        line = greeting_line(2, True)
    assert "checking myself over" in line and "nominal" not in line


def test_greeting_no_read_is_not_ready() -> None:
    with _with_operator(None):
        assert "checking myself over" in greeting_line(None, False)


def test_greeting_stale_nominal_is_not_ready() -> None:
    # code 0 but NOT fresh (stale cache) must NOT claim ready — freshness gates.
    with _with_operator(None):
        line = greeting_line(0, False)
    assert "checking myself over" in line and "governor nominal" not in line


def test_greeting_uses_name_when_known() -> None:
    with _with_operator("Justin"):
        assert ", Justin" in greeting_line(0, True)


def test_shutdown_line_is_a_safe_stop_message() -> None:
    line = shutdown_line()
    assert "safe stop" in line and len(line) > 0


# --- OTA matcher precedence (apply/status before check) ----------------------

def test_ota_matcher_precedence() -> None:
    assert match_command("apply the update") == "apply"
    assert match_command("install update") == "apply"
    assert match_command("update status") == "status"
    assert match_command("check for updates") == "check"
    assert match_command("any updates?") == "check"
    assert match_command("take us to the kitchen") is None


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
    print("KITT voice-logic host tests (pure, no hardware):")
    sys.exit(_run_all())
