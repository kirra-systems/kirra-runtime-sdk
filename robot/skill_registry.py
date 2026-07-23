#!/usr/bin/env python3
"""skill_registry.py — the Rabbit capability catalog + fail-closed dispatcher
(voice-cognition Slice 3, opt-in). Realizes the architecture ruling §5.2: the LLM
invokes NAMED, registered skills instead of free-form commands, and

  🔴 THE SKILL REGISTRY IS A CATALOG, NOT A NEW DOOR TO THE WHEELS.

A *motion* skill NEVER actuates directly — it compiles to a plain-words directive
that goes through the EXISTING fail-closed door (mick `POST /intent` →
`MickIntent::from_llm_json` grounding → Occy → the KIRRA checker), exactly the
path a typed directive already takes. Read-only skills (speak) never touch
actuation. A skill that is cataloged-but-unimplemented, or unknown, is REFUSED —
never faked, never a bypass. So the single-door invariant is preserved: the only
way a skill reaches the wheels is a `FENCE` decision, and that decision is just
directive text handed to the same door.

This module is PURE (no HTTP, no LLM) so the safety-critical mapping — which
skills can reach the fence, which are refused — is fully host-tested. The live
executor injects the real `offer_to_door` / speak sinks; motion flows ONLY
through the injected fence sink.

Opt-in: `KIRRA_SKILLS_ENABLED` (default off). Off → rabbit_converse keeps the
existing `{say, directive}` router, byte-identical.
"""
import json
import os
from collections import namedtuple

# ── skill kinds ──────────────────────────────────────────────────────────────
MOTION = "motion"            # compiles to a directive → the /intent fence
READONLY = "readonly"        # Channel-A, no actuation (e.g. speak)
UNIMPLEMENTED = "unimplemented"  # cataloged for the future → always REFUSED today

Skill = namedtuple("Skill", [
    "name", "kind", "description", "permission",
    "interruptible", "est_duration_s", "preconditions", "failure_modes",
])

# Decision kinds — the ONLY three things a dispatched skill can become.
FENCE = "fence"      # payload = directive text for offer_to_door (the sole motion path)
SPEAK = "speak"      # payload = text to speak (Channel A)
REFUSE = "refuse"    # payload = reason; NO motion, NO speech-of-content
Decision = namedtuple("Decision", ["kind", "payload"])


def _m(name, desc, precond, fail, dur):
    return Skill(name, MOTION, desc, "actuator", True, dur, precond, fail)


REGISTRY = {
    # ── MOTION — each maps to a directive through the EXISTING /intent fence ──
    "navigate": _m("navigate", "Drive to a named place the operator gave.",
                   ["localization", "map", "posture=nominal"],
                   ["no route", "checker refusal", "occlusion cap"], 60.0),
    "cruise": _m("cruise", "Drive forward at a requested speed.",
                 ["posture=nominal"], ["checker speed clamp"], 30.0),
    "turn": _m("turn", "Turn left / right / straight at the next junction.",
               ["localization"], ["no junction", "checker refusal"], 10.0),
    "pull_over": _m("pull_over", "Pull over to the side and stop.",
                    [], ["checker refusal"], 15.0),
    "stop": _m("stop", "Come to a controlled stop (MRC).",
               [], [], 5.0),
    # ── READONLY — Channel A, cannot actuate ──
    "speak": Skill("speak", READONLY, "Say something to the operator.",
                   "none", True, 3.0, [], []),
    # ── UNIMPLEMENTED — cataloged with metadata, REFUSED until a real, fenced
    #    backing exists. NEVER faked (that would be a fabricated capability). ──
    "dock": Skill("dock", UNIMPLEMENTED, "Dock at a charging/parking station.",
                  "actuator", True, 90.0, ["dock detected"], ["no dock"]),
    "follow_person": Skill("follow_person", UNIMPLEMENTED, "Follow a tracked person.",
                           "actuator", True, 120.0, ["person tracked"], ["lost track"]),
    "search_area": Skill("search_area", UNIMPLEMENTED, "Search an area for an object.",
                         "actuator", True, 300.0, ["map"], ["not found"]),
    "capture_image": Skill("capture_image", UNIMPLEMENTED, "Capture a camera image.",
                           "sensor", True, 2.0, ["camera"], ["camera fault"]),
    "read_qr": Skill("read_qr", UNIMPLEMENTED, "Read a QR / AprilTag in view.",
                     "sensor", True, 3.0, ["camera"], ["no code"]),
    "inspect_object": Skill("inspect_object", UNIMPLEMENTED, "Inspect a detected object.",
                            "sensor", True, 20.0, ["object detected"], ["not found"]),
    "flash_lights": Skill("flash_lights", UNIMPLEMENTED, "Flash the indicator lights.",
                          "actuator", True, 2.0, ["led"], ["no led"]),
}


# ── pure core (host-tested; no I/O) ──────────────────────────────────────────

def enabled():
    """Armed only on an explicit affirmative (fail-closed: unset/typo → off)."""
    return (os.environ.get("KIRRA_SKILLS_ENABLED") or "").strip().lower() \
        in ("1", "true", "yes", "on")


def _num(v):
    """A finite float from a JSON scalar, or None (fail-closed on junk/NaN/Inf)."""
    try:
        f = float(v)
    except (TypeError, ValueError):
        return None
    return f if f == f and abs(f) != float("inf") else None


def to_directive(name, params):
    """Compile a MOTION skill + params into the plain-words directive that goes to
    the /intent fence, or None if the skill is not motion / the params are bad
    (→ the caller REFUSES; nothing reaches the door)."""
    params = params if isinstance(params, dict) else {}
    if name == "navigate":
        target = str(params.get("target", "")).strip()
        return f"take us to {target}" if target else None
    if name == "cruise":
        speed = _num(params.get("speed_mps"))
        return f"cruise at {speed:g} meters per second" if speed is not None and speed >= 0 else None
    if name == "turn":
        d = str(params.get("direction", "")).strip().lower()
        return f"turn {d}" if d in ("left", "right", "straight") else None
    if name == "pull_over":
        return "pull over to the side and stop"
    if name == "stop":
        return "come to a controlled stop"
    return None


def dispatch(name, params):
    """Map ONE skill invocation to a Decision, fail-closed. Only a MOTION skill
    with valid params yields FENCE (the sole motion path); UNIMPLEMENTED and
    unknown names REFUSE; readonly `speak` SPEAKs its text."""
    skill = REGISTRY.get(name)
    if skill is None:
        return Decision(REFUSE, f"unknown skill '{name}'")
    if skill.kind == UNIMPLEMENTED:
        return Decision(REFUSE, f"'{name}' is not supported yet")
    if skill.kind == READONLY:
        text = str((params or {}).get("text", "")).strip() if isinstance(params, dict) else ""
        return Decision(SPEAK, text) if text else Decision(REFUSE, "nothing to say")
    # MOTION
    directive = to_directive(name, params)
    if directive is None:
        return Decision(REFUSE, f"bad parameters for '{name}'")
    return Decision(FENCE, directive)


def plan_skills(llm_json):
    """Parse the LLM's {say, skills:[{name, parameters}]} reply into (say,
    [Decision]). Fail-closed: bad JSON / wrong shapes → ('', []) or skipped
    entries, never a raw command."""
    try:
        j = json.loads(llm_json) if isinstance(llm_json, str) else llm_json
    except (ValueError, TypeError):
        return "", []
    if not isinstance(j, dict):
        return "", []
    say = j.get("say")
    say = say.strip() if isinstance(say, str) else ""
    decisions = []
    skills = j.get("skills")
    if isinstance(skills, list):
        for entry in skills:
            if not isinstance(entry, dict):
                continue
            name = entry.get("name")
            if not isinstance(name, str):
                continue
            decisions.append(dispatch(name, entry.get("parameters")))
    return say, decisions


def execute_skill_decisions(decisions, fence_fn, speak_fn):
    """Execute planned decisions with INJECTED sinks. Motion flows ONLY through
    `fence_fn` (the real offer_to_door → /intent → checker); nothing here builds
    a command. Returns counts. Host-testable: fence_fn is never called for a
    REFUSE/SPEAK decision (the single-door invariant, as an assertion point)."""
    counts = {FENCE: 0, SPEAK: 0, REFUSE: 0}
    for d in decisions:
        counts[d.kind] = counts.get(d.kind, 0) + 1
        if d.kind == FENCE:
            result = fence_fn(d.payload)          # the ONLY actuation-adjacent call
            if result != "ok":
                speak_fn("I heard that, but the governor wouldn't clear it, so I'm holding.")
        elif d.kind == SPEAK:
            speak_fn(d.payload)
        else:  # REFUSE
            speak_fn(f"I can't do that yet — {d.payload}.")
    return counts


def registered_skill_names():
    """Names the LLM is allowed to invoke (motion + readonly; unimplemented ones
    are cataloged but the prompt does not offer them)."""
    return sorted(n for n, s in REGISTRY.items() if s.kind != UNIMPLEMENTED)


def skills_prompt_fragment():
    """The additive system-prompt fragment describing the skill contract. Kept
    here (not in rabbit_converse) so the offered vocabulary and the dispatcher
    stay in lock-step."""
    names = ", ".join(registered_skill_names())
    return (
        "Reply with a JSON object and nothing else:\n"
        '  {"say": "<one or two sentences to speak>",\n'
        '   "skills": [{"name": "<one of the skills below>", "parameters": {…}}]}\n'
        f"Allowed skills: {names}.\n"
        "  navigate{target} · cruise{speed_mps} · turn{direction:left|right|straight} · "
        "pull_over{} · stop{} · speak{text}.\n"
        "Use `speak` (or an empty skills list) for chat/questions. Emit a motion "
        "skill ONLY when the operator clearly asks to move; copy any place/number "
        "they gave into parameters VERBATIM, and NEVER invent a destination or "
        "number they did not say. You are NOT the safety authority — the KIRRA "
        "checker bounds every motion; never drop a movement request to 'protect' "
        "the robot."
    )
