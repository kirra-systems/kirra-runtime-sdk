#!/usr/bin/env python3
"""Host tests for the pure mission planner + Executive in `mission.py`.

Planning, validation, and the Executive are pure (injected fence / speak / cancel
sinks — no HTTP/LLM), so the safety-critical routing runs on a plain host. The
single-door invariant is asserted directly: `run_mission` calls the fence sink
ONLY for a motion step, halts (never skips) on a checker refusal, and refuses a
mission with an unsupported step before any motion.

Runs standalone (`python3 robot/mission_test.py`, exit 1 on failure); also
importable under pytest.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mission  # noqa: E402
import skill_registry as sk  # noqa: E402
from mission import (  # noqa: E402
    CANCELLED, COMPLETED, HALTED, REFUSED, plan_mission, run_mission,
    validate_mission,
)

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


class _Fence:
    """A fake door: returns a scripted outcome per call, recording every directive
    it was handed (so we can assert motion routed ONLY here, and in order)."""
    def __init__(self, script=None):
        self.script = list(script or [])
        self.calls = []

    def __call__(self, directive):
        self.calls.append(directive)
        return self.script.pop(0) if self.script else "ok"


def test_enable_gate_fail_closed():
    prev = os.environ.get("KIRRA_MISSIONS_ENABLED")
    try:
        for off in (None, "", "0", "false", "off", "enabled"):
            if off is None:
                os.environ.pop("KIRRA_MISSIONS_ENABLED", None)
            else:
                os.environ["KIRRA_MISSIONS_ENABLED"] = off
            check(not mission.enabled(), f"{off!r} must NOT arm (fail-closed)")
        os.environ["KIRRA_MISSIONS_ENABLED"] = "1"
        check(mission.enabled(), "'1' should arm")
    finally:
        if prev is None:
            os.environ.pop("KIRRA_MISSIONS_ENABLED", None)
        else:
            os.environ["KIRRA_MISSIONS_ENABLED"] = prev


def test_plan_mission_fail_closed():
    check(plan_mission("not json") == ("", []), "bad JSON → empty")
    check(plan_mission('{"say":"hi"}') == ("hi", []), "no mission key → no steps")
    say, steps = plan_mission('{"say":"ok","mission":[{"name":"navigate","parameters":'
                              '{"target":"dock"}},{"noname":1},{"name":"pull_over"}]}')
    check(say == "ok" and steps == [("navigate", {"target": "dock"}), ("pull_over", None)],
          f"well-formed steps only: {steps}")


def test_validate_refuses_empty_and_unsupported():
    ok, _, reason = validate_mission([])
    check(not ok and "empty" in reason, "empty mission refused")
    ok, _, reason = validate_mission([("navigate", {"target": "dock"}),
                                      ("dock", {})])  # dock = unimplemented
    check(not ok and "dock" in reason, f"unsupported step refuses whole mission: {reason}")
    ok, decisions, reason = validate_mission([("navigate", {"target": "dock"}),
                                              ("pull_over", {})])
    check(ok and reason is None and all(d.kind == sk.FENCE for d in decisions),
          "all-motion mission validates to FENCE decisions")


def test_run_completes_motion_only_through_fence_in_order():
    _, decisions, _ = validate_mission([("navigate", {"target": "dock"}),
                                        ("speak", {"text": "arriving"}),
                                        ("pull_over", {})])
    fence = _Fence()
    spoke = []
    result = run_mission(decisions, fence_fn=fence, speak_fn=spoke.append)
    check(result.status == COMPLETED and result.steps_done == 3, f"should complete: {result}")
    check(fence.calls == ["take us to dock", "pull over to the side and stop"],
          f"ONLY motion steps hit the fence, in order: {fence.calls}")
    check(spoke == ["arriving"], f"speak step narrated: {spoke}")


def test_checker_refusal_halts_no_skip():
    _, decisions, _ = validate_mission([("navigate", {"target": "dock"}),
                                        ("cruise", {"speed_mps": 2}),
                                        ("pull_over", {})])
    fence = _Fence(script=["ok", "reject"])  # step 2 refused by the checker
    result = run_mission(decisions, fence_fn=fence, speak_fn=lambda t: None)
    check(result.status == HALTED and result.steps_done == 1, f"halt at the refusal: {result}")
    check("refused by the governor" in result.reason, f"reason names the refusal: {result.reason}")
    # The THIRD step must never have been attempted (no skip-and-continue).
    check(fence.calls == ["take us to dock", "cruise at 2 meters per second"],
          f"motion stopped at the refused step, never ran step 3: {fence.calls}")


def test_transient_error_retries_then_halts():
    _, decisions, _ = validate_mission([("navigate", {"target": "dock"})])
    fence = _Fence(script=["error", "error", "error"])  # never succeeds
    result = run_mission(decisions, fence_fn=fence, speak_fn=lambda t: None, max_retries=2)
    check(result.status == HALTED, f"exhausted retries → halt: {result}")
    check(len(fence.calls) == 3, f"1 try + 2 retries = 3 calls, got {len(fence.calls)}")

    fence2 = _Fence(script=["error", "ok"])  # recovers on the retry
    r2 = run_mission(decisions, fence_fn=fence2, speak_fn=lambda t: None, max_retries=2)
    check(r2.status == COMPLETED, f"a recovered retry completes: {r2}")


def test_cancel_stops_before_next_fence():
    _, decisions, _ = validate_mission([("navigate", {"target": "dock"}),
                                        ("pull_over", {})])
    fence = _Fence()
    # Cancel fires after the first step's fence call.
    state = {"n": 0}

    def cancel():
        # not cancelled at entry; cancelled once the first step has run
        return fence.calls and state["n"] >= 1

    def counting_fence(d):
        state["n"] += 1
        return fence(d)

    result = run_mission(decisions, fence_fn=counting_fence, speak_fn=lambda t: None,
                         cancel_check=cancel)
    check(result.status == CANCELLED, f"should cancel mid-mission: {result}")
    check(len(fence.calls) == 1, f"the second motion step must NOT have run: {fence.calls}")


def test_refuse_decision_defense_in_depth():
    # Feed a REFUSE decision straight to run_mission (bypassing validate) — it must
    # fail-closed to REFUSED and never call the fence.
    refuse = [sk.dispatch("dock", {})]  # unimplemented → REFUSE
    fence = _Fence()
    result = run_mission(refuse, fence_fn=fence, speak_fn=lambda t: None)
    check(result.status == REFUSED and not fence.calls,
          f"a REFUSE decision never reaches the fence: {result}, calls={fence.calls}")


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
    if _FAILURES:
        print(f"\n{len(_FAILURES)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"mission_test: all {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
