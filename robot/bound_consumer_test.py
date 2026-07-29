#!/usr/bin/env python3
"""Host tests for the evidence-bound V2 motor-consumer path.

Pure and hardware-free: no ROS, no motor board, and no libkirra_consumer_ffi.
The wire parser and the profile-digest reader are already pure; the release
path is exercised through `make_frame_handler` / `make_tick_handler`, which take
the consumer and the serial chokepoint as injected collaborators, so a fake
core can drive them. (`robot/ffi_smoke_test.py` covers the REAL .so; this file
covers the Python-side contract.)

What these pin, in one sentence: Python may never actuate on anything other
than a well-formed 272-byte V2 frame that the Rust core explicitly released.

Runs as a standalone script like the other robot smoke tests
(`python3 robot/bound_consumer_test.py`, exit 1 on any failure); also
importable under pytest (each `test_*` asserts).
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from kirra_ffi import (  # noqa: E402
    BOUND_FRAME_LEN,
    BOUND_PAYLOAD_LEN,
    BOUND_REFUSAL_NAMES,
    DIGEST_LEN,
    TOKEN_LEN,
    VK_LEN,
    bound_refusal_name,
    parse_profile_digest_hex,
    split_bound_frame,
    split_frame,
)
from kirra_motor_consumer import (  # noqa: E402
    decide_bound_frame,
    format_bound_health,
    make_frame_handler,
    make_tick_handler,
    read_profile_digest,
)

PROFILE_HEX = "ab" * 32
GOOD_FRAME = bytes(range(256)) * 2  # 512 bytes; sliced to 272 below
GOOD_FRAME = GOOD_FRAME[:BOUND_FRAME_LEN]


# ---------------------------------------------------------------------------
# Fakes
# ---------------------------------------------------------------------------

class _FrameResult:
    def __init__(self, write=1, refusal_code=-1, linear=0.3, angular=0.1):
        self.kind = 0 if write else 2
        self.refusal_code = refusal_code
        self.write = write
        self.sequence = 1
        self.nonce = 1
        self.scan_sequence = 813
        self.tracker_generation = 7
        self.linear = linear
        self.angular = angular


class _TickResult:
    def __init__(self, write=0, linear=0.0, angular=0.0):
        self.write = write
        self.linear = linear
        self.angular = angular


class _Health:
    def __init__(self, **over):
        self.releases = 0
        self.refusals_total = 0
        self.no_token = 0
        self.digest_mismatch = 0
        self.signature_invalid = 0
        self.undecodable = 0
        self.future_issued = 0
        self.expired = 0
        self.lifetime_too_long = 0
        self.sequence_not_advanced = 0
        self.nonce_not_advanced = 0
        self.profile_digest_mismatch = 0
        self.perception_frame_superseded = 0
        self.consecutive_signature_invalid = 0
        self.key_mismatch_alarm = 0
        self.silent = 0
        for k, v in over.items():
            setattr(self, k, v)


class _FakeCore:
    """Stands in for BoundKirraConsumer. Records exactly what Python handed it."""

    def __init__(self, results=None, ticks=None, health=None):
        self.frames = []          # (payload, token, now_ms)
        self.ticks = []           # now_ms
        self._results = list(results or [])
        self._ticks = list(ticks or [])
        self._health = health or _Health()

    def on_frame(self, payload, token, now_ms):
        self.frames.append((payload, token, now_ms))
        return self._results.pop(0) if self._results else _FrameResult()

    def on_tick(self, now_ms):
        self.ticks.append(now_ms)
        return self._ticks.pop(0) if self._ticks else _TickResult()

    def health(self):
        return self._health

    def alarm_explanation(self):
        return "KEY MISMATCH: the governor key does not match the pinned key."


class _Serial:
    """The single serial chokepoint, as a recorder."""

    def __init__(self):
        self.writes = []

    def __call__(self, linear, angular):
        self.writes.append((linear, angular))


class _Log:
    def __init__(self):
        self.messages = []

    def __call__(self, msg):
        self.messages.append(msg)


def _handler(core, serial, now=1000):
    warn, error = _Log(), _Log()
    handle = make_frame_handler(core, serial, warn=warn, error=error, now_fn=lambda: now)
    return handle, warn, error


# ---------------------------------------------------------------------------
# split_bound_frame — the strict V2 wire parse
# ---------------------------------------------------------------------------

def test_split_bound_frame_accepts_exactly_176_plus_96():
    payload, token = split_bound_frame(GOOD_FRAME)
    assert len(payload) == BOUND_PAYLOAD_LEN == 176
    assert len(token) == TOKEN_LEN == 96
    assert payload + token == GOOD_FRAME, "must be exact carriage, no reordering"


def test_split_bound_frame_boundary_is_where_the_governor_put_it():
    frame = b"\xaa" * BOUND_PAYLOAD_LEN + b"\xbb" * TOKEN_LEN
    payload, token = split_bound_frame(frame)
    assert payload == b"\xaa" * 176 and token == b"\xbb" * 96


def test_split_bound_frame_refuses_v1_128_byte_frame():
    # The load-bearing downgrade guard: a V1 signed frame carries no evidence
    # binding, so the live path must not accept it even though it is a
    # perfectly valid V1 frame.
    assert split_bound_frame(b"\x00" * 128) is None


def test_split_bound_frame_refuses_malformed_lengths():
    for n in (0, 1, 32, 96, 127, 128, 129, 176, 271, 273, 352, 544):
        assert split_bound_frame(b"\x00" * n) is None, n


def test_split_bound_frame_never_slices_an_overlong_frame():
    over = b"\x00" * (BOUND_FRAME_LEN + 64)
    assert split_bound_frame(over) is None, "must refuse, not truncate to a prefix"


def test_v1_split_frame_still_works_for_explicit_v1_callers():
    # V1 stays available as compatibility code; it just is not on the live path.
    assert split_frame(b"\x00" * 32) == (b"\x00" * 32, None)
    parsed = split_frame(b"\x01" * 128)
    assert parsed is not None and len(parsed[0]) == 32 and len(parsed[1]) == 96
    # ...and it does NOT accept a V2 frame, so the two parsers cannot be crossed.
    assert split_frame(b"\x00" * BOUND_FRAME_LEN) is None


# ---------------------------------------------------------------------------
# The pinned profile digest — trusted configuration only
# ---------------------------------------------------------------------------

def test_valid_lowercase_profile_digest_decodes_to_32_bytes():
    out = parse_profile_digest_hex(PROFILE_HEX)
    assert isinstance(out, bytes) and len(out) == DIGEST_LEN == 32
    assert out == bytes.fromhex(PROFILE_HEX)


def test_profile_digest_refuses_uppercase():
    # Refused, never folded: one identity must have exactly one spelling.
    assert parse_profile_digest_hex("AB" * 32) is None
    assert parse_profile_digest_hex("aB" * 32) is None


def test_profile_digest_refuses_absent_short_long_and_non_hex():
    for bad in (None, "", "  ", "a" * 63, "a" * 65, "a" * 32, "g" * 64,
                "0x" + "a" * 62, " " * 64, 42, b"a" * 64, []):
        assert parse_profile_digest_hex(bad) is None, repr(bad)


def test_read_profile_digest_from_environment():
    assert read_profile_digest({"KIRRA_PROFILE_DIGEST": PROFILE_HEX}) == bytes.fromhex(PROFILE_HEX)
    assert read_profile_digest({"KIRRA_PROFILE_DIGEST": f"  {PROFILE_HEX}  "}) is not None
    assert read_profile_digest({}) is None
    assert read_profile_digest({"KIRRA_PROFILE_DIGEST": ""}) is None
    assert read_profile_digest({"KIRRA_PROFILE_DIGEST": "AB" * 32}) is None


# ---------------------------------------------------------------------------
# decide_bound_frame — Python defers legitimacy entirely to the core
# ---------------------------------------------------------------------------

def test_valid_frame_is_handed_to_the_core_verbatim():
    core = _FakeCore()
    decide_bound_frame(core, GOOD_FRAME, 12345)

    assert len(core.frames) == 1
    payload, token, now = core.frames[0]
    assert payload == GOOD_FRAME[:176]
    assert token == GOOD_FRAME[176:]
    assert now == 12345


def test_malformed_frame_never_reaches_the_core():
    for n in (0, 32, 128, 271, 273):
        core = _FakeCore()
        decision = decide_bound_frame(core, b"\x00" * n, 1)
        assert core.frames == [], f"len {n} must not reach the FFI"
        assert decision.write is False
        assert decision.reason == f"MALFORMED_FRAME_{n}B"


def test_core_release_becomes_a_write_decision():
    core = _FakeCore([_FrameResult(write=1, linear=0.42, angular=-0.1)])
    decision = decide_bound_frame(core, GOOD_FRAME, 1)
    assert decision.write is True
    assert (decision.linear, decision.angular) == (0.42, -0.1)
    assert decision.reason is None


def test_core_refusal_becomes_a_named_no_write_decision():
    for code, name in BOUND_REFUSAL_NAMES.items():
        core = _FakeCore([_FrameResult(write=0, refusal_code=code)])
        decision = decide_bound_frame(core, GOOD_FRAME, 1)
        assert decision.write is False, name
        assert decision.reason == name
        assert (decision.linear, decision.angular) == (0.0, 0.0)


def test_unknown_refusal_code_still_produces_a_stable_tag():
    core = _FakeCore([_FrameResult(write=0, refusal_code=999)])
    assert decide_bound_frame(core, GOOD_FRAME, 1).reason == "code999"
    assert bound_refusal_name(999) == "code999"


# ---------------------------------------------------------------------------
# The serial chokepoint
# ---------------------------------------------------------------------------

def test_write_1_reaches_the_serial_seam_exactly_once():
    core = _FakeCore([_FrameResult(write=1, linear=0.25, angular=0.05)])
    serial = _Serial()
    handle, _warn, _error = _handler(core, serial)

    handle(GOOD_FRAME)

    assert serial.writes == [(0.25, 0.05)], "exactly one write, with the core's twist"


def test_write_0_reaches_the_serial_seam_zero_times():
    for code in BOUND_REFUSAL_NAMES:
        core = _FakeCore([_FrameResult(write=0, refusal_code=code)])
        serial = _Serial()
        handle, _warn, _error = _handler(core, serial)

        handle(GOOD_FRAME)

        assert serial.writes == [], f"refusal {code} must not actuate"


def test_malformed_frame_writes_nothing_and_calls_no_ffi():
    core = _FakeCore()
    serial = _Serial()
    handle, warn, _error = _handler(core, serial)

    handle(b"\x00" * 128)   # a V1 frame on the live path

    assert serial.writes == []
    assert core.frames == []
    assert any("MALFORMED_FRAME_128B" in m for m in warn.messages)


def test_each_released_frame_writes_once_no_more_no_less():
    core = _FakeCore([_FrameResult(write=1, linear=float(i), angular=0.0) for i in range(3)])
    serial = _Serial()
    handle, _warn, _error = _handler(core, serial)

    for _ in range(3):
        handle(GOOD_FRAME)

    assert [w[0] for w in serial.writes] == [0.0, 1.0, 2.0]


def test_a_refusal_between_releases_does_not_write():
    core = _FakeCore([
        _FrameResult(write=1, linear=0.2, angular=0.0),
        _FrameResult(write=0, refusal_code=105),   # EXPIRED
        _FrameResult(write=1, linear=0.3, angular=0.0),
    ])
    serial = _Serial()
    handle, _warn, _error = _handler(core, serial)

    for _ in range(3):
        handle(GOOD_FRAME)

    assert [w[0] for w in serial.writes] == [0.2, 0.3]


# ---------------------------------------------------------------------------
# Latched refusal logging
# ---------------------------------------------------------------------------

def test_repeated_identical_refusals_log_once():
    core = _FakeCore([_FrameResult(write=0, refusal_code=102) for _ in range(50)])
    handle, warn, _error = _handler(core, _Serial())

    for _ in range(50):
        handle(GOOD_FRAME)

    refusal_logs = [m for m in warn.messages if m.startswith("REFUSED")]
    assert len(refusal_logs) == 1, f"expected one latched log, got {len(refusal_logs)}"
    assert "SIGNATURE_INVALID" in refusal_logs[0]


def test_a_changed_refusal_reason_logs_again_with_the_suppressed_count():
    core = _FakeCore(
        [_FrameResult(write=0, refusal_code=102) for _ in range(4)]
        + [_FrameResult(write=0, refusal_code=109)]
    )
    handle, warn, _error = _handler(core, _Serial())

    for _ in range(5):
        handle(GOOD_FRAME)

    refusal_logs = [m for m in warn.messages if m.startswith("REFUSED")]
    assert len(refusal_logs) == 2
    assert "SIGNATURE_INVALID" in refusal_logs[0]
    assert "PROFILE_DIGEST_MISMATCH" in refusal_logs[1]
    assert any("+3 further" in m for m in warn.messages), warn.messages


def test_a_release_rearms_the_refusal_latch():
    core = _FakeCore([
        _FrameResult(write=0, refusal_code=102),
        _FrameResult(write=1),
        _FrameResult(write=0, refusal_code=102),
    ])
    handle, warn, _error = _handler(core, _Serial())

    for _ in range(3):
        handle(GOOD_FRAME)

    refusal_logs = [m for m in warn.messages if m.startswith("REFUSED")]
    assert len(refusal_logs) == 2, "a fresh episode after a release must log again"


def test_refusal_log_carries_health_diagnostics():
    health = _Health(profile_digest_mismatch=3, refusals_total=3)
    core = _FakeCore([_FrameResult(write=0, refusal_code=109)], health=health)
    handle, warn, _error = _handler(core, _Serial())

    handle(GOOD_FRAME)

    assert "profile_digest_mismatch=3" in warn.messages[0]


def test_key_mismatch_alarm_is_announced_once():
    health = _Health(key_mismatch_alarm=1, signature_invalid=10)
    core = _FakeCore([_FrameResult(write=0, refusal_code=102) for _ in range(5)], health=health)
    handle, _warn, error = _handler(core, _Serial())

    for _ in range(5):
        handle(GOOD_FRAME)

    assert len(error.messages) == 1
    assert "KEY MISMATCH" in error.messages[0]


def test_format_bound_health_names_the_diagnostic_counters():
    line = format_bound_health(_Health(
        signature_invalid=1, profile_digest_mismatch=2,
        perception_frame_superseded=3, sequence_not_advanced=4,
        nonce_not_advanced=5, expired=6, silent=1, key_mismatch_alarm=1,
    ))
    for expected in ("signature_invalid=1", "profile_digest_mismatch=2",
                     "perception_frame_superseded=3", "sequence_not_advanced=4",
                     "nonce_not_advanced=5", "expired=6", "silent=1",
                     "key_mismatch_alarm=1"):
        assert expected in line, (expected, line)
    assert format_bound_health(_Health()) == "no refusals recorded"


# ---------------------------------------------------------------------------
# Liveness / stop ramp
# ---------------------------------------------------------------------------

def test_tick_driven_stop_ramp_writes_are_preserved():
    ramp = [_TickResult(1, 0.20, 0.0), _TickResult(1, 0.10, 0.0),
            _TickResult(1, 0.0, 0.0), _TickResult(0)]
    core = _FakeCore(ticks=ramp)
    serial = _Serial()
    on_tick = make_tick_handler(core, serial, now_fn=lambda: 5000)

    for _ in range(4):
        on_tick()

    assert [w[0] for w in serial.writes] == [0.20, 0.10, 0.0]
    assert len(core.ticks) == 4, "the clock must run every period, ramp or not"


def test_tick_with_no_write_does_not_touch_the_motor():
    core = _FakeCore(ticks=[_TickResult(0) for _ in range(3)])
    serial = _Serial()
    on_tick = make_tick_handler(core, serial, now_fn=lambda: 1)

    for _ in range(3):
        on_tick()

    assert serial.writes == []


def test_refusals_do_not_feed_liveness():
    # A refusal flood must starve into the stop exactly as silence does: the
    # frame path writes nothing, and the tick path still ramps.
    core = _FakeCore(
        results=[_FrameResult(write=0, refusal_code=105) for _ in range(3)],
        ticks=[_TickResult(1, 0.1, 0.0)],
    )
    serial = _Serial()
    handle, _warn, _error = _handler(core, serial)
    on_tick = make_tick_handler(core, serial, now_fn=lambda: 9)

    for _ in range(3):
        handle(GOOD_FRAME)
    assert serial.writes == []

    on_tick()
    assert serial.writes == [(0.1, 0.0)], "the ramp is the only thing that may write"


# ---------------------------------------------------------------------------
# Wrapper argument validation (no .so needed — refused before the C call)
# ---------------------------------------------------------------------------

def test_bound_consumer_refuses_bad_key_and_digest_before_calling_c():
    from kirra_ffi import BoundKirraConsumer

    good_kwargs = dict(
        maximum_token_lifetime_ms=200, boot_wall_ms=0, control_period_ms=100, missed_periods=3,
        stop_decel_mps2=1.0, vx_max=0.15, vz_max=0.4,
        lib_path="/nonexistent/libkirra_consumer_ffi.so",
    )
    vk = b"\x01" * VK_LEN
    digest = b"\x02" * DIGEST_LEN

    # Wrong lengths / types must raise BEFORE the library is even loaded — the
    # bogus lib_path proves we never got as far as dlopen.
    for bad_vk in (b"\x01" * 31, b"\x01" * 33, b"", "not-bytes", None):
        try:
            BoundKirraConsumer(bad_vk, digest, **good_kwargs)
        except (ValueError, TypeError):
            pass
        else:
            raise AssertionError(f"bad vk accepted: {bad_vk!r}")

    for bad_digest in (b"\x02" * 31, b"\x02" * 33, b"", "not-bytes", None):
        try:
            BoundKirraConsumer(vk, bad_digest, **good_kwargs)
        except (ValueError, TypeError):
            pass
        else:
            raise AssertionError(f"bad digest accepted: {bad_digest!r}")


def test_bound_consumer_passes_the_pinned_profile_bytes_to_the_constructor():
    """The pinned digest must reach `kirra_bound_consumer_new` unchanged."""
    import kirra_ffi

    seen = {}

    class _FakeLib:
        def kirra_bound_consumer_new(self, vk, digest, *rest):
            seen["vk"] = vk
            seen["digest"] = digest
            seen["rest"] = rest
            return 0x1234  # a non-NULL handle

        def kirra_bound_consumer_free(self, handle):
            seen["freed"] = seen.get("freed", 0) + 1

    fake = _FakeLib()
    real_load, real_bind = kirra_ffi._load, kirra_ffi._bind_bound
    kirra_ffi._load = lambda path: fake
    kirra_ffi._bind_bound = lambda lib: lib
    try:
        vk = bytes(range(VK_LEN))
        digest = bytes.fromhex(PROFILE_HEX)
        consumer = kirra_ffi.BoundKirraConsumer(
            vk, digest,
            maximum_token_lifetime_ms=200, boot_wall_ms=0, control_period_ms=100,
            missed_periods=3, stop_decel_mps2=1.0, vx_max=0.15, vz_max=0.4,
            lib_path="ignored",
        )
        assert seen["vk"] == vk
        assert seen["digest"] == digest, "the pinned profile digest must pass through byte-for-byte"
        assert len(seen["digest"]) == DIGEST_LEN

        # free is idempotent, and the handle is not reusable afterwards.
        consumer.close()
        consumer.close()
        assert seen["freed"] == 1, "double close must free exactly once"
        try:
            consumer.on_tick(1)
        except ValueError:
            pass
        else:
            raise AssertionError("a closed consumer must refuse to be reused")
    finally:
        kirra_ffi._load, kirra_ffi._bind_bound = real_load, real_bind


def test_bound_consumer_treats_null_handle_as_fail_closed():
    import kirra_ffi

    class _NullLib:
        def kirra_bound_consumer_new(self, *a):
            return None  # the core refused the configuration

    real_load, real_bind = kirra_ffi._load, kirra_ffi._bind_bound
    kirra_ffi._load = lambda path: _NullLib()
    kirra_ffi._bind_bound = lambda lib: lib
    try:
        kirra_ffi.BoundKirraConsumer(
            b"\x01" * VK_LEN, b"\x02" * DIGEST_LEN,
            maximum_token_lifetime_ms=200, boot_wall_ms=0, control_period_ms=100, missed_periods=3,
            stop_decel_mps2=1.0, vx_max=0.15, vz_max=0.4, lib_path="ignored",
        )
    except ValueError as e:
        assert "fail-closed" in str(e)
    else:
        raise AssertionError("a NULL handle must raise, never yield a usable consumer")
    finally:
        kirra_ffi._load, kirra_ffi._bind_bound = real_load, real_bind


# ---------------------------------------------------------------------------
# Runner (house convention: standalone script, exit 1 on failure)
# ---------------------------------------------------------------------------

def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items())
             if k.startswith("test_") and callable(v)]
    failures = 0
    for t in tests:
        try:
            t()
            print(f"  ok   {t.__name__}")
        except AssertionError as e:
            failures += 1
            print(f"  FAIL {t.__name__}: {e}")
        except Exception as e:  # noqa: BLE001
            failures += 1
            print(f"  ERROR {t.__name__}: {type(e).__name__}: {e}")
    print(f"\n{len(tests) - failures}/{len(tests)} passed")
    return 1 if failures else 0


if __name__ == "__main__":
    print("evidence-bound V2 consumer host tests (pure, no ROS/motor/.so):")
    sys.exit(_run_all())


# ---------------------------------------------------------------------------
# #1214 — wall-clock step guard on the release path.
#
# The token's signed `expires_at_ms` is compared against the consumer's wall
# clock, in a different process from the signer. A step in that clock changes
# what "expired" means while every check goes on returning confident answers.
# These pin the handler's response.
# ---------------------------------------------------------------------------


def _clock_driven_handler(core, serial, guard):
    """A handler whose wall and monotonic clocks the test drives independently."""
    warn, error = _Log(), _Log()
    clocks = {"wall_ms": 1_700_000_000_000, "mono_us": 5_000_000}

    handle = make_frame_handler(
        core,
        serial,
        warn=warn,
        error=error,
        now_fn=lambda: clocks["wall_ms"],
        mono_fn=lambda: clocks["mono_us"],
        clock_guard=guard,
    )
    return handle, warn, clocks


def test_a_clock_step_refuses_the_frame_and_never_consults_the_core():
    # The core must not see the frame at all. Letting it judge would advance the
    # sequence, nonce and perception watermarks on the strength of a timestamp
    # comparison that is no longer meaningful — and those watermarks are what
    # refuse a later replay.
    from clock_step_guard import ClockStepGuard

    core, serial = _FakeCore(), _Serial()
    handle, warn, clocks = _clock_driven_handler(core, serial, ClockStepGuard(200))

    handle(GOOD_FRAME)  # baseline sample, released normally
    assert len(core.frames) == 1
    assert len(serial.writes) == 1

    # Real time advances 100 ms; the wall clock claims 250 ms.
    clocks["mono_us"] += 100_000
    clocks["wall_ms"] += 250
    handle(GOOD_FRAME)

    assert len(core.frames) == 1, "the core must not be consulted on a stepped clock"
    assert len(serial.writes) == 1, "and nothing may reach the motors"
    assert any("CLOCK_STEP_DETECTED" in m for m in warn.messages), warn.messages


def test_the_step_refusal_names_the_divergence_and_the_hold():
    # An operator reading one log line should be able to tell which clock moved
    # and how long the robot will hold, without reproducing the fault.
    from clock_step_guard import ClockStepGuard

    core, serial = _FakeCore(), _Serial()
    handle, warn, clocks = _clock_driven_handler(core, serial, ClockStepGuard(200))
    handle(GOOD_FRAME)

    clocks["mono_us"] += 100_000
    clocks["wall_ms"] += 250
    handle(GOOD_FRAME)

    message = next(m for m in warn.messages if "CLOCK_STEP_DETECTED" in m)
    assert "150 ms" in message, message
    assert "200 ms" in message, message


def test_release_resumes_once_every_pre_step_token_has_expired():
    # Non-vacuity: the guard must not latch. A detector that never releases is
    # indistinguishable from a broken robot, and would be "safe" in the useless
    # sense.
    from clock_step_guard import ClockStepGuard

    core, serial = _FakeCore(), _Serial()
    handle, _warn, clocks = _clock_driven_handler(core, serial, ClockStepGuard(200))
    handle(GOOD_FRAME)

    clocks["mono_us"] += 100_000
    clocks["wall_ms"] += 250
    handle(GOOD_FRAME)
    consulted_at_step = len(core.frames)

    # Both clocks now advance together, past the token lifetime.
    for _ in range(5):
        clocks["mono_us"] += 100_000
        clocks["wall_ms"] += 100
        handle(GOOD_FRAME)

    assert len(core.frames) > consulted_at_step, (
        "the hold must end once every pre-step token is expired under any offset"
    )
    assert len(serial.writes) > 1


def test_a_healthy_clock_is_never_disturbed_by_the_guard():
    # The guard sits on the release path, so a false positive stops a healthy
    # robot. Both clocks advancing together must be indistinguishable from the
    # guard being absent.
    from clock_step_guard import ClockStepGuard

    guarded_core, guarded_serial = _FakeCore(), _Serial()
    handle, warn, clocks = _clock_driven_handler(
        guarded_core, guarded_serial, ClockStepGuard(200))
    for _ in range(20):
        clocks["mono_us"] += 100_000
        clocks["wall_ms"] += 100
        handle(GOOD_FRAME)

    bare_core, bare_serial = _FakeCore(), _Serial()
    bare, _w, _e = _handler(bare_core, bare_serial)
    for _ in range(20):
        bare(GOOD_FRAME)

    assert len(guarded_core.frames) == len(bare_core.frames)
    assert len(guarded_serial.writes) == len(bare_serial.writes)
    assert warn.messages == []
