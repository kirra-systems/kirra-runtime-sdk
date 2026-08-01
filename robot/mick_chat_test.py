#!/usr/bin/env python3
"""Host tests for robot/mick_chat.py — the terminal client for the Mick
conversational sidecar. A stub HTTP server stands in for mick_chat_service:
no Ollama, no robot, no network beyond loopback.

Covers: a successful reply; a malformed response body; a timeout; 4xx/5xx
with the service's error token surfaced; EOF exit of the interactive loop;
and the separation proofs — the stub records every path so we can assert
/chat is the ONLY endpoint ever contacted, and the client source never
mentions the motion endpoint at all (no fallback to /intent can exist).

Runs standalone (`python3 robot/mick_chat_test.py`, exit 1 on any failure);
also importable under pytest.
"""
from __future__ import annotations

import io
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mick_chat  # noqa: E402

SEEN_PATHS: list[str] = []


class StubHandler(BaseHTTPRequestHandler):
    """Scriptable /chat endpoint. The class attribute `mode` selects the
    behavior; every request's path is recorded for the separation proof."""

    mode = "ok"

    def do_POST(self):  # noqa: N802 — http.server API
        SEEN_PATHS.append(self.path)
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length))
        assert "text" in body, "client must send the text field"
        if self.mode == "ok":
            reply = {"ok": True, "reply": f"echo: {body['text']}"}
            payload = json.dumps(reply).encode()
            self.send_response(200)
        elif self.mode == "garbage":
            payload = b"not json at all"
            self.send_response(200)
        elif self.mode == "error503":
            payload = json.dumps({"ok": False, "error": "MICK_CHAT_MODEL_ERROR"}).encode()
            self.send_response(503)
        elif self.mode == "error422":
            payload = json.dumps({"ok": False, "error": "MICK_CHAT_TEXT_TOO_LONG"}).encode()
            self.send_response(422)
        elif self.mode == "slow":
            time.sleep(2.0)
            payload = json.dumps({"ok": True, "reply": "late"}).encode()
            self.send_response(200)
        else:  # pragma: no cover - test misconfiguration
            raise AssertionError(f"unknown mode {self.mode}")
        try:
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        except BrokenPipeError:
            pass  # the timed-out client hung up — expected in the slow mode

    def log_message(self, *_args):  # silence the stub
        pass


def _serve() -> tuple[HTTPServer, str]:
    srv = HTTPServer(("127.0.0.1", 0), StubHandler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://127.0.0.1:{srv.server_port}"


def test_successful_reply_round_trips() -> None:
    srv, url = _serve()
    try:
        StubHandler.mode = "ok"
        reply, err = mick_chat.post_chat(url, "hello")
        assert err is None and reply == "echo: hello"
    finally:
        srv.shutdown()


def test_malformed_response_is_a_readable_error() -> None:
    srv, url = _serve()
    try:
        StubHandler.mode = "garbage"
        reply, err = mick_chat.post_chat(url, "hello")
        assert reply is None and "malformed" in err
    finally:
        srv.shutdown()


def test_http_errors_surface_the_service_token() -> None:
    srv, url = _serve()
    try:
        StubHandler.mode = "error503"
        reply, err = mick_chat.post_chat(url, "hello")
        assert reply is None and "503" in err and "MICK_CHAT_MODEL_ERROR" in err
        StubHandler.mode = "error422"
        reply, err = mick_chat.post_chat(url, "x" * 5000)
        assert reply is None and "422" in err and "MICK_CHAT_TEXT_TOO_LONG" in err
    finally:
        srv.shutdown()


def test_timeout_is_bounded_and_readable() -> None:
    srv, url = _serve()
    try:
        StubHandler.mode = "slow"
        reply, err = mick_chat.post_chat(url, "hello", timeout_s=0.3)
        assert reply is None and "timed out" in err
    finally:
        srv.shutdown()


def test_unreachable_service_names_the_fix() -> None:
    reply, err = mick_chat.post_chat("http://127.0.0.1:1", "hello")
    assert reply is None and "mick_chat_service" in err


def test_eof_exits_the_loop_cleanly() -> None:
    srv, url = _serve()
    try:
        StubHandler.mode = "ok"
        real_stdin, real_stdout, real_stderr = sys.stdin, sys.stdout, sys.stderr
        sys.stdin = io.StringIO("hello\n")  # one turn, then EOF
        sys.stdout = io.StringIO()
        sys.stderr = io.StringIO()
        import os
        os.environ["KIRRA_MICK_CHAT_URL"] = url
        try:
            rc = mick_chat.main()
            out = sys.stdout.getvalue()
        finally:
            sys.stdin, sys.stdout, sys.stderr = real_stdin, real_stdout, real_stderr
            os.environ.pop("KIRRA_MICK_CHAT_URL", None)
        assert "Mick: echo: hello" in out, f"reply not printed: {out!r}"
        assert rc == 0, "EOF must exit 0"
    finally:
        srv.shutdown()


def test_only_the_chat_endpoint_is_ever_contacted() -> None:
    # The separation proof, from the server's perspective — SELF-CONTAINED
    # (no reliance on test order): one known call, then assert the stub saw
    # exactly /chat and nothing else.
    srv, url = _serve()
    try:
        StubHandler.mode = "ok"
        SEEN_PATHS.clear()
        reply, err = mick_chat.post_chat(url, "separation probe")
        assert err is None and reply == "echo: separation probe"
        assert SEEN_PATHS == ["/chat"], f"unexpected endpoints contacted: {SEEN_PATHS}"
    finally:
        srv.shutdown()


def test_client_source_has_no_motion_endpoint_fallback() -> None:
    # And from the source's perspective: the motion endpoint is not merely
    # un-contacted, it is UNREACHABLE — the client never names it.
    src = Path(mick_chat.__file__).read_text()
    code = "\n".join(line.split("#")[0] for line in src.splitlines())
    needle = "/int" + "ent"  # built by concat so this test cannot match itself
    assert needle not in code, "client code references the motion endpoint"
    for forbidden in ("subprocess", "os.system"):
        assert forbidden not in code, f"client must not use {forbidden}"
    # No transcript persistence: a BARE open( is a file access (urlopen is
    # the network call and is fine).
    import re
    assert not re.search(r"(?<![\w.])open\(", code), "client must not open files"


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
    print("mick_chat_test:", "ALL OK" if failures == 0 else f"{failures} FAILURE(S)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(_run_all())
