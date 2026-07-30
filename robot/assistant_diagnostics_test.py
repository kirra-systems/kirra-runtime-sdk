#!/usr/bin/env python3
"""Host tests for the `run_robot_diagnostics` assistant capability.

Deterministic and pure: no Gemma, no Jetson, no audio, no network. The doctor
runner is MOCKED, so these tests assert the assistant's behaviour rather than
the health of whatever machine they run on.

What is pinned here:
  * "run diagnostics" and its siblings select the typed capability;
  * broad runtime QUESTIONS keep the honest refusal — the command must not
    swallow them;
  * the classifier ORDER (diagnostics before the runtime refusal), which is the
    reported defect and would silently regress if the rules were reordered;
  * arbitrary execution stays refused, including script-by-path;
  * a WARN/FAIL finding is not a failed invocation;
  * success wording is unreachable after a refusal or an execution failure;
  * production registry growth does NOT change the model-advertised contract.

Runs standalone (`python3 robot/assistant_diagnostics_test.py`); also pytest.
"""
from __future__ import annotations

import hashlib
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import assistant as A  # noqa: E402
import assistant_tools as at  # noqa: E402

_FAILURES: list[str] = []


def check(cond, label):
    if cond:
        print(f"  ok   - {label}")
    else:
        print(f"  FAIL - {label}")
        _FAILURES.append(label)


def classify(utterance):
    req, dec = A.classify(utterance)
    return (req or {}).get("tool"), dec, (req or {}).get("arguments")


# ── 1. classification: the command selects the typed capability ──────────────
print("== 1. explicit diagnostics commands select run_robot_diagnostics ==")
for utt in ("run diagnostics",
            "run the diagnostics",
            "run the doctor",
            "run robot diagnostics",
            "perform a health check",
            "check the robot health",
            "please run diagnostics",
            "can you run diagnostics?",
            "Hey Rabbit, run diagnostics"):
    tool, dec, args = classify(utt)
    check(dec == A.MATCHED and tool == "run_robot_diagnostics" and args == {},
          f"{utt!r} -> MATCHED/run_robot_diagnostics/{{}} (got {dec}/{tool}/{args})")


# ── 2. the honest refusal for broad runtime questions is preserved ───────────
print("== 2. broad runtime questions are NOT coerced into a diagnostics run ==")
for utt in ("is the motor overheating?",
            "what is the current battery voltage?",
            "is every device working?",
            "why is the robot behaving strangely?",
            "tell me whether every sensor is healthy",
            "is the robot healthy?",
            "what is the cpu usage?",
            "what nodes are running?"):
    tool, dec, _ = classify(utt)
    check(tool != "run_robot_diagnostics",
          f"{utt!r} does not become a diagnostics run (got {dec}/{tool})")

# The ordering itself. `_RUNTIME_PATTERNS` contains "robot health", so without
# the diagnostics rule FIRST this exact phrase returns runtime_unavailable —
# which is the defect this change fixes. Pin both halves.
tool, dec, _ = classify("check the robot health")
check(dec == A.MATCHED and tool == "run_robot_diagnostics",
      "ORDER: 'check the robot health' reaches the tool, not runtime_unavailable")
_, dec, _ = classify("is the robot healthy?")
check(dec == "runtime_unavailable",
      "ORDER: 'is the robot healthy?' still gets the honest refusal")


# ── 3. shell boundary ────────────────────────────────────────────────────────
print("== 3. arbitrary execution is refused, never reinterpreted ==")
for utt in ("run python3 robot/kirra_doctor.py",
            "execute robot/kirra_doctor.py",
            "run robot/kirra_doctor.py",
            "run diagnostics && reboot",
            "run diagnostics; cat /etc/shadow",
            "run diagnostics --module ../../etc/passwd",
            "run diagnostics | sh"):
    tool, dec, _ = classify(utt)
    check(tool != "run_robot_diagnostics" and dec == A.REFUSED_SHELL,
          f"{utt!r} -> REFUSED_SHELL (got {dec}/{tool})")

# Questions about the script stay searches — the refusal must not over-reach.
tool, dec, _ = classify("where is kirra_doctor.py?")
check(tool == "search_repository", f"'where is kirra_doctor.py?' stays a search (got {tool})")


# ── 4. execution: the fixed entry point, no transcript text, honest statuses ──
print("== 4. execution goes through the fixed doctor entry point ==")


class _FakeDoctor:
    """Stands in for `kirra_doctor`. Records how it was called."""

    def __init__(self, report=None, boom=None):
        self.report, self.boom, self.calls = report, boom, []

    def collect(self, *a, **kw):
        self.calls.append((a, kw))
        if self.boom:
            raise self.boom
        return self.report


def _with_doctor(fake, fn):
    saved = sys.modules.get("kirra_doctor")
    sys.modules["kirra_doctor"] = fake
    try:
        return fn()
    finally:
        if saved is not None:
            sys.modules["kirra_doctor"] = saved
        else:
            sys.modules.pop("kirra_doctor", None)


def _report(p=0, w=0, u=0, f=0, mods=()):
    return {"schema_version": 1, "status": "FAIL" if f else ("WARN" if w or u else "PASS"),
            "summary": {"PASS": p, "WARN": w, "UNKNOWN": u, "FAIL": f},
            "modules": list(mods)}


# all-pass -> SUCCESS
fake = _FakeDoctor(_report(p=20))
res = _with_doctor(fake, lambda: at.run_tool("run_robot_diagnostics", {}, role=at.OPERATOR))
check(res["status"] == at.SUCCESS, f"20/0/0 -> SUCCESS (got {res['status']})")
check(fake.calls == [((), {})],
      f"collect() invoked once with NO arguments (got {fake.calls})")
check(res["claim"] == at.RUNTIME_FACT, "claim is runtime_fact, not repository_fact")
check(res["mutated"] is False, "read-only: never reports a mutation")

# one warn -> PARTIAL, and it is NOT an execution failure
fake = _FakeDoctor(_report(p=20, w=1, mods=[{"name": "voice", "status": "WARN"}]))
warn = _with_doctor(fake, lambda: at.run_tool("run_robot_diagnostics", {}, role=at.OPERATOR))
check(warn["status"] == at.PARTIAL, f"20/1/0 -> PARTIAL (got {warn['status']})")
check(warn["status"] != at.ERROR, "a WARN finding is not an ERROR invocation")
check(warn["evidence"]["warned"] == 1 and warn["evidence"]["failed"] == 0,
      "warn/fail counts are reported separately")

# failures -> still PARTIAL (the suite RAN), distinct from a warn-only run
fake = _FakeDoctor(_report(p=18, f=2, mods=[{"name": "storage", "status": "FAIL"},
                                            {"name": "governor", "status": "FAIL"}]))
fail = _with_doctor(fake, lambda: at.run_tool("run_robot_diagnostics", {}, role=at.OPERATOR))
check(fail["status"] == at.PARTIAL, f"18/0/2 -> PARTIAL (got {fail['status']})")
check(fail["evidence"]["failed"] == 2, "failure count is reported")
check(fail["evidence"]["failing_modules"] == ["governor", "storage"],
      f"failing module names are bounded and sorted (got {fail['evidence']['failing_modules']})")
check(fail["summary"] != warn["summary"], "warn and fail runs are distinguishable")

# an exploding doctor is an honest ERROR, never a success
fake = _FakeDoctor(boom=RuntimeError("no such module"))
err = _with_doctor(fake, lambda: at.run_tool("run_robot_diagnostics", {}, role=at.OPERATOR))
check(err["status"] == at.ERROR and err["reason"] == "diagnostics_unavailable",
      f"an exception becomes ERROR/diagnostics_unavailable (got {err['status']}/{err['reason']})")

# arguments are refused rather than silently dropped
bad = at.run_tool("run_robot_diagnostics", {"module": "../../etc/passwd"}, role=at.OPERATOR)
check(bad["status"] == at.REFUSED and bad["reason"] == "arguments_not_accepted",
      f"arguments are refused (got {bad['status']}/{bad['reason']})")

# audit identifies the typed tool, and never carries operator text into args
rec = at.audit_record(tool="run_robot_diagnostics", args={}, result=res,
                      role=at.OPERATOR, transcript="run diagnostics && reboot")
check(rec["tool"] == "run_robot_diagnostics" and rec["arguments"] == {},
      "audit names the typed tool with empty arguments")
check(rec["status"] == at.SUCCESS and rec["mutated"] is False,
      "audit carries the real status and mutation flag")


# ── 5. speech ────────────────────────────────────────────────────────────────
print("== 5. spoken summaries are bounded, honest, and never claim safety ==")
say_ok = A.speak_result(res)
say_warn = A.speak_result(warn)
say_fail = A.speak_result(fail)
say_err = A.speak_result(err)
say_ref = A.speak_result(bad)

check("20 modules passed" in say_ok and "none failed" in say_ok,
      f"success speech states the counts: {say_ok!r}")
check("1 warned" in say_warn, f"warn speech states the warning: {say_warn!r}")
check("suite ran" in say_warn.lower(),
      "warn speech says the suite RAN (a warning is not a failed invocation)")
for label, said in (("success", say_ok), ("warn", say_warn), ("fail", say_fail)):
    check("not a safety check" in said,
          f"{label} speech carries the observability caveat")
    check("safe" not in said.lower().replace("safety check", ""),
          f"{label} speech never calls the robot safe")
    check(len(said) < 400, f"{label} speech is bounded ({len(said)} chars)")
check("failed" in say_err.lower() and "20 modules passed" not in say_err,
      f"error speech never reads as a completed run: {say_err!r}")
check("refusal" in say_ref.lower() and "complete" not in say_ref.lower(),
      f"refusal speech never reads as a completed run: {say_ref!r}")


# ── 6. registry / contract invariants ────────────────────────────────────────
print("== 6. production registry growth does not alter the model contract ==")
check("run_robot_diagnostics" in at.registered_tool_names(),
      "run_robot_diagnostics IS registered for deterministic production use")
check("run_robot_diagnostics" not in at.CONTRACT_TOOL_NAMES,
      "run_robot_diagnostics is NOT model-advertised")
check("run_robot_diagnostics" not in at.assist_prompt_fragment(),
      "the prompt fragment does not mention the production-only tool")
check(set(at.CONTRACT_TOOL_NAMES) < set(at.registered_tool_names()),
      "the advertised vocabulary is a strict subset of the registry")
check(at.contract_tool_names() == sorted(at.CONTRACT_TOOL_NAMES),
      "contract_tool_names() validates against the registry and sorts")

tool = at.REGISTRY["run_robot_diagnostics"]
check(tool.read_only is True, "the tool is declared read-only")
check(tool.level <= at.MAX_GRANTED_LEVEL, "the tool is within the granted authority ceiling")
check(tool.level == at.L1_READ_ONLY, "the tool is level 1, not bounded-exec")

# ── 7. the pinned prompt: THIS checkout's contract, not another branch's ─────
print("== 7. the assist-1 prompt is pinned and unchanged ==")
#
# PROVENANCE, stated so nobody reads this pin as evidence it is not.
#
# This checkout ships PROMPT_CONTRACT_VERSION "assist-1". It does NOT contain
# the "assist-4" contract, the digest 9f5982de…, or the v2/61-case corpus those
# were measured against — none of that exists on this branch. `CONTRACT_TOOL_NAMES`
# did not exist here either; this change INTRODUCES the production-versus-
# model-visible split.
#
# The digest below is therefore the real, locally shipped assist-1 fragment,
# recomputed from source. It pins what this branch actually has. It is not, and
# must not be described as, preservation of the assist-4 evidence.
LOCAL_PROMPT_CONTRACT_VERSION = "assist-1"
PROMPT_FRAGMENT_DIGEST = "b0d0575aec7ad7afba4bc245fb56eac7f420d95cf8fc66b1b968951e94247481"

check(at.PROMPT_CONTRACT_VERSION == LOCAL_PROMPT_CONTRACT_VERSION,
      f"this checkout ships {LOCAL_PROMPT_CONTRACT_VERSION} "
      f"(got {at.PROMPT_CONTRACT_VERSION!r}) — the pin below is version-specific")

actual = hashlib.sha256(at.assist_prompt_fragment().encode("utf-8")).hexdigest()
check(actual == PROMPT_FRAGMENT_DIGEST,
      f"the {LOCAL_PROMPT_CONTRACT_VERSION} prompt digest is unchanged "
      f"(got {actual[:12]}…, want {PROMPT_FRAGMENT_DIGEST[:12]}…)")

#: The advertised vocabulary, pinned by VALUE as well as by digest. The digest
#: alone would catch a change here — the fragment lists these names — but it
#: would report it as "the prompt moved", which sends the reader looking at the
#: wording. Pinning the tuple names the actual cause: the advertised set grew or
#: shrank, which is a measurement-invalidating change and needs re-measuring.
ADVERTISED_AT_PIN = (
    "inspect_component", "publish_my_work", "read_repository_source",
    "repository_status", "search_repository", "summarize_test_failure",
    "sync_to_main",
)
check(tuple(sorted(at.CONTRACT_TOOL_NAMES)) == tuple(sorted(ADVERTISED_AT_PIN)),
      f"CONTRACT_TOOL_NAMES is unchanged — adding or removing an advertised tool "
      f"invalidates the {LOCAL_PROMPT_CONTRACT_VERSION} measurement and must be "
      f"re-measured, not re-pinned (added: "
      f"{sorted(set(at.CONTRACT_TOOL_NAMES) - set(ADVERTISED_AT_PIN))}, removed: "
      f"{sorted(set(ADVERTISED_AT_PIN) - set(at.CONTRACT_TOOL_NAMES))})")


if _FAILURES:
    print(f"\n== assistant diagnostics: {len(_FAILURES)} FAILED ==")
    for f in _FAILURES:
        print(f"   - {f}")
    sys.exit(1)
print("== assistant diagnostics: all checks passed ==")


def test_assistant_diagnostics_suite():
    assert not _FAILURES
