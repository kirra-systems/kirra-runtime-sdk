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
import repo_command  # noqa: E402 — Robot Command Language bridge (allow-listed intents, no shell)
import assistant  # noqa: E402 — Kirra Engineering Assistant (typed read-only tools + the RCL)
import world_model  # noqa: E402 — opt-in situation report (read-only TTL'd projection)
import mission  # noqa: E402 — opt-in multi-step Executive (each step → the SAME /intent fence)
import rabbit_stream  # noqa: E402 — opt-in first-clause streaming of the reply to TTS
import rabbit_latency  # noqa: E402 — opt-in per-turn stage timing (observability only)
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
# Speed (Slice S, OPT-IN): cap the INPUT context window (num_ctx). Smaller ctx =
# smaller KV cache = less memory bandwidth and faster prefill/TTFT on the
# bandwidth-bound Orin. DEFAULT UNSET → Ollama's own default (check the effective
# size with `ollama ps`; only cap if it is defaulting large). SAME truncation
# caveat as num_predict but on the INPUT side: too small drops the system prompt /
# history / telemetry and can silently degrade routing, so keep it comfortably
# above one turn (system prompt + a few history pairs + the telemetry block) and
# let the smoketest (which imports these options) catch a bad value.
_num_ctx = (os.environ.get("KIRRA_RABBIT_NUM_CTX") or "").strip()
if _num_ctx.isdigit() and int(_num_ctx) > 0:
    ROUTER_LLM_OPTIONS["num_ctx"] = int(_num_ctx)

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

# Streaming reply-to-TTS (Slice T, OPT-IN via KIRRA_RABBIT_STREAM_TTS). Default
# OFF → this whole block is inert and the turn path is byte-identical. When ON,
# the router uses a DIRECTIVE-FIRST variant of the SAME contract so the routing
# decision is known BEFORE any speech: a CHAT turn streams its `say` aloud clause
# by clause as the model generates it (the latency win); a DRIVE turn voices
# nothing until the door has decided, exactly as today. All the routing
# guardrails above are preserved verbatim — only the field ORDER changes — and
# any non-directive-first / malformed reply falls back to the normal path, so
# streaming can only match or beat current behaviour, never alter routing.
STREAM_TTS = (os.environ.get("KIRRA_RABBIT_STREAM_TTS") or "").strip().lower() in \
    ("1", "true", "yes", "on")

STAGE2_SYSTEM_STREAM = (
    STAGE2_SYSTEM
    + "\n\nOUTPUT ORDER (IMPORTANT): put the `directive` field FIRST and `say` "
    "SECOND, so the object reads {\"directive\": <null or the request>, \"say\": "
    "\"...\"}. The routing decision must precede the spoken words. Everything else "
    "above — when to set `directive`, never nulling a drive request — is unchanged.\n"
    "Examples:\n"
    '  creep forward one meter  -> {"directive": "creep forward one meter", "say": "Creeping forward a meter; the checker will bound it."}\n'
    '  what do you see?         -> {"directive": null, "say": "Nearest obstacle is about two meters ahead."}'
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
    it is text-to-the-door, exactly what a human typing does.

    'ok' means AN INTENT EXISTS, not merely that the request was well-formed.
    mick has two 200 shapes and only one of them moves anything:

        {"ok":true,"intent":{...},"seq":n,"at_ms":t}   an intent was latched
        {"ok":true,"intent":null}                      deterministically non-motion
                                                       (mick's greeting/read-only
                                                       fence): no intent, no latch,
                                                       seq UNCHANGED — nothing moves

    Keying 'ok' on the `ok` flag alone would report the second shape as a
    successful drive, and Rabbit would say "On our way" while standing still —
    a false success acknowledgement, and worse in mission mode, where 'ok'
    ADVANCES to the next step. So an absent or null `intent` is 'reject': the
    door declined to produce motion, which is exactly what every caller's
    non-'ok' arm already handles (converse re-asks, mission halts, skills say so).

    `intent` is present and non-null on every accepted reply, before and after
    the fence existed, so this reads correctly against either mick build — no
    coordinated deploy."""
    try:
        r = requests.post(f"{MICK}/intent", timeout=60.0, json={"text": directive_text})
        j = r.json() if r.content else {}
        if (r.status_code == 200 and isinstance(j, dict) and j.get("ok")
                and j.get("intent") is not None):
            return "ok"
        return "reject"
    except Exception:  # noqa: BLE001
        return "error"


def _repo_sink(intent):
    """The Robot Command Language sink for a REPO_CMD skill decision.

    Receives an ALLOW-LISTED intent name and nothing else — no arguments, no
    model text — and returns the sentence derived from the executor's structured
    result. `repo_command.handle_intent` rejects any name outside its two-item
    allow-list, so a hallucinated command name reports an error rather than
    running. Kept as a named function (not a lambda) so the single seam between
    the voice layer and the repository executor is greppable.
    """
    _result, spoken = repo_command.handle_intent(intent)
    return spoken


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


def _stream_messages(history, context, utterance):
    """Router messages for the streaming path — identical to ask_llm's, but with
    the DIRECTIVE-FIRST system prompt so routing resolves before the spoken words."""
    return ([{"role": "system", "content": STAGE2_SYSTEM_STREAM}] + list(history)
            + [{"role": "user", "content": f"{context}\n\nOperator says: {utterance}"}])


def _make_stream_speaker():
    """(speak_clause, cancelled): speak streamed clauses sharing ONE barge-in
    baseline for the whole reply, so a single barge-in stops the REST of it (no
    regression vs the single-call _speak_reply). Off → plain speak()."""
    if not barge_in.enabled():
        return (lambda text: speak(text)), (lambda: False)
    tts_argv = (os.environ.get("KIRRA_TTS_CMD") or "").split()
    path = barge_in.signal_path()
    cancel_check = barge_in.make_file_cancel_check(path, barge_in.read_epoch(path))
    state = {"cancelled": False}

    def speak_clause(text):
        if state["cancelled"] or cancel_check():
            state["cancelled"] = True
            return
        barge_in.speak_interruptible(text, tts_argv, cancel_check)
        if cancel_check():
            state["cancelled"] = True

    return speak_clause, (lambda: state["cancelled"])


def route_stream(messages, on_clause, cancelled):
    """Stream the router reply, speaking CHAT clauses via on_clause as they arrive.
    Returns the parser's finish() plan (with `raw` for the caller's fallback
    parse), or None on any HTTP/stream failure → caller falls back to ask_llm.
    A DRIVE turn speaks nothing (the parser emits no clauses before routing)."""
    parser = rabbit_stream.StreamingSayParser()
    try:
        with requests.post(f"{OLLAMA}/api/chat", timeout=60.0, stream=True,
                           json={"model": MODEL, "stream": True, "messages": messages,
                                 "keep_alive": KEEP_ALIVE, "options": ROUTER_LLM_OPTIONS}) as r:
            if r.status_code != 200:
                return None
            for line in r.iter_lines():
                if cancelled():
                    break
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except Exception:  # noqa: BLE001
                    continue
                piece = (obj.get("message") or {}).get("content") or ""
                if piece:
                    for clause in parser.feed(piece):
                        on_clause(clause)
                if obj.get("done"):
                    break
    except Exception:  # noqa: BLE001 — any streaming failure → non-streaming fallback
        return None
    plan = parser.finish()
    for clause in plan["trailing"]:
        on_clause(clause)
    return plan


# The timer for the turn in flight. Module-level because handle_turn has many
# early returns (the deterministic OTA / diag / wake / sitrep commands), and
# _run_turn's finally is the one place that sees EVERY completed turn. Safe as
# a single slot: rabbit_converse handles one utterance at a time (the stdin
# loop is serial), the same assumption turn_state already relies on.
_TURN_TIMER = rabbit_latency.TurnTimer("reply")


def handle_turn(history, utterance):
    timer = _TURN_TIMER
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

    # ROBOT COMMAND LANGUAGE — deterministic repository commands (opt-in,
    # KIRRA_REPO_CMD_ENABLED). "Sync to main" / "Publish my work" and a closed set
    # of paraphrases resolve WITHOUT the model, because a canonical phrase must not
    # depend on inference being available or on the model choosing correctly. The
    # deterministic executor (scripts/robot-command.sh) authorizes and acts; this
    # only names an allow-listed intent and speaks the structured result — a
    # refusal is voiced as a refusal, never as success. No shell, no /intent, no
    # motion. Off, or not a repository command → None → falls through to the LLM.
    repo_reply = repo_command.handle(utterance)
    if repo_reply is not None:
        _speak_reply(repo_reply)
        history.append({"role": "user", "content": utterance})
        history.append({"role": "assistant", "content": repo_reply})
        del history[: max(0, len(history) - 2 * MAX_TURNS)]
        return

    # KIRRA ENGINEERING ASSISTANT — grounded repository questions (opt-in,
    # KIRRA_ASSIST_ENABLED). "Where is steering authority enforced?" / "is the
    # workspace clean?" are answered from TYPED, READ-ONLY tools (git plumbing,
    # git grep, bounded file reads) — never from model memory. Gemma interprets
    # and explains; the tools retrieve facts and enforce policy. Authority is
    # capped at level 2 (the two approved git workflows); levels 3-4 are defined
    # and refused. Placed AFTER repo_command so the RCL keeps first claim on its
    # own two phrases. Off, or not an engineering request → None → the LLM.
    assist_reply = assistant.handle(utterance)
    if assist_reply is not None:
        _speak_reply(assist_reply)
        history.append({"role": "user", "content": utterance})
        history.append({"role": "assistant", "content": assist_reply})
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
        # `_repo_sink` is the ONLY way a REPO_CMD decision executes. It receives
        # the intent NAME (allow-listed) and returns the sentence derived from the
        # executor's structured result. Not injecting it would make repo commands
        # refuse — never silently succeed.
        skill_registry.execute_skill_decisions(
            decisions, offer_to_door, _speak_reply, repo_fn=_repo_sink)
        history.append({"role": "user", "content": utterance})
        history.append({"role": "assistant", "content": say or "(skill request)"})
        del history[: max(0, len(history) - 2 * MAX_TURNS)]
        return

    # Default free-form {say, directive} router. Streaming (opt-in) voices a CHAT
    # reply clause-by-clause as it generates; a DRIVE turn and every failure mode
    # fall through to the exact non-streaming path below. `already_spoken` marks a
    # CHAT reply that streaming has already voiced (so we don't say it twice).
    say = directive = None
    already_spoken = False
    if STREAM_TTS:
        speak_clause, cancelled = _make_stream_speaker()
        plan = route_stream(_stream_messages(history, context, utterance),
                            speak_clause, cancelled)
        if plan is not None:
            # parse_reply is ALWAYS the authority on the directive (fail-closed);
            # streaming only decides whether `say` was voiced early.
            say, directive = parse_reply(plan["raw"])
            already_spoken = bool(plan.get("streamed")) and directive is None
    if say is None and directive is None and not already_spoken:
        say, directive = ask_llm(history, context, utterance)   # non-streaming / fallback
    timer.mark(rabbit_latency.LLM)

    if say is None:
        say = "My voice module is offline for a moment."
        directive = None

    if directive:
        result = offer_to_door(directive)
        timer.mark(rabbit_latency.DOOR)
        if result == "ok":
            spoken = say or f"On our way{name_slot()} — the governor will keep us honest."
        elif result == "reject":
            spoken = ("I heard a movement request, but I couldn't pin down a "
                      "safe destination — could you say it another way?")
        else:  # error
            spoken = "I can't reach my driving control right now, so I'm staying put."
    elif already_spoken:
        spoken = say                       # a CHAT reply streaming already voiced
    else:
        spoken = say

    if not already_spoken:
        _speak_reply(spoken)
    timer.mark(rabbit_latency.TTS)
    # rolling memory (store the spoken reply, not the raw grounding)
    history.append({"role": "user", "content": utterance})
    history.append({"role": "assistant", "content": spoken})
    del history[: max(0, len(history) - 2 * MAX_TURNS)]


def _run_turn(history, utterance):
    """One turn, bracketed by the cross-process turn-state signal so the wake
    listener re-arms its mic the instant the reply finishes (Slice R) instead of
    on a blind timer. mark_active spans exactly the LLM+TTS stretch the listener
    can't see; mark_done runs in a finally so a mid-turn error still re-arms."""
    global _TURN_TIMER
    # Stage timing for THIS turn. This process owns transcript→spoken; the
    # capture stages belong to rabbit_voice.sh, which reports its own span (the
    # trigger carries no payload to thread a start time through, and an invented
    # end-to-end total would be the one number here worth distrusting).
    _TURN_TIMER = rabbit_latency.TurnTimer("reply")
    turn_state.mark_active()
    try:
        handle_turn(history, utterance)
    finally:
        turn_state.mark_done()
        # ONE line per completed turn — stage names + durations only, never the
        # transcript or the reply. In `finally`, so a deterministic command (OTA
        # / diagnostics / sitrep) and a mid-turn error are reported too. Off
        # unless KIRRA_RABBIT_LATENCY_LOG=1.
        rabbit_latency.log_summary(_TURN_TIMER)


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
