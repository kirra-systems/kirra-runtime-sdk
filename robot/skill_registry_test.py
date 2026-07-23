#!/usr/bin/env python3
"""Host tests for the pure skill-registry core in `skill_registry.py`.

The catalog, dispatcher, planner, and executor are pure (no HTTP/LLM), so the
safety-critical mapping — which skills can reach the fence, which are refused —
runs on a plain host. The single-door invariant is asserted directly:
`execute_skill_decisions` calls the fence sink ONLY for a valid motion skill,
never for a refused/unimplemented/unknown one.

Runs standalone (`python3 robot/skill_registry_test.py`, exit 1 on failure);
also importable under pytest.
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import skill_registry as sk  # noqa: E402
from skill_registry import (  # noqa: E402
    FENCE, MOTION, READONLY, REFUSE, REGISTRY, SPEAK, UNIMPLEMENTED, Decision,
    dispatch, execute_skill_decisions, plan_skills, to_directive,
)

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


# ── catalog integrity ────────────────────────────────────────────────────────

def test_every_skill_has_metadata():
    for name, s in REGISTRY.items():
        check(s.name == name, f"{name}: name mismatch")
        check(s.kind in (MOTION, READONLY, UNIMPLEMENTED), f"{name}: bad kind {s.kind}")
        check(bool(s.description), f"{name}: missing description")
        check(isinstance(s.preconditions, list), f"{name}: preconditions not a list")
        check(isinstance(s.failure_modes, list), f"{name}: failure_modes not a list")
        check(isinstance(s.interruptible, bool), f"{name}: interruptible not bool")


def test_registered_names_exclude_unimplemented():
    offered = set(sk.registered_skill_names())
    for name, s in REGISTRY.items():
        if s.kind == UNIMPLEMENTED:
            check(name not in offered, f"unimplemented {name} must not be offered to the LLM")
        else:
            check(name in offered, f"{name} should be offered")


# ── dispatch decision table ──────────────────────────────────────────────────

def test_motion_yields_fence_directive():
    d = dispatch("navigate", {"target": "the loading dock"})
    check(d == Decision(FENCE, "take us to the loading dock"), f"navigate → {d}")
    d = dispatch("cruise", {"speed_mps": 1.5})
    check(d.kind == FENCE and "1.5" in d.payload, f"cruise → {d}")
    d = dispatch("turn", {"direction": "LEFT"})
    check(d == Decision(FENCE, "turn left"), f"turn → {d}")
    check(dispatch("pull_over", {}).kind == FENCE, "pull_over → fence")
    check(dispatch("stop", {}).kind == FENCE, "stop → fence")


def test_bad_params_refuse_not_fence():
    check(dispatch("navigate", {}).kind == REFUSE, "navigate w/o target → refuse")
    check(dispatch("cruise", {"speed_mps": "fast"}).kind == REFUSE, "cruise non-numeric → refuse")
    check(dispatch("cruise", {"speed_mps": float("inf")}).kind == REFUSE, "cruise inf → refuse")
    check(dispatch("turn", {"direction": "around"}).kind == REFUSE, "turn bad dir → refuse")


def test_unimplemented_and_unknown_refuse():
    for name in ("dock", "follow_person", "search_area", "capture_image",
                 "read_qr", "inspect_object", "flash_lights"):
        check(dispatch(name, {}).kind == REFUSE, f"{name} (unimplemented) → refuse")
    check(dispatch("hack_the_gibson", {}).kind == REFUSE, "unknown skill → refuse")


def test_readonly_speak():
    check(dispatch("speak", {"text": "hello"}) == Decision(SPEAK, "hello"), "speak text")
    check(dispatch("speak", {}).kind == REFUSE, "speak w/o text → refuse (nothing to say)")


def test_to_directive_motion_only():
    check(to_directive("speak", {"text": "x"}) is None, "readonly is not a directive")
    check(to_directive("dock", {}) is None, "unimplemented is not a directive")
    check(to_directive("unknown", {}) is None, "unknown is not a directive")


# ── planner: fail-closed parsing ─────────────────────────────────────────────

def test_plan_skills_fail_closed():
    check(plan_skills("not json") == ("", []), "bad JSON → empty")
    check(plan_skills('["a","list"]') == ("", []), "non-object → empty")
    say, decs = plan_skills('{"say":"ok","skills":"nope"}')
    check(say == "ok" and decs == [], "non-list skills → no decisions")
    say, decs = plan_skills('{"say":"hi","skills":[{"name":"navigate","parameters":{"target":"home"}},'
                            '{"noname":true},{"name":123}]}')
    check(say == "hi", "say parsed")
    check(len(decs) == 1 and decs[0].kind == FENCE, "only the well-formed skill dispatched")


# ── THE single-door invariant (executor) ─────────────────────────────────────

def test_executor_motion_only_through_fence():
    fenced, spoke = [], []
    decisions = [
        dispatch("navigate", {"target": "home"}),     # FENCE
        dispatch("dock", {}),                          # REFUSE (unimplemented)
        dispatch("hack", {}),                          # REFUSE (unknown)
        dispatch("speak", {"text": "on it"}),          # SPEAK
        dispatch("cruise", {"speed_mps": 2}),          # FENCE
    ]
    counts = execute_skill_decisions(
        decisions,
        fence_fn=lambda directive: (fenced.append(directive) or "ok"),
        speak_fn=lambda text: spoke.append(text),
    )
    check(fenced == ["take us to home", "cruise at 2 meters per second"],
          f"ONLY motion skills reached the fence: {fenced}")
    check(counts[FENCE] == 2 and counts[REFUSE] == 2 and counts[SPEAK] == 1,
          f"decision counts wrong: {counts}")
    # A refused/unknown skill must NEVER have produced a fence call.
    check("hack" not in " ".join(fenced) and "dock" not in " ".join(fenced),
          "a refused skill must never reach the fence")


def test_executor_rejected_by_checker_speaks_hold():
    spoke = []
    execute_skill_decisions([dispatch("navigate", {"target": "the wall"})],
                            fence_fn=lambda d: "reject",  # checker refuses
                            speak_fn=lambda t: spoke.append(t))
    check(any("holding" in s.lower() or "wouldn't clear" in s.lower() for s in spoke),
          f"a checker rejection should be narrated, got {spoke}")


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
    if _FAILURES:
        print(f"\n{len(_FAILURES)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"skill_registry_test: all {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
