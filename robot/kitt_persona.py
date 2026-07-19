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
