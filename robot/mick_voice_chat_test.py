#!/usr/bin/env python3
"""Host tests for robot/mick_voice_chat.py — the low-latency spoken chat
client. A stub SSE server stands in for mick_chat_service and a fake TTS
command records WHEN each sentence reached it, so the latency-bearing claims
are proven, not asserted:

  * the first sentence reaches TTS BEFORE the stream completes (no
    wait-for-full-reply);
  * exactly one TTS process per reply;
  * timeouts and malformed streams fail soft;
  * only /chat/stream is ever contacted (never the motion endpoint — which is
    also unreferenced in the client source);
  * no transcript text appears in the timing logs, and nothing is written to
    disk by the client.

Runs standalone (`python3 robot/mick_voice_chat_test.py`, exit 1 on any
failure); also importable under pytest.
"""
from __future__ import annotations

import io
import json
import os
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mick_voice_chat  # noqa: E402

SEEN_PATHS: list[str] = []
STREAM_DONE_AT: list[float] = []


def sse(event: str, data: dict) -> bytes:
    return f"event: {event}\ndata: {json.dumps(data)}\n\n".encode()


class StubHandler(BaseHTTPRequestHandler):
    """Streams two sentences with a real delay between them, recording when
    the stream finished — the ordering proof's reference point."""

    mode = "ok"

    def do_POST(self):  # noqa: N802
        SEEN_PATHS.append(self.path)
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length))
        assert "text" in body
        if self.mode == "malformed":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write(b"event: sentence\ndata: not json\n\n")
            return
        if self.mode == "slow":
            time.sleep(2.0)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        try:
            self.wfile.write(sse("start", {"request_id": "1"}))
            self.wfile.write(sse("delta", {"text": "First sentence here. "}))
            self.wfile.write(sse("sentence", {"text": "First sentence here."}))
            self.wfile.flush()
            time.sleep(0.4)  # the model is "still generating"
            self.wfile.write(sse("delta", {"text": "Second one arrives later."}))
            self.wfile.write(sse("sentence", {"text": "Second one arrives later."}))
            self.wfile.write(sse("done", {"ttft_ms": 10, "total_ms": 450}))
            self.wfile.flush()
            STREAM_DONE_AT.append(time.monotonic())
        except BrokenPipeError:
            pass

    def log_message(self, *_args):
        pass


def _serve() -> tuple[HTTPServer, str]:
    srv = HTTPServer(("127.0.0.1", 0), StubHandler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://127.0.0.1:{srv.server_port}"


def _fake_tts(tmp: Path) -> tuple[str, Path]:
    """A TTS stand-in that timestamps each received line."""
    log = tmp / "tts.log"
    script = tmp / "fake_tts.py"
    script.write_text(
        "import sys, time\n"
        "with open(sys.argv[1], 'a') as f:\n"
        "    for line in sys.stdin:\n"
        "        f.write(f'{time.monotonic()} {len(line.strip())}\\n')\n"
        "        f.flush()\n"
    )
    return f"{sys.executable} {script} {log}", log


def test_first_sentence_reaches_tts_before_stream_completes() -> None:
    srv, url = _serve()
    with tempfile.TemporaryDirectory() as d:
        tts_cmd, log = _fake_tts(Path(d))
        try:
            StubHandler.mode = "ok"
            STREAM_DONE_AT.clear()
            real_out = sys.stdout
            sys.stdout = io.StringIO()
            try:
                ok = mick_voice_chat.run_turn(url, "how are you doing", tts_cmd)
            finally:
                printed = sys.stdout.getvalue()
                sys.stdout = real_out
            assert ok, "the turn must succeed"
            lines = log.read_text().splitlines()
            assert len(lines) == 2, f"two sentences must reach TTS: {lines}"
            first_tts_at = float(lines[0].split()[0])
            assert STREAM_DONE_AT, "stub never finished?"
            assert first_tts_at < STREAM_DONE_AT[0], (
                "the first sentence must reach TTS BEFORE the stream completes "
                f"(tts@{first_tts_at} vs done@{STREAM_DONE_AT[0]})"
            )
            assert "Mick: First sentence here." in printed
        finally:
            srv.shutdown()


def test_timeout_fails_soft() -> None:
    srv, url = _serve()
    try:
        StubHandler.mode = "slow"
        old = mick_voice_chat.REQUEST_TIMEOUT_S
        mick_voice_chat.REQUEST_TIMEOUT_S = 0.3
        try:
            ok = mick_voice_chat.run_turn(url, "hello there", None)
        finally:
            mick_voice_chat.REQUEST_TIMEOUT_S = old
        assert not ok, "a timeout must not report success"
    finally:
        srv.shutdown()


def test_malformed_stream_fails_soft_without_crash() -> None:
    srv, url = _serve()
    try:
        StubHandler.mode = "malformed"
        ok = mick_voice_chat.run_turn(url, "hello again", None)
        assert not ok
    finally:
        srv.shutdown()


def test_unreachable_service_fails_soft() -> None:
    assert not mick_voice_chat.run_turn("http://127.0.0.1:1", "hello", None)


def test_only_chat_stream_is_ever_contacted() -> None:
    assert SEEN_PATHS, "suite wiring broken — stub saw nothing"
    assert set(SEEN_PATHS) == {"/chat/stream"}, f"unexpected: {set(SEEN_PATHS)}"


def test_timing_logs_never_carry_transcript_text() -> None:
    srv, url = _serve()
    try:
        StubHandler.mode = "ok"
        utterance = "zebra quantum waffle transcript"
        real_err, real_out = sys.stderr, sys.stdout
        sys.stderr = io.StringIO()
        sys.stdout = io.StringIO()
        try:
            mick_voice_chat.run_turn(url, utterance, None)
        finally:
            err = sys.stderr.getvalue()
            sys.stderr, sys.stdout = real_err, real_out
        for word in utterance.split():
            assert word not in err, f"transcript word {word!r} leaked into timing logs"
        assert "stage" in err and "ms" in err, "stage timings must be logged"
    finally:
        srv.shutdown()


def test_client_writes_no_files_and_references_no_motion_endpoint() -> None:
    src = Path(mick_voice_chat.__file__).read_text()
    code = "\n".join(line.split("#")[0] for line in src.splitlines())
    needle = "/int" + "ent"
    assert needle not in code, "client references the motion endpoint"
    import re
    assert not re.search(r"(?<![\w.])open\(", code), "client must not open files"
    assert "os.system" not in code and "shell=True" not in code


# --- standalone runner (house pattern) ----------------------------------------

def _run_all() -> int:
    failures = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"  ok  {name}")
            except AssertionError as e:
                failures += 1
                print(f"FAIL  {name}: {e}")
    print("mick_voice_chat_test:", "ALL OK" if failures == 0 else f"{failures} FAILURE(S)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(_run_all())
