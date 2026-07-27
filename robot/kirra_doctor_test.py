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


# ---- profile digest: pinned KIRRA_PROFILE_DIGEST vs what Taj computes --------
#
# The failure this guards is silent-by-construction: a mismatched pin makes the
# motor consumer refuse EVERY release with PROFILE_DIGEST_MISMATCH, which looks
# exactly like a broken signer. All network/ROS access is stubbed here.

from doctor.modules import governor as governor_mod  # noqa: E402
from doctor.modules import profile_digest as pd_mod  # noqa: E402

GOOD_DIGEST = "ab" * 32
OTHER_DIGEST = "cd" * 32

SCAN_ECHO = """header:
  stamp:
    sec: 1774
    nanosec: 500000000
  frame_id: laser_frame
angle_min: -3.14159
angle_max: 3.14159
angle_increment: 0.0087
time_increment: 0.0
scan_time: 0.1
range_min: 0.15
range_max: 12.0
ranges: '<sequence type: float, length: 720>'
intensities: '<sequence type: float, length: 720>'
---
"""

GEOMETRY = {"angle_min_rad": -3.14159, "angle_increment_rad": 0.0087,
            "range_min_m": 0.15, "range_max_m": 12.0}


def test_parse_scan_echo_reads_the_four_geometry_fields() -> None:
    assert pd_mod.parse_scan_echo(SCAN_ECHO) == GEOMETRY


def test_parse_scan_echo_ignores_nested_header_fields() -> None:
    # `header:` nests its own scalars; matching an indented key would silently
    # read the wrong number into a safety-relevant comparison.
    geo = pd_mod.parse_scan_echo(SCAN_ECHO)
    assert geo is not None and all(isinstance(v, float) for v in geo.values())
    assert "sec" not in geo and "frame_id" not in geo


def test_parse_scan_echo_takes_the_first_message_when_echo_repeats() -> None:
    doubled = SCAN_ECHO + SCAN_ECHO.replace("angle_min: -3.14159", "angle_min: -1.0")
    assert pd_mod.parse_scan_echo(doubled)["angle_min_rad"] == -3.14159


def test_parse_scan_echo_is_none_when_incomplete_or_unparseable() -> None:
    assert pd_mod.parse_scan_echo("") is None
    assert pd_mod.parse_scan_echo(None) is None
    assert pd_mod.parse_scan_echo("angle_min: -3.14\nrange_min: 0.1\n") is None  # missing 2
    assert pd_mod.parse_scan_echo(SCAN_ECHO.replace("range_max: 12.0",
                                                    "range_max: not-a-number")) is None


def test_build_probe_sends_exactly_one_ray() -> None:
    # An EMPTY ranges list makes Taj answer with its invalid-frame sentinel
    # (empty digest), which would read as a mismatch rather than an error.
    probe = pd_mod.build_probe(GEOMETRY, 8.0, False, 500)
    assert probe["ranges"] == [1.0]
    assert probe["forward_extent_m"] == 8.0
    for k, v in GEOMETRY.items():
        assert probe[k] == v


def test_build_probe_omits_camera_max_age_when_disarmed() -> None:
    # Mirrors camera_perception_fields: disarmed sends ONLY camera_armed, so
    # Taj hashes its own default. Sending the field would agree only while the
    # two defaults coincide, and would false-mismatch as soon as an operator
    # set a non-default age with the camera off.
    disarmed = pd_mod.build_probe(GEOMETRY, 8.0, False, 123)
    assert disarmed["camera_armed"] is False
    assert "camera_max_age_ms" not in disarmed

    armed = pd_mod.build_probe(GEOMETRY, 8.0, True, 123)
    assert armed["camera_armed"] is True and armed["camera_max_age_ms"] == 123


def test_looks_like_digest_rules_match_the_consumer() -> None:
    assert pd_mod.looks_like_digest(GOOD_DIGEST)
    for bad in ("AB" * 32, "a" * 63, "a" * 65, "", None, 42, "g" * 64):
        assert not pd_mod.looks_like_digest(bad), repr(bad)


def _pd_run(env, *, echo_rc=0, echo_out=SCAN_ECHO, taj=(GOOD_DIGEST, None)):
    """Run the module with ros2 + Taj stubbed."""
    real_cmd, real_taj = pd_mod.run_cmd, pd_mod.taj_profile_digest
    pd_mod.run_cmd = lambda *a, **k: (echo_rc, echo_out, "" if echo_rc == 0 else "boom")
    pd_mod.taj_profile_digest = lambda *a, **k: taj
    try:
        return pd_mod.run({"robot_env": env})["details"]
    finally:
        pd_mod.run_cmd, pd_mod.taj_profile_digest = real_cmd, real_taj


def _status_of(details, needle):
    for d in details:
        if needle in d["check"]:
            return d["status"], d["info"]
    raise AssertionError(f"no check matching {needle!r} in {[d['check'] for d in details]}")


def test_pd_matching_digest_passes() -> None:
    details = _pd_run({"KIRRA_PROFILE_DIGEST": GOOD_DIGEST})
    assert _status_of(details, "matches Taj")[0] == "PASS"


def test_pd_mismatched_digest_fails_loudly_with_the_fix() -> None:
    details = _pd_run({"KIRRA_PROFILE_DIGEST": OTHER_DIGEST})
    status, info = _status_of(details, "matches Taj")
    assert status == "FAIL"
    assert "PROFILE_DIGEST_MISMATCH" in info
    fix = next(d["fix"] for d in details if "matches Taj" in d["check"])
    # The fix must hand the operator the ACTUAL value, not just say "wrong".
    assert GOOD_DIGEST in fix


def test_pd_unset_pin_is_unknown_not_fail() -> None:
    # "I could not check this" must never read like "these disagree"; the
    # governor module owns the FAIL for a missing pin.
    for env in ({}, {"KIRRA_PROFILE_DIGEST": ""}, {"KIRRA_PROFILE_DIGEST": "AB" * 32}):
        details = _pd_run(env)
        assert _status_of(details, "pinned digest available")[0] == "UNKNOWN", env


def test_pd_no_ros2_or_no_scan_is_unknown() -> None:
    assert _status_of(_pd_run({"KIRRA_PROFILE_DIGEST": GOOD_DIGEST}, echo_rc=-1),
                      "live scan geometry")[0] == "UNKNOWN"
    assert _status_of(_pd_run({"KIRRA_PROFILE_DIGEST": GOOD_DIGEST}, echo_out=""),
                      "live scan geometry")[0] == "UNKNOWN"


def test_pd_taj_unreachable_is_unknown() -> None:
    details = _pd_run({"KIRRA_PROFILE_DIGEST": GOOD_DIGEST}, taj=(None, "connection refused"))
    status, info = _status_of(details, "Taj profile digest")
    assert status == "UNKNOWN" and "connection refused" in info


def test_pd_never_compares_against_an_unusable_taj_digest() -> None:
    # Taj's invalid-frame sentinel is an EMPTY digest. Comparing a real pin
    # against "" would report a confident FAIL from a failed query.
    details = _pd_run({"KIRRA_PROFILE_DIGEST": GOOD_DIGEST}, taj=(None, "not usable ('')"))
    assert _status_of(details, "Taj profile digest")[0] == "UNKNOWN"
    assert not any("matches Taj" in d["check"] for d in details)


def test_pd_module_is_opt_in() -> None:
    # It shells to ros2 and POSTs to a sidecar, so it must stay out of the
    # always-on read-only default set.
    assert pd_mod.DEFAULT is False
    default_names = [m.NAME for m in core.discover_modules() if getattr(m, "DEFAULT", True)]
    assert "profile_digest" not in default_names
    assert "profile_digest" in [m.NAME for m in core.discover_modules()]


# ---- governor: the pinned-digest FORMAT check (pure env, default module) -----

def _gov(env):
    return governor_mod.run({"robot_env": env})["details"]


def test_governor_passes_a_well_formed_pin() -> None:
    assert _status_of(_gov({"KIRRA_PROFILE_DIGEST": GOOD_DIGEST}),
                      "profile digest pinned")[0] == "PASS"


def test_governor_fails_an_absent_or_placeholder_pin() -> None:
    for env in ({}, {"KIRRA_PROFILE_DIGEST": ""}, {"KIRRA_PROFILE_DIGEST": "__FILLED__"}):
        assert _status_of(_gov(env), "profile digest pinned")[0] == "FAIL", env


def test_governor_fails_uppercase_and_wrong_length_pins() -> None:
    # The consumer aborts startup on these, so a WARN would understate it.
    for bad in ("AB" * 32, "a" * 63, "a" * 65, "g" * 64):
        status, _ = _status_of(_gov({"KIRRA_PROFILE_DIGEST": bad}), "profile digest pinned")
        assert status == "FAIL", bad


def test_governor_digest_predicate_matches_the_consumer_rule() -> None:
    assert governor_mod.looks_like_pinned_digest(GOOD_DIGEST)
    for bad in ("AB" * 32, "a" * 63, "", None, 42):
        assert not governor_mod.looks_like_pinned_digest(bad), repr(bad)


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
