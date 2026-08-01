#!/usr/bin/env python3
"""mick_chat.py — terminal client for the Mick CONVERSATIONAL sidecar.

Talk to Mick without going through rabbit_converse.py and WITHOUT touching the
motion-only endpoint on port 8102:

    python3 robot/mick_chat.py
    You: hello
    Mick: Hello. How can I help?

One line per turn; Ctrl-D (or Ctrl-C) exits. Speaks ONLY to the loopback
`POST /chat` endpoint of mick_chat_service (default http://127.0.0.1:8103,
override with KIRRA_MICK_CHAT_URL).

🔴 SEPARATION, deliberately structural in this client too:
  * /chat is conversation only — the service has NO motion authority, no
    latched state, and a motion-shaped model reply is replaced server-side
    with a routing explanation.
  * This client has NO fallback to the motion endpoint: its path never
    appears anywhere in this file (host-tested), so a chat failure can
    never be "retried" against the motion door.
  * No shell execution, no transcript persistence — turns live and die in
    the terminal scrollback.

stdlib only (urllib): no requests dependency for a bring-up tool.
"""
from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request

DEFAULT_URL = "http://127.0.0.1:8103"
# Last turn's server-reported timing (numbers only; module-level for display).
LAST_TIMING: dict = {}
# Bounded: a local 4B model can take a while on a busy Orin, but a hung
# service must hand the terminal back.
REQUEST_TIMEOUT_S = 90.0
SESSION_ID = "terminal"


def chat_url() -> str:
    return (os.environ.get("KIRRA_MICK_CHAT_URL") or DEFAULT_URL).rstrip("/")


def post_chat(base_url: str, text: str, timeout_s: float = REQUEST_TIMEOUT_S):
    """POST one turn to /chat. Returns (reply, None) or (None, error-string).
    Pure I/O seam — every failure is a readable one-liner, never a traceback."""
    body = json.dumps({"text": text, "session_id": SESSION_ID}).encode("utf-8")
    req = urllib.request.Request(
        base_url + "/chat", data=body,
        headers={"Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            payload = resp.read()
    except urllib.error.HTTPError as e:
        # The service's error tokens are the useful part — surface them.
        try:
            detail = json.loads(e.read().decode("utf-8", "replace")).get("error", "")
        except Exception:  # noqa: BLE001 — a broken error body is still an HTTP error
            detail = ""
        return None, f"HTTP {e.code}{f' ({detail})' if detail else ''}"
    except urllib.error.URLError as e:
        return None, (f"cannot reach {base_url} ({e.reason}) — is "
                      "mick_chat_service running? (GET /health to check)")
    except TimeoutError:
        return None, f"timed out after {timeout_s:.0f}s (model busy or hung)"
    try:
        parsed = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None, "malformed response from the chat service"
    if parsed.get("ok") is not True or not isinstance(parsed.get("reply"), str):
        return None, f"unexpected response shape: {sorted(parsed.keys())}"
    LAST_TIMING.clear()
    if isinstance(parsed.get("timing"), dict):
        LAST_TIMING.update(parsed["timing"])
    return parsed["reply"], None


def main() -> int:
    base = chat_url()
    print(f"mick_chat: conversation only (POST {base}/chat) — motion requests "
          "go through the governed door on the motion sidecar, not here. "
          "Ctrl-D quits.",
          file=sys.stderr)
    while True:
        try:
            line = input("You: ")
        except (EOFError, KeyboardInterrupt):
            print(file=sys.stderr)
            return 0
        text = line.strip()
        if not text:
            continue
        reply, err = post_chat(base, text)
        if err is not None:
            print(f"mick_chat: {err}", file=sys.stderr)
            continue
        print(f"Mick: {reply}")
        # Server-reported stage timings (numbers only, no content).
        if isinstance(LAST_TIMING.get("ttft_ms"), int):
            print(
                "mick_chat: timing "
                f"ttft={LAST_TIMING.get('ttft_ms')}ms "
                f"total={LAST_TIMING.get('total_ms')}ms",
                file=sys.stderr,
            )


if __name__ == "__main__":
    sys.exit(main())
