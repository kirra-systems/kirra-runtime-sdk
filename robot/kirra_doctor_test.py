#!/usr/bin/env python3
"""kirra_doctor_test — host tests for the diagnostics framework. CI-safe: no
hardware, no network beyond localhost-refused connects, no sudo, no LLM. Fake
modules exercise the runner/schema/isolation; the real voice wrapper runs
against a stub script; the rabbit_diag matcher is tested pure.
Runner style matches rabbit_voice_test.py (assert-based, stdlib only).
"""
from __future__ import annotations

import json
import os
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from doctor import core  # noqa: E402
import rabbit_diag  # noqa: E402


# ---- helpers: fake modules ---------------------------------------------------

def _fake(name, details=None, exc=None, sleep=0.0, default=True):
    class M:
        NAME, DESCRIPTION = name, f"fake {name}"
        DEFAULT, HEAVY, TIMEOUT_S = default, False, 1

        @staticmethod
        def run(_ctx):
            if sleep:
                time.sleep(sleep)
            if exc:
                raise exc
            return {"details": details or [core.detail("ok", "PASS", "fine")]}
    return M


def _report(mods):
    return core.run_all(mods, ctx={"robot_env": {}, "robot_env_path": "/none",
                                   "here": "/none", "repo": "/none", "env": {}},
                        parallel=True)


# ---- aggregation + exit codes ------------------------------------------------

def test_worst_severity_order() -> None:
    assert core.worst([]) == "PASS"
    assert core.worst(["PASS", "WARN"]) == "WARN"
    assert core.worst(["WARN", "UNKNOWN"]) == "UNKNOWN"
    assert core.worst(["UNKNOWN", "FAIL", "PASS"]) == "FAIL"
    assert core.worst(["garbage"]) == "UNKNOWN"          # unknown token never masks


def test_exit_code_mapping() -> None:
    assert core.EXIT_FOR["PASS"] == 0
    assert core.EXIT_FOR["WARN"] == 1 and core.EXIT_FOR["UNKNOWN"] == 1
    assert core.EXIT_FOR["FAIL"] == 2 and core.EXIT_INTERNAL == 3


# ---- schema stability --------------------------------------------------------

def test_report_schema_v1_shape_and_ordering() -> None:
    rep = _report([_fake("zeta"), _fake("alpha")])
    for key in ("schema_version", "status", "timestamp", "host", "duration_ms",
                "modules", "summary"):
        assert key in rep, key
    assert rep["schema_version"] == 1
    assert [m["name"] for m in rep["modules"]] == ["alpha", "zeta"]   # deterministic order
    m = rep["modules"][0]
    for key in ("name", "description", "status", "elapsed_ms", "details",
                "recommended_action", "metadata"):
        assert key in m, key
    assert m["metadata"]["read_only"] is True
    json.loads(json.dumps(rep))                                        # JSON-clean


# ---- isolation: one bad module never stops the others ------------------------

def test_raising_module_is_unknown_and_isolated() -> None:
    rep = _report([_fake("boom", exc=RuntimeError("kaput")), _fake("good")])
    by = {m["name"]: m for m in rep["modules"]}
    assert by["boom"]["status"] == "UNKNOWN"
    assert "kaput" in by["boom"]["details"][0]["info"]
    assert "Traceback" not in json.dumps(rep)             # no stack traces in reports
    assert by["good"]["status"] == "PASS"
    assert rep["status"] == "UNKNOWN"                     # worst-of


def test_hanging_module_times_out_isolated() -> None:
    slow = _fake("slow", sleep=5)
    slow.TIMEOUT_S = 0.2
    rep = _report([slow, _fake("fast")])
    by = {m["name"]: m for m in rep["modules"]}
    assert by["slow"]["status"] == "UNKNOWN" and "timed out" in by["slow"]["details"][0]["info"]
    assert by["fast"]["status"] == "PASS"


# ---- status + recommended action derivation ----------------------------------

def test_status_derived_worst_of_details_and_fix_promoted() -> None:
    mod = _fake("mix", details=[core.detail("a", "PASS", ""),
                                core.detail("b", "FAIL", "broken", fix="do X"),
                                core.detail("c", "WARN", "meh")])
    rep = _report([mod])
    m = rep["modules"][0]
    assert m["status"] == "FAIL"
    assert m["recommended_action"] == "do X"


# ---- selection ---------------------------------------------------------------

def test_selection_default_all_explicit_and_unknown() -> None:
    from kirra_doctor import select_modules
    mods = [_fake("a"), _fake("b", default=False)]
    assert [m.NAME for m in select_modules(mods)] == ["a"]
    assert [m.NAME for m in select_modules(mods, include_all=True)] == ["a", "b"]
    assert [m.NAME for m in select_modules(mods, names=["b"])] == ["b"]  # opt-in addressable
    try:
        select_modules(mods, names=["nope"])
        assert False, "unknown module must raise"
    except KeyError as e:
        assert "nope" in str(e)


# ---- speech summary ----------------------------------------------------------

def test_speech_summary_healthy_and_issue_capped() -> None:
    healthy = _report([_fake("a"), _fake("b")])
    s = core.speech_summary(healthy)
    assert "healthy" in s and "2 modules" in s
    bad = _report([_fake(n, details=[core.detail("dev", "FAIL", "x")]) for n in "abcde"])
    s2 = core.speech_summary(bad, max_issues=3)
    assert "5 problems found" in s2
    assert "more — see the full report" in s2             # capped, not a monologue
    assert "/" not in s2.split("report")[0]               # no paths spoken


# ---- the real voice wrapper against a stub shell doctor ----------------------

def test_voice_module_maps_stub_exit_codes() -> None:
    from doctor.modules import voice
    with tempfile.TemporaryDirectory() as tmp:
        stub = os.path.join(tmp, "kirra_voice_doctor.sh")
        ctx = {"here": tmp, "repo": tmp, "robot_env": {}, "robot_env_path": "/none", "env": {}}
        with open(stub, "w", encoding="utf-8") as f:
            f.write("#!/bin/sh\necho OK\nexit 0\n")
        assert core.run_module(voice, ctx)["status"] == "PASS"
        with open(stub, "w", encoding="utf-8") as f:
            f.write("#!/bin/sh\necho 'FAIL: mic drifted'\nexit 1\n")
        r = core.run_module(voice, ctx)
        assert r["status"] == "FAIL" and "mic drifted" in r["details"][0]["info"]


# ---- deterministic voice matcher ---------------------------------------------

def test_diag_matcher_positive() -> None:
    for u in ("Rabbit, run diagnostics", "run a self check", "run self diagnostics",
              "check yourself", "please run a self-test", "diagnose yourself",
              "how healthy are you"):
        assert rabbit_diag.matches(u), u


def test_diag_matcher_negative_no_overlap() -> None:
    for u in ("creep forward one meter", "take us to the door", "what do you see?",
              "check for updates", "apply the update", "nice weather today",
              "check the door", ""):
        assert not rabbit_diag.matches(u), u


# ---- CLI plumbing (list + json against the REAL registry, no env needed) -----

def test_cli_list_and_json_run() -> None:
    import kirra_doctor
    assert kirra_doctor.main(["--list"]) == 0
    # A full real run must complete without raising and exit 0/1/2 (environment-
    # dependent statuses are fine — the contract is bounded exit + valid schema).
    code = kirra_doctor.main(["--json", "--module", "storage"])
    assert code in (0, 1, 2)


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    print("kirra_doctor host tests (no hardware):")
    failed = 0
    for t in tests:
        try:
            t()
            print(f"  ok   {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"  FAIL {t.__name__}: {e}")
    print(f"\n{len(tests) - failed}/{len(tests)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(_run_all())
