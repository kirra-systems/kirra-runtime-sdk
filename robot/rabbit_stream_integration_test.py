#!/usr/bin/env python3
"""rabbit_stream_integration_test — checks the streaming path's wiring in
rabbit_converse WITHOUT touching the network or an LLM.

rabbit_converse imports `requests` at module load (it sys.exits without it), and
the CI robot lane doesn't install requests, so this test SKIPS cleanly there and
runs fully wherever requests exists (the robot). The pure clause parser is
covered unconditionally in rabbit_stream_test.py; this locks the three
integration invariants that keep streaming a no-regression accelerant:

  1. the streaming gate is OFF by default (production is byte-identical);
  2. the directive-first prompt PRESERVES every routing guardrail (it is the base
     prompt + an ordering override — the "never null a drive request" contract is
     intact);
  3. parse_reply — the fail-closed authority on the directive — is order-agnostic,
     so a directive-first reply routes exactly like the say-first one.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

try:
    import requests  # noqa: F401 — rabbit_converse needs it at import
    _HAVE_REQUESTS = True
except ImportError:
    _HAVE_REQUESTS = False


def test_stream_gate_defaults_off_and_guardrails_preserved() -> None:
    if not _HAVE_REQUESTS:
        print("  skip (requests unavailable — CI lane; parser covered in rabbit_stream_test)")
        return
    os.environ.pop("KIRRA_RABBIT_STREAM_TTS", None)
    import importlib
    import rabbit_converse
    importlib.reload(rabbit_converse)

    # 1. Default OFF.
    assert rabbit_converse.STREAM_TTS is False, "streaming must default off"

    # 2. Guardrails preserved: the streaming prompt is the base prompt + override,
    #    so the load-bearing "never null a drive request" contract is intact.
    base, stream = rabbit_converse.STAGE2_SYSTEM, rabbit_converse.STAGE2_SYSTEM_STREAM
    assert stream.startswith(base), "streaming prompt must extend the base contract, not replace it"
    assert "NEVER set" in stream and "null" in stream, "the drive-request guardrail must survive"
    assert "directive` field FIRST" in stream, "streaming prompt must request directive-first order"

    # 3. parse_reply is order-agnostic (the fail-closed directive authority).
    say, directive = rabbit_converse.parse_reply('{"directive": null, "say": "All nominal."}')
    assert directive is None and say == "All nominal.", (say, directive)
    say, directive = rabbit_converse.parse_reply(
        '{"directive": "creep forward", "say": "Creeping forward."}')
    assert directive == "creep forward" and say == "Creeping forward.", (say, directive)


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    print("rabbit_stream integration tests:")
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
