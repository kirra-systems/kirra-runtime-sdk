# The spoken governed loop — KITT demo (speech I/O over #894)

You speak; Mick proposes; Occy plans; KIRRA bounds; **the car says why — out
loud.** Speech is a thin UX shell around the governed loop: input transduction
and output rendering only. Nothing inside the loop changed.

```
audio in → STT (whisper.cpp, OS process) → text
        → mick_service POST /intent            ← the EXISTING fail-closed door
          (LlmBrain::decide_request → MickIntent::parse_llm_json)
        → GET /intent/last → occy_doer → planner_service → checker → …
verdict + reason (#893) → GET /narration/last → TTS (Piper, OS process) → audio out
```

🔴 **The one rule:** speech text becomes an intent ONLY through
`MickIntent::parse_llm_json` — the same parser typed text uses
(`crates/kirra-sidecars/src/mick.rs` `handle_text`). A misheard command, a
garbled transcription, or noise-as-words fails exactly like typed garbage:
422, no intent latched, no motion. The speech layer
(`crates/kirra-sidecars/src/speech.rs`) imports nothing from the planner and
can only hand a `String` to that door. TTS is a pure sink: it consumes the
door's ack line and the #893 narration sentence (spoken **verbatim** from the
reviewed explanation tables) and produces audio — no route, no command
surface.

STT and TTS engines are **external OS processes**, never crate dependencies of
the safety workspace — the `taj_service`-sidecar discipline. The Mick
actuation fence (`ci/check_mick_actuation_fence.py`) covers the speech module
and shell like everything else in `kirra-sidecars`.

## What runs in CI vs what's manual

| Layer | CI (deterministic, every push) | Manual demo only |
|---|---|---|
| Audio gate | `speech.rs` WAV validation over generated PCM fixtures (fail-closed on garbage/empty/non-PCM) | live `arecord` clips |
| STT | scripted `Transcriber` seam (the `MockModel`-vs-live-Ollama precedent) driving the REAL door + loop: `crates/kirra-actuation-consumer/tests/spoken_governed_loop.rs` | whisper.cpp on the Orin |
| The loop | real parse → real grounding → real checker → real chokepoint; refused-spoken + admitted-positive-control + garbled-fails-closed | same binaries, live |
| TTS | recording `Speaker` sink asserting the SPECIFIC refusal sentence (never `EXPLAIN_UNKNOWN`) is what gets spoken | Piper voices it |

## Manual demo setup (Orin / any Linux box)

1. **Engines** (outside the workspace):
   - [whisper.cpp](https://github.com/ggml-org/whisper.cpp): build `whisper-cli`,
     fetch a model (`ggml-base.en.bin` is plenty for command phrases).
   - [Piper](https://github.com/rhasspy/piper): fetch a voice, wrap playback in
     a one-line script, e.g. `speak.sh`:
     ```sh
     #!/bin/sh
     exec piper --model en_US-lessac-medium.onnx --output-raw | aplay -r 22050 -f S16_LE -t raw -
     ```
2. **The loop** (from #894): `mick_service` (+ Ollama), `planner_service`,
   `taj_service`, the `occy_doer` bridge — per `deploy/systemd/README.md` and
   `docs/testing/OCCY_DOER_BRIDGE.md`. Narration needs `mick_service` started
   with `KIRRA_VERIFIER_URL` + `KIRRA_MICK_AUDITOR_TOKEN` (an **auditor-role
   principal token — never the admin token**).
3. **The shell** (push-to-talk; no wake word, no open mic — each turn records
   one bounded clip):
   ```sh
   export KIRRA_STT_CMD="whisper-cli -m models/ggml-base.en.bin -np -nt -f"
   export KIRRA_TTS_CMD="./speak.sh"                       # unset → print-only
   export KIRRA_RECORD_CMD="arecord -d 4 -f S16_LE -r 16000 -c 1"
   cargo run -p kirra-sidecars --bin speech_shell           # Enter = talk
   # or, from a recorded clip:
   cargo run -p kirra-sidecars --bin speech_shell -- --wav clip.wav
   ```

Say something the corridor won't allow. The planner will try; the checker will
refuse; and the car will tell you which invariant you asked it to break —
in the exact reviewed sentence from the explanation tables, through a speaker.

## Explicitly out of scope

Serial protocol / motor bringup (hardware-gated; ADR-0033: the verifying
consumer *is* the motor bringup) · sros2 (Tier-2) · any change inside the loop
(verdict core, fence, parser, `TrajectoryVerdict` — all frozen) · wake-word /
always-listening.
