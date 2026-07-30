#!/usr/bin/env python3
"""repo_command.py — the voice bridge to the Robot Command Language.

Turns an operator utterance into ONE of two allow-listed repository intents,
runs the DETERMINISTIC executor (`scripts/robot-command.sh`), and renders the
structured result as a spoken sentence.

🔴 THE INVARIANT
   **Gemma interprets and explains. The deterministic command executor
   authorizes and acts.**

   Gemma may pick an intent NAME from a two-item allow-list. It never supplies a
   command, an argument, a path, a branch, or a flag — these intents take no
   parameters at all, so there is no channel through which model text can reach
   the executor. `run_intent` builds a fixed argv and calls it with
   `shell=False`; `INTENT_PHRASE` is the complete mapping and an intent outside
   it raises before anything runs.

WHY A DETERMINISTIC MATCHER FIRST
   `resolve_intent` recognizes the canonical phrases and a CLOSED set of
   conservative paraphrases without consulting the model at all. The LLM is the
   fallback for wording the matcher does not cover, and the explainer for
   results — it is never the thing that decides an ambiguous case. Two intents
   matching, or none, is a clarification question, never a guess.

WHAT THIS MODULE NEVER DOES
   No shell (`shell=False`, fixed argv). No `git` mutation of its own — the
   executor owns every repository operation, and its refusals pass through
   verbatim. No success claimed unless the executor exited 0.

Pure except for the injected `runner`, so every mapping is host-tested in
`robot/repo_command_test.py` with no git, no LLM, and no audio.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

# ── the allow-list ───────────────────────────────────────────────────────────
# The ONLY repository intents that exist. Adding a row here is the deliberate,
# reviewable act of granting the voice layer a new capability.
SYNC_TO_MAIN = "sync_to_main"
PUBLISH_MY_WORK = "publish_my_work"

# intent → the exact phrase handed to scripts/robot-command.sh. This dict IS the
# allow-list boundary: `run_intent` refuses any key not present, so no string
# from the model or the transcript can become an executable argument.
INTENT_PHRASE = {
    SYNC_TO_MAIN: "Sync to main",
    PUBLISH_MY_WORK: "Publish my work",
}

REPO_INTENTS = tuple(sorted(INTENT_PHRASE))

# The executor, resolved relative to this file so it cannot be redirected by cwd.
EXECUTOR = Path(__file__).resolve().parent.parent / "scripts" / "robot-command.sh"

# ── result statuses ──────────────────────────────────────────────────────────
SUCCESS = "success"
REFUSED = "refused"   # a precondition the executor enforces (safe, expected)
ERROR = "error"       # the operation was attempted and failed
STATUSES = (SUCCESS, REFUSED, ERROR)

# Executor exit code → (status, machine reason). Mirrors the table in
# docs/ROBOT_COMMAND_LANGUAGE.md; a code we do not know is an ERROR, never a
# success (fail-closed on an executor we no longer fully understand).
EXIT_MAP = {
    0:  (SUCCESS, "ok"),
    2:  (ERROR,   "usage_error"),
    3:  (ERROR,   "not_a_git_worktree"),
    # The executor uses one code for "no origin remote" and "fetch failed".
    # Both mean the operation did not happen, so both surface as an error.
    4:  (ERROR,   "no_origin_or_fetch_failed"),
    5:  (REFUSED, "working_tree_dirty"),
    6:  (ERROR,   "no_remote_main"),
    7:  (REFUSED, "not_fast_forwardable"),
    8:  (REFUSED, "detached_head"),
    9:  (REFUSED, "on_main"),
    10: (REFUSED, "no_commits"),
    11: (ERROR,   "push_failed"),
    12: (ERROR,   "verification_failed"),
}

# ── natural-language understanding (deterministic, closed vocabulary) ────────
#
# Conservative on purpose: each pattern is a phrase an operator plausibly says
# for THIS command and for nothing else. There is no general Git grammar here —
# "rebase onto main", "merge main", "reset to origin" match NOTHING and become a
# clarification question rather than a creative interpretation.
_SYNC_PATTERNS = (
    r"\bsync (?:up )?to main\b",
    r"\bsync main\b",
    r"\b(?:get|bring|put) (?:us|me|this|the workspace) (?:on)?to (?:the )?latest main\b",
    r"\bget (?:us|me) on(?:to)? (?:the )?latest main\b",
    r"\bupdate the (?:workspace|repo|repository) to main\b",
    r"\bget the latest origin[ /]main\b",
    r"\bget (?:the )?latest main\b",
    r"\bprepare the (?:repo|repository|workspace) for new work\b",
    r"\bsynchron(?:ise|ize) (?:with |to )?(?:origin[ /])?main\b",
)
_PUBLISH_PATTERNS = (
    r"\bpublish my work\b",
    r"\bpublish (?:this|the current|my) branch\b",
    r"\bpush my (?:current )?work\b",
    r"\bpush (?:this|the current) (?:feature )?branch\b",
    r"\bpush the current feature branch\b",
    r"\bmake sure (?:this|the current|my) branch is on origin\b",
    r"\bget (?:this|my) branch (?:up )?on(?:to)? origin\b",
    r"\bupload my work\b",
)

_SYNC_RE = tuple(re.compile(p) for p in _SYNC_PATTERNS)
_PUBLISH_RE = tuple(re.compile(p) for p in _PUBLISH_PATTERNS)

# Wake prefixes are stripped before matching, so "Hey Parker, sync to main"
# resolves identically to "sync to main". The wake phrase ACTIVATES the
# assistant; it never changes what is permitted.
_WAKE_PREFIX_RE = re.compile(
    r"^\s*(?:hey|hello|hi|yo|ok(?:ay)?)\s+(?:rabbit|parker)\s*[,.!:;-]*\s*",
    re.IGNORECASE,
)

# Decisions the resolver can return.
NO_MATCH = "no_match"
AMBIGUOUS = "ambiguous"


def normalize(text) -> str:
    """Lowercase, strip a wake prefix, drop punctuation, collapse whitespace.

    Case and harmless punctuation must not change the decision: STT output is
    inconsistently capitalized and punctuated, and "Sync to main." is the same
    request as "sync to main".
    """
    if not isinstance(text, str):
        return ""
    s = _WAKE_PREFIX_RE.sub("", text)
    s = s.lower()
    s = re.sub(r"[^a-z0-9/' ]+", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def resolve_intent(text):
    """Utterance → `(intent, decision)`.

    - a single family matches → `(intent, "matched")`
    - both families match     → `(None, AMBIGUOUS)` — ask, never pick
    - nothing matches         → `(None, NO_MATCH)`

    Ambiguity is a real case, not a theoretical one: "sync and publish" names
    two state transitions with different consequences, so the assistant must ask
    which one rather than silently ordering them.
    """
    norm = normalize(text)
    if not norm:
        return None, NO_MATCH
    sync = any(r.search(norm) for r in _SYNC_RE)
    publish = any(r.search(norm) for r in _PUBLISH_RE)
    if sync and publish:
        return None, AMBIGUOUS
    if sync:
        return SYNC_TO_MAIN, "matched"
    if publish:
        return PUBLISH_MY_WORK, "matched"
    return None, NO_MATCH


# ── execution ────────────────────────────────────────────────────────────────

def _default_runner(argv):
    """Run a fixed argv with NO shell. Returns `(rc, stdout, stderr)`."""
    try:
        p = subprocess.run(  # noqa: S603 — fixed argv, shell=False, no user string
            argv, shell=False, capture_output=True, text=True, timeout=180,
        )
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return 124, "", "executor timed out"
    except OSError as e:
        return 126, "", f"could not run executor: {e}"


def _git_facts(runner):
    """Branch + full/short SHA via plumbing. `(branch, sha, short)`; '' on failure.

    Queried from git rather than scraped out of the executor's human-facing
    report — the report is for the operator, plumbing is for the machine.
    """
    def one(args):
        rc, out, _ = runner(["git"] + args)
        return out.strip() if rc == 0 else ""
    return (
        one(["symbolic-ref", "--quiet", "--short", "HEAD"]),
        one(["rev-parse", "HEAD"]),
        one(["rev-parse", "--short", "HEAD"]),
    )


def _changed_files(runner):
    """Paths git reports as changed or untracked. Machine format, so parsing is
    correct here (`--porcelain=v1` is a documented stable contract)."""
    rc, out, _ = runner(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"])
    if rc != 0:
        return []
    files = []
    for line in out.splitlines():
        if len(line) > 3:
            files.append(line[3:].strip())
    return files


def run_intent(intent, runner=None):
    """Execute ONE allow-listed intent and return the structured result.

    Raises `KeyError` for an intent outside `INTENT_PHRASE` — the allow-list is
    enforced here, before any process starts, so an unregistered name cannot
    reach the executor even if a caller passes one.
    """
    if intent not in INTENT_PHRASE:
        raise KeyError(f"intent not in the allow-list: {intent!r}")
    run = runner or _default_runner

    # A fixed argv: the interpreter, the resolved script path, and the phrase
    # from the allow-list. Nothing here is built from a transcript or a model
    # reply, and `shell=False` means no metacharacter is ever interpreted.
    argv = ["bash", str(EXECUTOR), INTENT_PHRASE[intent]]
    rc, out, err = run(argv)

    status, reason = EXIT_MAP.get(rc, (ERROR, f"unexpected_exit_{rc}"))
    result = {
        "command": intent,
        "status": status,
        "reason": reason,
        "exit_code": rc,
    }
    if status == SUCCESS:
        branch, sha, short = _git_facts(run)
        result["branch"] = branch
        result["commit_sha"] = sha
        result["commit_short"] = short
        result["message"] = (
            "Workspace is synchronized and ready for new work."
            if intent == SYNC_TO_MAIN else
            "Branch is published and verified on origin."
        )
    else:
        if reason == "working_tree_dirty":
            result["changed_files"] = _changed_files(run)
        # The executor's own refusal line, for the operator and the audit trail.
        detail = (err or out or "").strip().splitlines()
        result["detail"] = detail[0] if detail else ""
    return result


# ── spoken rendering ─────────────────────────────────────────────────────────
#
# Distinct wording per outcome class. A refusal must never sound like a success:
# the operator's next action depends on which one it was.

_REFUSAL_SENTENCE = {
    "working_tree_dirty":
        "I didn't {verb} because the working tree has uncommitted changes",
    "not_fast_forwardable":
        "I didn't sync because local main has diverged from origin and can't fast-forward",
    "detached_head":
        "I didn't publish because HEAD is detached, so there's no branch to push",
    "on_main":
        "I didn't publish because we're on main — move the work to a feature branch first",
    "no_commits":
        "I didn't publish because this branch has no commits yet",
}

_ERROR_SENTENCE = {
    "not_a_git_worktree": "I'm not inside a git worktree, so the repository was not changed",
    "no_origin_or_fetch_failed": "the fetch failed, so the repository was not changed",
    "no_remote_main": "origin has no main branch, so nothing was changed",
    "push_failed": "the push failed, so remote history was not changed",
    "verification_failed": "I couldn't verify the result afterwards, so treat the state as unknown",
    "usage_error": "I couldn't invoke the repository command correctly, so nothing ran",
}


def speak_result(result) -> str:
    """Structured result → one spoken sentence. Never claims success unless
    `status == SUCCESS`."""
    if not isinstance(result, dict):
        return "Something went wrong reading the command result, so I can't confirm anything."
    intent = result.get("command")
    status = result.get("status")
    verb = "sync" if intent == SYNC_TO_MAIN else "publish the branch"

    if status == SUCCESS:
        short = result.get("commit_short") or result.get("commit_sha") or ""
        branch = result.get("branch") or ""
        if intent == SYNC_TO_MAIN:
            base = f"Main is synchronized with origin at commit {short}" if short \
                else "Main is synchronized with origin"
            return base + ", ready for new work."
        base = f"Published {branch} to origin at commit {short}" if branch and short \
            else "The branch is published to origin"
        return base + "."

    reason = result.get("reason", "")
    if status == REFUSED:
        text = _REFUSAL_SENTENCE.get(reason)
        if text is None:
            return f"I didn't {verb} — the repository command refused it."
        text = text.format(verb=verb)
        files = result.get("changed_files") or []
        if files:
            shown = ", ".join(files[:3])
            more = f", and {len(files) - 3} more" if len(files) > 3 else ""
            return f"{text}: {shown}{more}."
        return text + "."

    return (_ERROR_SENTENCE.get(reason)
            or f"the repository command failed ({reason})").capitalize() + "."


def clarification_question() -> str:
    """Asked when two intents match. Names both so the operator can just answer."""
    return "Do you want me to sync to main or publish the current branch?"


def unknown_command_reply() -> str:
    return "I didn't recognize that as an approved repository command."


# ── audit trail ──────────────────────────────────────────────────────────────

def audit_record(*, transcript, intent, decision, executed, result=None,
                 wake_identity=None, now_ms=None):
    """One auditable JSON record per request.

    Deliberately excluded: environment contents, tokens, credentials, remote
    URLs (a URL can embed a token). The transcript IS included because it is the
    operator's own words and is what makes a decision reviewable — the wake
    listener's privacy rule (never log continuous audio) is unaffected, since
    this is only the post-wake command utterance.
    """
    rec = {
        "ts_ms": int(now_ms if now_ms is not None else time.time() * 1000),
        "kind": "repo_command",
        "wake_identity": wake_identity or "unknown",
        "transcript": transcript if isinstance(transcript, str) else "",
        "intent": intent,
        "decision": decision,
        "executed": bool(executed),
    }
    if result is not None:
        rec["status"] = result.get("status")
        rec["reason"] = result.get("reason")
        rec["exit_code"] = result.get("exit_code")
        for k in ("branch", "commit_sha"):
            if result.get(k):
                rec[k] = result[k]
        if result.get("changed_files"):
            rec["changed_files"] = result["changed_files"]
    return rec


def append_audit(rec, path=None):
    """Append one record as JSONL. Opt-in: no path configured → no-op.

    Never raises into the voice loop — a full disk must not take the assistant
    down, and the spoken result is already the operator's primary feedback.
    """
    target = path or os.environ.get("KIRRA_REPO_CMD_AUDIT_PATH") or ""
    if not target:
        return False
    try:
        with open(target, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(rec, separators=(",", ":")) + "\n")
        return True
    except OSError as e:  # noqa: BLE001
        print(f"repo_command: audit append failed: {e}", file=sys.stderr)
        return False


# ── the one entry point the voice layer calls ────────────────────────────────

def handle_intent(intent, *, transcript="", wake_identity=None, runner=None,
                  audit_path=None):
    """Run an allow-listed intent, audit it, and return `(result, spoken)`.

    An intent outside the allow-list is REFUSED as a structured result — the
    voice layer gets a sentence to speak rather than an exception.
    """
    if intent not in INTENT_PHRASE:
        result = {"command": str(intent), "status": ERROR,
                  "reason": "not_an_approved_command", "exit_code": None}
        append_audit(audit_record(
            transcript=transcript, intent=str(intent), decision="rejected_not_allowlisted",
            executed=False, result=result, wake_identity=wake_identity), audit_path)
        return result, unknown_command_reply()

    result = run_intent(intent, runner=runner)
    append_audit(audit_record(
        transcript=transcript, intent=intent, decision="matched", executed=True,
        result=result, wake_identity=wake_identity), audit_path)
    return result, speak_result(result)


def enabled():
    """Armed only on an explicit affirmative (fail-closed: unset/typo → off).

    Same gate shape as `skill_registry.enabled()` / missions / world model. Off →
    `handle` returns None and the router is byte-identical to before.
    """
    return (os.environ.get("KIRRA_REPO_CMD_ENABLED") or "").strip().lower() \
        in ("1", "true", "yes", "on")


def handle(utterance, *, wake_identity=None, runner=None, audit_path=None):
    """DETERMINISTIC voice-command matcher, in the house `handle` shape.

    Returns the sentence to speak, or `None` to fall through to the LLM router.

    Placed before the model on purpose: a canonical phrase must not depend on
    inference being available or on the model choosing correctly. `None` is
    returned for anything this closed vocabulary does not recognize — including
    an utterance that merely mentions git — so ordinary conversation is
    unaffected. An AMBIGUOUS match returns the clarification question rather than
    falling through, because falling through would let the model resolve exactly
    the case the matcher refused to guess at.
    """
    if not enabled():
        return None
    intent, decision = resolve_intent(utterance)
    if intent is None:
        if decision == AMBIGUOUS:
            append_audit(audit_record(
                transcript=utterance, intent=None, decision=decision,
                executed=False, wake_identity=wake_identity), audit_path)
            return clarification_question()
        return None  # not a repository command → the LLM handles it
    _result, spoken = handle_intent(
        intent, transcript=utterance, wake_identity=wake_identity,
        runner=runner, audit_path=audit_path)
    return spoken


def handle_utterance(text, *, wake_identity=None, runner=None, audit_path=None):
    """Full transcript → `(result_or_None, spoken)`.

    Nothing executes unless exactly one intent matched: `NO_MATCH` and
    `AMBIGUOUS` both return `None` for the result, so a caller cannot mistake
    them for an attempted command.
    """
    intent, decision = resolve_intent(text)
    if intent is None:
        spoken = clarification_question() if decision == AMBIGUOUS else unknown_command_reply()
        append_audit(audit_record(
            transcript=text, intent=None, decision=decision, executed=False,
            wake_identity=wake_identity), audit_path)
        return None, spoken
    return handle_intent(intent, transcript=text, wake_identity=wake_identity,
                         runner=runner, audit_path=audit_path)
