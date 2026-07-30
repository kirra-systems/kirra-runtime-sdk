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
    FENCE, MOTION, READONLY, REFUSE, REGISTRY, REPO, REPO_CMD, SPEAK,
    UNIMPLEMENTED, Decision, dispatch, execute_skill_decisions, plan_skills,
    to_directive,
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
        check(s.kind in (MOTION, READONLY, REPO, UNIMPLEMENTED),
              f"{name}: bad kind {s.kind}")
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


# ── REPO kind: the Robot Command Language sink ───────────────────────────────

def test_repo_skills_dispatch_to_an_allowlisted_intent_name():
    for name in ("sync_to_main", "publish_my_work"):
        d = dispatch(name, None)
        check(d == Decision(REPO_CMD, name),
              f"{name} should dispatch to REPO_CMD carrying its own name, got {d}")


def test_repo_dispatch_discards_model_supplied_parameters():
    # A repo command takes no arguments. Whatever the model puts in `parameters`
    # must not travel with the decision — the payload is the intent name only, so
    # there is no channel from model text to the executor.
    hostile = {"args": "--force", "cmd": "rm -rf /", "branch": "main; whoami"}
    d = dispatch("sync_to_main", hostile)
    check(d.payload == "sync_to_main", f"payload must be the bare name, got {d.payload!r}")
    check("force" not in d.payload and "rm" not in d.payload and ";" not in d.payload,
          "no parameter text may leak into the repo decision payload")


def test_repo_command_never_reaches_the_motion_fence():
    fenced, spoke = [], []
    counts = execute_skill_decisions(
        [dispatch("sync_to_main", None), dispatch("publish_my_work", None)],
        fence_fn=lambda d: fenced.append(d) or "ok",
        speak_fn=lambda t: spoke.append(t),
        repo_fn=lambda intent: f"ran {intent}",
    )
    check(fenced == [], f"a repo command must never touch the motion fence: {fenced}")
    check(counts[REPO_CMD] == 2 and counts[FENCE] == 0, f"counts wrong: {counts}")
    check(spoke == ["ran sync_to_main", "ran publish_my_work"],
          f"the repo sink's sentence should be spoken verbatim, got {spoke}")


def test_repo_command_without_an_injected_sink_is_refused():
    # Fail-closed: a caller that did not wire repo_fn cannot run a repo command,
    # and the operator is told rather than left with silence.
    fenced, spoke = [], []
    execute_skill_decisions([dispatch("sync_to_main", None)],
                            fence_fn=lambda d: fenced.append(d) or "ok",
                            speak_fn=lambda t: spoke.append(t))  # no repo_fn
    check(fenced == [], "a missing repo sink must not fall back to the fence")
    check(len(spoke) == 1 and "can't run repository commands" in spoke[0],
          f"expected a spoken refusal, got {spoke}")


def test_only_registered_repo_names_dispatch():
    for bogus in ("merge_and_sync", "reset_hard", "deploy", "git_push"):
        d = dispatch(bogus, None)
        check(d.kind == REFUSE, f"{bogus} must be refused, got {d}")


def test_repo_intents_are_offered_in_the_prompt_fragment():
    frag = sk.skills_prompt_fragment()
    for name in ("sync_to_main", "publish_my_work"):
        check(name in frag, f"{name} must be offered to the model")
    check(name in sk.registered_skill_names(), "repo skills are in the allowed name list")
    check("NO parameters" in frag, "the prompt must state that repo commands take no parameters")
    check("ask which one" in frag, "the prompt must instruct asking on ambiguity")


def test_plan_skills_accepts_a_repo_skill_from_model_json():
    say, decisions = plan_skills(
        '{"say": "Syncing now.", "skills": [{"name": "sync_to_main", "parameters": {}}]}')
    check(say == "Syncing now.", f"say wrong: {say!r}")
    check(decisions == [Decision(REPO_CMD, "sync_to_main")], f"decisions wrong: {decisions}")


def test_plan_skills_rejects_a_shell_shaped_model_reply():
    # The model emitting something command-shaped must produce NO executable
    # decision — the name is not in the registry, so it refuses.
    for blob in ('{"say":"ok","skills":[{"name":"bash","parameters":{"cmd":"rm -rf /"}}]}',
                 '{"say":"ok","skills":[{"name":"sh -c whoami","parameters":{}}]}',
                 '{"say":"ok","skills":[{"name":"git push --force","parameters":{}}]}'):
        _say, decisions = plan_skills(blob)
        check(all(d.kind == REFUSE for d in decisions),
              f"shell-shaped skill names must refuse: {blob} -> {decisions}")


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
