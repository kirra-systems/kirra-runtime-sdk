#!/usr/bin/env python3
"""barge_in.py — interruptible speech + a priority arbiter (voice-cognition
Slice 2, opt-in). Lets a higher-priority event CUT Rabbit's current spoken reply
so it stops talking and listens, instead of blocking until the sentence finishes.

The live producer today is the **PTT button**: pressing it raises a barge-in
signal (a cross-process epoch file), and an in-progress conversational reply
polls that signal and stops. The button is independent of the mic hold-off, so
this works even while Rabbit is speaking. (Acoustic "say the wake word over
Rabbit" needs full-duplex audio / a shorter hold-off — a documented follow-up.)

🔴 SAFETY / DISCIPLINE — Channel A only, zero authority:
  * A barge-in only STOPS speech (cosmetic) — it never starts motion, emits an
    intent, or touches the fenced /intent door. Cutting a reply early is always
    safe.
  * The signal is a local epoch FILE (like rabbit_wake.py's control file), never
    stdout — the ptt_button / wake_word stdout trigger contract is preserved.
  * Fail-soft everywhere: a missing/corrupt signal file reads as "no barge-in"
    (epoch 0); a TTS/playback error is swallowed (speech is never load-bearing).
  * OFF by default: unless KIRRA_BARGE_IN_ENABLED=1, replies use the plain
    blocking speak() and producers never raise a signal — byte-identical.

Priority model (the architecture doc's event priorities):
  P0 e-stop  >  P1 {human-interrupt, wake, obstacle}  >  P2 mission  >  P3 info-speech
A conversational reply is P3, so any raised barge-in (P0/P1) cuts it.

Env:
  KIRRA_BARGE_IN_ENABLED  1/true/yes/on to arm; else off (default)
  KIRRA_BARGE_IN_FILE     epoch signal file (default /tmp/kirra_rabbit_bargein.epoch)

CLI:  python3 robot/barge_in.py --signal    # raise a barge-in (e-stop / event source)
"""
import os
import subprocess
import sys
import time

# Priority levels — LOWER number = MORE urgent (preempts).
P0_ESTOP = 0
P1_INTERRUPT = 1   # human interrupt / wake / obstacle
P2_MISSION = 2
P3_INFO = 3        # conversational / informational speech

DEFAULT_SIGNAL_FILE = "/tmp/kirra_rabbit_bargein.epoch"


def log(msg):
    print(f"barge_in: {msg}", file=sys.stderr, flush=True)


# ── pure core (host-tested; no process I/O) ──────────────────────────────────

def enabled():
    """Armed only on an explicit affirmative (fail-closed: unset/typo → off)."""
    return (os.environ.get("KIRRA_BARGE_IN_ENABLED") or "").strip().lower() \
        in ("1", "true", "yes", "on")


def signal_path():
    return (os.environ.get("KIRRA_BARGE_IN_FILE") or "").strip() or DEFAULT_SIGNAL_FILE


def should_interrupt(current_priority, incoming_priority):
    """A strictly MORE urgent event preempts current speech. Equal or lower does
    not (info-speech never interrupts info-speech; a P3 event never cuts P1)."""
    return incoming_priority < current_priority


def read_epoch(path):
    """Current barge-in epoch (monotonic counter). Absent/unreadable/corrupt → 0
    (fail-safe: 'no barge-in', never a crash)."""
    try:
        with open(path, "r", encoding="utf-8") as f:
            return int(f.read().strip() or "0")
    except (OSError, ValueError):
        return 0


def raise_barge_in(path):
    """Advance the epoch → any speaker baselined below it will cut. Atomic write
    (temp + rename) so a reader never sees a torn value. Returns the new epoch."""
    nxt = read_epoch(path) + 1
    tmp = f"{path}.tmp.{os.getpid()}"
    try:
        with open(tmp, "w", encoding="utf-8") as f:
            f.write(str(nxt))
        os.replace(tmp, path)
    except OSError as e:
        log(f"could not raise barge-in ({e})")
        try:
            os.unlink(tmp)
        except OSError:
            pass
        return read_epoch(path)
    return nxt


def make_file_cancel_check(path, baseline):
    """A predicate that becomes True once the epoch at `path` advances beyond
    `baseline` — i.e. a barge-in was raised AFTER this speech started (a signal
    left over from before is captured in the baseline and does NOT false-cut)."""
    return lambda: read_epoch(path) > baseline


# ── interruptible speak (the thin process seam; killable playback) ───────────

def speak_interruptible(text, tts_argv, cancel_check=None, poll_ms=100):
    """Speak `text` via `tts_argv` (argv list, text on STDIN — never a shell), but
    poll `cancel_check()` while it plays and KILL playback the moment it returns
    True. Returns True iff the speech was cut. Fail-soft: any error → returns
    False (speech is cosmetic). Prints the line first (parity with speak())."""
    print(text)
    if not tts_argv:
        return False
    if cancel_check is None:
        cancel_check = lambda: False  # noqa: E731
    try:
        proc = subprocess.Popen(tts_argv, stdin=subprocess.PIPE,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except Exception as e:  # noqa: BLE001
        log(f"tts failed to start: {e}")
        return False
    try:
        try:
            if proc.stdin:
                proc.stdin.write(text.encode())
                proc.stdin.close()
        except (BrokenPipeError, OSError):
            pass  # a TTS that ignores/closes stdin is fine
        poll_s = max(0.005, poll_ms / 1000.0)
        while True:
            if proc.poll() is not None:
                return False  # finished on its own
            if cancel_check():
                proc.terminate()
                try:
                    proc.wait(timeout=0.5)
                except Exception:  # noqa: BLE001
                    proc.kill()
                return True    # barged in
            time.sleep(poll_s)
    except Exception as e:  # noqa: BLE001
        log(f"tts playback error: {e}")
        try:
            proc.kill()
        except Exception:  # noqa: BLE001
            pass
        return False


def main(argv):
    if "--signal" in argv:
        epoch = raise_barge_in(signal_path())
        log(f"barge-in raised (epoch {epoch}) at {signal_path()}")
        return 0
    log("usage: barge_in.py --signal   (raise a barge-in for the current speaker)")
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
