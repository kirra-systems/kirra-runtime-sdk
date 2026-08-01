#!/usr/bin/env python3
"""mick_voice_benchmark.py — repeatable voice-stage latency measurement from a
pre-recorded WAV fixture (no speaking, no microphone, no motion).

    python3 robot/mick_voice_benchmark.py --wav /tmp/kirra-test.wav \
        --chat-url http://127.0.0.1:8103

Stages measured (monotonic; names + durations only, never content):

    stt_ms                KIRRA_STT_CMD over the fixture
    chat_ttft_ms          transcript POSTed → first streamed text
    first_sentence_ms     transcript POSTed → first speakable chunk
    tts_start_ms          first sentence → TTS process spawned
    first_audio_ms        first RAW AUDIO bytes out of the TTS engine —
                          requires KIRRA_TTS_STREAM_CMD (a command that
                          writes raw audio to stdout, e.g.
                          "piper --model … --output-raw"); with only a
                          play-through KIRRA_TTS_CMD this is reported null
    voice_to_first_audio_ms   transcript-ready → first audio bytes
    voice_total_ms        fixture in → reply complete (audio drained)

Run it several times; the first run after idle shows the cold model, the rest
show the warm path. No transcripts or replies are persisted or logged.
"""
from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import time
import urllib.error
import urllib.request


def sse_events(resp):
    event, data = None, None
    for raw in resp:
        line = raw.decode("utf-8", "replace").rstrip("\r\n")
        if line.startswith("event: "):
            event = line[7:]
        elif line.startswith("data: "):
            data = line[6:]
        elif line == "" and event is not None:
            try:
                yield event, json.loads(data or "{}")
            except json.JSONDecodeError:
                return
            event, data = None, None


def main() -> int:
    ap = argparse.ArgumentParser(description="voice-stage latency benchmark (WAV fixture)")
    ap.add_argument("--wav", required=True)
    ap.add_argument("--chat-url", default=os.environ.get("KIRRA_MICK_CHAT_URL",
                                                         "http://127.0.0.1:8103"))
    ap.add_argument("--no-tts", action="store_true")
    args = ap.parse_args()

    report: dict[str, object] = {}
    t_start = time.monotonic()

    # --- STT over the fixture ------------------------------------------------
    stt = os.environ.get("KIRRA_STT_CMD")
    if not stt:
        print("KIRRA_STT_CMD is required (wav path appended)", file=sys.stderr)
        return 1
    t0 = time.monotonic()
    try:
        p = subprocess.run(  # noqa: S603 — operator-configured argv
            shlex.split(stt) + [args.wav], capture_output=True, text=True,
            timeout=300.0)
    except (subprocess.TimeoutExpired, OSError) as e:
        print(f"STT failed: {e}", file=sys.stderr)
        return 1
    transcript = " ".join(p.stdout.split()).strip()
    report["stt_ms"] = round((time.monotonic() - t0) * 1000.0)
    if not transcript:
        print("STT produced an empty transcript", file=sys.stderr)
        return 1
    t_transcript = time.monotonic()

    # --- streamed chat + (optional) TTS -------------------------------------
    tts_stream_cmd = None if args.no_tts else os.environ.get("KIRRA_TTS_STREAM_CMD")
    tts_cmd = None if args.no_tts else os.environ.get("KIRRA_TTS_CMD")
    body = json.dumps({"text": transcript}).encode()
    req = urllib.request.Request(args.chat_url.rstrip("/") + "/chat/stream",
                                 data=body,
                                 headers={"Content-Type": "application/json"},
                                 method="POST")
    tts_proc = None
    aplay = None
    first_sentence_t = None
    tts_start_t = None
    first_audio_t = None
    try:
        with urllib.request.urlopen(req, timeout=300.0) as resp:
            for event, data in sse_events(resp):
                now = time.monotonic()
                if event == "done":
                    report["chat_ttft_ms"] = data.get("ttft_ms")
                    report["chat_total_ms"] = data.get("total_ms")
                elif event == "sentence" and data.get("text"):
                    if first_sentence_t is None:
                        first_sentence_t = now
                        report["first_sentence_ms"] = round(
                            (now - t_transcript) * 1000.0)
                    if tts_proc is None and (tts_stream_cmd or tts_cmd):
                        cmd = tts_stream_cmd or tts_cmd
                        tts_proc = subprocess.Popen(  # noqa: S603
                            shlex.split(cmd), stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE if tts_stream_cmd else subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
                        tts_start_t = time.monotonic()
                        report["tts_start_ms"] = round(
                            (tts_start_t - first_sentence_t) * 1000.0)
                    if tts_proc is not None and tts_proc.stdin is not None:
                        try:
                            tts_proc.stdin.write(
                                (data["text"].strip() + "\n").encode())
                            tts_proc.stdin.flush()
                        except (BrokenPipeError, OSError):
                            pass
                        # Raw-audio seam: first bytes out = first audio.
                        if (tts_stream_cmd and first_audio_t is None
                                and tts_proc.stdout is not None):
                            if aplay is None:
                                aplay = subprocess.Popen(  # noqa: S603
                                    ["aplay", "-q", "-r", "22050", "-f",
                                     "S16_LE", "-t", "raw", "-"],
                                    stdin=subprocess.PIPE,
                                    stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL)
                            chunk = tts_proc.stdout.read(4096)
                            if chunk:
                                first_audio_t = time.monotonic()
                                report["first_audio_ms"] = round(
                                    (first_audio_t - tts_start_t) * 1000.0)
                                report["voice_to_first_audio_ms"] = round(
                                    (first_audio_t - t_transcript) * 1000.0)
                                if aplay.stdin is not None:
                                    aplay.stdin.write(chunk)
    except (urllib.error.URLError, TimeoutError) as e:
        print(f"chat stream failed: {e}", file=sys.stderr)
        return 1
    finally:
        for proc in (tts_proc, aplay):
            if proc is not None:
                try:
                    if proc.stdin is not None:
                        proc.stdin.close()
                    if proc is tts_proc and tts_stream_cmd and proc.stdout and aplay:
                        # Drain remaining audio through the player.
                        while True:
                            chunk = proc.stdout.read(8192)
                            if not chunk:
                                break
                            if aplay.stdin is not None:
                                aplay.stdin.write(chunk)
                        if aplay.stdin is not None:
                            aplay.stdin.close()
                    proc.wait(timeout=60.0)
                except (subprocess.TimeoutExpired, OSError):
                    proc.kill()

    if "first_audio_ms" not in report:
        report["first_audio_ms"] = None
        report["voice_to_first_audio_ms"] = None
    report["voice_total_ms"] = round((time.monotonic() - t_start) * 1000.0)
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
