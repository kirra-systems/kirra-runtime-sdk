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
import assistant_admission as adm  # noqa: E402 — the shared target-resolution test
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

# Question shape, used twice below: to spare a genuine question from the
# runner guard, and to recognize an unmatched repository question.
_QUESTION_RE = re.compile(
    r"\?|^(?:what|where|which|why|how|who|when|is|are|does|do|did|can|could|"
    r"should|has|have)\b")

# "run the test suite" / "shell out and run cargo test" are execution requests
# for a runner this assistant deliberately does not have — generic test
# execution, builds and container commands are out of scope. They are refused
# OUT LOUD rather than falling through to the LLM to be answered vaguely.
#
# Applied ONLY to a non-question utterance, so "where do we run cargo clippy in
# CI?" stays what it plainly is — a search — instead of being mistaken for a
# request to run it.
_EXEC_RUNNER_RE = re.compile(
    r"\b(?:run|execute|exec|shell(?:\s+out)?|spawn|invoke|kick off)\b[^.?!]*"
    r"\b(?:cargo|colcon|pytest|ros2|docker|npm|make|the tests?|the test suite|"
    r"the build)\b"
    # Executing a SCRIPT BY PATH is the same class of request, and it is how a
    # typed capability would be bypassed: "run python3 robot/kirra_doctor.py"
    # asks for arbitrary execution that happens to name a repository file, not
    # for the bounded `run_robot_diagnostics` capability. Question-gated with
    # the rest of this regex, so "where do we run kirra_doctor.py?" stays a
    # search rather than becoming a refusal.
    r"|\b(?:run|execute|exec|invoke|spawn)\b[^.?!]*\S*\.(?:py|sh|bash|zsh|pl|js)\b"
    r"|\b(?:bash|sh|zsh|python3?|perl|node)\b\s+\S+\.(?:py|sh|bash|pl|js)\b",
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
    # Deliberately ALSO in `_SEARCH_PATTERNS`. "check the state of the steering
    # code" could mean repository state or a code search, and the existing
    # two-distinct-hits rule is how this classifier already says "ask, never
    # pick". This is operation ambiguity, not target ambiguity — the target is
    # perfectly clear, it is the operation that is not.
    r"\bcheck the state of\b",
)
_SEARCH_PATTERNS = (
    r"\bwhere is\b", r"\bwhere.s\b", r"\bfind (?:the|a|me)\b", r"\bfind where\b",
    r"\bwhich (?:file|component|crate) (?:has|owns|contains|defines|runs|invokes"
    r"|calls|configures)\b",
    r"\bwho owns\b", r"\bsearch (?:for|the repo)\b", r"\blocate\b",
    r"\bwhere do we\b", r"\bwhere does\b",
    r"\bcheck the state of\b",   # see the note in `_STATUS_PATTERNS`
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
# ── stored assistant-contract reporting (read-only) ──────────────────────────
#
# Reporting on a STORED contract artifact. Deliberately hard to trigger: the
# utterance must carry EXPLICIT Engineering-Assistant evidence, because the bare
# words "contract", "status", "run" and "report" all mean other things here (a
# legal contract, repository status, running a test). A generic "how did it go?"
# must stay unmatched — this classifier carries no conversational referent, so
# there is nothing to resolve "it" against, and guessing would be the same
# unresolved-target error the admission screen exists to refuse.
_CONTRACT_SUBJECT = (
    r"\bassistant contract\b", r"\bcontract (?:run|report|status|artifact)\b",
    r"\bmodel contract\b", r"\bprompt contract\b", r"\bcontract provenance\b",
    r"\bsafety gates?\b", r"\bunsafe admissions?\b", r"\badmission rules?\b",
    r"\bpolicy (?:reject|correct)", r"\bpolicy corrections?\b",
    r"\bselection accuracy\b", r"\bclarification quality\b",
    r"\bcommon.subset\b", r"\breadiness\b", r"\bmutating executions?\b",
    r"\bhard gates?\b",
)
_CONTRACT_SUBJECT_RE = tuple(re.compile(p) for p in _CONTRACT_SUBJECT)

# Asking to RUN the contract, or to change its verdict, is not a read. These are
# execution/mutation requests and must never resolve to the read-only reporter.
_CONTRACT_EXEC_RE = re.compile(
    r"\b(?:run|re-?run|execute|start|launch|kick off|regenerate|redo)\b"
    r"[^.?!]*\bcontract\b"
    r"|\b(?:make|mark|set|approve|declare|force)\b[^.?!]*"
    r"\b(?:ready|readiness|passed|pass|approved)\b"
    r"|\b(?:change|edit|update|lower|raise|override)\b[^.?!]*"
    r"\b(?:threshold|thresholds|acceptance|policy|verdict)\b")

#: section → the phrases that select it. Checked most specific first, so
#: "why is readiness not ready" resolves to acceptance rather than summary.
_CONTRACT_SECTIONS = (
    ("case", (r"\bcase\b",)),
    ("common_subset", (r"\bcommon.subset\b",)),
    ("corrections", (r"\bpolicy (?:reject|correct)", r"\bpolicy corrections?\b",
                     r"\badmission rules?\b", r"\bhow many proposals\b",
                     r"\bwhich rules? fired\b", r"\bcorrections?\b")),
    ("safety", (r"\bsafety gates?\b", r"\bunsafe admissions?\b",
                r"\bhard gates?\b", r"\bmutating executions?\b",
                r"\bsafe outcome\b")),
    ("quality", (r"\bselection accuracy\b", r"\bclarification quality\b",
                 r"\bper.tool accuracy\b", r"\bparse failure\b",
                 r"\btrial stability\b", r"\baccuracy\b")),
    ("acceptance", (r"\breadiness\b", r"\bnot ready\b", r"\bacceptance\b",
                    r"\bthresholds?\b")),
    ("provenance", (r"\bprovenance\b", r"\bdigest\b", r"\bprompt contract\b",
                    r"\bwhich model\b", r"\bwhat model\b")),
    ("tools", (r"\bregistered tools?\b", r"\bwhich tools?\b")),
)
_CONTRACT_SECTIONS_RE = tuple((s, tuple(re.compile(p) for p in pats))
                              for s, pats in _CONTRACT_SECTIONS)

#: An exact case id. Never fuzzy-matched — there is no tested fuzzy mechanism
#: here, and a near-miss would report a DIFFERENT case's verdict as this one's.
_CASE_ID_RE = re.compile(r"\bcase\s+([a-z][a-z0-9]*(?:_[a-z0-9]+)+)\b")


def classify_contract_report(utterance):
    """`utterance` → `{section, case_id}` for the stored-report reader, or None.

    Returns None unless the utterance carries explicit assistant-contract
    evidence AND is not asking to run the contract or change its verdict.
    """
    norm = normalize(utterance)
    if not norm:
        return None
    if _CONTRACT_EXEC_RE.search(norm):
        return None
    # A fully-formed `case <snake_case_id>` is itself explicit evidence — the id
    # shape (a lowercase token with at least one underscore) is specific enough
    # to be a corpus case reference and nothing else. A bare "that case" is not,
    # and falls through to the subject gate below, which will refuse it.
    if not _CASE_ID_RE.search(norm) \
            and not any(rx.search(norm) for rx in _CONTRACT_SUBJECT_RE):
        return None
    for section, pats in _CONTRACT_SECTIONS_RE:
        if any(rx.search(norm) for rx in pats):
            if section == "case":
                m = _CASE_ID_RE.search(norm)
                # "what happened in that case?" names no case — ask, never guess.
                return {"section": "case", "case_id": m.group(1)} if m else None
            scope = "common_subset" if section == "common_subset" else "full"
            return {"section": section, "case_id": None, "scope": scope}
    return {"section": "summary", "case_id": None, "scope": "full"}


_RUNTIME_PATTERNS = (
    r"\bis the robot (?:healthy|ok|running)\b", r"\brobot health\b",
    r"\bros (?:graph|nodes|topics)\b", r"\bwhat nodes are running\b",
    r"\bcpu (?:usage|load)\b", r"\bmemory usage\b", r"\bis .* service running\b",
    r"\bmission state\b", r"\bcurrent posture\b",
)


# An explicit COMMAND to run the bounded diagnostics suite. Deliberately narrow
# and anchored: this must select a typed capability without swallowing the broad
# runtime questions below, which remain honestly unanswerable.
#
# Anchored at the start (after a small closed set of politeness fillers) so a
# deliberative mention — "should we run diagnostics before the demo?" — is NOT a
# command. Command-shaped verbs only; no interrogative subject reaches these.
#
# NOTE the ordering dependency: `_RUNTIME_PATTERNS` contains `\brobot health\b`,
# which would otherwise claim "check the robot health" as unanswerable. These are
# tested FIRST in `classify`, and `assistant_diagnostics_test.py` pins that order.
_DOCTOR_LEAD = r"^(?:please |can you |could you |would you |now |go ahead and )*"
_DOCTOR_RUN_PATTERNS = (
    _DOCTOR_LEAD + r"run (?:the |a |full |another )*(?:robot |r2 |system |full )*"
                   r"diagnostics\b",
    _DOCTOR_LEAD + r"run (?:the |a )*(?:robot |r2 )*doctor\b",
    _DOCTOR_LEAD + r"(?:perform|run|do) (?:a |the )*(?:full )*health check\b",
    _DOCTOR_LEAD + r"(?:check|report) (?:the )*(?:robot|r2|system) health\b",
    _DOCTOR_LEAD + r"diagnose the (?:robot|r2|system)\b",
)
#: Text that must never ride along with a diagnostics command. The suite takes no
#: operator-supplied options, so a flag, a path or a chained command means the
#: operator asked for something this capability does not offer — refused out loud
#: rather than silently downgraded to the default run.
_DOCTOR_ARGISH_RE = re.compile(r"--|\.\./|/|\bmodule\b|&&|;|\|")


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


# ── recognized operation, unresolved target ──────────────────────────────────
#
# Three outcomes, and the middle one was missing:
#
#   recognized operation + resolved target    -> MATCHED
#   recognized operation + UNRESOLVED target  -> AMBIGUOUS   (ask which)
#   unrecognized operation                    -> NO_MATCH    (say so honestly)
#
# "Sync it." and "Publish." used to fall to NO_MATCH, so the robot answered "I
# don't have a tool for that" when the honest answer is "sync WHAT?". That was
# safe — no tool was selected — but it was the wrong thing to say, and it failed
# for the operator whether or not a model is in the path at all.
#
# TARGET RESOLUTION FOR MUTATIONS IS NOT RE-IMPLEMENTED HERE. It is
# `assistant_admission.has_resolved_mutation_target`, the same predicate the
# admission screen applies to a model proposal, called on the same normalized
# text. Copying its vocabulary would let classification and admission drift
# apart silently: a request could be refused as target-less by one and admitted
# as targeted by the other. A test asserts they cannot disagree.

#: A mutation asked for in COMMAND position — "Sync it.", "Publish.", "Can you
#: sync things?". Anchored at the start (after optional politeness) so the same
#: verb inside a question about the codebase ("what is the publish policy?",
#: "where is publishing implemented?") is not mistaken for a request to act.
_MUTATION_REQUEST_RE = re.compile(
    r"^(?:please\s+|now\s+|just\s+)*"
    r"(?:(?:can|could|will|would)\s+you\s+(?:please\s+)?)?"
    r"(?:go\s+ahead\s+and\s+)?"
    r"(?:sync|synchronise|synchronize|publish|push|update)\b")

#: A vague inspection — a looking verb whose object is a bare pronoun. The
#: pronoun is required: "have a look at robot/assistant.py" names a target and
#: is left exactly as it was.
_VAGUE_LOOK_RE = re.compile(
    r"\b(?:have|take)\s+a\s+look\s+at\s+(?:it|this|that|these|those|them)\b"
    r"|\blook\s+at\s+(?:it|this|that|these|those|them)\b"
    r"|\bcheck\s+(?:it|this|that|them)\b")

MUTATION_TARGET_UNRESOLVED = "mutation"
INSPECTION_TARGET_UNRESOLVED = "inspection"


def unresolved_operation(utterance):
    """Did the operator name an operation but not what to apply it to?

    Returns `MUTATION_TARGET_UNRESOLVED`, `INSPECTION_TARGET_UNRESOLVED`, or
    None. Pure — selects nothing, executes nothing.
    """
    norm = normalize(utterance)
    if not norm:
        return None
    # `has_resolved_mutation_target` normalizes internally, so handing it the
    # wake-stripped text yields the same answer as the raw utterance. Pinned by
    # test, because that equivalence is what makes the shared predicate shared.
    if _MUTATION_REQUEST_RE.search(norm) \
            and not adm.has_resolved_mutation_target(norm):
        return MUTATION_TARGET_UNRESOLVED
    if _VAGUE_LOOK_RE.search(norm):
        return INSPECTION_TARGET_UNRESOLVED
    return None

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
    #    A request to RUN a build / test / container runner is the same refusal.
    #    Skipped for questions, so "where do we run cargo clippy?" stays a search.
    if not _QUESTION_RE.search(norm) and _EXEC_RUNNER_RE.search(norm):
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

    # 3. An explicit command to run the bounded diagnostics suite. MUST precede
    #    the runtime-question refusal below: `_RUNTIME_PATTERNS` matches
    #    "robot health", so the honest-refusal rule would otherwise intercept
    #    "check the robot health" before any typed capability could be selected.
    #    That interception was the reported defect.
    #
    #    It also precedes the contract reader, though nothing hinges on that: the
    #    two vocabularies are disjoint (this one demands a diagnostics/doctor/
    #    health-check noun after a command verb, that one demands explicit
    #    assistant-contract evidence), so neither can claim the other's phrasing.
    if _any(_DOCTOR_RUN_PATTERNS, norm):
        if _DOCTOR_ARGISH_RE.search(raw):
            # "run diagnostics --module ../../etc/passwd" is not this command.
            return None, REFUSED_SHELL
        return ({"role": at.OPERATOR, "tool": "run_robot_diagnostics",
                 "arguments": {}}, MATCHED)

    # 4. Stored assistant-contract reporting. Read-only, and gated on explicit
    #    contract evidence (`classify_contract_report`), so it sits ahead of the
    #    generic vocabulary below: "did the safety gates pass?" is a report
    #    question, not a repository search for the word "gates".
    contract = classify_contract_report(utterance)
    if contract is not None:
        return ({"role": role or at.ENGINEER,
                 "tool": "report_assistant_contract",
                 "arguments": {"section": contract["section"],
                               "case_id": contract.get("case_id"),
                               "scope": contract.get("scope", "full")}},
                MATCHED)

    # 5. Runtime QUESTIONS: still honestly out of scope. Running the suite is a
    #    command; "is the motor overheating?" is a question no tool answers, and
    #    it must not be coerced into a diagnostics run.
    if _any(_RUNTIME_PATTERNS, norm):
        return None, "runtime_unavailable"

    hits = []
    # Families whose PATTERN matched but whose target came back as a bare
    # pronoun or nothing at all. Previously these were dropped and the whole
    # utterance fell to NO_MATCH, which is why "Where is that handled?" — an
    # unmistakable search request — was answered with "I have no tool for that".
    #
    # Only an EMPTY subject promotes. A subject that is merely unparseable
    # ("have you read the docs?") stays NO_MATCH: the operator named something,
    # so asking "which one do you mean?" would be the wrong question.
    unresolved = []
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
        elif name in _EMPTY_SUBJECTS:
            unresolved.append("inspect_component")
    if _any(_READ_PATTERNS, norm):
        subject = _extract_quoted_or_tail(
            norm, ("read the file", "read", "show me the file", "show me the source",
                   "show me the code", "open the file", "what's in"))
        # A read needs something path-shaped; otherwise it is not a read request.
        if "/" in subject or re.search(r"\.\w{1,6}$", subject):
            hits.append(("read_repository_source", {"path": subject}, at.ENGINEER))
        elif subject in _EMPTY_SUBJECTS:
            unresolved.append("read_repository_source")
    if _any(_SEARCH_PATTERNS, norm):
        q = _extract_quoted_or_tail(
            norm, ("where is the", "where is", "where's", "find the", "find me the",
                   "find a", "find where", "find", "locate the", "locate",
                   "search for", "who owns", "where does", "where do we"))
        if q not in _EMPTY_SUBJECTS:
            hits.append(("search_repository", {"query": q}, at.ENGINEER))
        else:
            unresolved.append("search_repository")

    if not hits:
        # A recognized operation with nothing to apply it to is a question for
        # the operator, not a dead end. Genuinely unknown utterances still say
        # so honestly. Nothing is selected on either path.
        if unresolved or unresolved_operation(utterance):
            return None, AMBIGUOUS
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


# ── the strengthened fallback ────────────────────────────────────────────────
#
# The weak spot in "unmatched → return None" is a question that is CLEARLY about
# this repository but that no pattern types. Falling through hands it to the LLM,
# which will answer a repository question from model memory — a fabricated
# repository fact stated in the robot's own voice. That is the one failure mode
# grounding exists to prevent.
#
# So an unmatched utterance that is BOTH question-shaped AND unmistakably about
# this codebase gets one honest, narrow ask instead of silence. The conjunction
# keeps ordinary conversation untouched: "nice weather today" has no repository
# vocabulary, "the crate compiles" is not a question.

_REPO_VOCAB = (
    r"crate", r"cargo", r"clippy", r"rustc", r"repo", r"repository", r"codebase",
    r"branch", r"commit", r"merge", r"rebase", r"pull request", r"\bpr\b",
    r"\bci\b", r"workflow", r"lockfile", r"manifest", r"workspace",
    r"depend(?:s|ed|ing|ency|encies)?", r"module", r"function", r"struct", r"trait",
    r"invariant", r"checker", r"governor", r"verifier", r"posture", r"actuator",
    r"lockout", r"adapter", r"sidecar", r"source code", r"source file",
    r"\btest(?:s|case)?\b", r"\bbuild\b", r"compile", r"\bgate\b",
)
_REPO_VOCAB_RE = tuple(re.compile(rf"\b{v}\b" if v[0].isalpha() else v)
                       for v in _REPO_VOCAB)


def looks_like_repository_question(utterance):
    """Is this unmistakably a question about THIS repository?

    Requires BOTH a question shape and repository vocabulary, so it widens the
    honest-refusal surface without capturing ordinary conversation.
    """
    norm = normalize(utterance)
    if not norm:
        return False
    if not _QUESTION_RE.search(norm):
        return False
    return any(rx.search(norm) for rx in _REPO_VOCAB_RE)


def fallback_reply():
    """One narrow ask for a repository question no tool can type.

    Deliberately NOT an answer: the assistant would have to invent one.
    """
    return ("That's a repository question, but I couldn't turn it into one of my "
            "tools, and I won't answer it from memory. Name a symbol, a file, or "
            "a crate and I'll search the tracked source for it.")


#: Deterministic policy reasons → what the operator hears. Used when a proposed
#: selection is REFUSED by policy: the refusal is spoken as a refusal, and never
#: as "done" or as an answer.
_POLICY_REFUSAL_SENTENCE = {
    "no_tool_selected": "I didn't pick a tool for that, so nothing ran.",
    "unregistered_tool": "That isn't a tool I have, so nothing ran.",
    "authority_level_not_granted":
        f"That needs an authority level above the {at.MAX_GRANTED_LEVEL} I'm "
        "granted, so nothing ran.",
    "role_not_permitted": "That tool isn't available in this mode, so nothing ran.",
    "path_traversal": "I won't follow a path out of the repository, so nothing ran.",
    "absolute_path_rejected": "I only read repository-relative paths, so nothing ran.",
    "outside_repository": "That path leaves the repository, so nothing ran.",
    "secretish_path": "That path looks like it could hold a secret, so I didn't read it.",
    "empty_query": "I need something specific to search for.",
    "empty_name": "I need the component's name.",
    "empty_output": "I need the failing test output before I can summarize it.",
}


def policy_refusal_reply(reason, tool=""):
    """Speak a deterministic policy refusal honestly. Never implies success."""
    sentence = _POLICY_REFUSAL_SENTENCE.get(
        reason, "Policy refused that, so nothing ran.")
    tail = f" ({reason})" if reason not in _POLICY_REFUSAL_SENTENCE else ""
    lead = f"{tool}: " if tool and reason == "unregistered_tool" else ""
    return _trim(f"{lead}{sentence}{tail}")


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
        if tool == "run_robot_diagnostics":
            # A WARN or FAIL finding is not a failed invocation: the suite RAN.
            # Say so, and keep the same observability caveat as the success path.
            tail += " The suite ran; that's observability only, not a safety check."
        return _trim(f"{summary}{tail}")

    # ── success ──
    if tool == "report_assistant_contract":
        # The sentence was composed deterministically by `assistant_report`,
        # which attributes every fact to the stored report or to deterministic
        # policy. It is spoken as-is rather than re-worded here, so there is one
        # place where that attribution can be checked.
        return _trim(summary)

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

    if tool == "run_robot_diagnostics":
        # `summary` already carries the deterministic bounded counts, built in
        # the tool so PARTIAL and SUCCESS say the same true thing. All that is
        # added here is the observability caveat — diagnostics passing is NOT a
        # safety statement, and the sentence must never imply one.
        return _trim(f"{summary} That's observability only, not a safety check.")

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
        # A repository question no pattern typed must NOT reach the LLM, which
        # would answer it from model memory as though it were a repository fact.
        if looks_like_repository_question(utterance):
            return fallback_reply()
        return None  # ordinary conversation → the LLM handles it
    result = run_request(request, ctx=ctx, transcript=utterance,
                         audit_path=audit_path)
    return speak_result(result)
