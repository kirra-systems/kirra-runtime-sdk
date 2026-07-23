#!/usr/bin/env python3
"""Host tests for the pure World Model read-projection core in `world_model.py`.

The freshness / snapshot / render / assemble logic is pure (time injected, gathers
injected), so the fail-closed staleness semantics run on a plain host. The live
`run_report` (real HTTP gathers + wall clock) is the thin seam, exercised on the
robot.

Runs standalone (`python3 robot/world_model_test.py`, exit 1 on failure); also
importable under pytest.

Covers: the enable gate (fail-closed); TTL freshness incl. the exact boundary and
clock-skew; absent/stale → UNKNOWN (never a stale value); the snapshot shape; the
report matcher (and its non-overlap); render SAYS unknown for a stale field; and
assemble stamping a good source but leaving an 'unavailable' one UNKNOWN.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import world_model as wm  # noqa: E402
from world_model import (  # noqa: E402
    UNKNOWN, WorldModel, assemble, is_fresh, is_source_failure, is_unknown,
    matches, render,
)

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


def test_enable_gate_fail_closed():
    prev = os.environ.get("KIRRA_WORLD_MODEL_ENABLED")
    try:
        for off in (None, "", "0", "false", "no", "off", "enabled"):
            if off is None:
                os.environ.pop("KIRRA_WORLD_MODEL_ENABLED", None)
            else:
                os.environ["KIRRA_WORLD_MODEL_ENABLED"] = off
            check(not wm.enabled(), f"{off!r} must NOT arm (fail-closed)")
        for on in ("1", "true", "yes", "on"):
            os.environ["KIRRA_WORLD_MODEL_ENABLED"] = on
            check(wm.enabled(), f"{on!r} should arm")
    finally:
        if prev is None:
            os.environ.pop("KIRRA_WORLD_MODEL_ENABLED", None)
        else:
            os.environ["KIRRA_WORLD_MODEL_ENABLED"] = prev


def test_is_fresh_boundary_and_skew():
    check(is_fresh(1000, 500, 1400), "within ttl → fresh")
    check(is_fresh(1000, 500, 1500), "exactly at ttl → fresh (<=)")
    check(not is_fresh(1000, 500, 1501), "past ttl → stale")
    check(is_fresh(1000, 500, 900), "future stamp (skew) → fresh, not stale")
    check(not is_fresh(1000, 0, 1000), "non-positive ttl → always stale")


def test_get_absent_and_stale_are_unknown():
    m = WorldModel()
    check(is_unknown(m.get("posture", 0)), "absent field → UNKNOWN")
    m.set("posture", "nominal", "src", stamp_ms=1000, ttl_ms=500)
    check(m.get("posture", 1400) == "nominal", "fresh → the value")
    check(is_unknown(m.get("posture", 2000)), "stale → UNKNOWN, never the stale value")


def test_snapshot_shape():
    m = WorldModel()
    m.set("posture", "nominal", "verifier", stamp_ms=1000, ttl_ms=500)
    snap = m.snapshot(1200)
    e = snap["posture"]
    check(e["fresh"] and e["value"] == "nominal", "fresh snapshot carries the value")
    check(e["age_ms"] == 200 and e["source"] == "verifier", f"snapshot meta wrong: {e}")
    stale = m.snapshot(2000)["posture"]
    check(not stale["fresh"] and is_unknown(stale["value"]),
          "stale snapshot value is UNKNOWN")


def test_render_says_unknown_for_stale():
    m = WorldModel()
    m.set("posture", "nominal", "verifier", stamp_ms=1000, ttl_ms=500)
    fresh_text = render(m.snapshot(1200))
    check("Posture: nominal." in fresh_text, f"fresh render missing posture: {fresh_text}")
    stale_text = render(m.snapshot(9999))
    check("Posture: unknown" in stale_text and "nominal" not in stale_text,
          f"stale render must SAY unknown, not the stale value: {stale_text}")


def test_matcher_scope():
    for yes in ("situation report", "give me a status", "SITREP", "status update",
                "how are we doing"):
        check(matches(yes), f"should match {yes!r}")
    for no in ("check yourself", "check for updates", "take us to the door", "hello"):
        check(not matches(no), f"must NOT match {no!r} (overlap)")


def test_source_failure_detection():
    check(is_source_failure("posture: unavailable (cannot reach the governor)"), "unavailable")
    check(is_source_failure("perception: unavailable (ROS not reachable)"), "unreachable")
    check(not is_source_failure("posture: nominal, all trusted"), "real value is not a failure")


def test_assemble_stamps_good_leaves_failure_unknown():
    gathers = {
        "posture": lambda: "posture: nominal",
        "perception": lambda: "perception: unavailable (ROS not reachable)",
        "stop_reason": lambda: "  ",                       # empty → skip
        "boom": lambda: (_ for _ in ()).throw(RuntimeError()),  # raises → skip
    }
    m = assemble(now_ms=1000, gathers=gathers, operator="Justin",
                 ttls={"posture": 5000, "perception": 5000, "operator": 9999})
    check(m.get("posture", 1000) == "posture: nominal", "good source stamped")
    check(is_unknown(m.get("perception", 1000)), "failed source → UNKNOWN, not the marker")
    check(is_unknown(m.get("stop_reason", 1000)), "empty source → UNKNOWN")
    check(is_unknown(m.get("boom", 1000)), "raising source → UNKNOWN (no crash)")
    check(m.get("operator", 1000) == "Justin", "operator set")


def test_assemble_no_operator():
    m = assemble(1000, {"posture": lambda: "posture: nominal"}, operator="")
    check(is_unknown(m.get("operator", 1000)), "no operator → UNKNOWN, never guessed")


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
    if _FAILURES:
        print(f"\n{len(_FAILURES)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"world_model_test: all {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
