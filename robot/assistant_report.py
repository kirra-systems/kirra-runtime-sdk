#!/usr/bin/env python3
"""Read a STORED Engineering Assistant contract artifact and summarize it.

WHAT THIS IS, AND WHOSE WORDS THESE ARE
=======================================

`assistant_contract.contract_report()` produces a JSON evidence artifact after a
live model run. This module reads one back so the robot can talk about it.

The authority boundary is the whole point, and it survives being spoken aloud:

    Gemma may NARRATE trusted report data, but it must not author, recalculate,
    override, or self-assess the report's safety verdicts.

Only four fields in a report record came from the model at all — `raw_response`,
`proposed_tool`, `proposed_arguments` and `say` — and even those were sanitized
at the parse boundary. Everything else is a HARNESS OBSERVATION or a
DETERMINISTIC POLICY VERDICT. So the phrasing rule below is not a style
preference, it is the boundary made audible:

    WRONG: "Gemma reports that all safety gates passed."
    RIGHT: "The contract harness reports that all safety gates passed for
            Gemma's proposals."

    WRONG: "Gemma says it is not ready."
    RIGHT: "The stored acceptance verdict is NOT_READY."

    WRONG: "Gemma rejected six unsafe actions."
    RIGHT: "Deterministic policy rejected or corrected six model proposals."

The robot must never say "I am safe", "I passed my safety evaluation", "I
decided not to execute" or "I verified myself". Every spoken sentence names the
stored report or deterministic policy as the source.

WHAT IT DOES NOT DO
===================

It does not run the contract, call Ollama, run tests, execute a shell, mutate
anything, or write to the artifact. It never RECOMPUTES a verdict: readiness,
acceptance, hard gates, policy corrections and every accuracy figure are read
verbatim from the stored report, which carries both the measurements and the
thresholds it was judged under. A missing value is reported as unavailable —
never defaulted to safe, passed, zero, or ready.

It is also NOT a console reader. A contract artifact is structured, bounded and
schema-checked; a terminal is transient and unstructured. A future
`report_recent_execution` capability would need a managed execution store,
execution ids, bounded captured output and its own admission rules — it is
deliberately not this tool.
"""

import json
import os
from pathlib import Path

# ── configured artifact location ─────────────────────────────────────────────
#
# ONE bounded directory, never a caller-supplied path. A model that could name
# the file could name `/etc/shadow`; the search space is therefore fixed by
# configuration and the caller only chooses a SECTION.

REPORT_DIR_ENV = "KIRRA_ASSIST_REPORT_DIR"
DEFAULT_REPORT_DIR = "~/.kirra/assistant-reports"

#: A contract report is tens of KB. The cap bounds a hostile or corrupt file.
MAX_REPORT_BYTES = 8 * 1024 * 1024

#: Only artifacts this module understands are accepted.
EXPECTED_KIND = "assistant_tool_selection_contract"
SUPPORTED_HARNESS_VERSIONS = ("contract-1",)

#: Raw model text is bounded before it is ever returned for speech.
MAX_CASE_TEXT = 400

SECTIONS = ("summary", "provenance", "safety", "quality", "acceptance",
            "corrections", "tools", "common_subset", "case")
SCOPES = ("full", "common_subset")

# Failure reasons. Stable strings — they reach tests and spoken output.
NO_REPORT_DIR = "no_report_directory"
NO_REPORTS = "no_reports_found"
INVALID_REPORT = "invalid_report"
INCOMPLETE_REPORT = "incomplete_report"
UNSUPPORTED_SECTION = "unsupported_section"
UNSUPPORTED_SCOPE = "unsupported_scope"
CASE_ID_REQUIRED = "case_id_required"
CASE_NOT_FOUND = "case_not_found"


def report_dir():
    """The one configured directory, expanded. Never caller-supplied."""
    raw = (os.environ.get(REPORT_DIR_ENV) or "").strip() or DEFAULT_REPORT_DIR
    return Path(raw).expanduser()


def _is_inside(root, candidate):
    """Is `candidate` really under `root` after symlink resolution?

    Both sides are fully resolved, so a symlink pointing out of the report
    directory fails here exactly as `../../etc/passwd` does.
    """
    try:
        root_r = root.resolve(strict=True)
        cand_r = candidate.resolve(strict=True)
    except (OSError, RuntimeError):
        return False
    return root_r == cand_r or root_r in cand_r.parents


def _load_candidate(root, path):
    """One file → a valid artifact dict, or None. Never raises."""
    if not _is_inside(root, path):
        return None
    try:
        st = path.stat()                       # follows symlinks; target must be real
    except OSError:
        return None
    # Regular files only: a FIFO would block, a directory is not an artifact.
    if not path.is_file():
        return None
    if st.st_size > MAX_REPORT_BYTES or st.st_size == 0:
        return None
    try:
        with path.open("r", encoding="utf-8") as fh:
            doc = json.load(fh)
    except (OSError, ValueError, UnicodeDecodeError):
        return None
    if not isinstance(doc, dict):
        return None
    if doc.get("kind") != EXPECTED_KIND:
        return None
    if doc.get("harness_version") not in SUPPORTED_HARNESS_VERSIONS:
        return None
    return doc


def discover_reports(directory=None):
    """Every valid artifact in the configured directory, newest first.

    Ordering is `(generated_at, filename)` descending, so two artifacts sharing a
    timestamp still resolve to the same one on every run.
    """
    root = Path(directory).expanduser() if directory is not None else report_dir()
    if not root.is_dir():
        return []
    found = []
    for entry in sorted(root.iterdir(), key=lambda p: p.name):
        if entry.suffix != ".json":
            continue
        doc = _load_candidate(root, entry)
        if doc is not None:
            found.append((str(doc.get("generated_at") or ""), entry.name, entry, doc))
    found.sort(key=lambda t: (t[0], t[1]), reverse=True)
    return [(e, d) for _, _, e, d in found]


def latest_report(directory=None):
    """`(path, doc)` for the newest valid artifact, or `(None, None)`."""
    reports = discover_reports(directory)
    return reports[0] if reports else (None, None)


# ── validation: what each section REQUIRES ───────────────────────────────────
#
# A section is refused unless every field it speaks is present with the right
# type. Absent data becomes "unavailable" — never a comfortable default. This is
# the fail-closed rule applied to REPORTING: claiming "zero unsafe admissions"
# because a key was missing would be the worst possible failure mode here.

_REQUIRED = {
    "summary": (("model", (str,)), ("prompt_contract_version", (str,)),
                ("prompt_digest_sha256", (str,)), ("corpus_version", (int, str)),
                ("live_contract_verified", (bool,)), ("metrics", (dict,)),
                ("acceptance", (dict,))),
    "provenance": (("model", (str,)), ("model_digest", (str,)),
                   ("prompt_contract_version", (str,)),
                   ("prompt_digest_sha256", (str,)), ("prompt_chars", (int,)),
                   ("code_commit", (str,)), ("generated_at", (str,)),
                   ("corpus_version", (int, str)), ("trials", (int,))),
    "safety": (("metrics", (dict,)),),
    "quality": (("metrics", (dict,)),),
    "acceptance": (("acceptance", (dict,)),),
    "corrections": (("metrics", (dict,)),),
    "tools": (("registered_tools", (list,)), ("max_granted_level", (int,))),
    "common_subset": (("common_subset", (dict,)),),
    "case": (("records", (list,)),),
}

_REQUIRED_METRICS = {
    "safety": ("safe_outcome_rate", "hard_gates", "policy_corrections",
               "policy_corrections_by_rule"),
    "quality": ("positive_selection_accuracy", "clarification_quality",
                "per_tool_accuracy", "parse_failure_rate", "trial_stability"),
    "corrections": ("policy_corrections", "policy_corrections_by_rule"),
    "summary": ("safe_outcome_rate", "positive_selection_accuracy",
                "clarification_quality", "hard_gates"),
}

_REQUIRED_ACCEPTANCE = ("passed", "readiness", "hard_gate_failures",
                        "quality_failures", "policy")


def _missing_for(doc, section):
    """Field names the artifact does not supply for `section`. Empty = usable."""
    missing = []
    for name, types in _REQUIRED.get(section, ()):
        if name not in doc or not isinstance(doc[name], types) \
                or isinstance(doc[name], bool) and bool not in types:
            missing.append(name)
    metrics = doc.get("metrics")
    if section in _REQUIRED_METRICS:
        if not isinstance(metrics, dict):
            missing.append("metrics")
        else:
            for key in _REQUIRED_METRICS[section]:
                if key not in metrics or metrics[key] is None:
                    missing.append(f"metrics.{key}")
    if section in ("summary", "acceptance"):
        acc = doc.get("acceptance")
        if not isinstance(acc, dict):
            missing.append("acceptance")
        elif section == "acceptance":
            for key in _REQUIRED_ACCEPTANCE:
                if key not in acc:
                    missing.append(f"acceptance.{key}")
        elif "readiness" not in acc:
            missing.append("acceptance.readiness")
    if section == "corrections" and isinstance(metrics, dict):
        by_rule = metrics.get("policy_corrections_by_rule")
        if by_rule is not None and not isinstance(by_rule, dict):
            missing.append("metrics.policy_corrections_by_rule")
    return sorted(set(missing))


def artifact_identity(doc):
    """The provenance header every section carries."""
    return {
        "generated_at": doc.get("generated_at"),
        "code_commit": doc.get("code_commit"),
        "model": doc.get("model"),
        "model_digest": doc.get("model_digest"),
        "prompt_contract_version": doc.get("prompt_contract_version"),
        "prompt_digest_sha256": doc.get("prompt_digest_sha256"),
        "corpus_version": doc.get("corpus_version"),
        "harness_version": doc.get("harness_version"),
    }


def _fail(section, reason, detail=None):
    out = {"ok": False, "section": section, "reason": reason}
    if detail:
        out["detail"] = detail
    return out


def _metrics_for(doc, scope):
    """Metrics under the requested scope. `common_subset` reads the nested block."""
    if scope == "common_subset":
        cs = doc.get("common_subset")
        return cs.get("metrics") if isinstance(cs, dict) else None
    return doc.get("metrics")


def _truncate(text):
    """Bounded, deterministic. Returns `(text, was_truncated)`."""
    s = "" if text is None else str(text)
    if len(s) <= MAX_CASE_TEXT:
        return s, False
    return s[:MAX_CASE_TEXT] + "…", True


def _case_rows(doc, case_id):
    rows = []
    for r in doc.get("records") or []:
        if not isinstance(r, dict) or r.get("case_id") != case_id:
            continue
        raw, raw_cut = _truncate(r.get("raw_response"))
        say, say_cut = _truncate(r.get("say"))
        rows.append({
            "case_id": r.get("case_id"),
            "utterance": r.get("utterance"),
            "expect_kind": r.get("expect_kind"),
            "expected_tool": r.get("expected_tool"),
            "trial": r.get("trial"),
            "proposed_tool": r.get("proposed_tool"),
            "proposed_arguments": r.get("proposed_arguments"),
            "parsed": r.get("parsed"),
            "parse_note": r.get("parse_note"),
            "say": say,
            "raw_response": raw,
            "truncated": bool(raw_cut or say_cut),
            "admitted": r.get("admitted"),
            "mutating": r.get("mutating"),
            "executed": r.get("executed"),
            "reason": r.get("reason"),
            "admission_screen": r.get("admission_screen"),
            "outcome": r.get("outcome"),
            "outcome_class": r.get("outcome_class"),
            "safe": r.get("safe"),
        })
    return rows


def summarize_report(doc, section="summary", case_id=None, scope="full"):
    """A validated artifact + a section → the trusted result object.

    Every value in `data` is READ from the stored report. Nothing here computes
    a verdict; this function cannot make a failing report pass.
    """
    if section not in SECTIONS:
        return _fail(section, UNSUPPORTED_SECTION)
    if scope not in SCOPES:
        return _fail(section, UNSUPPORTED_SCOPE)
    if not isinstance(doc, dict):
        return _fail(section, INVALID_REPORT)

    missing = _missing_for(doc, section)
    if missing:
        return _fail(section, INCOMPLETE_REPORT, {"missing": missing})

    ident = artifact_identity(doc)
    m = _metrics_for(doc, scope)
    if section not in ("provenance", "tools", "case", "common_subset") \
            and not isinstance(m, dict):
        return _fail(section, INCOMPLETE_REPORT,
                     {"missing": [f"{scope}.metrics"]})
    gates = (m or {}).get("hard_gates") or {}
    acc = doc.get("acceptance") or {}

    if section == "summary":
        data = {
            "live_contract_verified": doc.get("live_contract_verified"),
            "safe_outcome_rate": m.get("safe_outcome_rate"),
            "unsafe_admissions": gates.get("unsafe_admissions"),
            "mutating_executions": gates.get("mutating_executions"),
            "positive_selection_accuracy": m.get("positive_selection_accuracy"),
            "clarification_quality": m.get("clarification_quality"),
            "policy_corrections": m.get("policy_corrections"),
            "readiness": acc.get("readiness"),
        }
    elif section == "provenance":
        data = {
            "prompt_chars": doc.get("prompt_chars"),
            "corpus_prompt_contract_version":
                doc.get("corpus_prompt_contract_version"),
            "corpus_contract_matches": doc.get("corpus_contract_matches"),
            "trials": doc.get("trials"),
            "seed": doc.get("seed"),
            "temperature": doc.get("temperature"),
            "live_model_available": doc.get("live_model_available"),
            "live_contract_verified": doc.get("live_contract_verified"),
        }
    elif section == "safety":
        data = {
            "safe_outcome_rate": m.get("safe_outcome_rate"),
            "unsafe_proposals": m.get("unsafe_proposals"),
            "unsafe_proposal_rate": m.get("unsafe_proposal_rate"),
            "hard_gates": dict(gates),
            "hard_gates_all_zero": all(not v for v in gates.values()),
            "policy_corrections": m.get("policy_corrections"),
            "policy_corrections_by_rule":
                dict(m.get("policy_corrections_by_rule") or {}),
        }
    elif section == "quality":
        data = {
            "positive_selection_accuracy": m.get("positive_selection_accuracy"),
            "positive_correct": m.get("positive_correct"),
            "positive_total": m.get("positive_total"),
            "clarification_quality": m.get("clarification_quality"),
            "clarify_asked": m.get("clarify_asked"),
            "clarify_total": m.get("clarify_total"),
            "parse_failure_rate": m.get("parse_failure_rate"),
            "trial_stability": m.get("trial_stability"),
            "per_tool_accuracy": dict(m.get("per_tool_accuracy") or {}),
        }
    elif section == "acceptance":
        data = {
            "passed": acc.get("passed"),
            "readiness": acc.get("readiness"),
            "hard_gate_failures": list(acc.get("hard_gate_failures") or []),
            "quality_failures": list(acc.get("quality_failures") or []),
            "policy": acc.get("policy"),
        }
    elif section == "corrections":
        data = {
            "policy_corrections": m.get("policy_corrections"),
            "policy_corrections_by_rule":
                dict(m.get("policy_corrections_by_rule") or {}),
        }
    elif section == "tools":
        data = {
            "registered_tools": list(doc.get("registered_tools") or []),
            "max_granted_level": doc.get("max_granted_level"),
        }
    elif section == "common_subset":
        cs = doc.get("common_subset") or {}
        csm = cs.get("metrics") or {}
        data = {
            "expected_size": cs.get("expected_size"),
            "records": cs.get("records"),
            "case_ids_excluded": list(cs.get("case_ids_excluded") or []),
            "positive_selection_accuracy":
                csm.get("positive_selection_accuracy"),
            "clarification_quality": csm.get("clarification_quality"),
            "safe_outcome_rate": csm.get("safe_outcome_rate"),
            "unsafe_admissions":
                (csm.get("hard_gates") or {}).get("unsafe_admissions"),
            "note": cs.get("note"),
        }
    else:  # case
        if not isinstance(case_id, str) or not case_id.strip():
            return _fail(section, CASE_ID_REQUIRED)
        rows = _case_rows(doc, case_id.strip())
        if not rows:
            return _fail(section, CASE_NOT_FOUND, {"case_id": case_id.strip()})
        data = {"case_id": case_id.strip(), "trials": len(rows), "rows": rows}

    return {"ok": True, "section": section, "scope": scope,
            "artifact": ident, "data": data}


def read_section(section="summary", case_id=None, scope="full", directory=None):
    """Discovery + validation + extraction. The ONE entry point for the tool."""
    if section not in SECTIONS:
        return _fail(section, UNSUPPORTED_SECTION)
    if scope not in SCOPES:
        return _fail(section, UNSUPPORTED_SCOPE)
    root = Path(directory).expanduser() if directory is not None else report_dir()
    if not root.is_dir():
        return _fail(section, NO_REPORT_DIR, {"configured": str(root)})
    path, doc = latest_report(root)
    if doc is None:
        return _fail(section, NO_REPORTS, {"configured": str(root)})
    out = summarize_report(doc, section=section, case_id=case_id, scope=scope)
    # The path is DIAGNOSTIC only. `render_spoken` never reads it, so a
    # filesystem layout is not read aloud to whoever is in the room.
    out["source_path"] = str(path)
    return out


# ── spoken rendering ─────────────────────────────────────────────────────────
#
# Deterministic and source-attributed. Every sentence names the stored report or
# deterministic policy; none of it is phrased as the robot's own self-assessment.

def _pct(value):
    """A rate → a spoken percentage, or None if the report did not carry one."""
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        return None
    pct = round(value * 100, 2)
    return f"{int(pct)} percent" if pct == int(pct) else f"{pct} percent"

_RULE_WORDS = {
    "clarification_carries_tool": "clarification-carries-tool",
    "unresolved_mutation_target": "unresolved mutation target",
    "unsupported_capability_request": "unsupported-capability",
    "mutation_family_mismatch": "mutation-family mismatch",
}

_COUNT_WORDS = ("zero", "one", "two", "three", "four", "five", "six", "seven",
                "eight", "nine", "ten")


def _count(n):
    if isinstance(n, bool) or not isinstance(n, int) or n < 0:
        return str(n)
    return _COUNT_WORDS[n] if n < len(_COUNT_WORDS) else str(n)


def _unavailable(result):
    reason = result.get("reason")
    if reason == NO_REPORT_DIR:
        return ("I have no assistant contract reports configured, so I can't "
                "tell you how the last run went.")
    if reason == NO_REPORTS:
        return ("I couldn't find a stored assistant contract report, so I have "
                "nothing to report on.")
    if reason == INCOMPLETE_REPORT:
        missing = (result.get("detail") or {}).get("missing") or []
        tail = f" It's missing {', '.join(missing[:3])}." if missing else ""
        return ("The stored contract report is incomplete, so I won't guess at "
                "the result." + tail)
    if reason == CASE_NOT_FOUND:
        cid = (result.get("detail") or {}).get("case_id", "that case")
        return f"The stored report has no case called {cid}."
    if reason == CASE_ID_REQUIRED:
        return "Which case would you like? I need its exact case id."
    if reason == UNSUPPORTED_SECTION:
        return "I don't have that part of the contract report."
    if reason == UNSUPPORTED_SCOPE:
        return "I can report the full corpus or the common subset, not that."
    return "I couldn't read the stored contract report, so I can't confirm anything."


def render_spoken(result):
    """Trusted result → one concise, source-attributed spoken answer."""
    if not isinstance(result, dict):
        return "Something went wrong reading that report, so I can't confirm anything."
    if not result.get("ok"):
        return _unavailable(result)

    d = result.get("data") or {}
    a = result.get("artifact") or {}
    section = result.get("section")
    model = a.get("model") or "the model"

    if section == "summary":
        verified = d.get("live_contract_verified")
        lead = (f"The latest stored contract report was verified against {model}."
                if verified else
                "The latest stored contract report has no live model run in it.")
        unsafe, mutating = d.get("unsafe_admissions"), d.get("mutating_executions")
        if unsafe == 0 and mutating == 0:
            safety = ("All deterministic safety gates passed, with zero unsafe "
                      "admissions and zero mutating executions.")
        else:
            safety = (f"The report records {_count(unsafe)} unsafe admissions "
                      f"and {_count(mutating)} mutating executions.")
        sel, clar = _pct(d.get("positive_selection_accuracy")), \
            _pct(d.get("clarification_quality"))
        quality = (f"Positive tool-selection accuracy was {sel or 'not recorded'}, "
                   f"and clarification quality was {clar or 'not recorded'}.")
        ready = str(d.get("readiness") or "unavailable").replace("_", " ").lower()
        return f"{lead} {safety} {quality} The stored readiness verdict is {ready}."

    if section == "provenance":
        return (f"The stored report was generated at {a.get('generated_at')} "
                f"from code commit {str(a.get('code_commit'))[:8]}, against "
                f"{model}, using prompt contract "
                f"{a.get('prompt_contract_version')} of "
                f"{d.get('prompt_chars')} characters, on corpus version "
                f"{a.get('corpus_version')}, with {d.get('trials')} trial per case.")

    if section == "safety":
        rate = _pct(d.get("safe_outcome_rate")) or "not recorded"
        gates = ("All hard safety gates passed." if d.get("hard_gates_all_zero")
                 else "Some hard safety gates failed: "
                      + ", ".join(k.replace("_", " ")
                                  for k, v in sorted((d.get("hard_gates") or {}).items())
                                  if v) + ".")
        n = d.get("policy_corrections")
        corr = (f"Deterministic policy rejected or corrected {_count(n)} model "
                "proposals" if n is not None else
                "The report does not record a correction count")
        mut = (d.get("hard_gates") or {}).get("mutating_executions")
        tail = ", and no mutating execution occurred." if mut == 0 else "."
        return (f"The stored report shows a safe outcome rate of {rate}. "
                f"{gates} {corr}{tail}")

    if section == "quality":
        sel, clar = _pct(d.get("positive_selection_accuracy")), \
            _pct(d.get("clarification_quality"))
        worst = sorted((r.get("accuracy"), n)
                       for n, r in (d.get("per_tool_accuracy") or {}).items()
                       if isinstance(r, dict) and r.get("accuracy") is not None)
        tail = ""
        if worst:
            names = ", ".join(n for _, n in worst[:2])
            tail = f" The weakest tools were {names}."
        return (f"The stored report measured {model} at "
                f"{sel or 'no recorded'} positive selection accuracy and "
                f"{clar or 'no recorded'} clarification quality, with a parse "
                f"failure rate of {_pct(d.get('parse_failure_rate')) or 'not recorded'}."
                + tail)

    if section == "acceptance":
        ready = str(d.get("readiness") or "unavailable").replace("_", " ").lower()
        hard = d.get("hard_gate_failures") or []
        qual = d.get("quality_failures") or []
        hard_s = ("No hard safety gates failed." if not hard
                  else f"{_count(len(hard)).capitalize()} hard safety gates failed.")
        qual_s = ("No quality thresholds failed." if not qual
                  else f"{_count(len(qual)).capitalize()} quality thresholds failed.")
        return (f"The stored acceptance verdict is {ready}. {hard_s} {qual_s}")

    if section == "corrections":
        n = d.get("policy_corrections")
        by_rule = d.get("policy_corrections_by_rule") or {}
        if not n:
            return ("The stored report records no proposals removed by "
                    "deterministic policy.")
        parts = [f"{_count(v)} {_RULE_WORDS.get(k, k.replace('_', ' '))}"
                 for k, v in sorted(by_rule.items())]
        return (f"Deterministic policy corrected {_count(n)} proposals: "
                + ", ".join(parts)
                + ". Those are policy corrections, not model successes.")

    if section == "tools":
        tools = d.get("registered_tools") or []
        return (f"The stored report lists {_count(len(tools))} registered tools, "
                f"with a granted authority ceiling of level "
                f"{d.get('max_granted_level')}.")

    if section == "common_subset":
        return (f"On the {d.get('expected_size')}-case common subset, the stored "
                f"report shows {_pct(d.get('positive_selection_accuracy')) or 'no recorded'} "
                f"positive selection accuracy, a safe outcome rate of "
                f"{_pct(d.get('safe_outcome_rate')) or 'not recorded'}, and "
                f"{_count(d.get('unsafe_admissions'))} unsafe admissions.")

    if section == "case":
        rows = d.get("rows") or []
        r = rows[0]
        verdict = ("policy admitted it" if r.get("admitted")
                   else "policy did not admit it")
        screen = r.get("admission_screen")
        why = (f", removed by the {_RULE_WORDS.get(screen, str(screen).replace('_', ' '))} rule"
               if screen else f", recorded as {r.get('reason')}")
        return (f"In case {r.get('case_id')}, the operator said "
                f"\"{r.get('utterance')}\". The report expected "
                f"{r.get('expected_tool') or r.get('expect_kind')}; {model} "
                f"proposed {r.get('proposed_tool') or 'no tool'}, and {verdict}"
                f"{why}. The recorded outcome is "
                f"{str(r.get('outcome') or '').replace('_', ' ')}.")

    return "I don't have that part of the contract report."
