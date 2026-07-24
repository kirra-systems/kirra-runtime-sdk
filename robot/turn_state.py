#!/usr/bin/env python3
"""turn_state.py — a tiny cross-process "a conversation turn is in progress"
signal (Slice R, reliability). It exists to fix ONE bug: the wake listener
re-arming its microphone on a BLIND FIXED TIMER instead of when the turn it
triggered actually finishes.

THE BUG it fixes (wake_word.py's old post-trigger `time.sleep(holdoff_s)`):
  A turn (record → STT → LLM → TTS) has VARIABLE duration, so a fixed hold-off
  cannot fit it. If the turn finished FAST, the operator's immediate follow-up
  "hey rabbit" landed in the still-closed dead window and was DROPPED — "it only
  answered one question". If the turn ran LONG, the mic reopened mid-reply and
  captured Rabbit's own voice. Event-driven re-arm (like a Nest/Alexa) settles
  both: the listener waits for the turn to SIGNAL done, then reopens at once.

WHO WRITES / WHO READS:
  * rabbit_converse.py (the turn handler) MARKS a turn active at the top of each
    turn and done when the reply finishes speaking — it spans exactly the
    LLM+TTS stretch the wake listener cannot otherwise see.
  * wake_word.py (the always-on listener) READS it after firing a trigger and
    waits for the turn to complete before reopening the mic.

🔴 SAFETY / DISCIPLINE — Channel A only, ZERO authority (identical stance to
   barge_in.py):
  * This signal only paces the microphone hold-off. It never starts motion,
    emits an intent, or touches the fenced /intent door. A wrong value at worst
    reopens the mic a little early/late — never an unsafe motion.
  * It is a local state FILE (atomic temp+rename), never stdout — the
    ptt_button / wake_word stdout trigger contract is untouched.
  * FAIL-SAFE everywhere: an absent/corrupt file reads as "idle, seq 0"; every
    write swallows its error. The listener's wait is always bounded by timers,
    so a crashed/old writer degrades to the legacy hold-off, never a wedge.

Format (JSON): {"active": <bool>, "seq": <int>}.
  seq is a monotonic "turns completed" counter — it advances on mark_done, so a
  reader that baselined seq at trigger time can detect "the turn I caused has
  finished" even if it never observed the brief active=True window.

Env:
  KIRRA_WAKE_TURN_STATE_FILE   shared path (default /tmp/kirra_rabbit_turn.state,
                               or /dev/shm/... when that tmpfs exists)
"""
from __future__ import annotations

import json
import os
import sys

DEFAULT_STATE_FILE = "kirra_rabbit_turn.state"


def log(msg: str) -> None:
    print(f"turn_state: {msg}", file=sys.stderr, flush=True)


def default_state_path() -> str:
    """Prefer the /dev/shm tmpfs (never hits disk) when present — parity with
    wake_word.py's transcribe-and-discard tmpfs choice — else /tmp."""
    base = "/dev/shm" if os.path.isdir("/dev/shm") else "/tmp"
    return os.path.join(base, DEFAULT_STATE_FILE)


def state_path() -> str:
    return (os.environ.get("KIRRA_WAKE_TURN_STATE_FILE") or "").strip() \
        or default_state_path()


# ── pure read/parse (host-tested; fail-safe) ─────────────────────────────────

def parse_state(text: str | None) -> tuple[bool, int]:
    """(active, seq) from the file's text. Absent/corrupt/partial → (False, 0):
    'no turn in progress, nothing completed' — the availability-safe default,
    correct here because the signal carries no actuation authority."""
    if not text:
        return False, 0
    try:
        j = json.loads(text)
        active = bool(j.get("active", False))
        seq = int(j.get("seq", 0))
        return active, (seq if seq >= 0 else 0)
    except Exception:  # noqa: BLE001 — corrupt file reads as idle, never a crash
        return False, 0


def read_state(path: str | None = None) -> tuple[bool, int]:
    """(active, seq) from `path` (default the shared file). Fail-safe → (False, 0)."""
    p = path or state_path()
    try:
        with open(p, "r", encoding="utf-8") as f:
            return parse_state(f.read())
    except OSError:
        return False, 0


# ── the write seam (atomic temp+rename; fail-soft) ───────────────────────────

def _write(path: str, active: bool, seq: int) -> None:
    tmp = f"{path}.tmp.{os.getpid()}"
    try:
        with open(tmp, "w", encoding="utf-8") as f:
            f.write(json.dumps({"active": active, "seq": seq}))
        os.replace(tmp, path)
    except OSError as e:
        log(f"could not write turn state ({e})")
        try:
            os.unlink(tmp)
        except OSError:
            pass


def mark_active(path: str | None = None) -> None:
    """A turn has STARTED handling. Keeps the current seq (it counts COMPLETED
    turns) and raises the active flag. Fail-soft."""
    p = path or state_path()
    _, seq = read_state(p)
    _write(p, True, seq)


def mark_done(path: str | None = None) -> int:
    """A turn has FINISHED (its reply is spoken). Lowers the active flag and
    ADVANCES seq so a listener baselined below it detects completion. Returns the
    new seq (0 on a write it could not read back). Fail-soft."""
    p = path or state_path()
    _, seq = read_state(p)
    nxt = seq + 1
    _write(p, False, nxt)
    return nxt


# ── the pure re-arm gate (host-tested — the whole point of Slice R) ──────────

def rearm_decision(elapsed_s: float, turn_active: bool, turn_advanced: bool,
                   *, grace_s: float, max_s: float) -> str:
    """Should the wake listener REOPEN its mic now, or keep WAITING?

    Called by wake_word.py in a poll loop AFTER it fired a trigger, until it
    returns 'reopen'. Pure so the whole re-arm behaviour is host-testable with
    no audio.

      elapsed_s     — seconds since the trigger fired
      turn_active   — a turn is CURRENTLY in progress (writer marked active,
                      not yet done)
      turn_advanced — the turn we triggered has COMPLETED (seq advanced past the
                      baseline captured at trigger time)
      grace_s       — how long to wait for a turn to even START (must cover the
                      record clip + STT before rabbit_converse marks active); if
                      nothing has started by then the clip was empty/garbage →
                      reopen. Also the reopen time when NO writer is signalling
                      (old/absent rabbit_converse) — the fail-safe fallback.
      max_s         — hard ceiling on the whole wait, so a writer that marked
                      active but never marked done (crash mid-turn) can't wedge
                      the listener shut forever.

    Returns 'reopen' or 'wait'. The ordering matters: a COMPLETED turn reopens
    immediately (no dead window → the follow-up "hey rabbit" is heard), and an
    in-progress turn is NEVER cut short by the grace window — only by max_s."""
    if turn_advanced:
        return "reopen"                       # turn finished → re-arm at once
    if elapsed_s >= max_s:
        return "reopen"                       # fail-safe ceiling (writer hung)
    if turn_active:
        return "wait"                         # a turn is running — let it finish
    if elapsed_s >= grace_s:
        return "reopen"                       # nothing ever started (garbage clip)
    return "wait"                             # still inside the wait-for-start grace
