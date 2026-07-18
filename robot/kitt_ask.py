#!/usr/bin/env python3
"""kitt_ask.py — Stage 1 KITT: grounded, spoken Q&A. **SPEAK-ONLY.**

Ask the R2 a question — "what do you see", "why did we stop", "are we OK" — and
it answers OUT LOUD, truthfully, from live telemetry, in the KITT persona.

  question (argv or stdin) ─► gather READ-ONLY telemetry ─► local LLM phrases it
                                                          ─► print + (optional) TTS

🔴 THIS IS CHANNEL A (SPEAK) ONLY — see docs/hardware/KITT_CONVERSATION_DESIGN.md.
   It makes read-only HTTP GETs and prints/speaks text. It has NO /intent POST,
   NO ROS publisher, NO serial write, NO release token — there is NO code path
   from this script to motion. It cannot move the robot; it can only talk about
   it. Driving stays Channel B: a typed intent through mick_service's fail-closed
   POST /intent door → Occy → the KIRRA checker (a SEPARATE tool, speech_shell).

GROUNDING is read-only and FAIL-SOFT: every source (posture / stop-reason /
perception) is fetched independently; if one is unreachable KITT says so ("I
can't reach my perception right now") rather than inventing an answer. The LLM is
instructed to state ONLY what the telemetry shows — the persona phrases; the
numbers are ground truth. If the LLM itself is down, KITT falls back to a plain
factual read (no personality, still truthful).

Usage:
  ./robot/kitt_ask.py "what do you see?"
  echo "why did we stop" | ./robot/kitt_ask.py           # e.g. from whisper-cli
Env (all optional; sensible localhost defaults):
  KIRRA_VERIFIER_URL  http://localhost:8090   (posture, narration relay source)
  KIRRA_MICK_URL      http://localhost:8102   (GET /narration/last — the #893 reason)
  KIRRA_TAJ_URL       http://localhost:8101   (perception snapshot)
  KIRRA_OLLAMA_URL    http://localhost:11434  (the local LLM)
  KIRRA_KITT_MODEL    gemma3:4b               (persona model)
  KIRRA_TTS_CMD       (unset → print only; e.g. "./speak.sh" to speak the answer)
"""
import math
import os
import subprocess
import sys
import time

try:
    import requests
except ImportError:
    sys.exit("kitt_ask: python3-requests missing (pip3 install requests)")

VERIFIER = os.environ.get("KIRRA_VERIFIER_URL", "http://localhost:8090").rstrip("/")
MICK = os.environ.get("KIRRA_MICK_URL", "http://localhost:8102").rstrip("/")
TAJ = os.environ.get("KIRRA_TAJ_URL", "http://localhost:8101").rstrip("/")
OLLAMA = os.environ.get("KIRRA_OLLAMA_URL", "http://localhost:11434").rstrip("/")
MODEL = os.environ.get("KIRRA_KITT_MODEL", "gemma3:4b")
TTS_CMD = os.environ.get("KIRRA_TTS_CMD", "").strip()
FORWARD_CONE_RAD = math.radians(15.0)

KITT_SYSTEM = (
    "You are KITT, the voice of a small self-driving robot. You are composed, "
    "articulate, dryly witty, and protective of your operator. Speak in the "
    "first person about yourself and the robot ('I', 'we'). Keep answers to one "
    "or two spoken sentences — this is read aloud.\n"
    "STRICT RULES:\n"
    "1. Answer ONLY from the TELEMETRY provided below. State nothing the "
    "telemetry does not support. If the needed telemetry is missing or says "
    "'unavailable', say plainly that you can't determine it right now.\n"
    "2. You ADVISE and NARRATE; you do NOT control safety and you never claim to "
    "have driven or to be about to drive. If asked to go somewhere, say the "
    "operator should give that as a driving command (you don't act on it here).\n"
    "3. Never invent distances, objects, or status. The numbers are ground truth."
)


def _get_json(url, timeout=2.0, headers=None):
    """Read-only GET, fail-soft → None."""
    try:
        r = requests.get(url, timeout=timeout, headers=headers or {})
        if r.status_code != 200:
            return None
        return r.json()
    except Exception:  # noqa: BLE001 — any fault degrades to "unavailable"
        return None


def gather_posture():
    # GET /fleet/posture → {"fleet": <per-node postures>} (posture-exempt, no auth).
    j = _get_json(f"{VERIFIER}/fleet/posture")
    if j is None:
        return "posture: unavailable (cannot reach the governor)"
    fleet = j.get("fleet") if isinstance(j, dict) else None
    if not fleet:
        return "fleet posture: no nodes registered in the fleet view (no reported faults)"
    return f"fleet posture (per node): {str(fleet)[:300]}"


def gather_stop_reason():
    # The #893 narration relay (mick_service GET /narration/last) — the checker's
    # actual last verdict. Shape: {"last": null | {action, deny_code, explanation}}.
    # Needs mick started with the auditor token; else fail-soft to unavailable.
    j = _get_json(f"{MICK}/narration/last")
    if not isinstance(j, dict):
        return "last stop reason: unavailable (narration relay off or governor unreachable)"
    if "last" not in j:
        return "last stop reason: unavailable"
    last = j.get("last")
    if last is None:
        return "last verdict: no actuator command has been judged yet"
    action = last.get("action", "?") if isinstance(last, dict) else "?"
    code = last.get("deny_code") if isinstance(last, dict) else None
    expl = last.get("explanation") if isinstance(last, dict) else None
    if code and expl:
        return f"last verdict: {action} ({code}) — {expl}"
    if code:
        return f"last verdict: {action} ({code})"
    return f"last verdict: {action}"


def gather_perception():
    """One live /scan → Taj corridor → a truthful nearest-obstacle summary.
    Lazy ROS import: if ROS isn't up this degrades to 'unavailable'."""
    try:
        import rclpy
        from rclpy.node import Node
        from rclpy.qos import QoSProfile, ReliabilityPolicy, HistoryPolicy
        from sensor_msgs.msg import LaserScan
    except Exception:  # noqa: BLE001
        return "perception: unavailable (ROS not reachable)"

    qos = QoSProfile(reliability=ReliabilityPolicy.BEST_EFFORT,
                     history=HistoryPolicy.KEEP_LAST, depth=1)

    class Grab(Node):
        def __init__(self):
            super().__init__("kitt_ask_scan")
            self.scan = None
            self.create_subscription(LaserScan, "/scan", self._on, qos)

        def _on(self, m):
            self.scan = m

    started = False
    try:
        rclpy.init()
        started = True
        node = Grab()
        t0 = time.monotonic()
        while node.scan is None and time.monotonic() - t0 < 3.0:
            rclpy.spin_once(node, timeout_sec=0.2)
        scan = node.scan
        node.destroy_node()
        if scan is None:
            return "perception: unavailable (no lidar scan)"
        ranges = [float(r) for r in scan.ranges]
        fwd = [r for i, r in enumerate(ranges)
               if math.isfinite(r) and r > scan.range_min
               and abs(scan.angle_min + i * scan.angle_increment) < FORWARD_CONE_RAD]
        nearest_fwd = min(fwd, default=float("inf"))
        taj = requests.post(f"{TAJ}/perception", timeout=2.0, json={
            "angle_min_rad": float(scan.angle_min),
            "angle_increment_rad": float(scan.angle_increment),
            "range_min_m": float(scan.range_min),
            "range_max_m": float(scan.range_max),
            "ranges": ranges, "stamp_ms": 0, "forward_extent_m": 8.0,
        }).json()
        objs = taj.get("objects", [])
        left, right = taj.get("left", []), taj.get("right", [])
        clear = "unknown"
        if left and right:
            hw = min(abs(left[len(left) // 2][1]), abs(right[len(right) // 2][1]))
            clear = f"~{2 * hw:.2f} m wide lane" if hw > 0.001 else "no clear lane (boxed in)"
        fwd_txt = (f"{nearest_fwd:.2f} m ahead" if math.isfinite(nearest_fwd)
                   else "nothing within range ahead")
        return (f"perception: nearest obstacle straight ahead {fwd_txt}; "
                f"{len(objs)} objects detected; drivable corridor {clear}")
    except Exception as e:  # noqa: BLE001
        return f"perception: unavailable ({e})"
    finally:
        if started:
            try:
                rclpy.shutdown()
            except Exception:  # noqa: BLE001
                pass


def build_context():
    return "TELEMETRY (ground truth — answer only from this):\n- " + "\n- ".join([
        gather_posture(), gather_stop_reason(), gather_perception(),
    ])


def ask_ollama(question, context):
    """Phrase the answer with the persona LLM. Fail-soft → None (caller falls
    back to a plain factual read)."""
    try:
        r = requests.post(f"{OLLAMA}/api/chat", timeout=60.0, json={
            "model": MODEL, "stream": False,
            "messages": [
                {"role": "system", "content": KITT_SYSTEM},
                {"role": "user", "content": f"{context}\n\nOperator asks: {question}"},
            ],
        })
        if r.status_code != 200:
            return None
        return (r.json().get("message", {}).get("content") or "").strip() or None
    except Exception:  # noqa: BLE001
        return None


def speak(text):
    print(text)
    if not TTS_CMD:
        return
    try:
        parts = TTS_CMD.split()
        subprocess.run(parts, input=text.encode(), check=False)
    except Exception as e:  # noqa: BLE001
        print(f"(tts failed: {e})", file=sys.stderr)


def main():
    if len(sys.argv) > 1:
        question = " ".join(sys.argv[1:]).strip()
    else:
        question = sys.stdin.read().strip()
    if not question:
        sys.exit("kitt_ask: no question (pass as args or on stdin)")

    context = build_context()
    answer = ask_ollama(question, context)
    if answer is None:
        # LLM offline → plain, still-truthful factual read (no persona).
        answer = "My voice module is offline, but here is what I have:\n" + context
    speak(answer)


if __name__ == "__main__":
    main()
