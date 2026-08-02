#!/usr/bin/env python3
"""The configured model residency actually reaches Ollama — on EVERY path.

`KIRRA_RABBIT_KEEP_ALIVE` is the biggest per-turn latency lever on the Orin:
without it Ollama unloads phi3:3.8b after ~5 minutes idle and the next turn pays
a multi-second cold reload. On a dedicated robot the recommended value is `-1`
(pin indefinitely).

But a config value that never reaches the wire buys nothing, and the failure is
invisible — everything still works, just slowly, and only after an idle gap. So
this pins the plumbing: the value must be parsed correctly (`-1` is an INTEGER,
not the string "-1", which Ollama would reject) and must appear as `keep_alive`
in every Rabbit request body, including the streaming router path that is easy
to forget when adding one.

Residency only: `keep_alive` cannot change what the model says, so it cannot
affect the {say, directive} routing decision or anything downstream of the fence.

Stubs `requests` when it is absent (the CI robot lane) so this RUNS there rather
than skipping — an unexercised latency guard is not a guard.
"""
from __future__ import annotations

import importlib
import json
import os
import sys
import types
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))


def _install_requests_stub() -> None:
    def _refuse(*_a, **_k):
        raise AssertionError("the test made a REAL HTTP call — a seam is unstubbed")

    stub = types.ModuleType("requests")
    stub.get = _refuse
    stub.post = _refuse
    stub.RequestException = type("RequestException", (Exception,), {})
    stub.exceptions = types.SimpleNamespace(RequestException=stub.RequestException)
    sys.modules["requests"] = stub


try:
    import requests  # noqa: F401
except ImportError:
    _install_requests_stub()

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


class _Resp:
    def __init__(self, body, status=200):
        self.status_code = status
        self._body = body
        self.content = body.encode()

    def json(self):
        return json.loads(self._body)

    # the streaming path uses the response as a context manager + iter_lines
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def iter_lines(self):
        yield json.dumps({"message": {"content": self._body}, "done": True}).encode()


def _reload_with(keep_alive):
    """Reload rabbit_ask + rabbit_converse under a given KIRRA_RABBIT_KEEP_ALIVE.
    Both read it at import time, so a reload is how the value is applied."""
    prev = os.environ.get("KIRRA_RABBIT_KEEP_ALIVE")
    if keep_alive is None:
        os.environ.pop("KIRRA_RABBIT_KEEP_ALIVE", None)
    else:
        os.environ["KIRRA_RABBIT_KEEP_ALIVE"] = keep_alive
    import rabbit_ask
    import rabbit_converse
    importlib.reload(rabbit_ask)
    importlib.reload(rabbit_converse)
    return rabbit_ask, rabbit_converse, prev


def _restore(prev):
    if prev is None:
        os.environ.pop("KIRRA_RABBIT_KEEP_ALIVE", None)
    else:
        os.environ["KIRRA_RABBIT_KEEP_ALIVE"] = prev
    import rabbit_ask
    import rabbit_converse
    importlib.reload(rabbit_ask)
    importlib.reload(rabbit_converse)


# ── parsing ──────────────────────────────────────────────────────────────────

def test_minus_one_parses_as_an_integer_not_a_string():
    """`-1` must go on the wire as the JSON number -1. Ollama accepts a duration
    STRING ("30m") or a NUMBER of seconds; the string "-1" is neither, and the
    request would be rejected — silently costing exactly the residency this is
    meant to buy."""
    ask, _, prev = _reload_with("-1")
    try:
        check(ask.KEEP_ALIVE == -1, f"expected int -1, got {ask.KEEP_ALIVE!r}")
        check(isinstance(ask.KEEP_ALIVE, int), f"must be an int, got {type(ask.KEEP_ALIVE)}")
        check(json.dumps({"keep_alive": ask.KEEP_ALIVE}) == '{"keep_alive": -1}',
              "must serialize as a JSON number")
    finally:
        _restore(prev)


def test_a_duration_string_is_preserved_verbatim():
    for value in ("30m", "5m", "1h"):
        ask, _, prev = _reload_with(value)
        try:
            check(ask.KEEP_ALIVE == value, f"{value!r} must pass through, got {ask.KEEP_ALIVE!r}")
        finally:
            _restore(prev)


def test_the_default_is_a_finite_hold_not_pinned():
    """No global `-1`: pinning a model forever is a per-robot memory decision the
    operator opts into, not something a fresh checkout does on its own."""
    ask, _, prev = _reload_with(None)
    try:
        check(ask.KEEP_ALIVE == "30m", f"default changed to {ask.KEEP_ALIVE!r}")
    finally:
        _restore(prev)


def test_zero_unloads_immediately():
    ask, _, prev = _reload_with("0")
    try:
        check(ask.KEEP_ALIVE == 0, f"expected int 0, got {ask.KEEP_ALIVE!r}")
    finally:
        _restore(prev)


# ── it reaches the wire, on every path ───────────────────────────────────────

def _capture_bodies(rc, call):
    """Run `call` with rc.requests.post recording every request body."""
    bodies = []

    def fake_post(url, **kw):
        bodies.append((url, kw.get("json")))
        return _Resp('{"say": "ok", "directive": null}')

    saved = rc.requests.post
    rc.requests.post = fake_post
    try:
        call()
    finally:
        rc.requests.post = saved
    return bodies


def test_the_normal_router_path_sends_keep_alive():
    ask, rc, prev = _reload_with("-1")
    try:
        ctx = "TELEMETRY: posture NOMINAL"
        bodies = _capture_bodies(rc, lambda: rc.ask_llm([], ctx, "how are you"))
        check(len(bodies) == 1, f"expected one Ollama call, got {len(bodies)}")
        url, body = bodies[0]
        check("/api/chat" in url, f"unexpected endpoint: {url}")
        check(body.get("keep_alive") == -1,
              f"keep_alive missing/wrong on the normal path: {body}")
    finally:
        _restore(prev)


def test_the_streaming_router_path_sends_keep_alive():
    """The path most likely to drift: it builds its own request body. A missing
    keep_alive here would leave streaming users on the cold-reload stall while
    everyone else was fine."""
    ask, rc, prev = _reload_with("-1")
    try:
        msgs = rc._stream_messages([], "TELEMETRY: posture NOMINAL", "how are you")
        bodies = _capture_bodies(
            rc, lambda: rc.route_stream(msgs, lambda _c: None, lambda: False))
        check(len(bodies) == 1, f"expected one Ollama call, got {len(bodies)}")
        url, body = bodies[0]
        check(body.get("stream") is True, f"this must be the STREAMING path: {body}")
        check(body.get("keep_alive") == -1,
              f"keep_alive missing/wrong on the streaming path: {body}")
    finally:
        _restore(prev)


def test_every_ollama_caller_in_the_rabbit_path_sends_keep_alive():
    """Source-level sweep, so a NEW Ollama call added later cannot quietly skip
    residency: every `/api/chat` POST must carry keep_alive."""
    here = Path(__file__).resolve().parent
    missing = []
    for name in ("rabbit_converse.py", "rabbit_ask.py"):
        text = (here / name).read_text()
        for i, chunk in enumerate(text.split("/api/chat")[1:], 1):
            window = chunk[:400]
            if "keep_alive" not in window:
                missing.append(f"{name} call #{i}")
    check(not missing, f"Ollama calls without keep_alive: {missing}")


def test_the_model_is_unchanged():
    """Residency work must not become a model swap. phi3:3.8b is the vetted
    router model (rabbit_model_smoketest.py gates it).

    This constant is a RATCHET, not a preference: it exists so a latency or
    residency change cannot quietly swap the doer's weights. Updating it is
    therefore a deliberate act that must be paid for with a smoketest run —
    phi3:3.8b passed the doer contract 8/8 and its digest is pinned. Do not
    edit this line to make a failing test pass; re-vet the candidate first."""
    ask, _, prev = _reload_with("-1")
    try:
        check(ask.MODEL == "phi3:3.8b", f"model default changed to {ask.MODEL!r}")
    finally:
        _restore(prev)


def test_keep_alive_is_not_a_model_option():
    """It must be a top-level request field, never folded into `options` —
    `options` is sampling behaviour, and putting a residency hint there would
    both fail to work and muddy the "residency cannot change output" claim."""
    _, rc, prev = _reload_with("-1")
    try:
        check("keep_alive" not in rc.ROUTER_LLM_OPTIONS,
              f"keep_alive leaked into sampling options: {rc.ROUTER_LLM_OPTIONS}")
    finally:
        _restore(prev)


# ── the documented recommendation matches the code ───────────────────────────

def test_the_example_env_documents_both_choices():
    example = (Path(__file__).resolve().parent / "install" / "rabbit.env.example").read_text()
    check("KIRRA_RABBIT_KEEP_ALIVE=-1" in example,
          "the dedicated-robot recommendation (-1) must be documented")
    check("KIRRA_RABBIT_KEEP_ALIVE=30m" in example,
          "the shared-host choice (30m) must be documented")
    for phrase in ("dedicated", "shared", "memory"):
        check(phrase.lower() in example.lower(),
              f"the tradeoff must be explicit — {phrase!r} not mentioned")
    # And it must stay OPT-IN: no uncommented -1 that a fresh copy would inherit.
    live = [ln for ln in example.splitlines()
            if ln.strip().startswith("KIRRA_RABBIT_KEEP_ALIVE")]
    check(live == [], f"keep-alive must stay commented out in the example: {live}")


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items())
             if k.startswith("test_") and callable(v)]
    print("rabbit_keepalive_test:")
    for t in tests:
        before = len(_FAILURES)
        t()
        print(f"  {'ok  ' if len(_FAILURES) == before else 'FAIL'} {t.__name__}")
    if _FAILURES:
        print(f"\n{len(_FAILURES)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"\nall {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(_run_all())
