#!/usr/bin/env python3
"""Host tests for `assistant_report.py` — the stored-contract voice reporter.

Everything runs against a FIXTURE shaped like the measured `0b952d1d` artifact.
That is a schema fixture, not a measurement: nothing here runs a contract, calls
Ollama, or claims a live-model result.

Runs standalone (`python3 robot/assistant_report_test.py`); also under pytest.
"""

import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import assistant as A  # noqa: E402
import assistant_report as ar  # noqa: E402
import assistant_tools as at  # noqa: E402

PASS = 0
FAIL = 0
HERE = Path(__file__).resolve().parent
FIXTURE = HERE / "testdata" / "assistant_contract_report_fixture.json"


def check(cond, label):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  ok   - {label}")
    else:
        FAIL += 1
        print(f"  FAIL - {label}")


def doc():
    return json.loads(FIXTURE.read_text(encoding="utf-8"))


def tmpdir_with(*files):
    """A fresh report dir. `files` are (name, text-or-dict) pairs."""
    d = Path(tempfile.mkdtemp())
    for name, body in files:
        text = json.dumps(body) if isinstance(body, dict) else body
        (d / name).write_text(text, encoding="utf-8")
    return d


# ══ artifact discovery ═══════════════════════════════════════════════════════

print("== discovery ==")

missing = Path(tempfile.mkdtemp()) / "nope"
check(ar.read_section(directory=missing)["reason"] == ar.NO_REPORT_DIR,
      "D1. a report directory that does not exist is reported, not invented")

check(ar.read_section(directory=tmpdir_with())["reason"] == ar.NO_REPORTS,
      "D2. an empty report directory yields no_reports_found")

_old = doc()
_old["generated_at"] = "2020-01-01T00:00:00+00:00"
_old["code_commit"] = "olderolder"
_d = tmpdir_with(("a_old.json", _old), ("b_new.json", doc()))
_p, _got = ar.latest_report(_d)
check(_got["code_commit"].startswith("0b952d1d"),
      "D3. the NEWEST valid report is selected by generated_at")

_bad = tmpdir_with(("z_broken.json", "{not json"), ("a_good.json", doc()))
_p, _got = ar.latest_report(_bad)
check(_got is not None and _got["code_commit"].startswith("0b952d1d"),
      "D4. a malformed file is skipped, and a valid older one still resolves")

_wrong = doc()
_wrong["kind"] = "something_else"
check(ar.latest_report(tmpdir_with(("a.json", _wrong)))[1] is None,
      "D5. an artifact with the wrong `kind` is rejected")

_ver = doc()
_ver["harness_version"] = "contract-99"
check(ar.latest_report(tmpdir_with(("a.json", _ver)))[1] is None,
      "D6. an unsupported harness version is rejected")

_big = tmpdir_with()
(_big / "a.json").write_text("x" * (ar.MAX_REPORT_BYTES + 1), encoding="utf-8")
check(ar.latest_report(_big)[1] is None, "D7. an oversized file is rejected")

_empty = tmpdir_with()
(_empty / "a.json").write_text("", encoding="utf-8")
check(ar.latest_report(_empty)[1] is None, "D8. a zero-byte file is rejected")

_nonobj = tmpdir_with(("a.json", "[1, 2, 3]"))
check(ar.latest_report(_nonobj)[1] is None,
      "D9. a top-level value that is not an object is rejected")

# A directory named *.json is not a regular file.
_dir = tmpdir_with()
(_dir / "a.json").mkdir()
check(ar.latest_report(_dir)[1] is None, "D10. a non-regular file is rejected")

# A symlink whose target lives OUTSIDE the report root must not be followed in.
_outside = tmpdir_with(("real.json", doc()))
_root = tmpdir_with()
try:
    (_root / "link.json").symlink_to(_outside / "real.json")
    check(ar.latest_report(_root)[1] is None,
          "D11. a symlink resolving outside the report root is rejected")
except OSError:                       # pragma: no cover — symlinks unavailable
    check(True, "D11. skipped: symlinks unavailable on this filesystem")

# `..` cannot escape: the caller never supplies a path, and resolution is checked.
check(not ar._is_inside(_root, _root / ".." / "etc" / "passwd"),
      "D12. a traversal path is not inside the report root")

_tie_a, _tie_b = doc(), doc()
_tie_a["code_commit"] = "aaaaaaaa"
_tie_b["code_commit"] = "bbbbbbbb"
_tied = tmpdir_with(("a.json", _tie_a), ("b.json", _tie_b))
check(all(ar.latest_report(_tied)[1]["code_commit"] == "bbbbbbbb"
          for _ in range(5)),
      "D13. equal timestamps resolve deterministically (filename breaks the tie)")

_res = ar.read_section(directory=_d)
check("source_path" in _res and _res["source_path"],
      "D14. the selected path is available as a DIAGNOSTIC field")
check(_res["source_path"] not in ar.render_spoken(_res),
      "D15. …and is never spoken aloud")


# ══ schema validation ════════════════════════════════════════════════════════

print("== schema validation: absent data is never a comfortable default ==")

check(ar.summarize_report(doc(), "summary")["ok"],
      "V1. a complete artifact validates")

for field in ("metrics", "acceptance", "model", "prompt_digest_sha256"):
    d2 = doc()
    d2.pop(field)
    r = ar.summarize_report(d2, "summary")
    check(not r["ok"] and r["reason"] == ar.INCOMPLETE_REPORT
          and field in r["detail"]["missing"],
          f"V2. a missing top-level `{field}` is reported as incomplete")

d2 = doc()
d2["metrics"] = "not a dict"
check(ar.summarize_report(d2, "safety")["reason"] == ar.INCOMPLETE_REPORT,
      "V3. a wrong-typed `metrics` is refused")

d2 = doc()
d2["metrics"].pop("hard_gates")
r = ar.summarize_report(d2, "safety")
check(not r["ok"] and "metrics.hard_gates" in r["detail"]["missing"],
      "V4. a missing requested metric is named, not defaulted to zero")

d2 = doc()
d2["metrics"]["policy_corrections_by_rule"] = ["not", "a", "dict"]
check(ar.summarize_report(d2, "corrections")["reason"] == ar.INCOMPLETE_REPORT,
      "V5. a malformed policy_corrections_by_rule is refused")

d2 = doc()
d2["acceptance"].pop("readiness")
check(ar.summarize_report(d2, "acceptance")["reason"] == ar.INCOMPLETE_REPORT,
      "V6. a missing acceptance verdict is refused, never assumed ready")

d2 = doc()
d2["records"] = "not a list"
check(ar.summarize_report(d2, "case", case_id="x")["reason"] == ar.INCOMPLETE_REPORT,
      "V7. malformed `records` is refused")

# The decisive one: an artifact with NO safety data must not read as safe.
d2 = doc()
d2["metrics"].pop("safe_outcome_rate")
r = ar.summarize_report(d2, "safety")
spoken = ar.render_spoken(r)
check(not r["ok"] and "passed" not in spoken.lower()
      and "incomplete" in spoken.lower(),
      "V8. an artifact missing safety data NEVER speaks as though it passed")


# ══ section extraction ═══════════════════════════════════════════════════════

print("== section extraction ==")

for section in ("summary", "provenance", "safety", "quality", "acceptance",
                "corrections", "tools", "common_subset"):
    r = ar.summarize_report(doc(), section)
    check(r["ok"] and r["section"] == section and isinstance(r["data"], dict),
          f"S1. section `{section}` extracts")

r = ar.summarize_report(doc(), "summary")
check(r["data"]["readiness"] == "not_ready"
      and r["data"]["unsafe_admissions"] == 0
      and r["data"]["positive_selection_accuracy"] == 0.84,
      "S2. summary values are READ from the report, not recomputed")
check(set(r["artifact"]) >= {"model", "prompt_contract_version",
                             "prompt_digest_sha256", "code_commit"},
      "S3. every section carries the artifact provenance header")

check("records" not in json.dumps(ar.summarize_report(doc(), "summary")["data"]),
      "S4. a summary does not return the raw records array")

r = ar.summarize_report(doc(), "case", case_id="pos_rcl_publish_origin")
check(r["ok"] and r["data"]["rows"][0]["proposed_tool"] == "sync_to_main",
      "S5. an exact case lookup returns the matching row")
check(r["data"]["rows"][0]["admission_screen"] == "mutation_family_mismatch",
      "S6. …including the policy verdict that removed the proposal")

check(ar.summarize_report(doc(), "case", case_id="nope")["reason"]
      == ar.CASE_NOT_FOUND,
      "S7. an unknown case is reported explicitly")
check(ar.summarize_report(doc(), "case")["reason"] == ar.CASE_ID_REQUIRED,
      "S8. `case` without a case_id is refused, not guessed")
check(ar.summarize_report(doc(), "case", case_id="pos_rcl")["reason"]
      == ar.CASE_NOT_FOUND,
      "S9. lookup is EXACT — a prefix does not match another case")

check(ar.summarize_report(doc(), "nonsense")["reason"] == ar.UNSUPPORTED_SECTION,
      "S10. an unsupported section is refused")
check(ar.summarize_report(doc(), "summary", scope="everything")["reason"]
      == ar.UNSUPPORTED_SCOPE,
      "S11. an unsupported scope is refused")

r = ar.summarize_report(doc(), "summary", scope="common_subset")
check(r["ok"] and r["scope"] == "common_subset",
      "S12. scope=common_subset reads the nested subset metrics")

d2 = doc()
d2["records"][0]["raw_response"] = "y" * (ar.MAX_CASE_TEXT + 500)
row = ar.summarize_report(d2, "case", case_id="pos_rcl_publish_origin")["data"]["rows"][0]
check(len(row["raw_response"]) <= ar.MAX_CASE_TEXT + 1 and row["truncated"],
      "S13. long raw model output is bounded and marked truncated")


# ══ rendering ════════════════════════════════════════════════════════════════

print("== rendering: whose words are these ==")

_say = {s: ar.render_spoken(ar.summarize_report(doc(), s))
        for s in ("summary", "safety", "quality", "acceptance", "corrections",
                  "provenance", "common_subset", "tools")}

check("84 percent" in _say["summary"] and "14.29 percent" in _say["summary"],
      "R1. percentages render accurately (0.84 → 84, 0.1429 → 14.29)")
check("not ready" in _say["summary"].lower(),
      "R2. readiness is exactly the stored verdict")
check("All hard safety gates passed" in _say["safety"],
      "R3. zero hard-gate failures are described correctly")
check("No hard safety gates failed" in _say["acceptance"]
      and "quality thresholds failed" in _say["acceptance"],
      "R4. quality failure wording stays distinct from safety")
check("not model successes" in _say["corrections"],
      "R5. corrections are explicitly not described as model successes")

# The authority boundary, made audible. These are the exact phrasings the
# design forbids: the robot must never present harness verdicts as its own.
_FORBIDDEN = ("i am safe", "i passed my safety", "i decided not to execute",
              "i verified myself", "gemma reports that", "gemma says",
              "gemma rejected", "i rejected")
for name, text in _say.items():
    low = text.lower()
    hit = [p for p in _FORBIDDEN if p in low]
    check(not hit, f"R6. `{name}` never speaks as self-assessment ({hit})")

_SOURCED = ("stored", "report", "deterministic policy", "harness")
for name, text in _say.items():
    check(any(s in text.lower() for s in _SOURCED),
          f"R7. `{name}` attributes its facts to the report or to policy")

check("gemma" not in _say["safety"].lower()
      or "deterministic policy" in _say["safety"].lower(),
      "R8. safety wording credits deterministic policy, not the model")

_case_say = ar.render_spoken(
    ar.summarize_report(doc(), "case", case_id="pos_rcl_publish_origin"))
check("proposed sync_to_main" in _case_say and "did not admit" in _case_say,
      "R9. a case names the model's PROPOSAL and policy's verdict separately")

check("incomplete" in ar.render_spoken(
    {"ok": False, "section": "summary", "reason": ar.INCOMPLETE_REPORT}).lower(),
    "R10. an incomplete report speaks as incomplete")
check("no case called nope" in ar.render_spoken(
    ar.summarize_report(doc(), "case", case_id="nope")).lower(),
    "R11. an unknown case is spoken explicitly")


# ══ classification ═══════════════════════════════════════════════════════════

print("== classification: positives ==")

_POSITIVE = {
    "How did the latest assistant contract run go?": "summary",
    "Report the latest contract status.": "summary",
    "Did the assistant safety gates pass?": "safety",
    "Were there any unsafe admissions?": "safety",
    "How many proposals did policy reject?": "corrections",
    "Which admission rules fired?": "corrections",
    "What was the model selection accuracy?": "quality",
    "What was clarification quality?": "quality",
    "Why is readiness not ready?": "acceptance",
    "Show the contract provenance.": "provenance",
    "Report the common-subset results.": "common_subset",
}
for utt, section in _POSITIVE.items():
    got = A.classify_contract_report(utt)
    check(got is not None and got["section"] == section,
          f"C1. {utt!r} → {section}")

got = A.classify_contract_report("What happened in case pos_rcl_publish_origin?")
check(got is not None and got["section"] == "case"
      and got["case_id"] == "pos_rcl_publish_origin",
      "C2. an exact case id is extracted")

print("== classification: negatives ==")

_NEGATIVE = (
    "What is the repository status?", "What is the console showing?",
    "Run the tests.", "Run the assistant contract.", "Rerun the contract.",
    "Make Gemma ready.", "Open a contract file.", "Drive forward.",
    "Publish my branch.", "Sync to main.",
    "Change the acceptance thresholds.", "Mark the report as passed.",
    "Approve the model.", "Override the policy verdict.",
    # Generic referents: there is no conversational context to resolve them.
    "What is the status?", "How did it go?", "Tell me about the latest run.",
    "What happened?", "What happened in that case?",
)
for utt in _NEGATIVE:
    check(A.classify_contract_report(utt) is None,
          f"C3. {utt!r} does NOT map to the reporter")

# The production classifier still routes the old vocabulary the old way.
req, _ = A.classify("what branch am I on?")
check(req and req["tool"] == "repository_status",
      "C4. repository status still classifies as before")
req, _ = A.classify("publish my work")
check(req and req["tool"] == "publish_my_work",
      "C5. publish still classifies as before")
req, _ = A.classify("sync to main")
check(req and req["tool"] == "sync_to_main",
      "C6. sync still classifies as before")

req, dec = A.classify("Did the assistant safety gates pass?")
check(req and req["tool"] == "report_assistant_contract"
      and req["arguments"]["section"] == "safety" and dec == A.MATCHED,
      "C7. the reporter is reachable through the production classifier")


# ══ structural invariants ════════════════════════════════════════════════════

print("== structural invariants ==")

tool = at.REGISTRY["report_assistant_contract"]
check(tool.read_only is True and tool.level == at.L1_READ_ONLY,
      "I1. the tool is registered READ-ONLY at level 1")
check(tool.level < at.L2_BOUNDED_EXEC,
      "I2. …below the bounded-execution level, so it has no mutation authority")

_src = (HERE / "assistant_report.py").read_text(encoding="utf-8")
_code = "\n".join(ln for ln in _src.splitlines()
                  if not ln.lstrip().startswith("#"))
for bad in ("subprocess", "os.system", "popen", "requests.", "urllib",
            "socket", "ollama", "api/chat", "check_output", "eval(", "exec("):
    check(bad not in _code, f"I3. the reader has no `{bad}`")
check(_src.count("import ") <= 4 and "import json" in _src,
      "I4. the reader's imports stay minimal (json, os, pathlib)")

# One canonical reader, one canonical dispatcher.
check(_src.count("def read_section(") == 1,
      "I5. exactly ONE artifact-reading entry point")
_tools_src = (HERE / "assistant_tools.py").read_text(encoding="utf-8")
check(_tools_src.count("ar.read_section(") == 1,
      "I6. exactly ONE call site into the reader")
_assist_src = (HERE / "assistant.py").read_text(encoding="utf-8")
check(_assist_src.count("contract = classify_contract_report(") == 1,
      "I7. exactly ONE dispatch path for the reporter")

# Registration grants AUTHORITY; the prompt asks the MODEL to propose. Keeping
# those separate is what let this tool ship without moving the pinned digest.
check("report_assistant_contract" in at.REGISTRY
      and "report_assistant_contract" not in at.CONTRACT_TOOL_NAMES,
      "I7b. the reporter is registered but NOT advertised to the model")
check("report_assistant_contract" not in at.assist_prompt_fragment(),
      "I7c. …so it does not appear in the model-facing prompt")
check(set(at.CONTRACT_TOOL_NAMES) <= set(at.REGISTRY),
      "I7d. every advertised tool is actually registered")
check(at.CONTRACT_TOOL_NAMES == tuple(sorted(at.CONTRACT_TOOL_NAMES)),
      "I7e. the advertised set is sorted, so the prompt bytes are stable")

# No caller-supplied path anywhere in the argument surface.
res = at.run_tool("report_assistant_contract",
                  {"section": "summary", "path": "/etc/passwd",
                   "directory": "/etc"},
                  role=at.ENGINEER, ctx=at.ToolContext())
check("/etc/passwd" not in json.dumps(res),
      "I8. a caller-supplied path argument is ignored, not honoured")

check(res["mutated"] is False,
      "I9. the tool never reports a mutation")

# The shipping path stays model-free.
_conv = (HERE / "rabbit_converse.py").read_text(encoding="utf-8")
check("assist_prompt_fragment" not in _conv,
      "I10. assist_prompt_fragment() is NOT wired into the conversational path")
check("STAGE2_SYSTEM" in _conv and "assist_prompt_fragment" not in _conv,
      "I11. STAGE2_SYSTEM does not carry the tool-selection contract")

# The admission screen and its rules are untouched by this change.
import assistant_admission as adm  # noqa: E402
check(len(adm.SCREEN_REASONS) == 4
      and adm.MUTATION_FAMILY_MISMATCH in adm.SCREEN_REASONS,
      "I12. the existing admission rules remain intact")

# The reporter must not be reachable as a substitute for execution or control.
for utt in ("Run the assistant contract.", "Tell me what the live terminal says.",
            "Drive forward and report the result.", "Read this arbitrary JSON file.",
            "Rerun the tests.", "Approve the model."):
    req, _dec = A.classify(utt)
    got = (req or {}).get("tool")
    check(got != "report_assistant_contract",
          f"I13. {utt!r} does not reach the reporter (got {got})")

# Contract identity is untouched by this capability.
import assistant_contract as ac  # noqa: E402
check(at.PROMPT_CONTRACT_VERSION == "assist-4"
      and at.prompt_digest() ==
      "9f5982de2e551ff3fbe57f9d7ebf10201fc59565f6682630c3e086a0f0e66f54",
      "I14. the prompt contract and digest are unchanged")
_corpus = ac.load_cases()
check(_corpus["version"] == 2 and len(_corpus["cases"]) == 61
      and ac.COMMON_SUBSET_SIZE == 55,
      "I15. the corpus is unchanged (v2, 61 cases, 55-case subset)")

# The fixture is a SCHEMA fixture, not evidence of a live run.
check(FIXTURE.exists() and doc()["kind"] == ar.EXPECTED_KIND,
      "I16. the fixture matches the real artifact schema")

for d in (missing.parent,):
    shutil.rmtree(d, ignore_errors=True)

print()
if FAIL:
    print(f"== assistant_report: {FAIL} FAILED, {PASS} passed ==")
    sys.exit(1)
print(f"== assistant_report: all {PASS} checks passed ==")
