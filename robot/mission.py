#!/usr/bin/env python3
"""mission.py — the Rabbit mission planner + Executive (voice-cognition Slice 5,
opt-in). Expands a multi-step request ("go to the dock, then pull over") into a
sequence of REGISTERED skills and runs them with sequencing / retry / cancel /
progress — WITHOUT ever becoming a new path to the wheels.

🔴 THE EXECUTIVE IS A DOER; THE CHECKER STILL OWNS EACH STEP (ruling §5.2):
  * A mission is only an ORDERED LIST of the same skills `skill_registry`
    dispatches. Every MOTION step is executed by handing its directive text to
    the SAME fenced door (offer_to_door → mick /intent → MickIntent grounding →
    the KIRRA checker) — the executive never builds a command.
  * FAIL-CLOSED sequencing: a step the checker REFUSES HALTS the mission (never
    skip-and-continue into the next step); a transient door error retries a
    bounded number of times, then halts. Motion never continues past a refusal.
  * A mission containing ANY unimplemented/unknown skill is REFUSED BEFORE ANY
    MOTION (validate up front) — a partial mission with a bad step never starts.
  * CANCELLABLE: a barge-in (PTT / e-stop signal, Slice 2) cancels the mission
    between/within steps — the ego stops (the checker's own MRC), the executive
    never authors re-acceleration.

The Executive is PURE (injected fence / speak / cancel sinks, no HTTP/LLM), so
the safety-critical routing — motion ONLY through the fence sink, halt-on-refuse,
refuse-unsupported — is fully host-tested.

Opt-in: `KIRRA_MISSIONS_ENABLED` (default off). Off → rabbit_converse keeps its
existing single-turn router, byte-identical.
"""
import json
import os
import sys
from collections import namedtuple

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import skill_registry as sk  # noqa: E402 — the skill catalog + fail-closed dispatcher

# Mission outcome states.
COMPLETED = "completed"
HALTED = "halted"          # a checker refusal / unreachable door stopped it
CANCELLED = "cancelled"    # a barge-in cancelled it
REFUSED = "refused"        # the plan itself was rejected before any motion

MissionResult = namedtuple("MissionResult", ["status", "steps_done", "total", "reason"])

DEFAULT_MAX_RETRIES = 2


# ── pure core (host-tested; no I/O) ──────────────────────────────────────────

def enabled():
    """Armed only on an explicit affirmative (fail-closed: unset/typo → off)."""
    return (os.environ.get("KIRRA_MISSIONS_ENABLED") or "").strip().lower() \
        in ("1", "true", "yes", "on")


def plan_mission(llm_json):
    """Parse the LLM's {say, mission:[{name, parameters}]} into (say, [(name,
    params)]). Fail-closed: bad JSON / wrong shapes → ('', []) or skipped
    entries, never a raw command."""
    try:
        j = json.loads(llm_json) if isinstance(llm_json, str) else llm_json
    except (ValueError, TypeError):
        return "", []
    if not isinstance(j, dict):
        return "", []
    say = j.get("say")
    say = say.strip() if isinstance(say, str) else ""
    steps = []
    seq = j.get("mission")
    if isinstance(seq, list):
        for entry in seq:
            if isinstance(entry, dict) and isinstance(entry.get("name"), str):
                steps.append((entry["name"], entry.get("parameters")))
    return say, steps


def validate_mission(steps):
    """Dispatch every step up front. Returns (ok, decisions, reason). The mission
    is REFUSED (ok=False) if it is empty or ANY step dispatches to REFUSE — a
    mission with an unsupported/unknown skill never starts a single motion."""
    if not steps:
        return False, [], "the mission was empty"
    decisions = [sk.dispatch(name, params) for name, params in steps]
    for (name, _), d in zip(steps, decisions):
        if d.kind == sk.REFUSE:
            return False, decisions, f"'{name}' — {d.payload}"
    return True, decisions, None


def _decide(outcome, attempts, max_retries):
    """Pure per-step transition for a MOTION step. outcome ∈ 'ok'|'reject'|
    'error'. Returns 'advance' | 'retry' | 'halt'. A checker refusal halts
    immediately (retrying a refused command is pointless and hammers the fence);
    only a transient door error retries, up to the bound."""
    if outcome == "ok":
        return "advance"
    if outcome == "reject":
        return "halt"           # the checker refused — fail-closed stop
    return "retry" if attempts < max_retries else "halt"  # transient 'error'


def run_mission(decisions, fence_fn, speak_fn, cancel_check=None,
                max_retries=DEFAULT_MAX_RETRIES, progress_fn=None):
    """Execute pre-validated decisions with INJECTED sinks. Motion flows ONLY
    through `fence_fn` (offer_to_door → /intent → checker), which returns
    'ok'|'reject'|'error'. SPEAK steps narrate; a REFUSE decision (should have
    been validated out) fail-closes to REFUSED. Returns a MissionResult.

    Host-testable single-door invariant: `fence_fn` is called ONLY for a FENCE
    step; a cancel between/within steps stops before the next fence call."""
    cancel_check = cancel_check or (lambda: False)
    total = len(decisions)
    for i, d in enumerate(decisions):
        if cancel_check():
            return MissionResult(CANCELLED, i, total, "cancelled by the operator")
        if progress_fn:
            progress_fn(i + 1, total, d)
        if d.kind == sk.SPEAK:
            speak_fn(d.payload)
            continue
        if d.kind == sk.REFUSE:  # defense in depth — validate should have caught it
            return MissionResult(REFUSED, i, total, d.payload)
        # FENCE — the sole motion path.
        attempts = 0
        while True:
            if cancel_check():
                return MissionResult(CANCELLED, i, total, "cancelled by the operator")
            action = _decide(fence_fn(d.payload), attempts, max_retries)
            if action == "advance":
                break
            if action == "halt":
                reason = (f"step {i + 1} of {total} was refused by the governor"
                          if attempts == 0 else
                          f"step {i + 1} of {total} couldn't reach the driving control")
                return MissionResult(HALTED, i, total, reason)
            attempts += 1  # retry a transient error
    return MissionResult(COMPLETED, total, total, None)


def narrate_result(result):
    """A spoken summary of a finished mission."""
    if result.status == COMPLETED:
        return "Mission complete — all steps done."
    if result.status == CANCELLED:
        return f"Mission cancelled after {result.steps_done} of {result.total} steps; I'm holding."
    if result.status == REFUSED:
        return f"I can't run that mission — {result.reason}."
    return f"Mission halted — {result.reason}. I've stopped."


def narrate_progress(index, total, decision):
    """A short per-step line (Channel A)."""
    if decision.kind == sk.FENCE:
        return f"Step {index} of {total}: {decision.payload}."
    return None  # SPEAK steps narrate themselves; no meta-line needed


# ── live seam (thin; not unit-tested — wires the pure core to real sinks) ─────

def cancel_check_from_barge_in():
    """A cancel predicate driven by the Slice-2 barge-in signal (a PTT press /
    `barge_in.py --signal` cancels the running mission). None when barge-in is
    off → the mission runs to completion/halt uninterrupted."""
    try:
        import barge_in
        if not barge_in.enabled():
            return None
        path = barge_in.signal_path()
        baseline = barge_in.read_epoch(path)
        return barge_in.make_file_cancel_check(path, baseline)
    except Exception:  # noqa: BLE001 — a broken cancel source must not run motion unguarded
        return None


def missions_prompt_fragment():
    """The additive system-prompt fragment for mission planning. Lives here so the
    offered vocabulary stays in lock-step with skill_registry's dispatcher."""
    names = ", ".join(sk.registered_skill_names())
    return (
        "The operator may ask for a MULTI-STEP mission. Reply with a JSON object "
        "and nothing else:\n"
        '  {"say": "<one or two sentences to speak>",\n'
        '   "mission": [{"name": "<skill>", "parameters": {…}}, …]}\n'
        f"Allowed skills (each step must be one of these): {names}.\n"
        "  navigate{target} · cruise{speed_mps} · turn{direction} · pull_over{} · "
        "stop{} · speak{text}.\n"
        "List the steps IN ORDER. Copy any place/number the operator gave into "
        "parameters VERBATIM; NEVER invent a destination or number they did not "
        "say. For chat/questions with no multi-step motion, use an empty mission "
        "(and put your reply in `say`). You are NOT the safety authority — the "
        "KIRRA checker bounds every step and will halt the mission if a step is "
        "unsafe; never drop a step to 'protect' the robot."
    )
