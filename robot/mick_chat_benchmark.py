#!/usr/bin/env python3
"""mick_chat_benchmark.py — measure the Mick chat path, don't guess it.

Two modes:

1. SERVICE benchmark (default): N warm requests against the running
   mick_chat_service, reporting p50/p90 TTFT and total (both server-reported
   and client-measured wall), mean reply length, and failures.

       python3 robot/mick_chat_benchmark.py --url http://127.0.0.1:8103 --runs 10

2. MODEL comparison (--compare-models): drive Ollama /api/chat DIRECTLY with
   the same compact prompt across INSTALLED models (from /api/tags — nothing
   is ever downloaded), reporting warm TTFT, warm total, and tokens/second
   per model, optionally with a cold-start figure (--cold unloads the model
   with keep_alive:0 first — intrusive, off by default).

       python3 robot/mick_chat_benchmark.py --compare-models
       python3 robot/mick_chat_benchmark.py --compare-models --cold

No transcript persistence; the fixed prompt set is benign and hardcoded.
Latency numbers belong in benchmark reports, never in CI assertions.
"""
from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
import urllib.error
import urllib.request

PROMPTS = [
    "Hello Mick. How are you?",
    "What can you help me with?",
    "Tell me something short about robots.",
    "Thanks for the chat so far.",
    "Say one sentence about the weather.",
]

COMPACT_SYSTEM = (
    "You are Mick, a concise conversational robot assistant. "
    "Answer naturally in one or two short sentences."
)


def pctl(values: list[float], p: float) -> float:
    if not values:
        return float("nan")
    s = sorted(values)
    idx = min(len(s) - 1, max(0, round(p / 100.0 * (len(s) - 1))))
    return s[idx]


def bench_service(url: str, runs: int) -> int:
    ttfts, totals, walls, chars = [], [], [], []
    failures = 0
    for i in range(runs):
        prompt = PROMPTS[i % len(PROMPTS)]
        body = json.dumps({"text": prompt}).encode()
        req = urllib.request.Request(url.rstrip("/") + "/chat", data=body,
                                     headers={"Content-Type": "application/json"},
                                     method="POST")
        t0 = time.monotonic()
        try:
            with urllib.request.urlopen(req, timeout=120.0) as resp:
                parsed = json.loads(resp.read())
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as e:
            failures += 1
            print(f"  run {i + 1}: FAILED ({e})", file=sys.stderr)
            continue
        wall_ms = (time.monotonic() - t0) * 1000.0
        if parsed.get("ok") is not True:
            failures += 1
            continue
        timing = parsed.get("timing", {})
        ttfts.append(float(timing.get("ttft_ms", wall_ms)))
        totals.append(float(timing.get("total_ms", wall_ms)))
        walls.append(wall_ms)
        chars.append(len(parsed.get("reply", "")))
        print(f"  run {i + 1}: ttft={timing.get('ttft_ms')}ms "
              f"total={timing.get('total_ms')}ms wall={wall_ms:.0f}ms",
              file=sys.stderr)
    print(json.dumps({
        "count": runs,
        "failures": failures,
        "p50_ttft_ms": round(pctl(ttfts, 50)),
        "p90_ttft_ms": round(pctl(ttfts, 90)),
        "p50_total_ms": round(pctl(totals, 50)),
        "p90_total_ms": round(pctl(totals, 90)),
        "p50_wall_ms": round(pctl(walls, 50)),
        "mean_reply_chars": round(statistics.mean(chars)) if chars else 0,
    }, indent=2))
    return 1 if failures else 0


def ollama_chat_stream(base: str, model: str, prompt: str,
                       keep_alive) -> tuple[float, float, int] | None:
    """One streaming /api/chat run → (ttft_ms, total_ms, tokens)."""
    body = json.dumps({
        "model": model, "stream": True, "keep_alive": keep_alive,
        "options": {"num_predict": 48, "temperature": 0.2},
        "messages": [{"role": "system", "content": COMPACT_SYSTEM},
                     {"role": "user", "content": prompt}],
    }).encode()
    req = urllib.request.Request(base.rstrip("/") + "/api/chat", data=body,
                                 headers={"Content-Type": "application/json"})
    t0 = time.monotonic()
    ttft = None
    tokens = 0
    try:
        with urllib.request.urlopen(req, timeout=300.0) as resp:
            for line in resp:
                if not line.strip():
                    continue
                parsed = json.loads(line)
                if parsed.get("message", {}).get("content"):
                    tokens += 1
                    if ttft is None:
                        ttft = (time.monotonic() - t0) * 1000.0
                if parsed.get("done"):
                    break
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError):
        return None
    total = (time.monotonic() - t0) * 1000.0
    return (ttft or total, total, tokens)


def compare_models(base: str, cold: bool) -> int:
    try:
        with urllib.request.urlopen(base.rstrip("/") + "/api/tags", timeout=10.0) as r:
            models = [m["name"] for m in json.loads(r.read()).get("models", [])]
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as e:
        print(f"cannot list installed models: {e}", file=sys.stderr)
        return 1
    if not models:
        print("no models installed", file=sys.stderr)
        return 1
    print(f"installed models: {models}", file=sys.stderr)
    results = []
    for model in models:
        row = {"model": model}
        if cold:
            # Unload, then measure the first request (cold start). Intrusive.
            ollama_chat_stream(base, model, "hi", 0)
            time.sleep(1.0)
            r = ollama_chat_stream(base, model, PROMPTS[0], "10m")
            row["cold_total_ms"] = round(r[1]) if r else None
        # One warm-up, then measured warm runs.
        ollama_chat_stream(base, model, "hi", "10m")
        ttfts, totals, tokrates = [], [], []
        for prompt in PROMPTS[:3]:
            r = ollama_chat_stream(base, model, prompt, "10m")
            if r is None:
                continue
            ttft, total, tokens = r
            ttfts.append(ttft)
            totals.append(total)
            if total > ttft and tokens > 1:
                tokrates.append((tokens - 1) / ((total - ttft) / 1000.0))
        if ttfts:
            row.update({
                "warm_ttft_ms": round(statistics.mean(ttfts)),
                "warm_total_ms": round(statistics.mean(totals)),
                "tokens_per_s": round(statistics.mean(tokrates), 1) if tokrates else None,
            })
        else:
            row["error"] = "all runs failed"
        results.append(row)
        print(f"  {row}", file=sys.stderr)
    print(json.dumps(results, indent=2))
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="Mick chat latency benchmark")
    ap.add_argument("--url", default=os.environ.get("KIRRA_MICK_CHAT_URL",
                                                    "http://127.0.0.1:8103"))
    ap.add_argument("--runs", type=int, default=10)
    ap.add_argument("--compare-models", action="store_true")
    ap.add_argument("--ollama-url", default=os.environ.get("KIRRA_OLLAMA_URL",
                                                           "http://127.0.0.1:11434"))
    ap.add_argument("--cold", action="store_true",
                    help="also measure cold start (unloads models; intrusive)")
    args = ap.parse_args()
    if args.compare_models:
        return compare_models(args.ollama_url, args.cold)
    return bench_service(args.url, args.runs)


if __name__ == "__main__":
    sys.exit(main())
