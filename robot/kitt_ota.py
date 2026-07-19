#!/usr/bin/env python3
"""kitt_ota.py — the KITT "check for update" / "apply update" voice actions.

WHY this is a SEPARATE path from the KITT movement router: an OTA check is a
SYSTEM command, not a movement. KITT's conversational router only ever hands
*movement* directives to the ONE fenced door (mick POST /intent). OTA must NOT
ride that door — it runs the local `kirra-ota-ctl` instead, matched
DETERMINISTICALLY (keyword, not LLM) so an ambiguous utterance can never be
mistaken for "install software", and vice-versa.

SECURITY: this only TRIGGERS the governed OTA flow. The artifact is still
SHA-256 + Uptane verified and health-gated by `kirra-ota-ctl` regardless of who
asked — voice is a convenience, never an authorization bypass. "Stage and ask":
`check` only STAGES (never commits); `apply` runs the HEALTH-GATED probe
(auto-rollback on an unhealthy trial) — never a blind commit.

Importable (the router calls `handle(text)`), or runnable standalone:
    python3 robot/kitt_ota.py check     # stage-only pull
    python3 robot/kitt_ota.py status
    python3 robot/kitt_ota.py apply      # health-gated commit/rollback of a trial
"""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys

# The ota-ctl binary reads its OWN config from env (verifier URL, node id, slot
# paths) — we do NOT duplicate that here, we just invoke it.
OTA_CTL = os.environ.get("KIRRA_OTA_CTL") or shutil.which("kirra-ota-ctl") or "/opt/kirra/bin/kirra-ota-ctl"
_TIMEOUT_S = float(os.environ.get("KIRRA_OTA_CTL_TIMEOUT_S", "120"))


def _run(*args: str) -> tuple[int, str]:
    """Invoke kirra-ota-ctl; return (rc, combined_output). rc=127 if absent."""
    if not (os.path.isfile(OTA_CTL) or shutil.which(OTA_CTL)):
        return 127, f"{OTA_CTL} not found"
    try:
        p = subprocess.run(
            [OTA_CTL, *args], capture_output=True, text=True, timeout=_TIMEOUT_S
        )
        return p.returncode, (p.stdout + p.stderr).strip()
    except subprocess.TimeoutExpired:
        return 124, "timed out"
    except Exception as e:  # noqa: BLE001 — never crash the voice loop
        return 1, f"{type(e).__name__}: {e}"


# --- classification (heuristic on rc + output; conservative) ----------------
_STAGED_RX = re.compile(r"\b(stag|download|new digest|trying|armed|pending)\b", re.I)
_CURRENT_RX = re.compile(r"\b(up[- ]to[- ]date|no assignment|unchanged|already|current|no update)\b", re.I)


def check() -> tuple[str, str]:
    """Stage-only `pull`. Returns (state, spoken). state in
    {'staged','current','error'}. NEVER commits."""
    rc, out = _run("pull")
    if rc == 127:
        return "error", "My update tool isn't installed, so I can't check right now."
    if rc != 0:
        return "error", "I couldn't reach the update service just now — I'll stay on my current version."
    if _STAGED_RX.search(out) and not _CURRENT_RX.search(out):
        return "staged", ("There's a new version. I've downloaded and verified it and it's "
                          "staged, ready to go. Say 'apply update' when you want me to install it.")
    if _CURRENT_RX.search(out):
        return "current", "I checked — you're on the latest version, nothing to update."
    # Unknown-but-success: fall back to the authoritative slot state.
    st, _ = _run("status")
    return ("staged" if _STAGED_RX.search(st) else "current"), (
        "Update check complete. Say 'update status' if you'd like the details.")


def status() -> tuple[str, str]:
    rc, out = _run("status")
    if rc == 127:
        return "error", "My update tool isn't installed."
    if rc != 0:
        return "error", "I couldn't read my update status."
    staged = bool(_STAGED_RX.search(out))
    return ("staged" if staged else "current",
            "An update is staged and waiting — say 'apply update' to install." if staged
            else "No update is staged; you're current.")


def apply() -> tuple[str, str]:
    """HEALTH-GATED apply: `probe` runs the consecutive-success gate and auto
    commit-or-rollback. Never a blind commit — a bad artifact rolls back."""
    rc, out = _run("probe")
    if rc == 127:
        return "error", "My update tool isn't installed, so I can't apply anything."
    if rc != 0:
        # probe fails/rolls back an unhealthy trial, or there's no trial to apply.
        if re.search(r"\b(no trial|not in a trial|nothing to|idle)\b", out, re.I):
            return "none", "There's nothing staged to apply right now. Say 'check for update' first."
        return "error", "The update didn't pass its health check, so I rolled back to the safe version."
    return "ok", "Update applied and health-checked — I'm running the new version now."


# --- deterministic command matcher (NOT the LLM / NOT the movement door) -----
_CHECK_RX = re.compile(r"\b(check\s+for\s+(an?\s+)?updates?|any\s+updates?|are\s+you\s+up\s+to\s+date)\b", re.I)
_APPLY_RX = re.compile(r"\b(apply|install|confirm)\s+(the\s+)?updates?\b", re.I)
_STATUS_RX = re.compile(r"\bupdate\s+status\b", re.I)


def match_command(text: str):
    """Return 'apply' | 'status' | 'check' | None. Apply/status matched before
    check so 'apply the update' isn't caught by a loose 'update' in check."""
    if _APPLY_RX.search(text):
        return "apply"
    if _STATUS_RX.search(text):
        return "status"
    if _CHECK_RX.search(text):
        return "check"
    return None


def handle(text: str) -> "str | None":
    """If `text` is an OTA voice command, run it and return the spoken reply;
    else None (the caller falls through to the normal conversation path)."""
    cmd = match_command(text)
    if cmd is None:
        return None
    if cmd == "check":
        return check()[1]
    if cmd == "status":
        return status()[1]
    if cmd == "apply":
        return apply()[1]
    return None


if __name__ == "__main__":
    action = sys.argv[1] if len(sys.argv) > 1 else "check"
    fn = {"check": check, "status": status, "apply": apply}.get(action)
    if fn is None:
        sys.exit("usage: kitt_ota.py [check|status|apply]")
    state, spoken = fn()
    print(f"[{state}] {spoken}")
    sys.exit(0 if state in ("staged", "current", "ok", "none") else 1)
