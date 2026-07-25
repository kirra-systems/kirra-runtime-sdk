#!/usr/bin/env python3
"""rabbit_stream.py — first-clause streaming of the router's spoken reply to TTS.

The single biggest perceived-latency win for a voice loop is to start SPEAKING
the first clause of the answer while the model is still generating the rest,
instead of waiting for the whole reply. rabbit_converse's router emits a JSON
object, though — you cannot naively split a `{"say": "…"}` token stream on
punctuation and speak it, or you'd read the JSON syntax aloud.

This module makes streaming SAFE for that router by consuming a **directive-first**
JSON stream — `{"directive": <null|"…">, "say": "…"}` — and:

  * resolving the ROUTING decision (`directive`) FIRST, before any speech. A DRIVE
    turn (non-null directive) emits NO clauses at all — the caller runs the exact
    existing door/verdict path so the door-result-dependent line ("On our way" vs
    "couldn't pin down a safe destination") is never pre-empted. Only a CHAT turn
    (directive null) streams its `say` aloud.
  * emitting `say` as complete, JSON-UNESCAPED clauses (split on . ! ?) the instant
    each is available — never the JSON punctuation, quotes, or the trailing
    `,"directive":…` structure.

FAIL-SAFE BY CONSTRUCTION: if the stream is not directive-first, is malformed, or
is ambiguous in any way, the parser "gives up" and emits nothing — the caller then
falls back to the normal, non-streaming `parse_reply(raw_buffer)` and speaks the
reply exactly as it does today. Streaming is therefore a best-effort accelerant
that can only ever match or beat the current behaviour, never change routing.

Pure + stdlib only (no HTTP, no LLM) so the parsing is fully host-tested
(`robot/rabbit_stream_test.py`). The live streaming HTTP call lives in
rabbit_converse and feeds chunks in here.
"""
from __future__ import annotations

import json
import re

# A COMPLETE directive field: `"directive": null` or `"directive": "<escaped>"`.
# Anchored to a complete value (full quoted string or the null literal), so a
# match means the routing decision is fully known — never a truncated one.
_DIRECTIVE_RE = re.compile(r'"directive"\s*:\s*(null|"(?:[^"\\]|\\.)*")', re.IGNORECASE)
_SAY_START_RE = re.compile(r'"say"\s*:\s*"')
# Sentence boundary: one-or-more terminators followed by whitespace (so "3.5" or
# "e.g." mid-number/abbrev without a following space is not split prematurely).
_CLAUSE_RE = re.compile(r"[.!?]+(?=\s)")


def split_ready_clauses(text: str):
    """(complete_clauses, remainder). A clause ends at .!? followed by whitespace;
    the trailing unfinished fragment is returned separately."""
    clauses, last = [], 0
    for m in _CLAUSE_RE.finditer(text):
        end = m.end()
        clause = text[last:end].strip()
        if clause:
            clauses.append(clause)
        last = end
    return clauses, text[last:].strip()


def _safe_unescape(escaped: str) -> str:
    """JSON-unescape a possibly-truncated string body. Trims a dangling backslash
    or an incomplete \\uXXXX so json.loads never chokes on a mid-escape cut."""
    # Drop a trailing odd run of backslashes (an incomplete escape).
    i = len(escaped)
    while i > 0 and escaped[i - 1] == "\\":
        i -= 1
    if (len(escaped) - i) % 2 == 1:
        escaped = escaped[:-1]
    escaped = re.sub(r"\\u[0-9a-fA-F]{0,3}$", "", escaped)
    try:
        return json.loads('"' + escaped + '"')
    except Exception:  # noqa: BLE001 — a still-bad fragment just yields nothing yet
        return ""


def _find_string_end(s: str):
    """Index of the first UNESCAPED closing quote in a JSON string body, or None."""
    esc = False
    for i, ch in enumerate(s):
        if esc:
            esc = False
        elif ch == "\\":
            esc = True
        elif ch == '"':
            return i
    return None


class StreamingSayParser:
    """Feed router-JSON chunks in; get speakable clauses out. See module docstring.

    Lifecycle: `feed(chunk)` per streamed chunk (returns clauses to speak NOW),
    then `finish()` once the stream ends (returns the final plan + any trailing
    clause). `raw` is the full accumulated text for the caller's fallback parse.
    """

    def __init__(self) -> None:
        self.raw = ""
        self.phase = "seek"          # seek -> chat | drive ; giveup is terminal
        self.directive = None
        self._giveup = False
        self._say_from = None        # index in `raw` where the say VALUE begins
        self._say_closed = False
        self._say_text = ""          # unescaped say-so-far
        self._clauses_spoken = 0

    # -- resolution ---------------------------------------------------------

    def _resolve_route(self) -> None:
        """In `seek`: decide chat vs drive from the directive-first field. If a
        `say` value opens before any directive resolves, the stream is NOT
        directive-first → give up (fall back to non-streaming)."""
        m = _DIRECTIVE_RE.search(self.raw)
        say_m = _SAY_START_RE.search(self.raw)
        if m and (not say_m or m.start() < say_m.start()):
            tok = m.group(1)
            if tok.lower() == "null":
                self.phase = "chat"
            else:
                try:
                    self.directive = json.loads(tok).strip() or None
                except Exception:  # noqa: BLE001
                    self.directive = None
                self.phase = "chat" if self.directive is None else "drive"
        elif say_m:
            # say arrived first (or without a resolvable directive) → not
            # directive-first; cannot stream safely. Fall back.
            self._giveup = True

    def _pump_say(self):
        """In `chat`: emit any newly-complete say clauses."""
        if self._say_from is None:
            m = _SAY_START_RE.search(self.raw)
            if not m:
                return []
            self._say_from = m.end()
        body = self.raw[self._say_from:]
        end = _find_string_end(body)
        if end is None:
            usable = body                      # still open — unescape the safe prefix
        else:
            usable, self._say_closed = body[:end], True
        self._say_text = _safe_unescape(usable)
        clauses, _ = split_ready_clauses(self._say_text)
        new = clauses[self._clauses_spoken:]
        self._clauses_spoken = len(clauses)
        return new

    # -- public API ---------------------------------------------------------

    def feed(self, chunk: str):
        self.raw += chunk
        if self._giveup:
            return []
        if self.phase == "seek":
            self._resolve_route()
        if self.phase == "chat" and not self._giveup:
            return self._pump_say()
        return []

    def finish(self):
        """End the stream. Returns a plan dict:
          streamed:  True iff we handled it as a streamed CHAT turn (say voiced)
          route:     'chat' | 'drive' | 'unknown'
          directive: the resolved directive (drive turns), else None
          trailing:  final clause(s) the caller must still speak (chat turns)
          raw:       full accumulated text (the caller's fallback-parse source)
        """
        if self.phase == "seek" and not self._giveup:
            self._resolve_route()          # a very short reply may only resolve now
        trailing = []
        if self.phase == "chat" and not self._giveup:
            trailing = self._pump_say()
            if self._say_text:
                _, remainder = split_ready_clauses(self._say_text)
                if remainder:
                    trailing = trailing + [remainder]
        streamed = self.phase == "chat" and not self._giveup and self._say_from is not None
        route = self.phase if self.phase in ("chat", "drive") else "unknown"
        return {"streamed": streamed, "route": route, "directive": self.directive,
                "trailing": trailing, "raw": self.raw}
