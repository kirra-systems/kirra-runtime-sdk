#!/usr/bin/env python3
"""Host tests for the pure Rabbit voice logic (`rabbit_persona.py`, `rabbit_boot.py`,
`rabbit_ota.py` matcher).

Pure and dependency-light by construction: `rabbit_persona` is stdlib-only and
`rabbit_boot` imports `requests` LAZILY (inside `_read_posture`), so the boot
greeting decision and the `{name}` slot are exercised on a plain host with no
network, no ROS, and no TTS. Runs standalone (`python3 robot/rabbit_voice_test.py`,
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
from rabbit_boot import (  # noqa: E402
    RABIT_EXPANSION, greeting_line, misconfig_line, shutdown_line,
)
from rabbit_ota import match_command  # noqa: E402
from rabbit_persona import (  # noqa: E402
    classify_model_pin, name_slot, read_model_pin, read_model_pin_record,
    write_model_pin,
)
from rabbit_ask import RABBIT_SYSTEM  # noqa: E402 — persona prompt (requests is lazy inside rabbit_ask)


def _with_operator(value):
    """Context helper: set/clear KIRRA_RABBIT_OPERATOR, restoring the prior value.
    Single-process test env twiddling (not the Rust INV-13 set_var concern)."""
    class _Ctx:
        def __enter__(self):
            self.prev = os.environ.get("KIRRA_RABBIT_OPERATOR")
            if value is None:
                os.environ.pop("KIRRA_RABBIT_OPERATOR", None)
            else:
                os.environ["KIRRA_RABBIT_OPERATOR"] = value
            return self

        def __exit__(self, *a):
            if self.prev is None:
                os.environ.pop("KIRRA_RABBIT_OPERATOR", None)
            else:
                os.environ["KIRRA_RABBIT_OPERATOR"] = self.prev
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


def test_greeting_nominal_introduces_rabit_acronym() -> None:
    """On the nominal-ready path Rabbit introduces itself with the R.A.B.I.T.
    expansion — the name IS the doer/checker architecture. It must NOT leak onto
    the not-ready / degraded lines (Rabbit never overclaims while not nominal)."""
    assert RABIT_EXPANSION == "Robotic Agent, Bounded by Independent Trust"
    with _with_operator(None):
        nominal = greeting_line(0, True)
        degraded = greeting_line(1, True)
        not_ready = greeting_line(2, True)
    assert RABIT_EXPANSION in nominal and "Rabbit here" in nominal
    assert RABIT_EXPANSION not in degraded, "no self-introduction while degraded"
    assert RABIT_EXPANSION not in not_ready, "no self-introduction while not ready"


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


def test_misconfig_line_is_a_nonempty_advisory() -> None:
    # A6: a Channel-A advisory pointing at the doctor; never claims motion.
    line = misconfig_line()
    assert len(line) > 0 and "doctor" in line.lower()


# --- OTA matcher precedence (apply/status before check) ----------------------

def test_rabbit_persona_voice_in_kitt_name_out() -> None:
    """The Rabbit persona is KITT-FLAVOURED (formal, dry, no filler) but must NOT
    name KITT / Knight Rider, nor hardcode an operator name — the name comes from
    the {name} slot at runtime — and it must retain the load-bearing safety
    framing (ADVISE/narrate, ground truth)."""
    s = RABBIT_SYSTEM
    low = s.lower()
    assert "rabbit" in low, "persona is named Rabbit"
    for forbidden in ("kitt", "k.i.t.t", "knight industries", "knight rider", "michael"):
        assert forbidden not in low, f"persona must not name {forbidden!r}"
    # the KITT-flavoured voice traits are present
    assert "slang" in low and "emoji" in low, "the no-slang/no-emoji rule is present"
    assert ("wit" in low or "understate" in low), "the dry/understated voice is present"
    assert "operator by name" in low, "addresses the operator via the {name} slot, not a hardcode"
    # regression guard: the safety framing must survive persona edits
    assert "ADVISE" in s and "ground truth" in low, "advise-not-control + ground-truth retained"


def test_ota_matcher_precedence() -> None:
    assert match_command("apply the update") == "apply"
    assert match_command("install update") == "apply"
    assert match_command("update status") == "status"
    assert match_command("check for updates") == "check"
    assert match_command("any updates?") == "check"
    assert match_command("take us to the kitchen") is None


# --- model pin (stealth-update guard) ---------------------------------------

def test_classify_model_pin_states() -> None:
    assert classify_model_pin(None, "sha256:aa") == "unavailable"   # no running digest
    assert classify_model_pin("sha256:aa", None) == "unpinned"      # never vetted
    assert classify_model_pin("sha256:aa", "sha256:aa") == "ok"     # matches
    assert classify_model_pin("sha256:bb", "sha256:aa") == "changed"  # stealth update


def test_model_pin_round_trip_and_isolation(tmp_path=None) -> None:
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        pin = os.path.join(d, "pins")
        assert read_model_pin("gemma3:4b", pin) is None            # unpinned → None
        write_model_pin("gemma3:4b", "sha256:aa", pin)
        write_model_pin("gemma4:8b", "sha256:bb", pin)             # a second model
        assert read_model_pin("gemma3:4b", pin) == "sha256:aa"
        assert read_model_pin("gemma4:8b", pin) == "sha256:bb"     # both kept
        write_model_pin("gemma3:4b", "sha256:cc", pin)            # re-vet updates in place
        assert read_model_pin("gemma3:4b", pin) == "sha256:cc"
        assert read_model_pin("gemma4:8b", pin) == "sha256:bb"    # other untouched
        assert read_model_pin("never-pinned", pin) is None


def test_model_pin_records_timestamp_and_note() -> None:
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        pin = os.path.join(d, "pins")
        write_model_pin("gemma4:8b", "sha256:aa", pin,
                        vetted_at="2026-07-19T00:00:00+00:00", note="hf re-pull")
        assert read_model_pin_record("gemma4:8b", pin) == (
            "sha256:aa", "2026-07-19T00:00:00+00:00", "hf re-pull")
        assert read_model_pin("gemma4:8b", pin) == "sha256:aa"   # digest accessor still works
        # a tab in the note must not corrupt the record
        write_model_pin("m2", "sha256:bb", pin, vetted_at="t", note="a\tb")
        assert read_model_pin_record("m2", pin) == ("sha256:bb", "t", "a b")


def test_model_pin_legacy_two_field_line_reads() -> None:
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        pin = os.path.join(d, "pins")
        with open(pin, "w") as f:
            f.write("gemma3:4b\tsha256:cc\n")                    # legacy 2-field line
        assert read_model_pin("gemma3:4b", pin) == "sha256:cc"   # still reads the digest
        assert read_model_pin_record("gemma3:4b", pin) == ("sha256:cc", "", "")  # empty ts/note


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
    print("Rabbit voice-logic host tests (pure, no hardware):")
    sys.exit(_run_all())
