#!/usr/bin/env python3
"""rabbit_converse_test — the Rabbit conversation channel still TALKS, and the
motion door still only opens for motion, after mick's non-motion fence.

mick now answers a deterministically non-motion utterance with a 200 carrying
NO intent (`{"ok":true,"intent":null}`) instead of calling the LLM. That is
correct for the motion door — mick is not the conversation channel. The
question this file answers is the OTHER half: does Rabbit still greet, chat and
answer questions, and can the new reply shape mislead anyone?

The two channels, and why they cannot be confused (design doc §Channels):

    wake word ──▶ wake_word.py ──▶ ONE newline (no text!) ──▶ rabbit_voice.sh
                                                                  │ records a
                                                                  │ NEW clip
                                                                  ▼
                            transcript ──▶ rabbit_converse.handle_turn
                                              │
                        ┌─────────────────────┴──────────────────────┐
                   say (Channel A)                        directive (Channel B)
                   speak() → TTS sink                      offer_to_door()
                   NO /intent, no ROS,                     → mick POST /intent
                   no serial, no token                     → Occy → the checker

`directive: null` is the overwhelmingly common case — every greeting, question
and chat turn — and it makes NO call to mick at all. So the fence is not even
on the path for a greeting: Rabbit answers from Ollama and speaks.

Everything external is stubbed: Ollama, TTS, mick, and the telemetry GETs. No
network, no model, no audio.

`rabbit_converse` (and `rabbit_ask` under it) do `import requests` at module load
and `sys.exit` without it, and the CI robot lane does not install it. Rather than
skip there — which would leave the false-success regression ungated on the only
lane that runs automatically — we install a MINIMAL stub module first. Its
`get`/`post` raise unless a test has replaced them, so a genuine network attempt
is a loud failure, never a silent pass. When the real library is present (the
robot, a dev box) it is used unchanged.
"""
from __future__ import annotations

import json
import sys
import types
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))


def _install_requests_stub() -> None:
    """Make `import requests` succeed with a surface that cannot reach the
    network. Only `get`/`post` are ever touched by the code under test."""
    def _refuse(*_a, **_k):
        raise AssertionError(
            "the test tried to make a REAL HTTP call — a seam is unstubbed")

    stub = types.ModuleType("requests")
    stub.get = _refuse
    stub.post = _refuse
    stub.RequestException = type("RequestException", (Exception,), {})
    stub.exceptions = types.SimpleNamespace(RequestException=stub.RequestException)
    stub.__kirra_stub__ = True
    sys.modules["requests"] = stub


try:
    import requests  # noqa: F401 — rabbit_converse needs it at import
except ImportError:
    _install_requests_stub()

# The two mick 200 shapes, verbatim from crates/kirra-sidecars/src/mick.rs
# (`AcceptedIntent::to_post_wire` and mick_service's non-motion arm).
MICK_ACCEPTED = '{"ok":true,"intent":{"intent":"go_to","x_m":1.0,"y_m":0.0},"seq":1,"at_ms":10000}'
MICK_NON_MOTION = '{"ok":true,"intent":null}'


class _Resp:
    """Minimal stand-in for a requests.Response (status_code/content/json)."""

    def __init__(self, status_code: int, body: str):
        self.status_code = status_code
        self._body = body
        self.content = body.encode()

    def json(self):
        return json.loads(self._body)


class Harness:
    """Rabbit with every external seam replaced by a recorder.

    Records what was SPOKEN and what was POSTed to mick, so 'it talked' and
    'it asked for motion' are separate, checkable facts rather than one blur.
    """

    def __init__(self, rc, router_reply: str, mick_body: str = MICK_ACCEPTED,
                 mick_status: int = 200):
        self.rc = rc
        self.router_reply = router_reply
        self.mick_body = mick_body
        self.mick_status = mick_status
        self.spoken: list[str] = []
        self.mick_posts: list[dict] = []
        self.ollama_posts: list[dict] = []
        self._saved: dict = {}

    # -- seams ----------------------------------------------------------
    def _post(self, url, **kw):
        payload = kw.get("json") or {}
        if "/intent" in url:
            self.mick_posts.append(payload)
            return _Resp(self.mick_status, self.mick_body)
        if "/api/chat" in url:
            self.ollama_posts.append(payload)
            return _Resp(200, json.dumps(
                {"message": {"content": self.router_reply}}))
        raise AssertionError(f"unexpected POST to {url}")

    def __enter__(self):
        rc = self.rc
        self._saved = {
            "post": rc.requests.post,
            "speak": rc.speak,
            "gather_posture": rc.gather_posture,
            "gather_stop_reason": rc.gather_stop_reason,
            "gather_perception": rc.gather_perception,
        }
        rc.requests.post = self._post
        rc.speak = lambda text: self.spoken.append(text)   # the TTS sink
        rc.gather_posture = lambda: "posture: NOMINAL"
        rc.gather_stop_reason = lambda: "last verdict: none"
        rc.gather_perception = lambda: "perception: nearest object 2.0 m ahead"
        return self

    def __exit__(self, *exc):
        rc = self.rc
        rc.requests.post = self._saved["post"]
        rc.speak = self._saved["speak"]
        rc.gather_posture = self._saved["gather_posture"]
        rc.gather_stop_reason = self._saved["gather_stop_reason"]
        rc.gather_perception = self._saved["gather_perception"]
        return False

    # -- convenience ----------------------------------------------------
    def turn(self, utterance: str) -> None:
        self.rc.handle_turn([], utterance)

    @property
    def said(self) -> str:
        return " ".join(self.spoken)


def _rc():
    import rabbit_converse
    return rabbit_converse


# ---------------------------------------------------------------------------
# Channel A — conversation still works
# ---------------------------------------------------------------------------

def test_hello_produces_a_spoken_reply_and_no_mick_call() -> None:
    """The headline claim: a greeting still gets a spoken answer, and the
    motion door is never even contacted."""
    rc = _rc()
    reply = '{"say": "Hello! All nominal here.", "directive": null}'
    with Harness(rc, reply) as h:
        h.turn("hello")
        assert h.spoken == ["Hello! All nominal here."], h.spoken
        assert h.mick_posts == [], f"a greeting reached the motion door: {h.mick_posts}"
        assert len(h.ollama_posts) == 1, "the greeting must reach the conversation model"


def test_how_are_you_produces_a_spoken_reply_and_no_mick_call() -> None:
    rc = _rc()
    reply = '{"say": "Running well — governor is nominal.", "directive": null}'
    with Harness(rc, reply) as h:
        h.turn("how are you")
        assert h.spoken == ["Running well — governor is nominal."], h.spoken
        assert h.mick_posts == []


def test_what_do_you_see_uses_the_readonly_grounding_and_speaks() -> None:
    """A perception question takes the READ-ONLY grounding path (gather_perception
    is folded into the prompt) and produces speech — never motion."""
    rc = _rc()
    assert rc.perception_relevant("what do you see"), \
        "the perception grab must be armed for this utterance"
    reply = '{"say": "Nearest object is about two meters ahead.", "directive": null}'
    with Harness(rc, reply) as h:
        h.turn("what do you see")
        assert h.spoken == ["Nearest object is about two meters ahead."], h.spoken
        assert h.mick_posts == [], "a perception question must not reach the motion door"
        # The read-only telemetry really reached the model.
        sent = json.dumps(h.ollama_posts[0])
        assert "nearest object 2.0 m ahead" in sent, \
            "the read-only perception grounding must be in the prompt"


def test_no_mick_request_is_made_for_a_null_directive() -> None:
    """Stated as its own invariant over a spread of conversational turns:
    `directive: null` means the /intent door is not called AT ALL."""
    rc = _rc()
    for utterance in ("hello", "hey rabbit", "how are you", "what do you see",
                      "thanks", "what's our status"):
        with Harness(rc, '{"say": "Sure thing.", "directive": null}') as h:
            h.turn(utterance)
            assert h.spoken, f"{utterance!r} produced no speech"
            assert h.mick_posts == [], f"{utterance!r} contacted mick"


# ---------------------------------------------------------------------------
# Channel B — motion still routes, exactly once
# ---------------------------------------------------------------------------

def test_drive_forward_sends_exactly_one_directive_to_mick() -> None:
    rc = _rc()
    reply = ('{"say": "Creeping forward a meter.", '
             '"directive": "drive forward one meter"}')
    with Harness(rc, reply) as h:
        h.turn("drive forward one meter")
        assert len(h.mick_posts) == 1, f"expected exactly one door call: {h.mick_posts}"
        assert h.mick_posts[0] == {"text": "drive forward one meter"}, h.mick_posts[0]
        assert h.spoken == ["Creeping forward a meter."], h.spoken


def test_wake_prefix_plus_command_in_one_transcript_still_routes() -> None:
    """A single-turn transcript that carries the wake words AND a command:
    Rabbit extracts the movement request, and what reaches mick is the
    directive — which mick's fence does not catch (it is not a bare wake
    phrase). Supported end to end."""
    rc = _rc()
    reply = ('{"say": "On our way.", "directive": "drive forward one meter"}')
    with Harness(rc, reply) as h:
        h.turn("hello rabbit, drive forward one meter")
        assert len(h.mick_posts) == 1, h.mick_posts
        assert h.mick_posts[0]["text"] == "drive forward one meter"
        assert h.spoken == ["On our way."]


# ---------------------------------------------------------------------------
# 🔴 The regression this file exists for: no false success acknowledgement
# ---------------------------------------------------------------------------

def test_offer_to_door_rejects_a_200_that_carries_no_intent() -> None:
    """mick's non-motion 200 is NOT a successful drive.

    Before the fence, `{"ok":true,...}` always carried an intent, so keying on
    `ok` alone was sound. The fence added a second 200 shape that moves
    nothing; reading it as success would make Rabbit announce motion that never
    happens — and would ADVANCE a mission past a step that never ran."""
    rc = _rc()
    with Harness(rc, "unused", mick_body=MICK_NON_MOTION):
        assert rc.offer_to_door("hello rabbit") == "reject"
    with Harness(rc, "unused", mick_body='{"ok":true}'):
        assert rc.offer_to_door("hello rabbit") == "reject", \
            "an absent `intent` field must not read as success either"
    # Positive control: a real accepted intent is still 'ok'.
    with Harness(rc, "unused", mick_body=MICK_ACCEPTED):
        assert rc.offer_to_door("drive forward one meter") == "ok"


def test_a_fenced_directive_never_claims_the_robot_is_moving() -> None:
    """End to end at the spoken layer. If the router ever emits a directive
    that mick's fence absorbs, Rabbit must NOT say the on-our-way line."""
    rc = _rc()
    reply = '{"say": "On our way — heading off now.", "directive": "hello rabbit"}'
    with Harness(rc, reply, mick_body=MICK_NON_MOTION) as h:
        h.turn("hello rabbit")
        assert len(h.mick_posts) == 1, "the directive was offered to the door"
        said = h.said.lower()
        assert "on our way" not in said, \
            f"false success acknowledgement — nothing moved but Rabbit said: {h.said!r}"
        assert said, "Rabbit must still say something"
        # The truthful arm: it asks the operator to rephrase.
        assert "another way" in said or "couldn't" in said, h.said


def test_mission_does_not_advance_on_a_no_intent_reply() -> None:
    """The second consumer of offer_to_door's verdict. 'ok' would ADVANCE the
    Executive past a step that never moved; 'reject' halts, fail-closed."""
    import mission
    assert mission._decide("ok", 0, 2) == "advance"
    assert mission._decide("reject", 0, 2) == "halt", \
        "a door that produced no intent must halt the mission, never advance it"


# ---------------------------------------------------------------------------
# The Rabbit ↔ mick boundary, made explicit
# ---------------------------------------------------------------------------

def test_the_same_words_are_conversation_to_rabbit_and_nothing_to_mick() -> None:
    """The separation, in one test.

    "hello rabbit" sent DIRECTLY to mick yields no intent — that is the fence,
    and it is correct: mick is the motion door. The same conversational turn
    passed through Rabbit produces a spoken reply and never touches mick. Two
    channels, one utterance, no overlap."""
    rc = _rc()

    # Rabbit's side: speech, no door call.
    with Harness(rc, '{"say": "Hello there!", "directive": null}') as h:
        h.turn("hello rabbit")
        assert h.spoken == ["Hello there!"]
        assert h.mick_posts == []

    # mick's side, at the wire: the fenced body carries no intent, so Rabbit's
    # door helper reports no motion. (The fence itself is proven in Rust —
    # crates/kirra-sidecars/src/mick_fence.rs + tests/mick_non_motion_endpoint.rs;
    # here we pin how Rabbit READS that reply.)
    with Harness(rc, "unused", mick_body=MICK_NON_MOTION):
        assert rc.offer_to_door("hello rabbit") == "reject"


# ---------------------------------------------------------------------------
# No authority regression
# ---------------------------------------------------------------------------

def test_a_conversational_turn_makes_no_actuation_adjacent_call() -> None:
    """The single-door invariant, as an assertion. A chat turn's only outbound
    call is the Ollama chat POST; nothing reaches /intent."""
    rc = _rc()
    with Harness(rc, '{"say": "All good.", "directive": null}') as h:
        h.turn("how are you")
        assert h.mick_posts == []
        assert len(h.ollama_posts) == 1
        # Every outbound POST went to Ollama — the harness raises on any other
        # host, so reaching here means no third endpoint was contacted.


def test_offer_to_door_is_the_only_intent_poster_in_the_rabbit_path() -> None:
    """Source-level: exactly ONE `/intent` POST exists in the conversation
    path, and it is offer_to_door's. A new route would show up here."""
    here = Path(__file__).resolve().parent
    posters = []
    for name in ("rabbit_converse.py", "rabbit_ask.py", "rabbit_watch.py",
                 "rabbit_boot.py", "rabbit_diag.py", "rabbit_wake.py",
                 "rabbit_ota.py", "skill_registry.py", "mission.py",
                 "world_model.py", "rabbit_persona.py", "barge_in.py",
                 "rabbit_stream.py", "turn_state.py"):
        for i, line in enumerate((here / name).read_text().splitlines(), 1):
            code = line.split("#", 1)[0]
            if "/intent" in code and ".post(" in code:
                posters.append(f"{name}:{i}")
    assert len(posters) == 1 and posters[0].startswith("rabbit_converse.py:"), \
        f"expected exactly one /intent poster (offer_to_door), found {posters}"


def test_tts_is_a_pure_sink() -> None:
    """speak() prints and pipes to a TTS process. It cannot publish a ROS goal,
    write the motor serial, or mint a release token — asserted over the source
    so a future edit that adds one fails here."""
    src = (Path(__file__).resolve().parent / "rabbit_persona.py").read_text()
    body = src[src.index("def speak("):]
    for forbidden in ("requests", "/intent", "rclpy", "publish", "serial",
                      "ReleaseToken", "signing", "Twist"):
        assert forbidden not in body, \
            f"speak() must stay a pure sink; found {forbidden!r}"


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items())
             if k.startswith("test_") and callable(v)]
    print("rabbit_converse tests:")
    failed = 0
    for t in tests:
        try:
            t()
            print(f"  ok   {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"  FAIL {t.__name__}: {e}")
    print(f"\n{len(tests) - failed}/{len(tests)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(_run_all())
