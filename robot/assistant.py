#!/usr/bin/env python3
"""assistant.py — the Kirra Engineering Assistant seam.

🔴 **The assistant may propose broadly, but it may only observe and act through
   typed, policy-controlled tools.**

   Gemma interprets, reasons, and explains. Authoritative tools retrieve facts,
   enforce policy, and perform actions.

WHAT THIS MODULE IS
   transcript → role + typed tool request → `assistant_tools.run_tool` →
   `ToolResult` → a grounded, concise spoken answer.

   Classification is DETERMINISTIC and derives from the OPERATOR'S WORDS ONLY.
   That is the structural prompt-injection defence: retrieved file content flows
   into the *answer*, never into the dispatch decision, so there is no path from
   a malicious source comment to a tool call.

WHAT IT REFUSES
   Anything not in the closed pattern set executes NOTHING. There is no
   natural-language-to-shell path: a request to "run" something arbitrary is a
   refusal, not a translation. Ambiguity asks one narrow question.

GROUNDING
   `speak_result` branches on the tool's `status` FIRST, so success wording is
   unreachable unless the tool actually succeeded, and a `partial` result speaks
   its uncertainty aloud instead of guessing.

Pure (no HTTP, no LLM, no audio), so every policy decision is host-tested.
"""
from __future__ import annotations

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import assistant_tools as at  # noqa: E402

# ── roles: policy + context profiles, NOT personas ───────────────────────────
#
# There is deliberately no per-wake-word persona: the wake trigger's contract is
# one newline on stdout, which carries no identity, so "Hey Parker" and "Hey
# Rabbit" cannot select different authority. A role is inferred from the REQUEST.

ROLE_PURPOSE = {
    at.OPERATOR: "invoke approved workflows, report repository state, explain refusals",
    at.ENGINEER: "inspect source, explain components, trace dependencies, read failures",
    at.ARCHITECT: "explain component ownership and authority boundaries",
    at.SAFETY: "surface contracts, invariants, hazards, evidence, acceptance",
}

# ── decisions ────────────────────────────────────────────────────────────────
NO_MATCH = "no_match"
AMBIGUOUS = "ambiguous"
REFUSED_SHELL = "refused_shell_request"
MATCHED = "matched"

SPOKEN_MAX_CHARS = 420  # a voice answer must be short enough to follow aloud

_WAKE_PREFIX_RE = re.compile(
    r"^\s*(?:hey|hello|hi|yo|ok(?:ay)?)\s+(?:rabbit|parker)\s*[,.!:;-]*\s*",
    re.IGNORECASE)


def normalize(text):
    """Lowercase, strip a wake prefix, drop punctuation, collapse whitespace."""
    if not isinstance(text, str):
        return ""
    s = _WAKE_PREFIX_RE.sub("", text).lower()
    s = re.sub(r"[^a-z0-9_./:*'\- ]+", " ", s)
    return re.sub(r"\s+", " ", s).strip()


# A request to execute something arbitrary is REFUSED, never translated. This is
# checked before any tool pattern so "run rm -rf /" can never be creatively
# reinterpreted as a search for the word "run".
# Checked against the RAW utterance, BEFORE normalization: normalize() strips
# shell metacharacters, so running this afterwards would blind the guard to
# exactly the characters that make something a shell request ("eval $(...)").
_SHELL_REQUEST_RE = re.compile(
    # a shell verb followed by something command-shaped
    r"\b(?:run|execute|exec|shell|sudo|eval|spawn|invoke)\b[^.?!]*"
    r"(?:command|shell|script|\brm\b|curl|wget|chmod|systemctl|reboot|kill)"
    # a bare interpreter invocation, with or without flags
    r"|\b(?:bash|sh|zsh|python3?|perl|node)\b\s+-\w"
    r"|\b(?:bash|sh|zsh)\s+-c\b"
    # unambiguous shell shapes
    r"|\brm\s+-rf\b|\bsudo\b|\bcurl\b\s*http|\|\s*(?:sh|bash)\b"
    r"|`[^`]*`|\$\(",
    re.IGNORECASE)

# ── the closed classifier vocabulary ─────────────────────────────────────────
#
# Conservative on purpose. A phrase earns a row only if it plausibly means THIS
# and nothing else. There is no general grammar: an unmatched question falls
# through honestly rather than being coerced into the nearest tool.

_STATUS_PATTERNS = (
    r"\bwhat branch\b", r"\bwhich branch\b", r"\bcurrent branch\b",
    r"\bis the (?:workspace|worktree|repo|repository) clean\b",
    r"\bare we clean\b", r"\bwhat changed locally\b", r"\bwhat.s changed\b",
    r"\bare we ahead\b", r"\bahead of origin\b", r"\bbehind origin\b",
    r"\brepo(?:sitory)? status\b", r"\bshow me the repo(?:sitory)? status\b",
    r"\bgit status\b", r"\bwhere are we\b",
)
_SEARCH_PATTERNS = (
    r"\bwhere is\b", r"\bwhere.s\b", r"\bfind (?:the|a|me)\b", r"\bfind where\b",
    r"\bwhich (?:file|component|crate) (?:has|owns|contains|defines)\b",
    r"\bwho owns\b", r"\bsearch (?:for|the repo)\b", r"\blocate\b",
    r"\bwhere do we\b", r"\bwhere does\b",
)
_READ_PATTERNS = (
    r"\bread\b", r"\bshow me (?:the )?(?:file|source|code)\b", r"\bopen the file\b",
    r"\bwhat.s in\b",
)
_COMPONENT_PATTERNS = (
    r"\bexplain (?:this|the) (?:rust )?(?:crate|component|package|module|node)\b",
    r"\bexplain (?:the )?[a-z0-9_./-]+ (?:rust )?(?:crate|component|package|module|node)\b",
    r"\bexplain (?:the )?(?:crate|component|package|module|node) [a-z0-9_./-]+\b",
    r"\bwhat does .* depend on\b", r"\bdependencies of\b",
    r"\binspect (?:the )?(?:crate|component|package)\b",
    r"\btell me about (?:the )?(?:crate|component|package)\b",
)
_FAILURE_PATTERNS = (
    r"\bsummar(?:ize|ise) (?:the |this |that )?(?:latest )?test failure\b",
    r"\bwhy did (?:the )?test(?:s)? fail\b", r"\bwhat (?:test )?failed\b",
    r"\bexplain (?:this|the) (?:test )?failure\b",
    r"\bwhich component (?:likely )?owns this error\b",
)
# Runtime questions this slice cannot answer. Matched explicitly so the
# assistant says "I have no runtime tool" instead of falling through to a search
# and implying a runtime fact from repository text.
_RUNTIME_PATTERNS = (
    r"\bis the robot (?:healthy|ok|running)\b", r"\brobot health\b",
    r"\bros (?:graph|nodes|topics)\b", r"\bwhat nodes are running\b",
    r"\bcpu (?:usage|load)\b", r"\bmemory usage\b", r"\bis .* service running\b",
    r"\bmission state\b", r"\bcurrent posture\b",
)


def _any(patterns, text):
    return any(re.search(p, text) for p in patterns)


def _extract_quoted_or_tail(text, keywords):
    """Best-effort subject extraction from the operator's own words.

    Only ever used to build a tool ARGUMENT, and every tool validates its own
    arguments — so a bad extraction produces a refusal, not an injection.
    """
    m = re.search(r"['\"]([^'\"]{1,120})['\"]", text)
    if m:
        return m.group(1).strip()
    t = text
    for kw in keywords:
        idx = t.find(kw)
        if idx >= 0:
            t = t[idx + len(kw):]
            break
    t = re.sub(r"^\s*(?:the|a|an|is|are|in|for|to|of|this|that)\b\s*", "", t.strip())
    t = re.sub(r"\b(?:enforced|defined|implemented|located|handled|created)\b.*$", "", t)
    return t.strip(" ?.!,")


#: A subject that is just a stop-word carries no query.
_EMPTY_SUBJECTS = {"", "it", "this", "that", "there", "here", "them"}

Request = None  # (documented shape below; a plain dict keeps it JSON-native)


def classify(utterance, *, role=None):
    """transcript → `(request, decision)`.

    `request` is `{"role", "tool", "arguments"}` or `None`. `decision` is one of
    `MATCHED` / `AMBIGUOUS` / `NO_MATCH` / `REFUSED_SHELL` / `"runtime_unavailable"`.

    Nothing executes here — this only selects. `run_request` executes.
    """
    raw = utterance if isinstance(utterance, str) else ""
    norm = normalize(utterance)
    if not norm:
        return None, NO_MATCH

    # 1. An arbitrary-execution request is refused outright — checked on the RAW
    #    text so metacharacters survive to be seen.
    if _SHELL_REQUEST_RE.search(raw):
        return None, REFUSED_SHELL

    # 2. The Robot Command Language keeps its own deterministic resolver as the
    #    authority on its two phrases — no second implementation here.
    import repo_command
    rcl_intent, rcl_decision = repo_command.resolve_intent(utterance)
    if rcl_decision == repo_command.AMBIGUOUS:
        return None, AMBIGUOUS
    if rcl_intent is not None:
        return ({"role": at.OPERATOR, "tool": rcl_intent,
                 "arguments": {"transcript": utterance}}, MATCHED)

    # 3. Runtime questions: honestly out of scope for this slice.
    if _any(_RUNTIME_PATTERNS, norm):
        return None, "runtime_unavailable"

    hits = []
    if _any(_STATUS_PATTERNS, norm):
        hits.append(("repository_status", {}, at.OPERATOR))
    if _any(_FAILURE_PATTERNS, norm):
        hits.append(("summarize_test_failure", {}, at.ENGINEER))
    if _any(_COMPONENT_PATTERNS, norm):
        name = _extract_quoted_or_tail(
            norm, ("explain the", "explain this", "explain", "inspect the",
                   "inspect", "tell me about the", "tell me about",
                   "dependencies of"))
        name = re.sub(r"\b(?:rust |the )?(?:crate|component|package|module|node)\b",
                      "", name).strip()
        if name not in _EMPTY_SUBJECTS:
            hits.append(("inspect_component", {"name": name}, at.ARCHITECT))
    if _any(_READ_PATTERNS, norm):
        subject = _extract_quoted_or_tail(
            norm, ("read the file", "read", "show me the file", "show me the source",
                   "show me the code", "open the file", "what's in"))
        # A read needs something path-shaped; otherwise it is not a read request.
        if "/" in subject or re.search(r"\.\w{1,6}$", subject):
            hits.append(("read_repository_source", {"path": subject}, at.ENGINEER))
    if _any(_SEARCH_PATTERNS, norm):
        q = _extract_quoted_or_tail(
            norm, ("where is the", "where is", "where's", "find the", "find me the",
                   "find a", "find where", "find", "locate the", "locate",
                   "search for", "who owns", "where does", "where do we"))
        if q not in _EMPTY_SUBJECTS:
            hits.append(("search_repository", {"query": q}, at.ENGINEER))

    if not hits:
        return None, NO_MATCH
    # De-duplicate by tool, preserving order.
    uniq = list(dict.fromkeys(h[0] for h in hits))
    if len(uniq) > 1:
        # Two genuinely different tools could apply → ask, never pick.
        return None, AMBIGUOUS
    tool, arguments, inferred_role = hits[0]
    return ({"role": role or inferred_role, "tool": tool,
             "arguments": arguments}, MATCHED)


# ── spoken rendering: conclusion → strongest evidence → one next action ──────

def _trim(text):
    text = " ".join(str(text).split())
    if len(text) <= SPOKEN_MAX_CHARS:
        return text
    return text[:SPOKEN_MAX_CHARS - 1].rstrip() + "…"


def clarification_question(options=None):
    if options:
        return "Did you mean " + " or ".join(options) + "?"
    return ("I could take that more than one way — do you want repository state, "
            "a code search, or one of the git workflows?")


def unknown_reply():
    return ("That isn't something I can look up with the tools I have. I can "
            "report repository status, search the code, read a file, inspect a "
            "component, or summarize a test failure.")


def shell_refusal_reply():
    return ("I can't run commands. I only have typed, read-only inspection tools "
            "plus the two approved git workflows.")


def runtime_unavailable_reply():
    return ("I can't answer that yet — I have no runtime diagnostics tool, only "
            "repository evidence. I'd be guessing, so I won't.")


def speak_result(result):
    """`ToolResult` → one concise, grounded spoken answer.

    Branches on `status` FIRST: success wording is unreachable for a refusal,
    error, or partial result.
    """
    if not isinstance(result, dict):
        return "Something went wrong reading that result, so I can't confirm anything."
    tool = result.get("tool", "")
    status = result.get("status")
    ev = result.get("evidence") or {}
    summary = result.get("summary") or ""

    if status == at.REFUSED:
        return _trim(f"{summary} That's a refusal, not a failure — nothing changed.")
    if status == at.ERROR:
        return _trim(f"{summary} That failed, so treat the result as unknown.")

    if status == at.PARTIAL:
        missing = ev.get("unestablished") or ev.get("unresolved") or []
        tail = ""
        if missing:
            tail = (" I could not establish " + ", ".join(str(m) for m in missing[:2])
                    + ".")
        return _trim(f"{summary}{tail}")

    # ── success ──
    if tool == "repository_status":
        head = ev.get("head_short") or ""
        branch = "a detached HEAD" if ev.get("detached") else ev.get("branch", "")
        state = "clean" if ev.get("clean") else "not clean"
        sync = str(ev.get("remote_sync", "")).replace("_", " ")
        line = f"The workspace is {state} on {branch}, {sync} with {ev.get('upstream','origin')}."
        if head:
            line += f" The current commit is {head}."
        if not ev.get("clean"):
            files = (ev.get("changed_files") or []) + (ev.get("untracked_files") or [])
            if files:
                line += f" Changed: {', '.join(files[:3])}."
            line += " Commit or set those aside before syncing."
        else:
            line += " You're ready to continue."
        return _trim(line)

    if tool == "search_repository":
        matches = ev.get("matches") or []
        top = matches[0]
        line = (f"{len(matches)} match{'es' if len(matches) != 1 else ''} for "
                f"'{ev.get('query','')}'. The strongest is "
                f"{top['path']} line {top['line']}: {top['excerpt']}")
        others = [m["path"] for m in matches[1:] if m["path"] != top["path"]]
        if others:
            line += f" Also in {others[0]}."
        line += " That's from repository search. Want me to read that file?"
        return _trim(line)

    if tool == "read_repository_source":
        line = (f"{ev.get('path','')} has {ev.get('total_lines', 0)} lines; I have "
                f"lines {ev.get('start_line')} to {ev.get('end_line')}. "
                "The full text is in the result if you want detail.")
        return _trim(line)

    if tool == "inspect_component":
        deps = ev.get("declared_dependencies") or []
        line = (f"{ev.get('name','')} is a {ev.get('language','')} component at "
                f"{ev.get('path','')}, declared in {ev.get('manifest','')}.")
        if deps:
            line += (f" It declares {len(deps)} dependencies, including "
                     f"{', '.join(deps[:3])}.")
        ifaces = ev.get("public_interfaces") or []
        if ifaces:
            line += f" Public surface starts with {', '.join(ifaces[:3])}."
        un = ev.get("unestablished") or []
        if un:
            line += f" I could not establish {un[0]}."
        return _trim(line)

    if tool == "summarize_test_failure":
        line = summary
        owner = ev.get("likely_owner")
        oev = ev.get("owner_evidence") or []
        if owner and oev:
            line += (f" It most likely belongs to {owner}, based on "
                     f"{oev[0]['path']} line {oev[0]['line']}.")
        steps = ev.get("diagnostic_next_steps") or []
        if steps:
            line += f" Next, {steps[0]}."
        unresolved = ev.get("unresolved") or []
        if unresolved:
            line += f" Still open: {unresolved[0]}."
        return _trim(line)

    if tool in ("sync_to_main", "publish_my_work"):
        # The RCL already produced the truthful sentence; do not re-word it.
        return _trim(summary)

    return _trim(summary)


# ── execution + the router seam ──────────────────────────────────────────────

def enabled():
    """Armed only on an explicit affirmative (fail-closed: unset/typo → off)."""
    return (os.environ.get("KIRRA_ASSIST_ENABLED") or "").strip().lower() \
        in ("1", "true", "yes", "on")


def run_request(request, *, ctx=None, transcript="", audit_path=None):
    """Execute a classified request through `run_tool` and audit it."""
    args = dict(request.get("arguments") or {})
    role = request.get("role")
    tool = request.get("tool")
    result = at.run_tool(tool, args, role=role, ctx=ctx)
    at.append_audit(at.audit_record(tool=tool, args=args, result=result,
                                    role=role, transcript=transcript), audit_path)
    return result


def handle(utterance, *, ctx=None, audit_path=None, role=None):
    """DETERMINISTIC assistant matcher in the house `handle` shape.

    Returns the sentence to speak, or `None` to fall through to the LLM router.
    `None` is returned only for genuinely unrecognized input, so ordinary
    conversation is untouched.
    """
    if not enabled():
        return None
    request, decision = classify(utterance, role=role)
    if decision == REFUSED_SHELL:
        return shell_refusal_reply()
    if decision == AMBIGUOUS:
        return clarification_question()
    if decision == "runtime_unavailable":
        return runtime_unavailable_reply()
    if request is None:
        return None  # not an engineering request → the LLM handles it
    result = run_request(request, ctx=ctx, transcript=utterance,
                         audit_path=audit_path)
    return speak_result(result)
