#!/usr/bin/env python3
"""Host tests for voice_turn_router.py — endpoint isolation, fail-safety,
speech behavior, privacy, the source-level actuation fence, and the service
units' mutual exclusion. Stub HTTP servers stand in for mick_chat_service
(:chat) and mick_service (:intent); no Ollama, no robot, loopback only.

The safety proofs:
  * conversation contacts ONLY the chat stub; motion ONLY the intent stub;
    ambiguous contacts NEITHER; one transcript → at most one HTTP request;
  * motion never touches /chat and conversation never touches /intent —
    including when the chosen endpoint FAILS (no cross-fallback);
  * 422 / 429 / connection-refused / malformed JSON / {"intent":null}
    (route disagreement) each speak a fixed safe line, with NO retry;
  * the acceptance ack claims ADMISSION only — never execution;
  * transcripts never appear on stderr/stdout logging (stage tokens only);
  * the router is stateless — no transcript persistence, so a restart
    cannot replay prior turns (asserted at the source level: no file
    writes at all);
  * the router source references no ROS/serial/motor/planner/release
    machinery (the voice-router fence);
  * rabbit-voice.service and mick-voice-router.service declare mutual
    Conflicts=, run in the user session, use absolute paths and bounded
    on-failure restarts.

Runs standalone (`python3 robot/voice_turn_router_test.py`, exit 1 on any
failure); importable under pytest.
"""
from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import voice_turn_router as router  # noqa: E402

_F: list[str] = []


def check(cond, msg):
    if not cond:
        _F.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


# --- stub endpoints -----------------------------------------------------------

class StubState:
    chat_paths: list[str] = []
    intent_paths: list[str] = []
    intent_mode = "accept"
    intent_bodies: list[dict] = []
    # The typed-destination door lives on the SAME sidecar as /intent, so the
    # intent stub serves both paths and one list records every request — that
    # is what makes "exactly one endpoint per transcript" checkable.
    dest_mode = "resolved"
    dest_bodies: list[dict] = []


def _sse(handler, events):
    handler.send_response(200)
    handler.send_header("Content-Type", "text/event-stream")
    handler.end_headers()
    for ev, data in events:
        handler.wfile.write(
            f"event: {ev}\ndata: {json.dumps(data)}\n\n".encode())


class ChatStub(BaseHTTPRequestHandler):
    def do_POST(self):  # noqa: N802
        StubState.chat_paths.append(self.path)
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        _sse(self, [
            ("start", {}),
            ("delta", {"text": "Doing "}),
            ("sentence", {"text": "Doing well, thanks for asking."}),
            ("done", {"ttft_ms": 5, "total_ms": 9}),
        ])

    def log_message(self, *_):
        pass


DEST_REFUSALS = {
    "not_found": ("DEST_NOT_FOUND", 422),
    "no_registry": ("DEST_NO_REGISTRY", 422),
    "ambiguous": ("DEST_AMBIGUOUS", 422),
    "stale": ("DEST_STALE", 422),
    "unsupported": ("DEST_UNSUPPORTED_ADDRESS", 422),
    "low_confidence": ("DEST_LOW_CONFIDENCE", 422),
    "rate": ("DEST_RATE_LIMITED", 429),
    "coords": ("DEST_COORDINATES_FORBIDDEN", 422),
}


class IntentStub(BaseHTTPRequestHandler):
    def do_POST(self):  # noqa: N802
        StubState.intent_paths.append(self.path)
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        if self.path == "/destination":
            StubState.dest_bodies.append(body)
            return self._destination()
        StubState.intent_bodies.append(body)
        mode = StubState.intent_mode
        if mode == "accept":
            body, code = {"ok": True, "seq": 12, "at_ms": 1,
                          "intent": {"intent": "go_to", "x_m": 1.0}}, 200
        elif mode == "nonmotion":
            body, code = {"ok": True, "intent": None}, 200
        elif mode == "422":
            body, code = {"ok": False, "error": "MICK_JSON_PARSE_ERROR"}, 422
        elif mode == "429":
            body, code = {"ok": False, "error": "MICK_RATE_LIMITED"}, 429
        elif mode == "garbage":
            payload = b"not json"
            self.send_response(200)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        else:  # pragma: no cover
            raise AssertionError(mode)
        payload = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _destination(self):
        mode = StubState.dest_mode
        if mode == "resolved":
            body, code = {"ok": True, "seq": 4, "at_ms": 1, "outcome": "resolved",
                          "kind": "named_place", "source": "place_registry",
                          "route_resolution": "none"}, 200
        elif mode == "resolved_route":
            body, code = {"ok": True, "seq": 5, "at_ms": 1, "outcome": "resolved",
                          "kind": "saved_route", "source": "route_registry",
                          "route_resolution": "first_waypoint_only"}, 200
        elif mode == "garbage":
            payload = b"not json"
            self.send_response(200)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        elif mode == "ok_without_outcome":
            # A 200 that never says "resolved" is NOT an acceptance.
            body, code = {"ok": True, "seq": 9}, 200
        elif mode in DEST_REFUSALS:
            err, code = DEST_REFUSALS[mode]
            body = {"ok": False, "error": err, "detail": "refused"}
        else:  # pragma: no cover
            raise AssertionError(mode)
        payload = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_):
        pass


def serve(handler):
    srv = HTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://127.0.0.1:{srv.server_port}"


class Turn:
    """Run one router turn against the stubs, capturing stdout/stderr."""

    def __init__(self, chat_url, intent_url, text, turn=1):
        os.environ["KIRRA_MICK_CHAT_URL"] = chat_url
        os.environ["KIRRA_MICK_INTENT_URL"] = intent_url
        os.environ.pop("KIRRA_TTS_CMD", None)
        StubState.chat_paths.clear()
        StubState.intent_paths.clear()
        StubState.intent_bodies.clear()
        StubState.dest_bodies.clear()
        out, err = io.StringIO(), io.StringIO()
        real_out, real_err = sys.stdout, sys.stderr
        sys.stdout, sys.stderr = out, err
        try:
            router.handle_turn(text, turn, tts_cmd=None)
        finally:
            sys.stdout, sys.stderr = real_out, real_err
            for k in ("KIRRA_MICK_CHAT_URL", "KIRRA_MICK_INTENT_URL"):
                os.environ.pop(k, None)
        self.out, self.err = out.getvalue(), err.getvalue()
        self.chat = list(StubState.chat_paths)
        # Every request to the Mick sidecar, by path — so a test can assert
        # BOTH which door was used and that no second door was touched.
        self.mick = list(StubState.intent_paths)
        self.intent = [p for p in StubState.intent_paths if p == "/intent"]
        self.dest = [p for p in StubState.intent_paths if p == "/destination"]
        self.dest_bodies = list(StubState.dest_bodies)


# --- endpoint isolation -------------------------------------------------------

def test_conversation_contacts_only_chat():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        t = Turn(cu, iu, "how are you")
        check(t.chat == ["/chat/stream"], f"conversation must hit /chat/stream once: {t.chat}")
        check(t.intent == [], f"conversation must NEVER touch /intent: {t.intent}")
        check("Doing well" in t.out, "streamed sentence must be spoken/printed")
    finally:
        cs.shutdown()
        is_.shutdown()


def test_motion_contacts_only_intent_and_acks_admission():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        StubState.intent_mode = "accept"
        t = Turn(cu, iu, "drive forward one meter")
        check(t.intent == ["/intent"], f"motion must hit /intent once: {t.intent}")
        check(t.chat == [], f"motion must NEVER touch chat: {t.chat}")
        check(StubState.intent_bodies == [{"text": "drive forward one meter"}],
              "the transcript text is the only payload")
        check("Request accepted." in t.out, "the short ack is spoken")
        for banned in ("go_to", "x_m", "moving", "moved", "executing",
                       "complete", "started"):
            check(banned not in t.out,
                  f"ack must claim ADMISSION only, not {banned!r}: {t.out!r}")
    finally:
        cs.shutdown()
        is_.shutdown()


def test_ambiguous_contacts_neither_endpoint():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        for text in ("go ahead", "do it", "take me there"):
            t = Turn(cu, iu, text)
            check(t.chat == [] and t.intent == [],
                  f"ambiguous must contact NOTHING: {text!r} → chat={t.chat} intent={t.intent}")
            check("Please say exactly where or how you want me to move." in t.out,
                  "the fixed clarification is spoken")
    finally:
        cs.shutdown()
        is_.shutdown()


def test_whisper_drive_variants_contact_only_intent():
    # The two known Whisper renderings of "drive forward one meter" must
    # reach the governed door exactly once each — never chat, never a
    # retry — and the spoken line stays the admission-only ack.
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        StubState.intent_mode = "accept"
        for text in ("drive one meter", "drive for one meter"):
            t = Turn(cu, iu, text)
            check(t.intent == ["/intent"],
                  f"{text!r} must hit /intent exactly once: {t.intent}")
            check(t.chat == [],
                  f"{text!r} must NEVER touch chat: {t.chat}")
            check("Request accepted." in t.out,
                  f"{text!r}: admission-only ack spoken")
            for banned in ("moving", "moved", "executing", "complete"):
                check(banned not in t.out,
                      f"{text!r}: ack must not claim execution ({banned!r})")
        # And a broad drive phrase goes ONLY to chat (never /intent).
        t = Turn(cu, iu, "how does a motor drive work")
        check(t.chat == ["/chat/stream"] and t.intent == [],
              f"broad drive prose must go to chat only: "
              f"chat={t.chat} intent={t.intent}")
    finally:
        cs.shutdown()
        is_.shutdown()


def test_one_transcript_one_request_and_no_replay():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        StubState.intent_mode = "accept"
        t1 = Turn(cu, iu, "turn left", turn=1)
        t2 = Turn(cu, iu, "turn left", turn=2)
        check(len(t1.intent) == 1 and len(t2.intent) == 1,
              "each spoken turn maps to exactly ONE request — the router "
              "itself never replays or retries")
    finally:
        cs.shutdown()
        is_.shutdown()


# --- typed destinations -------------------------------------------------------

def test_destination_contacts_only_the_resolver_door():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        StubState.dest_mode = "resolved"
        t = Turn(cu, iu, "drive to the kitchen")
        check(t.dest == ["/destination"],
              f"a destination must hit /destination exactly once: {t.mick}")
        check(t.intent == [], f"a destination must NEVER touch /intent: {t.mick}")
        check(t.chat == [], f"a destination must NEVER touch chat: {t.chat}")
        check(t.dest_bodies == [{"destination": {"kind": "named_place",
                                                 "query": "kitchen"}}],
              f"the typed request is the ONLY payload: {t.dest_bodies}")
        check(router.LINE_DEST_ACCEPTED in t.out,
              f"the admission line is spoken: {t.out!r}")
    finally:
        cs.shutdown()
        is_.shutdown()


def test_the_admission_ack_claims_no_movement_and_speaks_no_geometry():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        StubState.dest_mode = "resolved"
        t = Turn(cu, iu, "drive to the kitchen")
        for banned in ("moving", "moved", "driving", "arrived", "arrival",
                       "executing", "complete", "started", "x_m", "y_m",
                       "metre", "meters", "coordinates", "map"):
            check(banned not in t.out.lower(),
                  f"the ack must be admission-only, found {banned!r}: {t.out!r}")
    finally:
        cs.shutdown()
        is_.shutdown()


def test_every_refusal_outcome_speaks_its_fixed_line_with_no_fallback():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        cases = [
            ("not_found", "drive to the kitchen", router.LINE_DEST_NOT_FOUND),
            ("no_registry", "drive to the kitchen", router.LINE_DEST_NOT_FOUND),
            ("ambiguous", "drive to the red cup", router.LINE_DEST_AMBIGUOUS),
            ("stale", "drive to the red cup", router.LINE_DEST_STALE),
            ("low_confidence", "drive to the red cup", router.LINE_DEST_STALE),
            ("unsupported", "drive to 125 Main Street",
             router.LINE_DEST_UNSUPPORTED),
            ("rate", "drive to the kitchen", router.LINE_DEST_RATE),
            ("coords", "drive to the kitchen", router.LINE_DEST_INVALID),
            ("garbage", "drive to the kitchen", router.LINE_DEST_INVALID),
            ("ok_without_outcome", "drive to the kitchen",
             router.LINE_DEST_INVALID),
        ]
        for mode, text, line in cases:
            StubState.dest_mode = mode
            t = Turn(cu, iu, text)
            check(len(t.dest) == 1,
                  f"{mode}: exactly one attempt, NO retry: {t.mick}")
            check(t.intent == [],
                  f"{mode}: a destination failure must NEVER fall back to /intent")
            check(t.chat == [],
                  f"{mode}: a destination failure must NEVER fall back to chat")
            check(line in t.out, f"{mode}: must speak {line!r}, got {t.out!r}")
        StubState.dest_mode = "resolved"
    finally:
        cs.shutdown()
        is_.shutdown()


def test_saved_route_admission_says_first_waypoint_only():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        StubState.dest_mode = "resolved_route"
        t = Turn(cu, iu, "run route A")
        check(t.dest_bodies == [{"destination": {"kind": "saved_route",
                                                 "query": "route a"}}],
              f"the typed route request: {t.dest_bodies}")
        check(router.LINE_DEST_ACCEPTED_FIRST_WAYPOINT in t.out,
              f"Phase 1 must be spoken, not implied: {t.out!r}")
        # It must never sound like the whole route is under way.
        for banned in ("route started", "following the route", "en route"):
            check(banned not in t.out.lower(),
                  f"must not imply full route execution ({banned!r}): {t.out!r}")
        check("first_waypoint_only" in t.err,
              "the Phase-1 marker must be in the stage log")
        StubState.dest_mode = "resolved"
    finally:
        cs.shutdown()
        is_.shutdown()


def test_unreachable_resolver_fails_safe_without_any_fallback():
    cs, cu = serve(ChatStub)
    try:
        t = Turn(cu, "http://127.0.0.1:1", "drive to the kitchen")
        check(router.LINE_DEST_DOWN in t.out,
              f"an unreachable resolver speaks the outage line: {t.out!r}")
        check(t.chat == [], "a resolver outage must not reroute to chat")
    finally:
        cs.shutdown()


def test_one_destination_transcript_makes_exactly_one_request():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        StubState.dest_mode = "resolved"
        t1 = Turn(cu, iu, "drive to the kitchen", turn=1)
        t2 = Turn(cu, iu, "drive to the kitchen", turn=2)
        check(len(t1.dest) == 1 and len(t2.dest) == 1,
              "each spoken turn maps to exactly ONE resolver request — the "
              "router never replays or retries a grounded destination")
        # A fresh process (the module is stateless) cannot replay either: the
        # statelessness test pins that no transcript or request is persisted.
    finally:
        cs.shutdown()
        is_.shutdown()


def test_ambiguous_destination_contacts_nothing():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        for text in ("go there", "drive to it", "run it", "follow that"):
            t = Turn(cu, iu, text)
            check(t.mick == [] and t.chat == [],
                  f"{text!r} must contact NOTHING: mick={t.mick} chat={t.chat}")
    finally:
        cs.shutdown()
        is_.shutdown()


def test_destination_prose_still_routes_to_chat_only():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        for text in ("tell me about the kitchen", "what is route planning",
                     "where is the red cup"):
            t = Turn(cu, iu, text)
            check(t.chat == ["/chat/stream"] and t.mick == [],
                  f"{text!r} must be chat-only: chat={t.chat} mick={t.mick}")
    finally:
        cs.shutdown()
        is_.shutdown()


def test_destination_content_never_appears_in_logs():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        StubState.dest_mode = "resolved"
        # Distinctive markers in the place name, the address and the object.
        for text in ("drive to the zebrawaffle",
                     "drive to 125 Unicornfeather Street",
                     "drive to the red cup"):
            t = Turn(cu, iu, text)
            for marker in ("zebrawaffle", "unicornfeather", "red cup",
                           "Unicornfeather"):
                check(marker not in t.err,
                      f"destination content leaked to logs for {text!r}")
        # The refusal DETAIL from the resolver is never echoed to logs either.
        StubState.dest_mode = "not_found"
        t = Turn(cu, iu, "drive to the zebrawaffle")
        check("zebrawaffle" not in t.err and "refused" not in t.err,
              f"a refusal must log the CODE only: {t.err!r}")
        check("resolve_outcome=dest_not_found" in t.err,
              f"the stable outcome token is logged: {t.err!r}")
        StubState.dest_mode = "resolved"
    finally:
        cs.shutdown()
        is_.shutdown()


def test_destination_speech_holds_the_playback_lock():
    # Every spoken destination line goes through speak_line, which holds the
    # PlaybackGuard for the whole utterance — so the robot cannot hear its own
    # acknowledgement and start another turn (the #1290 self-echo regression).
    import inspect
    import voice_playback
    src = inspect.getsource(router.speak_line)
    check("PlaybackGuard" in src, "speak_line must hold the playback guard")
    body = inspect.getsource(router.handle_turn)
    check(body.count("speak_line") >= 3,
          "every non-chat branch (motion, destination, ambiguous) speaks "
          "through the guarded path")
    # And while the lock is held no trigger may be emitted at all — so a
    # destination acknowledgement cannot self-trigger the next turn.
    check(not voice_playback.should_emit_trigger(
        playback=True, episode_followups=0, max_followups=3),
        "no turn may be created while playback holds the lock")
    # The episode cap is unchanged by this PR: a destination turn is one
    # real user turn and adds no follow-up of its own.
    check(voice_playback.should_emit_trigger(
        playback=False, episode_followups=0, max_followups=3),
        "a genuine wake-free follow-up under the cap is still allowed")
    check(not voice_playback.should_emit_trigger(
        playback=False, episode_followups=3, max_followups=3),
        "the existing follow-up cap still binds")


def test_destination_turn_creates_no_automatic_follow_up():
    # A destination turn is ONE user turn: the router speaks once and returns.
    # It never issues a second request, and it never enqueues another turn.
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        for mode in ("resolved", "not_found", "unsupported"):
            StubState.dest_mode = mode
            t = Turn(cu, iu, "drive to the kitchen")
            check(len(t.dest) == 1 and t.chat == [],
                  f"{mode}: exactly one request, no follow-up turn")
        StubState.dest_mode = "resolved"
    finally:
        cs.shutdown()
        is_.shutdown()


# --- fail-safety --------------------------------------------------------------

def test_intent_failures_speak_safe_lines_without_retry_or_chat_fallback():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        cases = [
            ("nonmotion", router.LINE_ROUTE_DISAGREE),
            ("422", router.LINE_INTENT_INVALID),
            ("429", router.LINE_INTENT_RATE),
            ("garbage", router.LINE_INTENT_DOWN),
        ]
        for mode, line in cases:
            StubState.intent_mode = mode
            t = Turn(cu, iu, "drive forward one meter")
            check(len(t.intent) == 1,
                  f"{mode}: exactly one attempt, no retry: {t.intent}")
            check(t.chat == [],
                  f"{mode}: a failed motion request must NOT fall back to chat")
            check(line in t.out, f"{mode}: must speak {line!r}, got {t.out!r}")
        StubState.intent_mode = "accept"
    finally:
        cs.shutdown()
        is_.shutdown()


def test_unreachable_intent_service_fails_safe():
    cs, cu = serve(ChatStub)
    try:
        t = Turn(cu, "http://127.0.0.1:1", "pull over")
        check(router.LINE_INTENT_DOWN in t.out,
              f"unreachable /intent must speak the outage line: {t.out!r}")
        check(t.chat == [], "an /intent outage must not reroute to chat")
    finally:
        cs.shutdown()


def test_unreachable_chat_service_fails_safe_without_intent_fallback():
    is_, iu = serve(IntentStub)
    try:
        t = Turn("http://127.0.0.1:1", iu, "tell me a joke")
        check(t.intent == [],
              "a chat outage must never reroute conversation to /intent")
    finally:
        is_.shutdown()


# --- privacy ------------------------------------------------------------------

def test_transcript_never_appears_in_logs():
    cs, cu = serve(ChatStub)
    is_, iu = serve(IntentStub)
    try:
        StubState.intent_mode = "accept"
        marker = "zebra unicorn waffle"  # distinctive transcript content
        for text in (f"drive forward {marker}", f"explain {marker}",
                     f"go ahead {marker}"):
            t = Turn(cu, iu, text)
            check(marker not in t.err,
                  f"transcript leaked to stderr logs for {text!r}")
        StubState.intent_mode = "accept"
    finally:
        cs.shutdown()
        is_.shutdown()


def _code_only(src: str) -> str:
    """Strip # comments AND docstrings — commentary may (and does) explain
    what the router is fenced FROM; only code touching it is a breach."""
    import re
    no_doc = re.sub(r'"""[\s\S]*?"""', "", src)
    return "\n".join(line.split("#")[0] for line in no_doc.splitlines())


def test_router_is_stateless_no_persistence():
    import re
    code = _code_only((HERE / "voice_turn_router.py").read_text())
    # A BARE open( is a file access; urlopen( is the network call and fine.
    check(not re.search(r"(?<![\w.])open\(", code),
          "router must not open files — restart safety depends on statelessness")
    for token in ("Path(", "tempfile", "pickle", "sqlite", "shelve"):
        check(token not in code,
              f"router must not persist anything (found {token!r})")


# --- the voice-router fence ---------------------------------------------------

def test_router_sources_know_only_the_two_http_doors():
    join = lambda a, b: a + b  # noqa: E731 — needles built by concat so the
    #                            fence test cannot match its own text
    forbidden = [
        join("rcl", "py"), join("ro", "s2"), join("ser", "ial"),
        join("Motor", "Serial"), join("cmd", "_vel"), join("Twi", "st"),
        join("release", "_token"), join("issue_ros", "_release"),
        join("kirra_", "ffi"), join("plan", "ner_service"),
        join("/pl", "an"), join("81", "00"),  # planner port
        join("80", "90"),                     # verifier port
        "subprocess.call", "os.system",
    ]
    for name in ("voice_turn_router.py", "voice_route.py"):
        code = _code_only((HERE / name).read_text())
        for needle in forbidden:
            check(needle not in code,
                  f"{name} references {needle!r} — the router may know ONLY "
                  "the chat and intent HTTP doors")
    # The chat client the router reuses is itself fenced (its own test), but
    # pin the property here too: no /intent in the chat path.
    code = _code_only((HERE / "mick_voice_chat.py").read_text())
    check(join("/int", "ent") not in code,
          "the shared chat client must stay motion-free")


def test_voice_router_cannot_import_or_reference_actuation():
    """The structural fence, stated as a property of the SOURCE: no module in
    the voice-router layer may import or name any actuation machinery. Only
    comments and docstrings (which explain what the layer is fenced FROM) are
    exempt — `_code_only` strips them, so a breach must be real code."""
    import ast
    join = lambda a, b: a + b  # noqa: E731 — needles built by concat so this
    #                            test cannot match its own source text
    forbidden_tokens = [
        join("rcl", "py"), join("ro", "s2"), join("ser", "ial"),
        join("cmd", "_vel"), join("Twi", "st"), join("release", "_token"),
        join("Release", "Token"), join("actu", "ator"), join("Motor", "Serial"),
        join("kirra_", "ffi"), join("set_car", "_motion"), join("/pl", "an"),
    ]
    forbidden_imports = {join("rcl", "py"), join("ser", "ial"),
                         join("kirra_", "ffi"), join("kirra_motor", "_consumer"),
                         join("kirra_release", "_publisher")}
    layer = ("voice_route.py", "voice_turn_router.py", "mick_voice_chat.py")
    for name in layer:
        src = (HERE / name).read_text()
        code = _code_only(src)
        for needle in forbidden_tokens:
            check(needle.lower() not in code.lower(),
                  f"{name} references actuation machinery {needle!r}")
        # An import is checked structurally, not textually: a renamed or
        # aliased import still shows up as a module name in the AST.
        for node in ast.walk(ast.parse(src)):
            mods = []
            if isinstance(node, ast.Import):
                mods = [a.name for a in node.names]
            elif isinstance(node, ast.ImportFrom):
                mods = [node.module or ""]
            for m in mods:
                root = m.split(".")[0]
                check(root not in forbidden_imports,
                      f"{name} imports {m!r} — the voice layer has no "
                      "actuation dependency")


def test_explicit_motion_cannot_reach_chat_under_normal_routing():
    # Belt and suspenders with the chat sidecar's own refusal: the router
    # classifies first, so a motion order never even reaches /chat.
    from voice_route import RouteKind, classify_transcript
    for t in ("drive forward one meter", "stop", "pull over",
              "hey parker turn left"):
        check(classify_transcript(t).kind is RouteKind.MOTION,
              f"motion order must classify MOTION before chat: {t!r}")


# --- service units ------------------------------------------------------------

def unit(name: str) -> str:
    return (HERE / "install" / "systemd" / "user" / name).read_text()


def test_units_declare_mutual_exclusion():
    rv = unit("rabbit-voice.service")
    mv = unit("mick-voice-router.service")
    check("Conflicts=mick-voice-router.service" in rv,
          "rabbit-voice must conflict with the router")
    check("Conflicts=rabbit-voice.service" in mv,
          "the router must conflict with rabbit-voice")


def test_router_unit_shape():
    mv = unit("mick-voice-router.service")
    check("EnvironmentFile=/etc/kirra/robot.env" in mv, "env file consumed")
    check("ExecStart=/opt/kirra/robot/mick_voice_router.sh" in mv,
          "absolute ExecStart path")
    check("Restart=on-failure" in mv, "bounded restart policy")
    check("RestartSec=" in mv, "bounded restart delay")
    check("WantedBy=default.target" in mv, "user-session unit")
    check("Restart=always" not in mv, "never an unconditional restart loop")


def test_installer_stages_router_pipeline():
    src = (HERE / "install" / "install_robot_units.sh").read_text()
    for f in ("voice_route.py", "voice_turn_router.py",
              "voice_transcribe.sh", "mick_voice_router.sh",
              "mick-voice-router.service"):
        check(f in src, f"installer must stage {f}")


def test_shared_transcriber_is_the_single_recorder():
    rv = (HERE / "rabbit_voice.sh").read_text()
    check("voice_transcribe.sh" in rv,
          "rabbit_voice.sh must use the shared transcriber")
    check("KIRRA_RECORD_CMD" not in rv.split("voice_transcribe.sh")[0]
          or "transcribe()" not in rv,
          "the inline recorder copy must be gone from rabbit_voice.sh")
    mr = (HERE / "mick_voice_router.sh").read_text()
    check("voice_transcribe.sh" in mr and "wake_word.py" in mr,
          "the router pipeline must reuse wake + the shared transcriber")


# --- standalone runner (house pattern) ----------------------------------------

def main() -> int:
    t0 = time.monotonic()
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"  ok  {name}")
    if _F:
        print(f"voice_turn_router_test: {len(_F)} FAILURE(S)")
        return 1
    print(f"voice_turn_router_test: ALL OK ({time.monotonic() - t0:.1f}s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
