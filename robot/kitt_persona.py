#!/usr/bin/env python3
"""kitt_persona.py — shared, dependency-free KITT persona helpers.

The one home for (a) how KITT addresses the operator (the `{name}` slot) and
(b) how it speaks (`speak`). Deliberately stdlib-only so EVERY KITT script can
import it — including the lean, requests-free ones (kitt_ota.py).

🔴 CHANNEL A ONLY (docs/hardware/KITT_CONVERSATION_DESIGN.md, docs/kitt/KITT_VOICE_LINES.md).
   These helpers personalize and render SPEECH. They carry ZERO actuation
   authority: knowing the operator's name never authorizes a command, and
   `speak` only prints / pipes text to a TTS process. The KIRRA checker bounds
   every motion identically whether the operator is known, unknown, or misheard.
"""
from __future__ import annotations

import os
import subprocess
import sys


def operator_name() -> str:
    """The current operator's name for direct address, or '' if unknown.

    Priority (highest first):
      1. the operator RECOGNIZER feed (face/voice) — slots in here later; it is
         a read-only grounding source with NO actuation authority, and
      2. KIRRA_KITT_OPERATOR (a configured default name), else
      3. '' (unknown → KITT stays polite, just nameless).
    """
    # (recognizer feed reads in here first once it lands — Channel A, read-only.)
    return os.environ.get("KIRRA_KITT_OPERATOR", "").strip()


def name_slot() -> str:
    """Render the `{name}` slot used throughout docs/kitt/KITT_VOICE_LINES.md:
    ', Justin' when the operator is known, '' when not — so a line reads
    naturally either way ("Good morning{name}." → "Good morning, Justin." | "Good morning.")."""
    n = operator_name()
    return f", {n}" if n else ""


# ── LLM model pin (stealth-update guard) ─────────────────────────────────────
# A "no version bump" model update (same Ollama tag, different weights) must not
# pass silently. The smoketest records the digest it VETTED here; boot compares
# the RUNNING digest against it and warns on a mismatch. Pure/stdlib so the
# comparison is unit-testable without a network — the digest FETCH lives in
# kitt_model_smoketest.py (it needs requests + a live Ollama).
KITT_MODEL_PIN_ENV = "KIRRA_KITT_MODEL_PIN_FILE"


def model_pin_path() -> str:
    """Where the vetted `<model>\\t<digest>` pins live (per-user, override via env)."""
    p = os.environ.get(KITT_MODEL_PIN_ENV, "").strip()
    return p or os.path.expanduser("~/.kirra_kitt_model.pin")


def _pin_clean(s: "str | None") -> str:
    """Strip tabs/newlines so a field can't corrupt the TSV record."""
    return (s or "").replace("\t", " ").replace("\n", " ").strip()


def _load_pins(path: str) -> dict:
    """model → (digest, vetted_at, note). Back-compat: legacy 2-field lines
    (`model\\tdigest`) read with empty vetted_at/note."""
    rows = {}
    try:
        with open(path) as f:
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if len(parts) >= 2 and parts[0]:
                    rows[parts[0]] = (
                        parts[1],
                        parts[2] if len(parts) >= 3 else "",
                        parts[3] if len(parts) >= 4 else "",
                    )
    except OSError:
        pass
    return rows


def read_model_pin_record(model: str, path: "str | None" = None):
    """(digest, vetted_at, note) for `model`, or None if unpinned — the
    reproducibility trail: WHICH weights, verified WHEN, and a provenance note."""
    return _load_pins(path or model_pin_path()).get(model)


def read_model_pin(model: str, path: "str | None" = None) -> "str | None":
    """The vetted digest for `model`, or None if unpinned/unreadable."""
    rec = read_model_pin_record(model, path)
    return (rec[0] or None) if rec else None


def write_model_pin(model: str, digest: str, path: "str | None" = None,
                    vetted_at: str = "", note: str = "") -> None:
    """Record `model`→(digest, vetted_at, note) as vetted (merges with any other
    pinned models; preserves their fields). `vetted_at` is an ISO timestamp the
    caller supplies (kept out of here so the write stays clock-free/testable)."""
    path = path or model_pin_path()
    rows = _load_pins(path)
    rows[model] = (digest, _pin_clean(vetted_at), _pin_clean(note))
    parent = os.path.dirname(path)
    if parent:
        os.makedirs(parent, exist_ok=True)
    with open(path, "w") as f:
        for k in sorted(rows):
            d, v, n = rows[k]
            f.write(f"{k}\t{d}\t{v}\t{n}\n")


def classify_model_pin(running: "str | None", pinned: "str | None") -> str:
    """Pure decision: 'unavailable' (no running digest) / 'unpinned' (never
    vetted) / 'ok' (matches) / 'changed' (stealth update — same tag, new weights)."""
    if running is None:
        return "unavailable"
    if pinned is None:
        return "unpinned"
    return "ok" if running == pinned else "changed"


def speak(text: str) -> None:
    """Print the line and, if KIRRA_TTS_CMD is set, pipe it to that TTS process.
    Fail-soft: a TTS failure never crashes the caller (speech is cosmetic)."""
    print(text)
    tts = os.environ.get("KIRRA_TTS_CMD", "").strip()
    if not tts:
        return
    try:
        subprocess.run(tts.split(), input=text.encode(), check=False)
    except Exception as e:  # noqa: BLE001 — speech is never load-bearing
        print(f"(tts failed: {e})", file=sys.stderr)
