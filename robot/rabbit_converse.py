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
    RABBIT_SYSTEM, MICK, MODEL, OLLAMA, gather_perception, gather_posture,
    gather_stop_reason, speak,
)
import rabbit_ota  # noqa: E402 — deterministic OTA voice commands (NOT the movement door)
from rabbit_persona import name_slot, operator_name  # noqa: E402

MAX_TURNS = 10  # rolling conversation memory (user+assistant pairs kept)
PERCEPTION_WORDS = ("see", "around", "ahead", "front", "obstacle", "clear",
                    "look", "there", "path", "way", "block")

STAGE2_SYSTEM = (
    RABBIT_SYSTEM
    + "\n\nEACH TURN, reply with a JSON object and nothing else:\n"
    '  {"say": "<one or two sentences to speak aloud>",\n'
    '   "directive": <null, OR the operator\'s movement request in plain words>}\n'
    "Set `directive` ONLY when the operator clearly wants the robot to DRIVE "
    "somewhere (e.g. 'creep forward a meter', 'turn left', 'take us to the "
    "door'). Pass their movement request faithfully — do NOT invent destinations, "
    "coordinates, or numbers they did not give. For questions, status, chat, or "
    "anything ambiguous, `directive` is null. You do not decide safety — you hand "
    "the request to the governed door, and the KIRRA checker bounds what actually "
    "moves; say so if useful."
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
                          json={"model": MODEL, "stream": False, "messages": messages})
        if r.status_code != 200:
            return None, None
        raw = (r.json().get("message", {}).get("content") or "").strip()
    except Exception:  # noqa: BLE001
        return None, None
    return parse_reply(raw)


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

    context = context_for(utterance)
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

    speak(spoken)
    # rolling memory (store the spoken reply, not the raw grounding)
    history.append({"role": "user", "content": utterance})
    history.append({"role": "assistant", "content": spoken})
    del history[: max(0, len(history) - 2 * MAX_TURNS)]


def main():
    once = "--once" in sys.argv[1:]
    history = []
    if once:
        utterance = sys.stdin.read().strip()
        if utterance:
            handle_turn(history, utterance)
        else:
            # Empty transcript (e.g. PTT released with nothing intelligible) → F2.
            speak(f"I didn't quite catch that{name_slot()}.")
        return
    print("rabbit_converse: talk to Rabbit — one line per turn (Ctrl-D quits).",
          file=sys.stderr)
    for line in sys.stdin:
        utterance = line.strip()
        if utterance:
            handle_turn(history, utterance)


if __name__ == "__main__":
    main()
