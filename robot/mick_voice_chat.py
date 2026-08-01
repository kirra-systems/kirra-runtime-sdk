#!/usr/bin/env python3
"""mick_voice_chat.py — low-latency SPOKEN conversation with Mick.

The fast path this client exists for:

    transcript → POST /chat/stream (SSE) → sentence chunks → ONE TTS process
                                                             per reply, fed
                                                             incrementally

The user hears the FIRST sentence while the model is still generating the
rest, and the TTS engine (piper via KIRRA_TTS_CMD) is spawned once per reply
— not once per sentence — so its model load is paid once per turn.

Inputs (two modes, both repeatable without a microphone):
  * default: one TRANSCRIPT per line on stdin (the rabbit_converse seam — any
    trigger/recorder/STT front-end can pipe in);
  * --wav FILE: transcribe FILE once via KIRRA_STT_CMD, run one turn, exit
    (the voice-benchmark fixture mode).

🔴 SEPARATION: talks ONLY to the conversational sidecar (default
http://127.0.0.1:8103, override KIRRA_MICK_CHAT_URL). No fallback to the
motion door — its path never appears in this file (host-tested). No shell
strings, no transcript persistence.

TIMING: every stage is measured on time.monotonic and logged to stderr as
STAGE NAMES AND DURATIONS ONLY — transcript and reply text are never logged.
Stages: stt_started/transcript_ready (wav mode), chat_request_started,
first_text_received, first_sentence_ready, tts_started, first_audio_written,
response_audio_complete → summary line with chat_ttft_ms, first_sentence_ms,
tts_first_audio_ms, voice_to_first_audio_ms, voice_total_ms.

Optional immediate acknowledgement: KIRRA_CHAT_ACK_TONE=1 plays a short
locally generated tone (via aplay) the moment a turn starts — a nonverbal
"heard you" while the model thinks. Off by default; it must never mask the
mic (it plays once, ~120 ms, on the speaker).

Env:
  KIRRA_MICK_CHAT_URL           default http://127.0.0.1:8103
  KIRRA_TTS_CMD                 text lines on stdin → audio out (speak.sh)
  KIRRA_STT_CMD                 --wav mode only; wav path appended
  KIRRA_CHAT_ACK_TONE           1 → play the ack tone (default off)
  KIRRA_CHAT_TTS_CHUNK_MIN_CHARS / _MAX_CHARS   client-side re-chunk bounds
                                (defaults 20 / 120; server sentences are
                                usually already right-sized)
"""
from __future__ import annotations

import argparse
import json
import math
import os
import shlex
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import voice_playback  # noqa: E402 — wake-listener suppression while speaking

DEFAULT_URL = "http://127.0.0.1:8103"
REQUEST_TIMEOUT_S = 90.0


def log_stage(name: str, ms: float) -> None:
    """Stage telemetry: name + duration only. NEVER content."""
    print(f"mick_voice_chat: stage {name} {ms:.0f}ms", file=sys.stderr, flush=True)


def chat_url() -> str:
    return (os.environ.get("KIRRA_MICK_CHAT_URL") or DEFAULT_URL).rstrip("/")


def ack_tone() -> None:
    """~120 ms locally generated sine via aplay. Best-effort, nonverbal, and
    never blocks the turn — a broken speaker must not add latency."""
    if (os.environ.get("KIRRA_CHAT_ACK_TONE") or "") not in ("1", "true"):
        return
    rate, dur_s, freq = 22050, 0.12, 880.0
    n = int(rate * dur_s)
    pcm = b"".join(
        struct.pack("<h", int(12000 * math.sin(2 * math.pi * freq * i / rate) *
                              min(1.0, (n - i) / (0.25 * n))))
        for i in range(n)
    )
    try:
        subprocess.Popen(  # noqa: S603 — fixed argv, no shell
            ["aplay", "-q", "-r", str(rate), "-f", "S16_LE", "-t", "raw", "-"],
            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        ).communicate(pcm, timeout=2.0)
    except Exception:  # noqa: BLE001 — the tone is UX, never load-bearing
        pass


def sse_events(resp):
    """Parse an SSE byte stream into (event, data-dict) pairs, incrementally.
    Malformed data lines end the stream (the caller falls back gracefully)."""
    event, data = None, None
    for raw in resp:
        line = raw.decode("utf-8", "replace").rstrip("\n").rstrip("\r")
        if line.startswith("event: "):
            event = line[7:]
        elif line.startswith("data: "):
            data = line[6:]
        elif line == "" and event is not None:
            try:
                yield event, json.loads(data or "{}")
            except json.JSONDecodeError:
                return  # malformed stream → stop consuming, fail soft
            event, data = None, None


class TtsPipe:
    """ONE TTS process per reply. Sentences are written as lines the moment
    they arrive; piper synthesizes line by line, so audio starts on the first
    sentence. Teardown is bounded; a dead TTS never wedges the turn."""

    def __init__(self, cmd: str):
        self.first_write_ms: float | None = None
        self.proc = subprocess.Popen(  # noqa: S603 — operator-configured argv
            shlex.split(cmd), stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def say(self, sentence: str) -> None:
        if self.proc.stdin is None:
            return
        try:
            self.proc.stdin.write((sentence.strip() + "\n").encode("utf-8"))
            self.proc.stdin.flush()
            if self.first_write_ms is None:
                self.first_write_ms = time.monotonic() * 1000.0
        except (BrokenPipeError, OSError):
            pass  # TTS died — keep the turn alive, text still printed

    def close(self, timeout_s: float = 30.0) -> None:
        try:
            if self.proc.stdin is not None:
                self.proc.stdin.close()
            self.proc.wait(timeout=timeout_s)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        except OSError:
            pass


def run_turn(base_url: str, text: str, tts_cmd: str | None) -> bool:
    """One spoken turn. Returns False on transport failure (caller decides).

    The WHOLE turn — ack tone, streamed sentences, TTS drain, process wait,
    cooldown — runs under the playback guard (voice_playback.PlaybackGuard),
    so the wake listener cannot hear the robot's own reply as operator
    speech (the live self-conversation loop). The guard is re-entrant: a
    caller that already holds it (voice_turn_router) nests without
    deadlock. It releases in ALL exits, KeyboardInterrupt included — the
    kernel drops the flock even on a hard kill, so suppression can never
    stick after a crash."""
    with voice_playback.PlaybackGuard():
        return _run_turn_guarded(base_url, text, tts_cmd)


def _run_turn_guarded(base_url: str, text: str, tts_cmd: str | None) -> bool:
    t0 = time.monotonic() * 1000.0
    ack_tone()
    body = json.dumps({"text": text}).encode("utf-8")
    req = urllib.request.Request(
        base_url + "/chat/stream", data=body,
        headers={"Content-Type": "application/json"}, method="POST")
    log_stage("chat_request_started", 0.0)
    tts: TtsPipe | None = None
    first_text_ms = first_sentence_ms = None
    ok = False
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT_S) as resp:
            for event, data in sse_events(resp):
                now = time.monotonic() * 1000.0
                if event == "delta" and first_text_ms is None:
                    first_text_ms = now - t0
                    log_stage("first_text_received", first_text_ms)
                elif event == "sentence":
                    sent = data.get("text", "")
                    if not sent:
                        continue
                    if first_sentence_ms is None:
                        first_sentence_ms = now - t0
                        log_stage("first_sentence_ready", first_sentence_ms)
                    print(f"Mick: {sent}")
                    if tts_cmd and tts is None:
                        tts = TtsPipe(tts_cmd)
                        log_stage("tts_started", time.monotonic() * 1000.0 - t0)
                    if tts is not None:
                        tts.say(sent)
                        if tts.first_write_ms is not None:
                            log_stage("first_audio_written",
                                      tts.first_write_ms - t0)
                elif event == "done":
                    ok = True
                    log_stage("chat_ttft_ms", float(data.get("ttft_ms", 0)))
                    log_stage("chat_total_ms", float(data.get("total_ms", 0)))
    except urllib.error.HTTPError as e:
        print(f"mick_voice_chat: HTTP {e.code}", file=sys.stderr)
    except (urllib.error.URLError, TimeoutError) as e:
        print(f"mick_voice_chat: cannot reach {base_url} ({e})", file=sys.stderr)
    finally:
        if tts is not None:
            tts.close()
            log_stage("response_audio_complete", time.monotonic() * 1000.0 - t0)
    total = time.monotonic() * 1000.0 - t0
    if ok:
        first_audio = (tts.first_write_ms - t0) if tts and tts.first_write_ms else None
        summary = (
            f"voice_total_ms={total:.0f}"
            + (f" first_sentence_ms={first_sentence_ms:.0f}" if first_sentence_ms else "")
            + (f" voice_to_first_audio_ms={first_audio:.0f}" if first_audio else "")
        )
        print(f"mick_voice_chat: summary {summary}", file=sys.stderr, flush=True)
    return ok


def transcribe_wav(wav_path: str) -> str | None:
    """--wav mode: run KIRRA_STT_CMD (wav path appended) and time it."""
    stt = os.environ.get("KIRRA_STT_CMD")
    if not stt:
        print("mick_voice_chat: KIRRA_STT_CMD required for --wav", file=sys.stderr)
        return None
    t0 = time.monotonic() * 1000.0
    log_stage("stt_started", 0.0)
    try:
        p = subprocess.run(  # noqa: S603 — operator-configured argv
            shlex.split(stt) + [wav_path], capture_output=True, text=True,
            timeout=120.0)
    except (subprocess.TimeoutExpired, OSError) as e:
        print(f"mick_voice_chat: STT failed ({e})", file=sys.stderr)
        return None
    log_stage("transcript_ready", time.monotonic() * 1000.0 - t0)
    text = " ".join(p.stdout.split()).strip()
    return text or None


def main() -> int:
    ap = argparse.ArgumentParser(description="spoken conversation with Mick (chat only, no motion)")
    ap.add_argument("--wav", help="transcribe this WAV once, run one turn, exit")
    ap.add_argument("--no-tts", action="store_true", help="print only, no speech")
    args = ap.parse_args()
    base = chat_url()
    tts_cmd = None if args.no_tts else os.environ.get("KIRRA_TTS_CMD")
    if tts_cmd is None and not args.no_tts:
        print("mick_voice_chat: KIRRA_TTS_CMD unset — printing only",
              file=sys.stderr)
    if args.wav:
        text = transcribe_wav(args.wav)
        if text is None:
            return 1
        return 0 if run_turn(base, text, tts_cmd) else 1
    print("mick_voice_chat: transcript lines on stdin → spoken replies "
          "(conversation only; motion uses the governed door). Ctrl-D quits.",
          file=sys.stderr)
    for line in sys.stdin:
        text = line.strip()
        if text:
            run_turn(base, text, tts_cmd)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        # A clean operator interrupt, not a fault: the guard/TtsPipe finally
        # blocks have already run by the time this propagates here.
        print("mick_voice_chat: interrupted", file=sys.stderr)
        sys.exit(130)
