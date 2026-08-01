#!/usr/bin/env python3
"""voice_turn_router.py — wake-driven hybrid voice frontend: one transcript
per stdin line → deterministic route → EXACTLY ONE endpoint (or none).

    conversation → POST /chat/stream (mick_chat_service) → streamed sentences
                   → piper, via the existing mick_voice_chat turn runner
    motion       → POST /intent (mick_service) — TEXT ONLY; Mick's typed
                   parse, Occy, the checker, release and the consumer are
                   unchanged downstream
    ambiguous    → a FIXED local clarification; NO endpoint is contacted

🔴 SEPARATION (all host-tested):
  * this process knows exactly two HTTP endpoints — the chat sidecar and the
    intent door. No planner, ROS, verifier, release-token, serial, or motor
    API is imported or addressed, and the actuation-fence test pins that;
  * a transcript goes to AT MOST one endpoint, never both;
  * NO automatic retry anywhere on the motion path (a retry could duplicate
    an accepted intent); every failure is spoken and the turn ends;
  * an explicit motion request that fails at /intent is NEVER re-routed to
    chat (and conversational text is never re-routed to /intent);
  * transcripts are transcribe-and-discard: never persisted, never logged —
    stage names, stable reason tokens and durations only;
  * acceptance is ADMISSION, not execution: the ack must not claim the robot
    moved (the checker + consumer govern that downstream).

Voice "stop" routes to /intent like any motion text and becomes Mick's typed
`hold` intent (governed decel-and-hold) — it is NOT a motor cutoff and NOT
the e-stop, which remains separate physical hardware.

Env (all optional; multi-word values quoted in robot.env):
  KIRRA_MICK_INTENT_URL          default http://127.0.0.1:8102 (falls back to
                                 the existing KIRRA_MICK_URL convention)
  KIRRA_MICK_CHAT_URL            default http://127.0.0.1:8103 (read by the
                                 shared chat turn runner)
  KIRRA_VOICE_ROUTER_TIMEOUT_MS  /intent POST timeout, default 5000
  KIRRA_VOICE_MOTION_ACK         default "Request accepted."
  KIRRA_VOICE_AMBIGUOUS_REPLY    default "Please say exactly where or how
                                 you want me to move."
  KIRRA_TTS_CMD                  text lines on stdin → audio (speak.sh)
  KIRRA_VOICE_ROUTER_LOG         1 → stage/timing lines on stderr (default on;
                                 0 disables — content is never logged either way)
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import mick_voice_chat  # noqa: E402 — the shared streaming chat turn runner
from voice_route import RouteKind, classify_transcript  # noqa: E402

DEFAULT_INTENT_URL = "http://127.0.0.1:8102"
DEFAULT_MOTION_ACK = "Request accepted."
DEFAULT_AMBIGUOUS_REPLY = "Please say exactly where or how you want me to move."
LINE_INTENT_INVALID = "I couldn't turn that into a valid motion request."
LINE_INTENT_RATE = "Please wait a moment before giving another motion request."
LINE_INTENT_DOWN = "The governed motion service is unavailable."
LINE_ROUTE_DISAGREE = ("That didn't register as a motion request. "
                       "Please say exactly where or how you want me to move.")


def _log_enabled() -> bool:
    return (os.environ.get("KIRRA_VOICE_ROUTER_LOG") or "1") not in ("0", "false")


def log_event(turn: int, token: str, ms: float | None = None) -> None:
    """Stable tokens + durations only. NEVER transcript or reply text."""
    if not _log_enabled():
        return
    tail = f" {ms:.0f}ms" if ms is not None else ""
    print(f"voice_turn_router: turn={turn} {token}{tail}",
          file=sys.stderr, flush=True)


def intent_url() -> str:
    base = (os.environ.get("KIRRA_MICK_INTENT_URL")
            or os.environ.get("KIRRA_MICK_URL") or DEFAULT_INTENT_URL)
    return base.rstrip("/")


def timeout_s() -> float:
    raw = os.environ.get("KIRRA_VOICE_ROUTER_TIMEOUT_MS") or "5000"
    try:
        ms = int(raw)
    except ValueError:
        ms = 5000
    return max(0.5, ms / 1000.0)


def speak_line(line: str, tts_cmd: str | None) -> None:
    """One fixed sentence through ONE bounded TTS process. Also printed, so
    a speakerless bench run still shows the outcome."""
    print(f"Mick: {line}")
    if not tts_cmd:
        return
    tts = mick_voice_chat.TtsPipe(tts_cmd)
    tts.say(line)
    tts.close()


def post_intent(text: str, turn: int) -> tuple[str, float]:
    """POST the transcript to the governed intent door. ONE request, NO
    retry. Returns (spoken line, http_ms)."""
    body = json.dumps({"text": text}).encode("utf-8")
    req = urllib.request.Request(
        intent_url() + "/intent", data=body,
        headers={"Content-Type": "application/json"}, method="POST")
    t0 = time.monotonic() * 1000.0
    try:
        with urllib.request.urlopen(req, timeout=timeout_s()) as resp:
            raw = resp.read()
    except urllib.error.HTTPError as e:
        ms = time.monotonic() * 1000.0 - t0
        if e.code == 429:
            log_event(turn, "intent_rate_limited", ms)
            return LINE_INTENT_RATE, ms
        log_event(turn, f"intent_rejected_{e.code}", ms)
        return LINE_INTENT_INVALID, ms
    except (urllib.error.URLError, TimeoutError, OSError):
        ms = time.monotonic() * 1000.0 - t0
        log_event(turn, "intent_unreachable", ms)
        return LINE_INTENT_DOWN, ms
    ms = time.monotonic() * 1000.0 - t0
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError:
        log_event(turn, "intent_malformed_response", ms)
        return LINE_INTENT_DOWN, ms
    if payload.get("ok") and payload.get("intent") is not None:
        # ADMISSION only — never claim motion started or completed. The
        # intent JSON is deliberately not read aloud.
        log_event(turn, "intent_accepted", ms)
        return os.environ.get("KIRRA_VOICE_MOTION_ACK") or DEFAULT_MOTION_ACK, ms
    if payload.get("ok") and payload.get("intent") is None:
        # The router said motion, Mick's own fence said non-motion. Fail
        # closed: no retry, no chat fallback — a fixed clarification.
        log_event(turn, "route_disagreement_non_motion", ms)
        return LINE_ROUTE_DISAGREE, ms
    log_event(turn, "intent_error_body", ms)
    return LINE_INTENT_INVALID, ms


def handle_turn(text: str, turn: int, tts_cmd: str | None) -> None:
    t0 = time.monotonic() * 1000.0
    decision = classify_transcript(text)
    route_ms = time.monotonic() * 1000.0 - t0
    log_event(turn, f"route={decision.kind.value} reason={decision.reason} "
                    f"route_decision_ms", route_ms)

    if decision.kind is RouteKind.CONVERSATION:
        # The optimized streaming path, shared with mick_voice_chat:
        # /chat/stream SSE → sentence chunks → one TTS process. Its stage
        # logs (first sentence, first audio, totals) are content-free.
        mick_voice_chat.run_turn(mick_voice_chat.chat_url(), text, tts_cmd)
        return
    if decision.kind is RouteKind.MOTION:
        line, http_ms = post_intent(text, turn)
        log_event(turn, "intent_http_ms", http_ms)
        speak_line(line, tts_cmd)
        return
    # AMBIGUOUS — no endpoint is contacted at all; a fixed local reply.
    log_event(turn, "ambiguous_local_clarification")
    speak_line(os.environ.get("KIRRA_VOICE_AMBIGUOUS_REPLY")
               or DEFAULT_AMBIGUOUS_REPLY, tts_cmd)


def main() -> int:
    tts_cmd = os.environ.get("KIRRA_TTS_CMD")
    if not tts_cmd:
        print("voice_turn_router: KIRRA_TTS_CMD unset — printing only",
              file=sys.stderr)
    print("voice_turn_router: transcript lines on stdin → conversation via "
          "chat, explicit motion via the governed /intent door. Ctrl-D quits.",
          file=sys.stderr)
    turn = 0
    for raw in sys.stdin:
        text = " ".join(raw.split()).strip()
        if not text:
            continue
        turn += 1  # per-turn monotonic local id; transcripts are DISCARDED
        t0 = time.monotonic() * 1000.0
        handle_turn(text, turn, tts_cmd)
        log_event(turn, "turn_total_ms", time.monotonic() * 1000.0 - t0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
