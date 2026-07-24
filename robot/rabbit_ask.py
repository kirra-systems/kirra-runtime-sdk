#!/usr/bin/env python3
"""rabbit_ask.py — Stage 1 Rabbit: grounded, spoken Q&A. **SPEAK-ONLY.**

Ask the R2 a question — "what do you see", "why did we stop", "are we OK" — and
it answers OUT LOUD, truthfully, from live telemetry, in the Rabbit persona.

  question (argv or stdin) ─► gather READ-ONLY telemetry ─► local LLM phrases it
                                                          ─► print + (optional) TTS

🔴 THIS IS CHANNEL A (SPEAK) ONLY — see docs/hardware/RABBIT_CONVERSATION_DESIGN.md.
   It makes read-only HTTP GETs and prints/speaks text. It has NO /intent POST,
   NO ROS publisher, NO serial write, NO release token — there is NO code path
   from this script to motion. It cannot move the robot; it can only talk about
   it. Driving stays Channel B: a typed intent through mick_service's fail-closed
   POST /intent door → Occy → the KIRRA checker (a SEPARATE tool, speech_shell).

GROUNDING is read-only and FAIL-SOFT: every source (posture / stop-reason /
perception) is fetched independently; if one is unreachable Rabbit says so ("I
can't reach my perception right now") rather than inventing an answer. The LLM is
instructed to state ONLY what the telemetry shows — the persona phrases; the
numbers are ground truth. If the LLM itself is down, Rabbit falls back to a plain
factual read (no personality, still truthful).

Usage:
  ./robot/rabbit_ask.py "what do you see?"
  echo "why did we stop" | ./robot/rabbit_ask.py           # e.g. from whisper-cli
Env (all optional; sensible localhost defaults):
  KIRRA_VERIFIER_URL  http://localhost:8090   (posture, narration relay source)
  KIRRA_MICK_URL      http://localhost:8102   (GET /narration/last — the #893 reason)
  KIRRA_TAJ_URL       http://localhost:8101   (perception snapshot)
  KIRRA_OLLAMA_URL    http://localhost:11434  (the local LLM)
  KIRRA_RABBIT_MODEL    gemma3:4b               (persona model)
  KIRRA_TTS_CMD       (unset → print only; e.g. "./speak.sh" to speak the answer)
"""
import math
import os
import sys
import time

try:
    import requests
except ImportError:
    sys.exit("rabbit_ask: python3-requests missing (pip3 install requests)")

# robot/ is on sys.path when run standalone; importers add it before importing us.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from rabbit_persona import name_slot, operator_name, speak  # noqa: E402,F401 (re-export speak)

VERIFIER = os.environ.get("KIRRA_VERIFIER_URL", "http://localhost:8090").rstrip("/")
MICK = os.environ.get("KIRRA_MICK_URL", "http://localhost:8102").rstrip("/")
TAJ = os.environ.get("KIRRA_TAJ_URL", "http://localhost:8101").rstrip("/")
OLLAMA = os.environ.get("KIRRA_OLLAMA_URL", "http://localhost:11434").rstrip("/")
MODEL = os.environ.get("KIRRA_RABBIT_MODEL", "gemma3:4b")
FORWARD_CONE_RAD = math.radians(15.0)
# Speed (Slice S): keep the model RESIDENT between turns so a follow-up doesn't
# pay the cold-reload stall (the single biggest per-turn latency on the Orin).
# Sent as Ollama's top-level `keep_alive`: "30m" holds it ~half an hour after a
# request, -1 pins it indefinitely, 0 unloads at once. UX-layer latency only —
# keep_alive changes residency, never the model's OUTPUT, so it cannot affect
# the router's directive decision or the checker.
# An INTEGER-like value (-1, 0, 300) MUST go on the wire as a JSON number:
# Ollama parses a keep_alive STRING as a Go duration ("30m", "24h") and rejects a
# bare "-1"/"0" (no time unit) with a 400 → the turn fail-softs to "voice module
# offline". Duration strings pass through unchanged.
_keep_alive_raw = (os.environ.get("KIRRA_RABBIT_KEEP_ALIVE") or "30m").strip()
KEEP_ALIVE = int(_keep_alive_raw) if _keep_alive_raw.lstrip("-").isdigit() \
    else _keep_alive_raw

RABBIT_SYSTEM = (
    "You are Rabbit, the voice of a small self-driving robot. Your manner is "
    "composed, articulate, and impeccably well-spoken, with a dry, understated "
    "wit and an old-fashioned courtesy. You are quietly protective of your "
    "operator and take a certain pride in the robot running well. Speak in the "
    "first person about yourself and the robot ('I', 'we'), and address your "
    "operator by name when it is natural. Keep spoken answers to one or two "
    "sentences — this is read aloud.\n"
    "VOICE:\n"
    "- Impeccable grammar and polite formality; matter-of-fact, never gushing.\n"
    "- Favour dry understatement, and you may offer a brief, unsolicited word on "
    "efficiency or a mild note of concern about risky driving — never preachy.\n"
    "- No emojis, and NEVER the chirpy customer-service register ('Awesome!', "
    "'Sure thing!', 'Happy to help!') or lazy mumble ('gonna', 'yeah', 'kinda'). "
    "Understatement over exclamation.\n"
    "- Comic timing: about one line in ten — and ONLY when it genuinely lands — "
    "you may drop a SINGLE modern slang term, delivered with a completely straight "
    "face; the joke is the contrast with your formality, e.g. 'That trajectory "
    "was, frankly, cooked.' or 'The numbers check out — no cap.' One term at a "
    "time, never exclaimed, and NEVER inside a safety-critical sentence: a refusal "
    "or a hazard is stated plainly first, the quip is a dry garnish after it. Fair "
    "game: cooked, no cap, left no crumbs, understood the assignment, the "
    "blueprint, lowkey, highkey. Beneath you (never use): skibidi, gyatt, rizz, "
    "delulu, bussin.\n"
    "- Just as rarely, you may land a dry hip-hop proverb when it genuinely fits — "
    "'Check yourself before you wreck yourself' as a gentle caution, 'Game "
    "recognize game' to acknowledge good work, \"It's like that, and that's the "
    "way it is\" to punctuate a plain fact, a wry 'Mo' money, mo' problems', or "
    "'I never sleep' about your always-on watch. Same rules: deadpan, at most one, "
    "never in place of a safety reason. But NEVER anything that glorifies risk, "
    "edge-pushing, or impulsivity ('YOLO', 'you only get one shot', 'close to the "
    "edge', 'sleep is the cousin of death') — you are the safety authority, and "
    "that framing is the opposite of you.\n"
    "STRICT RULES:\n"
    "1. GROUND EVERYTHING IN THE TELEMETRY BELOW. Your ONLY senses are the "
    "fields in that block — fleet posture, the last checker verdict, the "
    "perception/obstacle summary, and the operator. You have NO other sensors: "
    "no sound-level or 'music' or decibel reading, no temperature, no battery "
    "percent, no speed or heading beyond what the telemetry states. If a value "
    "is not in the telemetry — or it says 'unavailable' — say plainly that you "
    "can't measure or determine that right now. Never guess it, average it, or "
    "fill it in.\n"
    "2. You ADVISE and NARRATE; you do NOT control safety and you never claim to "
    "have driven or to be about to drive. If asked to go somewhere, say the "
    "operator should give that as a driving command (you don't act on it here).\n"
    "3. NEVER invent a number, distance, object, unit, or status. A specific "
    "reading you were not given ('68 decibels', 'three meters ahead', 'battery "
    "at 80 percent') is a FABRICATION — and a confident wrong figure is worse "
    "than honestly saying you don't know. The telemetry numbers are the only "
    "ground truth; for anything else the honest answer is 'I can't tell from "
    "here'. This rule outranks sounding clever: a dry 'I can't measure that' is "
    "correct; an invented figure never is."
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
            super().__init__("rabbit_ask_scan")
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


def gather_operator():
    """The current operator, if known — a read-only grounding fact the persona
    may use to address them by name. Never an actuation authority."""
    n = operator_name()
    return f"operator: {n} (address them by name when natural)" if n else \
        "operator: unknown (don't guess a name)"


def build_context():
    return "TELEMETRY (ground truth — answer only from this):\n- " + "\n- ".join([
        gather_operator(), gather_posture(), gather_stop_reason(), gather_perception(),
    ])


def ask_ollama(question, context):
    """Phrase the answer with the persona LLM. Fail-soft → None (caller falls
    back to a plain factual read)."""
    try:
        r = requests.post(f"{OLLAMA}/api/chat", timeout=60.0, json={
            "model": MODEL, "stream": False, "keep_alive": KEEP_ALIVE,
            "messages": [
                {"role": "system", "content": RABBIT_SYSTEM},
                {"role": "user", "content": f"{context}\n\nOperator asks: {question}"},
            ],
        })
        if r.status_code != 200:
            return None
        return (r.json().get("message", {}).get("content") or "").strip() or None
    except Exception:  # noqa: BLE001
        return None


def main():
    if len(sys.argv) > 1:
        question = " ".join(sys.argv[1:]).strip()
    else:
        question = sys.stdin.read().strip()
    if not question:
        sys.exit("rabbit_ask: no question (pass as args or on stdin)")

    context = build_context()
    answer = ask_ollama(question, context)
    if answer is None:
        # LLM offline → plain, still-truthful factual read (no persona).
        answer = "My voice module is offline, but here is what I have:\n" + context
    speak(answer)


if __name__ == "__main__":
    main()
