#!/usr/bin/env python3
"""voice_playback.py — the cross-process "the robot is SPEAKING" signal that
stops the wake listener from hearing the robot's own voice.

THE LIVE DEFECT THIS FIXES: the hybrid router spoke a reply, the wake
listener's follow-up mode reopened the mic mid-playback, Piper's output
crossed the RMS floor, and

    wake_word: follow-up: speech onset — firing without a wake word

fired a trigger on the robot's OWN speech — which was then recorded,
transcribed, routed to chat, answered aloud, and re-heard: an unbounded
self-conversation loop (≥15 turns on the R2 before Ctrl-C).

MECHANISM — an advisory `flock` held for the WHOLE playback lifetime:

    TTS side:  with PlaybackGuard():          # open + LOCK_EX
                   speak / stream sentences / drain audio / process wait
                   ...                        # hold through the cooldown
               # release on exit — ALWAYS, KeyboardInterrupt included

    listener:  playback_active(path)          # LOCK_EX|LOCK_NB probe
               → True  ⇒ discard the audio frame, no STT, no onset, no trigger
               → False ⇒ listen normally

WHY A LOCK AND NOT A MARKER FILE: the kernel releases an flock automatically
when its holder dies — a crashed/killed TTS process can NEVER leave a stale
"speaking=true" behind that would suppress listening forever. There is
exactly one holder at a time, probing is non-blocking, and no content is
ever written that needs cleanup.

The guard is REENTRANT within a process (a router turn wraps the shared chat
turn runner, which guards its own TTS): nested enters are counted, the lock
is taken once and released when the outermost exit runs.

The COOLDOWN is held INSIDE the guard (release happens after it), so the
speaker's acoustic tail decays while the listener is still suppressed; on
release the listener additionally resets its energy/ring accumulators so no
buffered speaker audio survives into the listening window.

🔴 Channel A only, ZERO authority (the turn_state.py stance): this signal
paces the MICROPHONE. It never starts motion, never touches /intent, and a
wrong value at worst suppresses or admits a listening window — never motion.
Fail-open for LISTENING (an unusable state path means the listener behaves
as before this fix), fail-quiet for SPEAKING (a broken path never blocks
speech).

Env:
  KIRRA_VOICE_PLAYBACK_STATE        state-file path. The user unit passes
                                    %t/kirra-voice-playback (systemd resolves
                                    %t to /run/user/<uid>). Unset → XDG_RUNTIME_DIR,
                                    else /dev/shm, else /tmp.
  KIRRA_VOICE_PLAYBACK_COOLDOWN_MS  post-playback tail discard, default 500,
                                    clamped to 0..3000 (invalid → default).
  KIRRA_VOICE_MAX_FOLLOWUP_TURNS    wake-free follow-up turns per episode,
                                    default 3, clamped to 0..10 (invalid →
                                    default; 0 = every turn needs a wake;
                                    "unlimited" does not exist).
"""
from __future__ import annotations

import fcntl
import os
import sys
import time

DEFAULT_STATE_BASENAME = "kirra-voice-playback"
DEFAULT_COOLDOWN_MS = 500
COOLDOWN_MAX_MS = 3000
DEFAULT_MAX_FOLLOWUPS = 3
MAX_FOLLOWUPS_CEILING = 10

_warned_paths: set[str] = set()


def _log(msg: str) -> None:
    print(f"voice_playback: {msg}", file=sys.stderr, flush=True)


def state_path() -> str:
    """The shared lock-file path. Precedence: env override → the user session
    runtime dir (tmpfs, per-user, wiped at logout) → /dev/shm → /tmp."""
    p = (os.environ.get("KIRRA_VOICE_PLAYBACK_STATE") or "").strip()
    if p:
        return p
    run = (os.environ.get("XDG_RUNTIME_DIR") or "").strip()
    if run and os.path.isdir(run):
        return os.path.join(run, DEFAULT_STATE_BASENAME)
    base = "/dev/shm" if os.path.isdir("/dev/shm") else "/tmp"
    return os.path.join(base, DEFAULT_STATE_BASENAME)


def parse_cooldown_ms(raw: str | None) -> int:
    """0..3000 ms; anything absent/invalid/out-of-range → the safe default."""
    try:
        v = int((raw or "").strip())
    except ValueError:
        return DEFAULT_COOLDOWN_MS
    if 0 <= v <= COOLDOWN_MAX_MS:
        return v
    return DEFAULT_COOLDOWN_MS


def parse_max_followups(raw: str | None) -> int:
    """0..10 follow-up turns; absent/invalid/out-of-range → the default (3).
    0 means every turn needs an explicit wake. There is NO unlimited value."""
    try:
        v = int((raw or "").strip())
    except ValueError:
        return DEFAULT_MAX_FOLLOWUPS
    if 0 <= v <= MAX_FOLLOWUPS_CEILING:
        return v
    return DEFAULT_MAX_FOLLOWUPS


def playback_active(path: str | None = None) -> bool:
    """Is some process currently holding the playback lock?

    Non-blocking probe: try LOCK_EX|LOCK_NB on the file; success means nobody
    holds it (we release immediately), failure means playback is live. An
    absent file means no playback. An UNUSABLE path fails OPEN for listening
    (as before this fix) with one actionable log line, never a crash."""
    p = path or state_path()
    try:
        fd = os.open(p, os.O_RDONLY)
    except FileNotFoundError:
        return False
    except OSError as e:
        if p not in _warned_paths:
            _warned_paths.add(p)
            _log(f"cannot probe playback state at {p} ({e}) — playback "
                 "suppression is INACTIVE; check KIRRA_VOICE_PLAYBACK_STATE "
                 "points into a writable per-user runtime dir")
        return False
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            return True          # someone holds it → the robot is speaking
        fcntl.flock(fd, fcntl.LOCK_UN)
        return False
    finally:
        os.close(fd)


class PlaybackGuard:
    """Hold the playback lock for the full lifetime of ONE spoken output,
    cooldown included. Re-entrant per process; never blocks or breaks speech
    on a bad path (fail-quiet with one actionable log line)."""

    _depth = 0          # process-wide nesting (the router wraps the chat runner)
    _fd: int | None = None

    def __init__(self, path: str | None = None, cooldown_ms: int | None = None):
        self.path = path or state_path()
        self.cooldown_ms = (parse_cooldown_ms(
            os.environ.get("KIRRA_VOICE_PLAYBACK_COOLDOWN_MS"))
            if cooldown_ms is None else cooldown_ms)

    def __enter__(self):
        cls = PlaybackGuard
        if cls._depth == 0:
            try:
                fd = os.open(self.path, os.O_RDWR | os.O_CREAT, 0o600)
                fcntl.flock(fd, fcntl.LOCK_EX)   # one speaker at a time
                cls._fd = fd
            except OSError as e:
                if self.path not in _warned_paths:
                    _warned_paths.add(self.path)
                    _log(f"cannot hold playback state at {self.path} ({e}) — "
                         "speaking WITHOUT suppression; check "
                         "KIRRA_VOICE_PLAYBACK_STATE")
                cls._fd = None
        cls._depth += 1
        return self

    def __exit__(self, exc_type, exc, tb):
        cls = PlaybackGuard
        cls._depth -= 1
        if cls._depth == 0 and cls._fd is not None:
            try:
                # The cooldown lives INSIDE the lock: the speaker tail decays
                # while the listener is still suppressed.
                if self.cooldown_ms > 0 and exc_type is not KeyboardInterrupt:
                    time.sleep(self.cooldown_ms / 1000.0)
            finally:
                try:
                    fcntl.flock(cls._fd, fcntl.LOCK_UN)
                except OSError:
                    pass
                try:
                    os.close(cls._fd)
                except OSError:
                    pass
                cls._fd = None
        return False    # never swallow exceptions — shutdown must propagate


def should_emit_trigger(*, playback: bool, episode_followups: int,
                        max_followups: int) -> bool:
    """The ONE pipeline invariant, as a pure function (host-tested):
    no trigger while the robot is speaking (or its cooldown tail is held),
    and no wake-free follow-up beyond the episode cap."""
    if playback:
        return False
    return episode_followups < max_followups
