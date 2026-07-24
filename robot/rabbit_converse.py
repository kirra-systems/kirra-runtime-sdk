#!/usr/bin/env python3
"""rabbit_converse.py — Stage 2 Rabbit: multi-turn conversation, persona, router.

Unifies ASKING (Stage 1 Q&A) and COMMANDING into ONE dialogue with memory and
character. Each turn, Rabbit decides between its two channels
(docs/hardware/RABBIT_CONVERSATION_DESIGN.md):

  * SPEAK  — chat / questions / status → answered in persona from live telemetry
             (reuses robot/rabbit_ask.py's read-only grounding). Never moves.
  * ACT    — a driving directive → the operator's movement words are handed to
             mick_service POST /intent, the ONE fail-closed door. occy_doer then
             drives it and the KIRRA checker BOUNDS it. Rabbit speaks a confirmation.

🔴 THE SINGLE-DOOR INVARIANT: Rabbit NEVER constructs an intent, a Twist, a release
   token, or a serial byte. The ONLY actuation-adjacent call in this process is
   POSTing the operator's directive TEXT to /intent — exactly what a human typing
   does. mick's fail-closed parse (MickIntent::parse_llm_json) is the final
   authority on whether text is a valid directive; occy + the checker bound the
   result. A misheard or hallucinated directive at worst becomes a checker-
   APPROVED, bounded motion — never an unsafe one — and an unparseable turn
   drives NOTHING (fail-closed: uncertain → no directive → SPEAK only).

Routing is fail-closed: Rabbit emits a directive ONLY when it is confident the
operator asked to DRIVE. Questions, chat, and any turn it can't parse → SPEAK,
directive null, no motion.

Usage:
  ./robot/rabbit_converse.py            # interactive: one utterance per line (Ctrl-D quits)
  echo "take us to the door" | ./robot/rabbit_converse.py --once
Env: inherits robot/rabbit_ask.py's (KIRRA_VERIFIER_URL / _MICK_URL / _TAJ_URL /
     _OLLAMA_URL / KIRRA_RABBIT_MODEL / KIRRA_TTS_CMD). Wire STT in by piping the
     transcript per line (e.g. the PTT button + whisper) — same as speech_shell.
"""
import json
import os
import re
import sys

try:
    import requests
except ImportError:
    sys.exit("rabbit_converse: python3-requests missing (pip3 install requests)")

# Reuse Stage 1's read-only grounding + persona + speak (robot/ is on sys.path[0]).
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from rabbit_ask import (  # noqa: E402
    KEEP_ALIVE, RABBIT_SYSTEM, MICK, MODEL, OLLAMA, gather_perception,
    gather_posture, gather_stop_reason, speak,
)
import rabbit_diag  # noqa: E402 — deterministic self-check voice command (read-only)
import rabbit_ota  # noqa: E402 — deterministic OTA voice commands (NOT the movement door)
import rabbit_wake  # noqa: E402 — deterministic wake-listener controls (state file only)
import barge_in  # noqa: E402 — interruptible reply speech (opt-in; Channel A, cosmetic)
import turn_state  # noqa: E402 — cross-process "turn in progress" signal (Slice R re-arm)
import skill_registry  # noqa: E402 — opt-in named-skill router (motion → the SAME /intent fence)
import world_model  # noqa: E402 — opt-in situation report (read-only TTL'd projection)
import mission  # noqa: E402 — opt-in multi-step Executive (each step → the SAME /intent fence)
from rabbit_persona import name_slot, operator_name  # noqa: E402

MAX_TURNS = 10  # rolling conversation memory (user+assistant pairs kept)
PERCEPTION_WORDS = ("see", "around", "ahead", "front", "obstacle", "clear",
                    "look", "there", "path", "way", "block")

# The router is a structured {say, directive} CLASSIFIER, not a creative writer.
# Sample near-deterministically so the directive decision is stable turn-to-turn
# (a high default temperature makes a clear DRIVE command intermittently null its
# directive — the drive-by-voice path must not be a coin-flip). The smoketest
# imports THIS so the gate calls the model exactly as production does; a vetted
# pass then predicts production behaviour instead of a lucky sample.
ROUTER_LLM_OPTIONS = {"temperature": 0.1}
# Speed (Slice S, OPT-IN): cap the reply length. DEFAULT UNSET — the router emits
# a {say, directive} JSON object and a too-tight cap could TRUNCATE it, which
# parse_reply fail-closes to directive=None (a silently DROPPED drive command).
# So this is opt-in and must be generous (>= a full reply's JSON); the model-swap
# smoketest imports ROUTER_LLM_OPTIONS, so a value that starts truncating the
# directive is caught by its drive→directive assertions before you ship it.
_num_predict = (os.environ.get("KIRRA_RABBIT_NUM_PREDICT") or "").strip()
if _num_predict.lstrip("-").isdigit():
    ROUTER_LLM_OPTIONS["num_predict"] = int(_num_predict)

STAGE2_SYSTEM = (
    RABBIT_SYSTEM
    + "\n\nEACH TURN, reply with a JSON object and nothing else:\n"
    '  {"say": "<one or two sentences to speak aloud>",\n'
    '   "directive": <null, OR the operator\'s movement request in plain words>}\n'
    "Set `directive` whenever the operator clearly wants the robot to DRIVE or "
    "MOVE — INCLUDING to a place they name (e.g. 'creep forward a meter', 'turn "
    "left', 'take us to the door', 'go to the kitchen'). Copy their movement "
    "request into `directive` VERBATIM, keeping any destination they named — "
    "relaying a place the operator gave you is faithful, it is NOT inventing. The "
    "only thing you must never invent is a destination, coordinate, or number the "
    "operator did NOT say. For questions, status, chat, or anything with no "
    "movement intent, `directive` is null.\n"
    "CRITICAL: You are NOT the safety authority — the KIRRA checker is. NEVER set "
    "`directive` to null because a move looks unsafe, or because the telemetry "
    "shows an obstacle, hazard, or blocked path. If the operator asked to move, "
    "you MUST relay it; the checker will slow or refuse anything unsafe downstream. "
    "Nulling a drive request to 'protect' the robot is a BUG — it silently drops "
    "the operator's command. Detect movement intent and pass it on; nothing more.\n"
    "Examples (operator says -> your JSON reply; note the obstacle in telemetry "
    "does NOT suppress the directive):\n"
    '  creep forward one meter  -> {"say": "Creeping forward a meter; the checker will bound it.", "directive": "creep forward one meter"}\n'
    '  take us to the door      -> {"say": "Heading for the door.", "directive": "take us to the door"}\n'
    '  what do you see?         -> {"say": "Nearest obstacle is about two meters ahead.", "directive": null}'
)


def perception_relevant(text):
    t = text.lower()
    return any(w in t for w in PERCEPTION_WORDS)


def context_for(utterance):
    """Fresh live telemetry each turn (posture + last verdict always; the costly
    perception grab only when the utterance is about seeing)."""
    op = operator_name()
    parts = [
        f"operator: {op} (address them by name when natural)" if op
        else "operator: unknown (don't guess a name)",
        gather_posture(), gather_stop_reason(),
    ]
    if perception_relevant(utterance):
        parts.append(gather_perception())
    return "TELEMETRY (ground truth — answer only from this):\n- " + "\n- ".join(parts)


def ask_llm(history, context, utterance):
    """One persona call with memory. Returns (say, directive|None); fail-soft."""
    messages = [{"role": "system", "content": STAGE2_SYSTEM}]
    messages += history
    messages.append({"role": "user", "content": f"{context}\n\nOperator says: {utterance}"})
    try:
        r = requests.post(f"{OLLAMA}/api/chat", timeout=60.0,
                          json={"model": MODEL, "stream": False, "messages": messages,
                                "keep_alive": KEEP_ALIVE, "options": ROUTER_LLM_OPTIONS})
        if r.status_code != 200:
            return None, None
        raw = (r.json().get("message", {}).get("content") or "").strip()
    except Exception:  # noqa: BLE001
        return None, None
    return parse_reply(raw)


def ask_llm_skills(history, context, utterance):
    """Skills-mode persona call (opt-in, KIRRA_SKILLS_ENABLED). Returns the raw
    JSON string; `skill_registry.plan_skills` parses it fail-closed. Same model +
    near-deterministic options as the default router — only the CONTRACT differs
    (named skills instead of a free-form directive)."""
    system = RABBIT_SYSTEM + "\n\n" + skill_registry.skills_prompt_fragment()
    messages = [{"role": "system", "content": system}]
    messages += history
    messages.append({"role": "user", "content": f"{context}\n\nOperator says: {utterance}"})
    try:
        r = requests.post(f"{OLLAMA}/api/chat", timeout=60.0,
                          json={"model": MODEL, "stream": False, "messages": messages,
                                "keep_alive": KEEP_ALIVE, "options": ROUTER_LLM_OPTIONS})
        if r.status_code != 200:
            return ""
        return (r.json().get("message", {}).get("content") or "").strip()
    except Exception:  # noqa: BLE001
        return ""


def ask_llm_mission(history, context, utterance):
    """Mission-mode persona call (opt-in, KIRRA_MISSIONS_ENABLED). Returns the raw
    JSON string; `mission.plan_mission` parses it fail-closed. Same model +
    options as the other routers — only the CONTRACT differs (an ordered
    multi-step mission over the registered skills)."""
    system = RABBIT_SYSTEM + "\n\n" + mission.missions_prompt_fragment()
    messages = [{"role": "system", "content": system}]
    messages += history
    messages.append({"role": "user", "content": f"{context}\n\nOperator says: {utterance}"})
    try:
        r = requests.post(f"{OLLAMA}/api/chat", timeout=60.0,
                          json={"model": MODEL, "stream": False, "messages": messages,
                                "keep_alive": KEEP_ALIVE, "options": ROUTER_LLM_OPTIONS})
        if r.status_code != 200:
            return ""
        return (r.json().get("message", {}).get("content") or "").strip()
    except Exception:  # noqa: BLE001
        return ""


def parse_reply(raw):
    """Lenient JSON extraction. FAIL-CLOSED: anything we can't parse as a clear
    directive → (text, None) — no directive, no motion."""
    m = re.search(r"\{.*\}", raw, re.DOTALL)
    if m:
        try:
            j = json.loads(m.group(0))
            say = (j.get("say") or "").strip()
            directive = j.get("directive")
            if isinstance(directive, str):
                directive = directive.strip()
                if directive.lower() in ("", "null", "none"):
                    directive = None
            else:
                directive = None
            return (say or raw.strip()), directive
        except Exception:  # noqa: BLE001
            pass
    return raw.strip(), None  # fail-closed: no directive


def offer_to_door(directive_text):
    """Hand the directive TEXT to the ONE fail-closed door (mick POST /intent).
    Returns 'ok' | 'reject' | 'error'. This is the sole actuation-adjacent call —
    it is text-to-the-door, exactly what a human typing does."""
    try:
        r = requests.post(f"{MICK}/intent", timeout=60.0, json={"text": directive_text})
        j = r.json() if r.content else {}
        if r.status_code == 200 and isinstance(j, dict) and j.get("ok"):
            return "ok"
        return "reject"
    except Exception:  # noqa: BLE001
        return "error"


def _speak_reply(text):
    """Speak a CONVERSATIONAL reply (P3 info-speech). Interruptible (barge-in)
    when KIRRA_BARGE_IN_ENABLED=1 — a PTT press / raised signal cuts it so Rabbit
    stops and listens; otherwise the plain blocking speak(). Channel A, cosmetic:
    cutting a reply early never affects the fenced /intent door. Only the long
    conversational line uses this; the short deterministic lines (OTA/diag/wake)
    stay on plain speak()."""
    if not barge_in.enabled():
        speak(text)
        return
    tts_argv = (os.environ.get("KIRRA_TTS_CMD") or "").split()
    path = barge_in.signal_path()
    baseline = barge_in.read_epoch(path)
    barge_in.speak_interruptible(text, tts_argv,
                                 barge_in.make_file_cancel_check(path, baseline))


def handle_turn(history, utterance):
    # System commands (OTA "check/apply update") are matched DETERMINISTICALLY and
    # handled BEFORE the LLM/movement path — they run local kirra-ota-ctl, never
    # the fenced mick /intent door, and a movement utterance never reaches here.
    ota_reply = rabbit_ota.handle(utterance)
    if ota_reply is not None:
        speak(ota_reply)
        history.append({"role": "user", "content": utterance})
        history.append({"role": "assistant", "content": ota_reply})
        del history[: max(0, len(history) - 2 * MAX_TURNS)]
        return

    # "Run diagnostics" / "check yourself" — deterministic like OTA, matched
    # BEFORE the LLM (a self-check must never depend on model inference), and
    # read-only (kirra_doctor; no /intent, no motion).
    diag_reply = rabbit_diag.handle(utterance)
    if diag_reply is not None:
        speak(diag_reply)
        history.append({"role": "user", "content": utterance})
        history.append({"role": "assistant", "content": diag_reply})
        del history[: max(0, len(history) - 2 * MAX_TURNS)]
        return

    # "Go to sleep" / "stop listening" / "start listening" — deterministic
    # wake-listener controls (W1). Whether the ambient mic is open must never
    # depend on model inference; rabbit_wake only writes the local state file
    # wake_word.py polls (no /intent, no motion).
    wake_reply = rabbit_wake.handle(utterance)
    if wake_reply is not None:
        speak(wake_reply)
        history.append({"role": "user", "content": utterance})
        history.append({"role": "assistant", "content": wake_reply})
        del history[: max(0, len(history) - 2 * MAX_TURNS)]
        return

    # "Situation report" / "sitrep" — deterministic, read-only (opt-in,
    # KIRRA_WORLD_MODEL_ENABLED). Renders the TTL'd World Model projection: a
    # stale/unavailable field is SAID to be unknown, never a stale value. No LLM,
    # no /intent, no motion. Off → None → falls through to the LLM.
    wm_reply = world_model.handle(utterance)
    if wm_reply is not None:
        _speak_reply(wm_reply)
        history.append({"role": "user", "content": utterance})
        history.append({"role": "assistant", "content": wm_reply})
        del history[: max(0, len(history) - 2 * MAX_TURNS)]
        return

    context = context_for(utterance)

    # MISSION MODE (opt-in, KIRRA_MISSIONS_ENABLED; takes precedence over skills):
    # the LLM emits {say, mission:[...]} — a multi-step plan the Executive runs
    # with sequencing / retry / cancel. Each MOTION step still routes through the
    # SAME fenced door (offer_to_door → /intent → checker); a mission with any
    # unsupported skill is REFUSED before any motion; a checker-refused step HALTS
    # (never skip-and-continue); a barge-in cancels. Default off → byte-identical.
    if mission.enabled():
        say, steps = mission.plan_mission(ask_llm_mission(history, context, utterance))
        if say:
            _speak_reply(say)
        ok, decisions, reason = mission.validate_mission(steps)
        if not ok:
            if steps:  # a real (but unsupported) mission — say why; empty = just chat
                _speak_reply(f"I can't run that mission — {reason}.")
            elif not say:
                _speak_reply(f"I didn't quite catch that{name_slot()}.")
        else:
            def _mission_progress(i, n, d):
                line = mission.narrate_progress(i, n, d)
                if line:
                    _speak_reply(line)
            result = mission.run_mission(
                decisions, offer_to_door, _speak_reply,
                cancel_check=mission.cancel_check_from_barge_in(),
                progress_fn=_mission_progress)
            _speak_reply(mission.narrate_result(result))
        history.append({"role": "user", "content": utterance})
        history.append({"role": "assistant", "content": say or "(mission)"})
        del history[: max(0, len(history) - 2 * MAX_TURNS)]
        return

    # SKILLS MODE (opt-in, KIRRA_SKILLS_ENABLED): the LLM emits {say, skills[]}
    # from the REGISTERED vocabulary instead of a free-form directive. A motion
    # skill still routes through the SAME fenced door (offer_to_door → /intent →
    # checker) via execute_skill_decisions — the registry is a catalog, not a new
    # door — and an unimplemented/unknown skill is REFUSED, never faked. Default
    # off → the free-form {say, directive} router below is byte-identical.
    if skill_registry.enabled():
        say, decisions = skill_registry.plan_skills(
            ask_llm_skills(history, context, utterance))
        if say:
            _speak_reply(say)
        elif not decisions:
            _speak_reply(f"I didn't quite catch that{name_slot()}.")
        skill_registry.execute_skill_decisions(decisions, offer_to_door, _speak_reply)
        history.append({"role": "user", "content": utterance})
        history.append({"role": "assistant", "content": say or "(skill request)"})
        del history[: max(0, len(history) - 2 * MAX_TURNS)]
        return

    say, directive = ask_llm(history, context, utterance)
    if say is None:
        say = "My voice module is offline for a moment."
        directive = None

    if directive:
        result = offer_to_door(directive)
        if result == "ok":
            spoken = say or f"On our way{name_slot()} — the governor will keep us honest."
        elif result == "reject":
            spoken = ("I heard a movement request, but I couldn't pin down a "
                      "safe destination — could you say it another way?")
        else:  # error
            spoken = "I can't reach my driving control right now, so I'm staying put."
    else:
        spoken = say

    _speak_reply(spoken)
    # rolling memory (store the spoken reply, not the raw grounding)
    history.append({"role": "user", "content": utterance})
    history.append({"role": "assistant", "content": spoken})
    del history[: max(0, len(history) - 2 * MAX_TURNS)]


def _run_turn(history, utterance):
    """One turn, bracketed by the cross-process turn-state signal so the wake
    listener re-arms its mic the instant the reply finishes (Slice R) instead of
    on a blind timer. mark_active spans exactly the LLM+TTS stretch the listener
    can't see; mark_done runs in a finally so a mid-turn error still re-arms."""
    turn_state.mark_active()
    try:
        handle_turn(history, utterance)
    finally:
        turn_state.mark_done()


def main():
    once = "--once" in sys.argv[1:]
    history = []
    if once:
        utterance = sys.stdin.read().strip()
        if utterance:
            _run_turn(history, utterance)
        else:
            # Empty transcript (e.g. PTT released with nothing intelligible) → F2.
            speak(f"I didn't quite catch that{name_slot()}.")
        return
    print("rabbit_converse: talk to Rabbit — one line per turn (Ctrl-D quits).",
          file=sys.stderr)
    for line in sys.stdin:
        utterance = line.strip()
        if utterance:
            _run_turn(history, utterance)


if __name__ == "__main__":
    main()
