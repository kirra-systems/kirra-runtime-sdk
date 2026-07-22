#!/usr/bin/env python3
"""Host tests for the PURE ADR-0033 Tier-3 sentinel logic
(`robot/serial_exclusivity.py`): owner/mode violations against the
AOU-ACTUATION-SERIAL-001 contract, holder reporting, the acknowledgment
classifier, and the fail-closed startup verdict. No device, no /proc, no
ioctl — the OS-touching helpers are exercised on the robot (and by the
consumer's own startup), not here.

Runs standalone (`python3 robot/serial_exclusivity_test.py`, exit 1 on any
failure); also importable under pytest.
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from serial_exclusivity import (  # noqa: E402
    ack_given, holder_violations, mode_violations, startup_verdict,
)

UID = 1000


def _mode(perm: int) -> int:
    """A character-device st_mode with the given permission bits."""
    return 0o020000 | perm  # S_IFCHR | perm


# --- mode/owner --------------------------------------------------------------

def test_strict_0600_owned_is_clean() -> None:
    assert mode_violations(_mode(0o600), UID, UID, "/dev/myserial") == []


def test_vendor_0777_is_flagged() -> None:
    # The stock Yahboom rule ships MODE:=0777 — the exact #887 hazard.
    v = mode_violations(_mode(0o777), UID, UID, "/dev/myserial")
    assert len(v) == 1 and "0777" in v[0] and "group/other" in v[0]


def test_group_access_alone_is_flagged() -> None:
    for perm in (0o660, 0o640, 0o604, 0o620):
        assert mode_violations(_mode(perm), UID, UID, "/dev/myserial"), f"{perm:o}"


def test_wrong_owner_is_flagged_even_at_0600() -> None:
    v = mode_violations(_mode(0o600), 0, UID, "/dev/myserial")
    assert len(v) == 1 and "owned by uid 0" in v[0]


def test_wrong_owner_and_loose_mode_are_both_reported() -> None:
    v = mode_violations(_mode(0o666), 0, UID, "/dev/myserial")
    assert len(v) == 2


# --- holders -------------------------------------------------------------------

def test_holders_are_named_per_process() -> None:
    v = holder_violations([(4242, "vendor_driver"), (4300, "")], "/dev/myserial")
    assert len(v) == 2
    assert "4242" in v[0] and "vendor_driver" in v[0]
    assert "unknown" in v[1]
    assert holder_violations([], "/dev/myserial") == []


# --- acknowledgment classifier ---------------------------------------------------

def test_ack_truthy_set_matches_consumer_flags() -> None:
    for yes in ("1", "true", "YES", " on "):
        assert ack_given(yes), yes
    for no in (None, "", "0", "false", "off", "enabled", "y"):
        assert not ack_given(no), repr(no)


# --- the fail-closed startup policy ----------------------------------------------

def test_clean_port_is_ok() -> None:
    verdict, msg = startup_verdict([], acknowledged=False)
    assert verdict == "ok" and "OK" in msg


def test_violation_without_ack_refuses() -> None:
    verdict, msg = startup_verdict(["loose"], acknowledged=False)
    assert verdict == "refuse"
    assert "KIRRA_ALLOW_SHARED_SERIAL" in msg, "the fix hint must name the escape hatch"
    assert "99-kirra-serial-exclusivity.rules" in msg, "the fix hint must name the udev rule"


def test_violation_with_ack_degrades_loudly() -> None:
    verdict, msg = startup_verdict(["loose"], acknowledged=True)
    assert verdict == "acknowledged"
    assert "NOT enforced" in msg, "an acknowledged run must say authority is degraded"


def test_ack_on_a_clean_port_is_still_just_ok() -> None:
    verdict, _ = startup_verdict([], acknowledged=True)
    assert verdict == "ok", "the escape hatch must not change a clean verdict"


# --- standalone runner (house pattern) --------------------------------------------

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
    print("serial_exclusivity_test:",
          "ALL OK" if failures == 0 else f"{failures} FAILURE(S)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(_run_all())
