#!/usr/bin/env python3
"""Host tests for serial-authority detection (`motor_authority.py`).

Every case below is a real state observed or reachable on the live R2, driven
through the PURE decision functions with injected data — no serial port, no
systemd, no /proc, no robot.

The ten required states plus the four live false positives that motivated the
rewrite. Cases 7-10 are the important ones: each is a process that the OLD
name-based matcher flagged as a vendor motor base and that the new logic must
ignore, because none of them holds the motor device.
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from motor_authority import (  # noqa: E402
    FAIL, MOTOR_BASE_SIGNATURES, PASS, Holder, classify, is_motor_base,
    parse_active_state, parse_lsof_pids, parse_mainpid, resolve_device,
    verdict_for,
)

_FAILURES: list[str] = []


def check(cond, msg):
    if not cond:
        _FAILURES.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


CONSUMER = 4242
VENDOR = 9001


def _consumer(pid=CONSUMER):
    return Holder(pid, "/usr/bin/python3",
                  "python3 /opt/kirra/robot/kirra_motor_consumer.py", "python3")


def _vendor(pid=VENDOR):
    return Holder(pid, "/usr/bin/python3",
                  "python3 /home/jetson/yahboomcar_ros2_ws/rosmaster_main.py", "python3")


# ── the ten required states ──────────────────────────────────────────────────

def test_1_consumer_is_sole_opener_passes():
    v, summary, _ = classify([_consumer()], CONSUMER, True)
    check(v == PASS, f"sole consumer must PASS, got {v}: {summary}")
    check(str(CONSUMER) in summary, f"the summary must name the pid: {summary}")


def test_2_consumer_plus_vendor_both_open_fails():
    v, summary, ev = classify([_consumer(), _vendor()], CONSUMER, True)
    check(v == FAIL, f"a shared port must FAIL, got {v}")
    check(str(VENDOR) in summary, f"the summary must name the intruder: {summary}")
    check(any(str(VENDOR) in e for e in ev), "the evidence must list the intruder")


def test_3_vendor_is_sole_opener_fails():
    v, summary, _ = classify([_vendor()], CONSUMER, True)
    check(v == FAIL, f"vendor-only must FAIL, got {v}")
    check("NOT" in summary or "not" in summary,
          f"the summary must say the consumer does not hold it: {summary}")


def test_4_no_opener_and_consumer_inactive_is_reported_pass():
    v, summary, _ = classify([], None, False)
    check(v == PASS, f"nothing running, nothing held → PASS, got {v}")
    check("not active" in summary, f"and it must SAY so: {summary}")


def test_5_no_opener_while_consumer_claims_active_fails():
    """The dangerous silent state: systemd says active, but the process holds
    no fd — so the chokepoint everyone assumes is simply not there."""
    v, summary, _ = classify([], CONSUMER, True)
    check(v == FAIL, f"active-but-holding-nothing must FAIL, got {v}")
    check("holds no handle" in summary, f"and be actionable: {summary}")


def test_6_symlink_and_underlying_tty_resolve_to_one_device(tmp=None):
    import os
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        tty = os.path.join(d, "ttyUSB1")
        link = os.path.join(d, "myserial")
        open(tty, "w").close()
        os.symlink(tty, link)
        check(resolve_device(link) == resolve_device(tty),
              "the symlink and its target must resolve equal")
        check(resolve_device(link) == os.path.realpath(tty), "resolve to the real tty")


def test_7_lidar_in_a_vendor_workspace_is_ignored():
    """The live kill. The YDLIDAR driver lives under ~/yahboomcar_ros2_ws and
    was killed by the old matcher. It is a SENSOR."""
    exe = "/home/jetson/yahboomcar_ros2_ws/install/ydlidar_ros2_driver/lib/ydlidar_ros2_driver_node"
    check(not is_motor_base(exe, "ydlidar_ros2_driver_node --ros-args"),
          "a lidar in a vendor workspace must NOT be a motor base")


def test_8_avahi_hostname_is_ignored():
    """`avahi-daemon: running [yahboom.local]` — a hostname, not a process."""
    check(not is_motor_base("/usr/sbin/avahi-daemon",
                            "avahi-daemon: running [yahboom.local]"),
          "the mDNS hostname must NOT be a motor base")


def test_9_ollama_with_a_vendor_path_is_ignored():
    """The live disable. ollama.service merely had the workspace on PATH."""
    check(not is_motor_base("/usr/local/bin/ollama", "ollama serve"),
          "ollama must NOT be a motor base")
    check(not is_motor_base(
        "/usr/local/bin/ollama",
        "PATH=/home/jetson/yahboomcar_ros2_ws/install/bin:/usr/bin ollama serve"),
        "a vendor workspace on PATH must NOT make ollama a motor base")


def test_10_oled_service_is_ignored():
    check(not is_motor_base("/home/jetson/yahboomcar_ros2_ws/oled_node",
                            "python3 oled_display.py"),
          "the OLED display must NOT be a motor base")


# ── the positive control: a real motor base IS still recognised ──────────────

def test_a_real_rosmaster_base_is_still_recognised():
    check(is_motor_base("/usr/bin/python3",
                        "python3 /home/jetson/yahboomcar_ros2_ws/rosmaster_main.py"),
          "rosmaster_main.py must still match")
    check(is_motor_base("/usr/bin/python3", "python3 -m Rosmaster_Lib.driver"),
          "Rosmaster_Lib must still match")
    check(is_motor_base("/opt/ros/humble/lib/ros_robot_controller/node", "ros_robot_controller"),
          "ros_robot_controller must still match")


def test_every_signature_names_a_program_not_a_vendor_or_workspace():
    """The rule that keeps this from regressing: a bare vendor name is not a
    signature. `yahboom` alone matched a hostname, a lidar, an OLED and a PATH."""
    for sig in MOTOR_BASE_SIGNATURES:
        check(sig.lower() not in ("yahboom", "yahboomcar", "rosmaster"),
              f"{sig!r} is a vendor/workspace name, not a program — too broad")
        check(len(sig) >= 8, f"{sig!r} is short enough to match incidentally")


def test_non_vacuity_the_old_broad_pattern_would_have_matched_all_four():
    """Proof the new logic actually changed something: the OLD pattern matches
    every one of the four live false positives; the NEW one matches none."""
    import re
    old = re.compile(
        r"rosmaster_main|Rosmaster_Lib|yahboom|Yahboom|ros_robot_controller"
        r"|handsfree|bringup.*rosmaster|start_ros\.sh")
    live_false_positives = [
        ("/usr/local/bin/ollama",
         "PATH=/home/jetson/yahboomcar_ros2_ws/install/bin ollama serve"),
        ("/home/jetson/yahboomcar_ros2_ws/install/ydlidar_ros2_driver/lib/ydlidar_ros2_driver_node",
         "ydlidar_ros2_driver_node --ros-args"),
        ("/home/jetson/yahboomcar_ros2_ws/oled_node", "python3 oled_display.py"),
        ("/usr/sbin/avahi-daemon", "avahi-daemon: running [yahboom.local]"),
    ]
    for exe, cmd in live_false_positives:
        check(bool(old.search(f"{exe} {cmd}")),
              f"the OLD pattern should have matched (non-vacuity): {exe}")
        check(not is_motor_base(exe, cmd),
              f"the NEW check must NOT match: {exe}")


# ── systemd / lsof parsing ───────────────────────────────────────────────────

def test_mainpid_and_active_state_parse():
    out = "MainPID=4242\nActiveState=active\n"
    check(parse_mainpid(out) == 4242, "MainPID must parse")
    check(parse_active_state(out) == "active", "ActiveState must parse")
    check(parse_mainpid("MainPID=0\nActiveState=inactive\n") is None,
          "MainPID=0 means not running → None, not pid 0")
    check(parse_active_state("MainPID=0\nActiveState=inactive\n") == "inactive", "inactive")
    check(parse_mainpid("") is None, "empty output → None, not a crash")
    check(parse_mainpid("MainPID=garbage\n") is None, "garbage → None")


def test_lsof_pid_parsing_is_tolerant():
    check(parse_lsof_pids("4242\n9001\n") == [4242, 9001], "two pids")
    check(parse_lsof_pids("") == [], "no output → no holders")
    check(parse_lsof_pids("lsof: WARNING\n4242\n") == [4242],
          "a warning line must not invent a holder")


# ── the rendered result ──────────────────────────────────────────────────────

def test_missing_device_is_its_own_failure():
    r = verdict_for("/dev/myserial", "/dev/myserial", [], CONSUMER, True, exists=False)
    check(r["verdict"] == FAIL, "a missing device must FAIL")
    check("does not exist" in r["summary"], f"and say why: {r['summary']}")


def test_verdict_carries_the_evidence_a_operator_needs():
    r = verdict_for("/dev/myserial", "/dev/ttyUSB1", [_consumer()], CONSUMER, True)
    check(r["verdict"] == PASS, "the live good state must PASS")
    check(r["resolved"] == "/dev/ttyUSB1", "the resolved device must be carried")
    check(r["consumer_pid"] == CONSUMER, "the consumer pid must be carried")
    check(any("4242" in e for e in r["evidence"]), "evidence must name the holder")


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items())
             if k.startswith("test_") and callable(v)]
    print("motor_authority_test:")
    for t in tests:
        before = len(_FAILURES)
        t()
        print(f"  {'ok  ' if len(_FAILURES) == before else 'FAIL'} {t.__name__}")
    if _FAILURES:
        print(f"\n{len(_FAILURES)} check(s) FAILED", file=sys.stderr)
        return 1
    print(f"\nall {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(_run_all())
