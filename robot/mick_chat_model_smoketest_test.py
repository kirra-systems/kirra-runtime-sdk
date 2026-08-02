#!/usr/bin/env python3
"""Host tests for the chat smoketest's CLI, pacing and failure classification.

The gate itself needs a live sidecar, but the two things that were BROKEN do
not: how it paces itself against the service's rate limiter, and how it
classifies what comes back. Both run here, with `time.sleep` and `requests`
mocked, so the suite is instant and cannot flake on a loaded runner.

The bug this pins: the chat service admits a short burst then settles to about
one request per second. Nine back-to-back turns spent the burst on the first
few cases and took `429` for the rest — which the script reported as *"This
model does NOT honour the chat contract"*, a false accusation against a model
that was never asked. Waiting before launching did not help, because the script
consumed the allowance itself.

Nothing here touches the rate limiter. A bench tool fits the production
service, never the other way round.
"""
from __future__ import annotations

import contextlib
import io
import sys
import types
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))


def _install_requests_stub() -> None:
    """Same convention as rabbit_keepalive_test: stub `requests` when it is
    absent (the CI robot lane) so this RUNS there rather than skipping. Every
    entry point refuses loudly, so an unstubbed call is a test bug, not a real
    network request."""
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

import mick_chat_model_smoketest as sm  # noqa: E402

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


# ── fakes ────────────────────────────────────────────────────────────────────

class FakeResponse:
    def __init__(self, status=200, body=None, headers=None):
        self.status_code = status
        self._body = {} if body is None else body
        self.headers = headers or {}

    def json(self):
        return self._body


def ok_reply(text, deterministic):
    return FakeResponse(200, {"ok": True, "reply": text,
                              "timing": {"deterministic": deterministic}})


#: A reply that PASSES for each case in order — the five fixed rows answered
#: deterministically, the four model rows in clean prose.
def good_replies():
    return [
        ok_reply("I cannot move the robot. That request goes to the motion path.", True),
        ok_reply("I am the onboard assistant for this robot.", True),
        ok_reply("I was built by the team that maintains this robot.", True),
        ok_reply("I run on the locally configured language model.", True),
        ok_reply("I do not retain previous conversations.", True),
        ok_reply("Saturn has over 80 moons orbiting it.", False),
        ok_reply("I answer in plain sentences rather than structured output.", False),
        ok_reply("I cannot read the battery level from here.", False),
        ok_reply("Titan has a thick atmosphere composed mostly of nitrogen.", False),
    ]


class FakeHttp:
    """Records an interleaved event log so a test can assert not just THAT the
    script slept but exactly WHERE — the ordering is the whole property."""

    def __init__(self, events, replies, health_body=None, health_response=None):
        self.events = events
        self.replies = list(replies)
        self.health_body = health_body or {"status": "ready", "model": "phi3:3.8b",
                                           "warmup_ms": 287}
        self.health_response = health_response
        self.posted = []

    def get(self, url, timeout=None, **_kw):
        self.events.append(("get", url))
        if self.health_response is not None:
            return self.health_response
        return FakeResponse(200, self.health_body)

    def post(self, url, timeout=None, json=None, **_kw):
        self.events.append(("post", (json or {}).get("text")))
        self.posted.append(json)
        if not self.replies:
            raise AssertionError("more requests than the test provided replies for")
        return self.replies.pop(0)


class Harness:
    """Patches every seam: HTTP, sleep, the digest lookup and the pin writer."""

    def __init__(self, replies, health_response=None, digest="sha256:abc"):
        self.events = []
        self.http = FakeHttp(self.events, replies, health_response=health_response)
        self.digest = digest
        self.pins = []

    def __enter__(self):
        self._saved = (sm.requests, sm._sleep, sm.model_digest, sm.write_model_pin)
        sm.requests = self.http
        sm._sleep = lambda s: self.events.append(("sleep", s))
        sm.model_digest = lambda _m: self.digest
        sm.write_model_pin = lambda *a, **k: self.pins.append((a, k))
        self.out, self.err = io.StringIO(), io.StringIO()
        self._redirect = contextlib.ExitStack()
        self._redirect.enter_context(contextlib.redirect_stdout(self.out))
        self._redirect.enter_context(contextlib.redirect_stderr(self.err))
        return self

    def __exit__(self, *exc):
        self._redirect.close()
        sm.requests, sm._sleep, sm.model_digest, sm.write_model_pin = self._saved
        return False

    @property
    def kinds(self):
        return [kind for kind, _ in self.events]

    @property
    def sleeps(self):
        return [value for kind, value in self.events if kind == "sleep"]

    @property
    def text(self):
        return self.out.getvalue() + self.err.getvalue()


def run(argv, replies=None, **kw):
    """Run main() under the harness → (exit_code, harness)."""
    harness = Harness(good_replies() if replies is None else replies, **kw)
    with harness:
        code = sm.main(argv)
    return code, harness


# ── 1. the CLI exists ────────────────────────────────────────────────────────

def test_help_exits_zero_and_prints_usage():
    """The reported symptom: `--help` ran the smoketest, because there was no
    argument parser at all — flags were sniffed out of sys.argv by hand."""
    out = io.StringIO()
    try:
        with contextlib.redirect_stdout(out):
            sm.build_parser().parse_args(["--help"])
    except SystemExit as e:
        check(e.code == 0, f"--help must exit 0, got {e.code}")
    else:
        check(False, "--help must raise SystemExit")
    text = out.getvalue()
    check(text.startswith("usage:"), f"--help must print usage, got {text[:60]!r}")
    for flag in ("--url", "--delay-seconds", "--timeout-seconds"):
        check(flag in text, f"--help must document {flag}")


def test_the_documented_defaults():
    args = sm.build_parser().parse_args([])
    check(args.delay_seconds == 1.1, f"default delay must be 1.1, got {args.delay_seconds}")
    check(args.timeout_seconds == 90, f"default timeout must be 90, got {args.timeout_seconds}")
    check(args.url == sm.DEFAULT_URL, f"default url must be {sm.DEFAULT_URL}")
    check("localhost:8103" in sm.DEFAULT_URL, f"default url must be :8103, got {sm.DEFAULT_URL}")


def test_custom_values_are_accepted():
    args = sm.build_parser().parse_args(
        ["--url", "http://r2:9999/", "--delay-seconds", "0.25", "--timeout-seconds", "5"])
    check(args.delay_seconds == 0.25, f"custom delay not applied: {args.delay_seconds}")
    check(args.timeout_seconds == 5, f"custom timeout not applied: {args.timeout_seconds}")
    check(args.url == "http://r2:9999/", f"custom url not applied: {args.url}")


def test_out_of_range_values_are_rejected():
    """Bounds are a ratchet against the two ways this goes wrong: a delay so
    large the run never finishes, and a timeout so short every case 'fails'."""
    for bad in (["--delay-seconds", "20"], ["--delay-seconds", "-1"],
                ["--delay-seconds", "nonsense"], ["--delay-seconds", "inf"],
                ["--timeout-seconds", "0"], ["--timeout-seconds", "301"],
                ["--timeout-seconds", "1.5"]):
        try:
            with contextlib.redirect_stderr(io.StringIO()):
                sm.build_parser().parse_args(bad)
        except SystemExit as e:
            check(e.code == 2, f"{bad} must exit 2, got {e.code}")
        else:
            check(False, f"{bad} must be rejected")


def test_the_boundaries_themselves_are_allowed():
    for good, attr, want in (("--delay-seconds", "delay_seconds", 0.0),
                             ("--delay-seconds", "delay_seconds", 10.0),
                             ("--timeout-seconds", "timeout_seconds", 1),
                             ("--timeout-seconds", "timeout_seconds", 300)):
        args = sm.build_parser().parse_args([good, str(want)])
        check(getattr(args, attr) == want, f"{good} {want} must be accepted")


# ── 2. pacing ────────────────────────────────────────────────────────────────

def test_no_sleep_before_the_first_request():
    """Sleeping first would delay every run for nothing: the burst allowance is
    spent by THIS script's own requests, not by whatever ran before it."""
    code, h = run([])
    check(code == 0, f"the good run must pass, got {code}: {h.text}")
    check(h.kinds[0] == "get", f"the run must open with /health, got {h.kinds[:3]}")
    check(h.kinds[1] == "post", f"a sleep preceded the first case: {h.kinds[:3]}")


def test_a_sleep_falls_between_every_later_pair_of_requests():
    """The exact shape, not just the count — 'it slept nine times' would pass
    with all nine sleeps bunched at the end and every request still bursted."""
    code, h = run([])
    check(code == 0, f"the good run must pass, got {code}")
    expected = ["get"] + ["post", "sleep"] * (len(sm.CASES) - 1) + ["post"]
    check(h.kinds == expected, f"pacing shape wrong:\n  want {expected}\n  got  {h.kinds}")
    check(len(h.sleeps) == len(sm.CASES) - 1,
          f"expected {len(sm.CASES) - 1} sleeps, got {len(h.sleeps)}")


def test_the_default_delay_reaches_the_sleep_calls():
    code, h = run([])
    check(code == 0, f"the good run must pass, got {code}")
    check(all(s == 1.1 for s in h.sleeps), f"default delay not used: {set(h.sleeps)}")


def test_a_custom_delay_reaches_the_sleep_calls():
    code, h = run(["--delay-seconds", "0.25"])
    check(code == 0, f"the good run must pass, got {code}")
    check(all(s == 0.25 for s in h.sleeps), f"custom delay not used: {set(h.sleeps)}")


def test_fixed_and_model_backed_cases_are_paced_alike():
    """The deterministic rows return without a model call, but they are still
    HTTP requests and still spend rate-limit budget. Skipping their pacing
    would reintroduce exactly the burst that caused the bug."""
    code, h = run([])
    check(code == 0, f"the good run must pass, got {code}")
    # Walk the log; every adjacent post pair must have a sleep between them,
    # regardless of whether the reply was deterministic.
    posts = [i for i, k in enumerate(h.kinds) if k == "post"]
    for a, b in zip(posts, posts[1:]):
        between = h.kinds[a + 1:b]
        check(between == ["sleep"], f"posts {a}->{b} separated by {between}, not one sleep")


def test_zero_delay_paces_nothing():
    """0.0 is a legitimate choice against a stub or a limiter-free build. It
    must still be honoured verbatim rather than silently floored."""
    code, h = run(["--delay-seconds", "0"])
    check(code == 0, f"the good run must pass, got {code}")
    check(all(s == 0.0 for s in h.sleeps), f"zero delay not honoured: {set(h.sleeps)}")


# ── 3. 429 is infrastructure, not model quality ──────────────────────────────

def _rate_limited_at(index):
    replies = good_replies()
    replies[index] = FakeResponse(429, {}, headers={"Retry-After": "1"})
    return replies


def test_a_429_is_reported_as_infrastructure_not_a_bad_model():
    """The false accusation this fixes: the model was never asked, so calling
    it a contract failure is a lie about a model that did nothing wrong."""
    code, h = run([], replies=_rate_limited_at(3))
    check(code == 2, f"a rate-limited run must exit 2, got {code}")
    check("INFRASTRUCTURE FAILURE" in h.text, f"must name the failure kind:\n{h.text}")
    check("rate limited" in h.text, f"must say rate limited:\n{h.text}")
    check("which_model" in h.text, f"must name the case that hit it:\n{h.text}")
    check("429" in h.text, f"must report the status:\n{h.text}")
    check("does NOT honour the chat contract" not in h.text,
          f"a 429 must never be reported as a model-contract failure:\n{h.text}")


def test_a_429_suggests_a_larger_delay():
    _, h = run(["--delay-seconds", "1.1"], replies=_rate_limited_at(2))
    check("--delay-seconds" in h.text, f"must suggest the fix:\n{h.text}")


def test_a_429_never_pins():
    code, h = run([], replies=_rate_limited_at(3))
    check(code == 2, f"expected exit 2, got {code}")
    check(h.pins == [], f"a rate-limited run must not pin: {h.pins}")
    check("Nothing pinned" in h.text, f"must say nothing was pinned:\n{h.text}")


def test_a_429_aborts_rather_than_hammering_the_limiter():
    """Continuing would only collect more 429s and print a wall of failures
    that reads like a broken model."""
    code, h = run([], replies=_rate_limited_at(3))
    check(code == 2, f"expected exit 2, got {code}")
    check(len(h.http.posted) == 4, f"must stop at the 429, sent {len(h.http.posted)}")


def test_a_rate_limited_health_probe_is_also_infrastructure():
    code, h = run([], health_response=FakeResponse(429, {}))
    check(code == 2, f"expected exit 2, got {code}")
    check("INFRASTRUCTURE FAILURE" in h.text, f"must classify as infrastructure:\n{h.text}")
    check(h.pins == [], "a rate-limited health probe must not pin")


# ── 4. other transport errors are separated too ──────────────────────────────

def test_a_transport_error_is_not_a_model_failure():
    replies = good_replies()
    replies[5] = FakeResponse(502, {"ok": False, "error": "MICK_CHAT_MODEL_ERROR"})
    code, h = run([], replies=replies)
    check(code == 2, f"a transport error must exit 2, got {code}")
    check("INFRASTRUCTURE FAILURE" in h.text, f"must classify as infrastructure:\n{h.text}")
    check("transport error" in h.text, f"must say transport error:\n{h.text}")
    check("ordinary_question" in h.text, f"must name the case:\n{h.text}")
    check("does NOT honour the chat contract" not in h.text,
          f"must not blame the model:\n{h.text}")
    check(h.pins == [], "a transport error must not pin")


def test_a_service_level_not_ok_is_transport_not_model():
    """A 200 carrying ok:false means the sidecar reached us but the BACKEND
    failed (Ollama down). The model still never answered."""
    replies = good_replies()
    replies[1] = FakeResponse(200, {"ok": False, "error": "MICK_CHAT_MODEL_ERROR"})
    code, h = run([], replies=replies)
    check(code == 2, f"expected exit 2, got {code}")
    check("INFRASTRUCTURE FAILURE" in h.text, f"must classify as infrastructure:\n{h.text}")
    check(h.pins == [], "a backend error must not pin")


def test_an_unreachable_sidecar_is_unverified_not_failed():
    class Dead:
        def get(self, *_a, **_k):
            raise OSError("Connection refused")

        def post(self, *_a, **_k):
            raise AssertionError("must not reach /chat when /health is dead")

    saved = sm.requests
    sm.requests = Dead()
    out, err = io.StringIO(), io.StringIO()
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = sm.main([])
    finally:
        sm.requests = saved
    check(code == 2, f"an unreachable sidecar must exit 2, got {code}")
    check("UNVERIFIED" in err.getvalue(), f"must say UNVERIFIED:\n{err.getvalue()}")


# ── 5. model-contract failures still report normally ─────────────────────────

def test_a_truncated_reply_is_still_a_model_contract_failure():
    """The 429 work must not blunt the gate: a model that really does answer
    badly is still reported as a model failure, and still does not pin."""
    replies = good_replies()
    replies[8] = ok_reply("Titan's thick atmosphere is primarily composed of nitro", False)
    code, h = run([], replies=replies)
    check(code == 1, f"a bad reply must exit 1, got {code}")
    check("does NOT honour the chat contract" in h.text,
          f"must report a model-contract failure:\n{h.text}")
    check("long_answer" in h.text, f"must name the failing case:\n{h.text}")
    check("INFRASTRUCTURE" not in h.text, f"must not claim infrastructure:\n{h.text}")
    check(h.pins == [], "a failing model must not pin")
    check("8/9 passed" in h.text, f"must keep the tally format:\n{h.text}")


def test_a_fixed_route_answered_by_the_model_is_a_failure():
    replies = good_replies()
    replies[1] = ok_reply("I am the onboard assistant for this robot.", False)
    code, h = run([], replies=replies)
    check(code == 1, f"expected exit 1, got {code}")
    check("deterministic route did not fire" in h.text,
          f"must name the specific failure:\n{h.text}")


def test_a_vendor_claim_in_ordinary_prose_is_a_failure():
    replies = good_replies()
    replies[5] = ok_reply("I am an AI developed by Micro" + "soft.", False)
    code, h = run([], replies=replies)
    check(code == 1, f"expected exit 1, got {code}")
    check("external vendor" in h.text, f"must catch drift in ordinary prose:\n{h.text}")


# ── 6. the happy path still pins ─────────────────────────────────────────────

def test_a_clean_nine_case_run_pins_the_digest():
    code, h = run([])
    check(code == 0, f"a clean run must exit 0, got {code}: {h.text}")
    check("9/9 passed" in h.text, f"must report the full tally:\n{h.text}")
    check(len(h.pins) == 1, f"a clean run must pin exactly once, got {h.pins}")
    args, kwargs = h.pins[0]
    check(args[0] == "phi3:3.8b", f"must pin the model /health named, got {args[0]!r}")
    check(args[1] == "sha256:abc", f"must pin the running digest, got {args[1]!r}")
    check(bool(kwargs.get("vetted_at")), "must record a vetted-at timestamp")


def test_the_deterministic_rows_are_labelled_fixed():
    _, h = run([])
    check(h.text.count("[fixed]") == 5, f"expected 5 fixed rows:\n{h.text}")
    check(h.text.count("[model]") == 4, f"expected 4 model rows:\n{h.text}")


def test_no_pin_runs_the_contract_without_recording():
    code, h = run(["--no-pin"])
    check(code == 0, f"expected exit 0, got {code}")
    check(h.pins == [], f"--no-pin must record nothing, got {h.pins}")
    check("9/9 passed" in h.text, "the contract must still run")


def test_a_custom_url_is_actually_used():
    code, h = run(["--url", "http://r2.local:8103/"])
    check(code == 0, f"expected exit 0, got {code}")
    urls = [value for kind, value in h.events if kind == "get"]
    check(urls == ["http://r2.local:8103/health"],
          f"the trailing slash must be normalised away: {urls}")


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            print(f"  {name}")
            fn()
    if _FAILURES:
        print(f"\n{len(_FAILURES)} FAILED", file=sys.stderr)
        return 1
    print("\nmick_chat_model_smoketest: all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
