#!/usr/bin/env python3
"""Host tests for the playback self-echo suppression (voice_playback.py +
its wake_word.py / voice_turn_router.py / mick_voice_chat.py wiring).

The live defect: the robot heard its own Piper playback, follow-up mode
fired "speech onset" triggers on it, and the pipeline conversed with itself
(≥15 turns until Ctrl-C). These tests pin the whole suppression chain
without audio, PulseAudio, Ollama, or the robot:

  1. the lock-state layer (no owner → listening; held → suppressed; release
     → cooldown; crash → auto-release; single owner; bad paths fail safe);
  2. the pure trigger invariant (no trigger during playback/cooldown; the
     follow-up episode cap with no unlimited mode);
  3. the SELF-ECHO REGRESSION: a spoken reply whose exact "audio activity"
     re-enters the pipeline produces NO second turn, NO second HTTP request,
     and a later real user turn still works;
  4. every spoken path of the router holds the guard (streamed conversation,
     motion ack, 422/429/unreachable/malformed error lines, ambiguity
     clarification) and the guard spans ALL sentences of a streamed reply;
  5. barge-in stays un-wired on this path; logs stay content-free.

Runs standalone (`python3 robot/voice_playback_test.py`, exit 1 on failure);
importable under pytest.
"""
from __future__ import annotations

import io
import json
import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import voice_playback as vp  # noqa: E402
import wake_word  # noqa: E402
import voice_turn_router as router  # noqa: E402

_F: list[str] = []


def check(cond, msg):
    if not cond:
        _F.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


def tmp_state() -> str:
    fd, p = tempfile.mkstemp(prefix="kirra-vp-test-")
    os.close(fd)
    return p


# --- 1. lock-state layer ------------------------------------------------------

def test_no_owner_means_listening_allowed():
    p = tmp_state()
    try:
        check(vp.playback_active(p) is False, "no owner → not active")
        check(vp.playback_active(p + ".absent") is False,
              "absent file → not active (fail-open for listening)")
    finally:
        os.unlink(p)


def test_guard_holds_and_releases_with_cooldown_inside():
    p = tmp_state()
    try:
        t0 = time.monotonic()
        with vp.PlaybackGuard(p, cooldown_ms=150):
            check(vp.playback_active(p) is True, "guard held → active")
        held = time.monotonic() - t0
        check(vp.playback_active(p) is False, "guard exited → inactive")
        check(held >= 0.15,
              f"cooldown must be held INSIDE the lock (held {held:.3f}s)")
    finally:
        os.unlink(p)


def test_guard_is_reentrant_single_lock():
    p = tmp_state()
    try:
        with vp.PlaybackGuard(p, cooldown_ms=0):
            with vp.PlaybackGuard(p, cooldown_ms=0):   # the router wraps run_turn
                check(vp.playback_active(p) is True, "nested guard still held")
            check(vp.playback_active(p) is True,
                  "inner exit must NOT release the outer guard")
        check(vp.playback_active(p) is False, "outermost exit releases")
    finally:
        os.unlink(p)


def test_crashed_owner_releases_automatically():
    p = tmp_state()
    code = (
        "import sys, os, time; sys.path.insert(0, %r); import voice_playback as vp\n"
        "g = vp.PlaybackGuard(%r, cooldown_ms=0); g.__enter__()\n"
        "print('HELD', flush=True); time.sleep(30)\n" % (str(HERE), p)
    )
    proc = subprocess.Popen([sys.executable, "-c", code],
                            stdout=subprocess.PIPE, text=True)
    try:
        line = proc.stdout.readline()
        check(line.strip() == "HELD", "child must report the lock held")
        check(vp.playback_active(p) is True, "child holds → active")
        proc.send_signal(signal.SIGKILL)          # hard crash, no cleanup code
        proc.wait(timeout=10)
        deadline = time.monotonic() + 5.0
        while vp.playback_active(p) and time.monotonic() < deadline:
            time.sleep(0.05)
        check(vp.playback_active(p) is False,
              "a SIGKILLed owner must release the lock (kernel flock) — "
              "suppression can never become stale")
    finally:
        if proc.poll() is None:
            proc.kill()
        os.unlink(p)


def test_two_owners_cannot_hold_simultaneously():
    p = tmp_state()
    code = (
        "import sys, time; sys.path.insert(0, %r); import voice_playback as vp\n"
        "with vp.PlaybackGuard(%r, cooldown_ms=0):\n"
        "    print('HELD', flush=True); time.sleep(3)\n" % (str(HERE), p)
    )
    proc = subprocess.Popen([sys.executable, "-c", code],
                            stdout=subprocess.PIPE, text=True)
    try:
        check(proc.stdout.readline().strip() == "HELD", "child holds first")
        import fcntl
        fd = os.open(p, os.O_RDWR)
        try:
            got = True
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                got = False
            check(got is False, "a second owner must NOT acquire while held")
        finally:
            os.close(fd)
    finally:
        proc.wait(timeout=10)
        os.unlink(p)


def test_bad_paths_fail_safe_both_sides():
    # Distinct paths for the two halves: the warn-once dedup is keyed by
    # path, and each side must be seen to log its OWN actionable line.
    bad_probe = "/proc/definitely/not/writable/kirra-vp-probe"
    bad_guard = "/proc/definitely/not/writable/kirra-vp-guard"
    err, real = io.StringIO(), sys.stderr
    sys.stderr = err
    try:
        check(vp.playback_active(bad_probe) is False,
              "unusable probe path → listening allowed (fail-open), not a crash")
        with vp.PlaybackGuard(bad_guard, cooldown_ms=0):
            pass   # speaking must proceed even when the lock cannot be held
    finally:
        sys.stderr = real
    logs = err.getvalue()
    check("KIRRA_VOICE_PLAYBACK_STATE" in logs,
          f"a bad path must be logged actionably (got: {logs!r})")
    check(bad_guard in logs,
          "the guard's log must name the unusable path it could not hold")


def test_state_path_resolution_prefers_runtime_dir():
    old_env = os.environ.get("KIRRA_VOICE_PLAYBACK_STATE")
    old_xdg = os.environ.get("XDG_RUNTIME_DIR")
    try:
        os.environ["KIRRA_VOICE_PLAYBACK_STATE"] = "/x/y"
        check(vp.state_path() == "/x/y", "env override wins")
        del os.environ["KIRRA_VOICE_PLAYBACK_STATE"]
        with tempfile.TemporaryDirectory() as d:
            os.environ["XDG_RUNTIME_DIR"] = d
            check(vp.state_path() == os.path.join(d, vp.DEFAULT_STATE_BASENAME),
                  "XDG_RUNTIME_DIR is the user-unit default")
        os.environ["XDG_RUNTIME_DIR"] = "/does/not/exist"
        check(vp.state_path().startswith(("/dev/shm", "/tmp")),
              "missing runtime dir → tmpfs fallback, never an error")
    finally:
        for k, v in (("KIRRA_VOICE_PLAYBACK_STATE", old_env),
                     ("XDG_RUNTIME_DIR", old_xdg)):
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def test_config_validators_are_bounded():
    check(vp.parse_cooldown_ms(None) == 500, "cooldown default 500")
    check(vp.parse_cooldown_ms("0") == 0 and vp.parse_cooldown_ms("3000") == 3000,
          "cooldown bounds inclusive")
    for bad in ("-1", "3001", "junk", ""):
        check(vp.parse_cooldown_ms(bad) == 500, f"invalid cooldown {bad!r} → default")
    check(vp.parse_max_followups(None) == 3, "follow-up cap default 3")
    check(vp.parse_max_followups("0") == 0 and vp.parse_max_followups("10") == 10,
          "cap bounds inclusive (0 = wake required every turn)")
    for bad in ("-1", "11", "unlimited", "1e9", ""):
        check(vp.parse_max_followups(bad) == 3,
              f"invalid cap {bad!r} → default — NO unlimited mode exists")


# --- 2. the pure trigger invariant --------------------------------------------

def test_no_trigger_during_playback_or_past_cap():
    for followups in (0, 1, 2, 99):
        check(vp.should_emit_trigger(playback=True, episode_followups=followups,
                                     max_followups=3) is False,
              "playback (lock held, cooldown inside) must veto every trigger")
    check(vp.should_emit_trigger(playback=False, episode_followups=0,
                                 max_followups=3) is True, "idle → allowed")
    check(vp.should_emit_trigger(playback=False, episode_followups=2,
                                 max_followups=3) is True, "under cap → allowed")
    check(vp.should_emit_trigger(playback=False, episode_followups=3,
                                 max_followups=3) is False,
          "at cap → wake required")
    check(vp.should_emit_trigger(playback=False, episode_followups=0,
                                 max_followups=0) is False,
          "cap 0 → every turn needs an explicit wake")


def test_wake_source_wires_the_invariant():
    src = (HERE / "wake_word.py").read_text()
    check("voice_playback.playback_active" in src,
          "wake_word must probe the playback lock")
    check("should_emit_trigger" in src,
          "wake_word must gate emission through the pure invariant")
    check("follow-up cap reached; wake required" in src,
          "the cap transition is logged once, actionably")
    check("playback suppression engaged" in src
          and "playback suppression released" in src,
          "suppression transitions are logged (once, not per frame)")
    check(src.count("episode_followups += 1") == 1,
          "only a REAL emitted follow-up increments the episode count — "
          "playback can never consume turns")


# --- 3. the self-echo regression (load-bearing) -------------------------------

class ChatStub(BaseHTTPRequestHandler):
    requests = 0

    def do_POST(self):  # noqa: N802
        ChatStub.requests += 1
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for ev, data in (("delta", {"text": "I "}),
                         ("sentence", {"text": "I am very well thanks."}),
                         ("done", {"ttft_ms": 3, "total_ms": 6})):
            self.wfile.write(f"event: {ev}\ndata: {json.dumps(data)}\n\n".encode())

    def log_message(self, *_):
        pass


def test_self_echo_cannot_create_a_second_turn():
    """user trigger → router speaks the reply → the reply's own 'audio
    activity' hits the wake listener → NO second trigger, NO second chat
    request; after release+cooldown a real user turn proceeds."""
    srv = HTTPServer(("127.0.0.1", 0), ChatStub)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    state = tmp_state()
    os.environ["KIRRA_MICK_CHAT_URL"] = f"http://127.0.0.1:{srv.server_port}"
    os.environ["KIRRA_VOICE_PLAYBACK_STATE"] = state
    # A "TTS" that emits playback for a while: the reply audio window during
    # which the fake microphone below observes the robot's own voice.
    tts_cmd = f"{sys.executable} -c \"import sys,time; sys.stdin.read(); time.sleep(0.4)\""
    ChatStub.requests = 0
    emitted_triggers = []

    def fake_mic_hears_the_reply():
        """The wake listener's follow-up loop, modeled with the REAL gates:
        while the reply is audible (guard held) it keeps observing loud
        frames; the invariant must refuse to fire."""
        deadline = time.monotonic() + 5.0
        heard_playback = False
        while time.monotonic() < deadline:
            if vp.playback_active(state):
                heard_playback = True   # 'loud frame' — the robot's own voice
                if vp.should_emit_trigger(playback=True, episode_followups=0,
                                          max_followups=3):
                    emitted_triggers.append("during-playback")
            elif heard_playback:
                return True             # playback + cooldown over
            time.sleep(0.02)
        return heard_playback

    listener = threading.Thread(target=fake_mic_hears_the_reply, daemon=True)
    listener.start()
    try:
        out, err = io.StringIO(), io.StringIO()
        real_out, real_err = sys.stdout, sys.stderr
        sys.stdout, sys.stderr = out, err
        try:
            router.handle_turn("how are you", 1, tts_cmd)
        finally:
            sys.stdout, sys.stderr = real_out, real_err
        listener.join(timeout=6.0)
        check(ChatStub.requests == 1,
              f"exactly ONE chat request for one user turn, got {ChatStub.requests}")
        check(emitted_triggers == [],
              f"NO trigger may fire from the robot's own reply: {emitted_triggers}")
        check(vp.playback_active(state) is False,
              "after the turn, suppression is fully released")
        # A REAL user turn afterwards still works.
        check(vp.should_emit_trigger(playback=vp.playback_active(state),
                                     episode_followups=1, max_followups=3),
              "a later real follow-up (under cap, no playback) is allowed")
        router.handle_turn("tell me a joke", 2, None)
        check(ChatStub.requests == 2, "the later real turn reaches chat once")
    finally:
        os.environ.pop("KIRRA_MICK_CHAT_URL", None)
        os.environ.pop("KIRRA_VOICE_PLAYBACK_STATE", None)
        srv.shutdown()
        os.unlink(state)


def test_streamed_multi_sentence_reply_holds_one_continuous_guard():
    """The guard must span sentence 1 → sentence N → drain → cooldown with no
    release between sentences (a gap would re-admit the listener mid-reply)."""
    state = tmp_state()
    os.environ["KIRRA_VOICE_PLAYBACK_STATE"] = state
    gaps = []
    stop = threading.Event()

    def watch_for_gaps():
        seen_active = False
        while not stop.is_set():
            active = vp.playback_active(state)
            if active:
                seen_active = True
            elif seen_active and not stop.is_set():
                gaps.append(time.monotonic())
                return
            time.sleep(0.01)

    try:
        import mick_voice_chat as mvc
        watcher = threading.Thread(target=watch_for_gaps, daemon=True)
        watcher.start()
        with vp.PlaybackGuard(state, cooldown_ms=100):
            tts = mvc.TtsPipe(
                f"{sys.executable} -c \"import sys,time; sys.stdin.read(); time.sleep(0.1)\"")
            for sentence in ("First sentence.", "Second one.", "Third."):
                tts.say(sentence)
                time.sleep(0.05)
            tts.close()
        stop.set()
        watcher.join(timeout=2.0)
        check(gaps == [], "the guard released BETWEEN sentences — must hold "
                          "through the whole streamed reply + drain + cooldown")
        check(vp.playback_active(state) is False, "released at the end")
    finally:
        stop.set()
        os.environ.pop("KIRRA_VOICE_PLAYBACK_STATE", None)
        os.unlink(state)


# --- 4. every spoken router path is guarded -----------------------------------

class IntentStub(BaseHTTPRequestHandler):
    mode = "accept"

    def do_POST(self):  # noqa: N802
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if IntentStub.mode == "accept":
            body, code = {"ok": True, "seq": 1, "at_ms": 1,
                          "intent": {"intent": "hold"}}, 200
        elif IntentStub.mode == "422":
            body, code = {"ok": False, "error": "MICK_JSON_PARSE_ERROR"}, 422
        elif IntentStub.mode == "429":
            body, code = {"ok": False, "error": "MICK_RATE_LIMITED"}, 429
        else:
            body, code = {"ok": True, "intent": None}, 200
        payload = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_):
        pass


def _spoken_under_guard(fn) -> bool:
    """Run fn (which speaks via a slow fake TTS) and report whether the
    playback lock was observed held at any point."""
    state = tmp_state()
    os.environ["KIRRA_VOICE_PLAYBACK_STATE"] = state
    observed = threading.Event()
    stop = threading.Event()

    def watch():
        while not stop.is_set():
            if vp.playback_active(state):
                observed.set()
            time.sleep(0.01)

    t = threading.Thread(target=watch, daemon=True)
    t.start()
    try:
        fn(f"{sys.executable} -c \"import sys,time; sys.stdin.read(); time.sleep(0.15)\"")
    finally:
        stop.set()
        t.join(timeout=2.0)
        os.environ.pop("KIRRA_VOICE_PLAYBACK_STATE", None)
        os.unlink(state)
    return observed.is_set()


def test_every_spoken_router_path_holds_the_guard():
    srv = HTTPServer(("127.0.0.1", 0), IntentStub)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    os.environ["KIRRA_MICK_INTENT_URL"] = f"http://127.0.0.1:{srv.server_port}"
    out, real = io.StringIO(), sys.stdout
    sys.stdout = out
    try:
        cases = [
            ("motion ack", "accept", "stop"),
            ("motion 422", "422", "stop"),
            ("motion 429", "429", "stop"),
            ("route disagreement", "nonmotion", "stop"),
            ("ambiguity clarification", "accept", "go ahead"),
        ]
        for label, mode, text in cases:
            IntentStub.mode = mode
            held = _spoken_under_guard(
                lambda tts, t=text: router.handle_turn(t, 1, tts))
            check(held, f"{label}: spoken line must run under the playback guard")
        # Unreachable intent service (no stub): the outage line is guarded too.
        os.environ["KIRRA_MICK_INTENT_URL"] = "http://127.0.0.1:1"
        held = _spoken_under_guard(lambda tts: router.handle_turn("pull over", 1, tts))
        check(held, "intent-unreachable error line must run under the guard")
    finally:
        sys.stdout = real
        os.environ.pop("KIRRA_MICK_INTENT_URL", None)
        srv.shutdown()


# --- 5. barge-in fence + content-free logs ------------------------------------

def test_barge_in_stays_unwired_on_this_path():
    join = lambda a, b: a + b  # noqa: E731
    needle = join("barge", "_in")
    for name in ("voice_turn_router.py", "voice_route.py", "mick_voice_chat.py",
                 "voice_playback.py", "mick_voice_router.sh"):
        src = (HERE / name).read_text()
        code = "\n".join(line.split("#")[0] for line in src.splitlines())
        check(needle not in code,
              f"{name}: barge-in must stay DISABLED on the hybrid path until "
              "AEC exists — RMS alone cannot tell operator from speaker")


def test_suppression_logs_never_carry_content():
    state = tmp_state()
    os.environ["KIRRA_VOICE_PLAYBACK_STATE"] = state
    marker = "walrus kumquat trireme"
    err, real = io.StringIO(), sys.stderr
    out, real_out = io.StringIO(), sys.stdout
    sys.stderr, sys.stdout = err, out
    try:
        router.speak_line(f"reply mentioning {marker}", None)   # print path
        with vp.PlaybackGuard(state, cooldown_ms=0):
            pass
    finally:
        sys.stderr, sys.stdout = real, real_out
        os.environ.pop("KIRRA_VOICE_PLAYBACK_STATE", None)
        os.unlink(state)
    check(marker not in err.getvalue(),
          "stderr logs must never carry reply content")


# --- standalone runner (house pattern) ----------------------------------------

def main() -> int:
    t0 = time.monotonic()
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"  ok  {name}")
    if _F:
        print(f"voice_playback_test: {len(_F)} FAILURE(S)")
        return 1
    print(f"voice_playback_test: ALL OK ({time.monotonic() - t0:.1f}s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
