#!/usr/bin/env python3
"""world_model.py — the Rabbit World Model READ PROJECTION (voice-cognition
Slice 4, opt-in). A non-authoritative, TTL'd view the Conversation Manager (and
later the planner) can READ to answer "what's my situation?" in one place.

🔴 IT IS A READ PROJECTION, NOT AN AUTHORITY (architecture ruling §5.1):
  * No subsystem depends on it for a safety decision — the KIRRA checker reads
    its OWN inputs directly. This is operator-facing narration only (Channel A).
  * Every field carries its own `source`, `stamp_ms`, and `ttl_ms`. On read, a
    field older than its TTL reads as UNKNOWN — a stale value is NEVER presented
    as current. Absent field → UNKNOWN. That is the whole point: fail-closed
    freshness, kept LOCAL and checkable, instead of one shared mutable brain that
    turns "is this fresh enough?" into a global question.

The pure core (freshness, snapshot, render) is host-tested. The live assembler
pulls the read-only grounding that already exists (posture / perception / last
stop reason / operator) and stamps each field; a source that reports
"unavailable/unreachable" leaves its field UNSET → UNKNOWN (fail-closed), it is
never fabricated. Fields without a producer yet (battery, localization, nav
state, known people) are simply absent → UNKNOWN, documented projection slots.

Opt-in: `KIRRA_WORLD_MODEL_ENABLED` (default off). Off → the deterministic
"situation report" voice command is inert and rabbit_converse is byte-identical.
"""
import os
import re
import time
from collections import namedtuple


class _Unknown:
    __slots__ = ()

    def __repr__(self):
        return "UNKNOWN"


UNKNOWN = _Unknown()  # singleton sentinel — a field that is stale/absent/unavailable


def is_unknown(v):
    return isinstance(v, _Unknown)


Field = namedtuple("Field", ["value", "source", "stamp_ms", "ttl_ms"])

# Default per-field freshness budgets (ms). Posture mirrors POSTURE_CACHE_TTL_MS.
DEFAULT_TTLS = {
    "posture": 5_000,
    "perception": 3_000,
    "stop_reason": 15_000,
    "operator": 3_600_000,
}

# The spoken order + labels for the situation report.
_REPORT_ORDER = [
    ("posture", "Posture"),
    ("perception", "Ahead"),
    ("stop_reason", "Last stop"),
    ("operator", "Operator"),
]

_REPORT_RE = re.compile(
    r"\b(situation\s+report|status\s+report|status\s+update|sitrep"
    r"|give\s+me\s+(a\s+)?status|how\s+are\s+we\s+doing)\b",
    re.IGNORECASE,
)


# ── pure core (host-tested; no I/O) ──────────────────────────────────────────

def enabled():
    """Armed only on an explicit affirmative (fail-closed: unset/typo → off)."""
    return (os.environ.get("KIRRA_WORLD_MODEL_ENABLED") or "").strip().lower() \
        in ("1", "true", "yes", "on")


def is_fresh(stamp_ms, ttl_ms, now_ms):
    """Fresh iff read within the TTL of the stamp. A future stamp (clock skew)
    reads fresh, not stale; a non-positive TTL is always stale."""
    if ttl_ms <= 0:
        return False
    return (now_ms - stamp_ms) <= ttl_ms


def is_source_failure(text):
    """A gather that returned an 'unavailable/unreachable' marker is NOT data — the
    projection must leave that field UNKNOWN rather than present the marker as a
    fact."""
    t = (text or "").lower()
    return any(m in t for m in ("unavailable", "unreachable", "not reachable",
                                "not available", "cannot reach", "offline"))


class WorldModel:
    """A bag of TTL'd fields. `get`/`snapshot` return UNKNOWN for absent-or-stale;
    time is injected (now_ms) so reads are deterministic under test."""

    def __init__(self):
        self._fields = {}

    def set(self, name, value, source, stamp_ms, ttl_ms):
        self._fields[name] = Field(value, source, stamp_ms, ttl_ms)

    def get(self, name, now_ms):
        f = self._fields.get(name)
        if f is None or not is_fresh(f.stamp_ms, f.ttl_ms, now_ms):
            return UNKNOWN
        return f.value

    def fresh(self, name, now_ms):
        f = self._fields.get(name)
        return f is not None and is_fresh(f.stamp_ms, f.ttl_ms, now_ms)

    def snapshot(self, now_ms):
        """A read-time view: {name: {value|UNKNOWN, fresh, age_ms, source, ttl_ms}}."""
        out = {}
        for name, f in self._fields.items():
            fresh = is_fresh(f.stamp_ms, f.ttl_ms, now_ms)
            out[name] = {
                "value": f.value if fresh else UNKNOWN,
                "fresh": fresh,
                "age_ms": now_ms - f.stamp_ms,
                "source": f.source,
                "ttl_ms": f.ttl_ms,
            }
        return out


def render(snapshot):
    """A spoken situation report. A stale/absent/UNKNOWN field is SAID to be
    unknown — never a stale value dressed as current."""
    lines = ["Here's my situation report."]
    for name, label in _REPORT_ORDER:
        entry = snapshot.get(name)
        if entry is None:
            lines.append(f"{label}: unknown — no reading.")
        elif entry["fresh"] and not is_unknown(entry["value"]):
            lines.append(f"{label}: {entry['value']}.")
        else:
            lines.append(f"{label}: unknown — stale or unavailable.")
    return " ".join(lines)


def matches(utterance):
    """Deterministic 'situation report' matcher — no LLM. Does not overlap the
    diagnostics ('check yourself'), OTA ('check for updates'), or drive matchers."""
    return bool(_REPORT_RE.search(utterance or ""))


def assemble(now_ms, gathers, operator, ttls=None):
    """Build a WorldModel from injected gather callables (pure: no HTTP here).
    `gathers` maps field-name → a zero-arg callable returning a string; a string
    that `is_source_failure` leaves the field UNSET → UNKNOWN (fail-closed). This
    is the seam the live report drives with rabbit_ask's real gathers."""
    ttls = ttls or DEFAULT_TTLS
    wm = WorldModel()
    for name, gather in gathers.items():
        try:
            text = gather()
        except Exception:  # noqa: BLE001 — a broken source is UNKNOWN, never a crash
            continue
        if isinstance(text, str) and text.strip() and not is_source_failure(text):
            wm.set(name, text.strip(), name, now_ms, ttls.get(name, 5_000))
    if operator:
        wm.set("operator", operator, "env", now_ms, ttls.get("operator", 3_600_000))
    return wm


# ── live seam (thin; not unit-tested — drives the pure core with real gathers) ─

def run_report():
    """Assemble from the live read-only grounding and render the spoken report."""
    try:
        import sys
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from rabbit_ask import gather_perception, gather_posture, gather_stop_reason
        from rabbit_persona import operator_name
    except Exception:  # noqa: BLE001
        return "I can't reach my situation sources right now."
    now_ms = int(time.time() * 1000)
    wm = assemble(now_ms, {
        "posture": gather_posture,
        "perception": gather_perception,
        "stop_reason": gather_stop_reason,
    }, operator_name())
    return render(wm.snapshot(now_ms))


def handle(utterance):
    """rabbit_diag.handle contract: the spoken report, or None if this isn't a
    situation-report request / the feature is off (→ falls through to the LLM)."""
    if not enabled() or not matches(utterance):
        return None
    return run_report()
