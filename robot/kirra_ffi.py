"""ctypes binding to libkirra_consumer_ffi (ADR-0033 decision (c)).

This is the ONLY place Python talks to the verify core. It exposes the C ABI of
`crates/kirra-consumer-ffi` as small Pythonic wrappers. There is NO verification
logic here: every gate/watermark/freshness/liveness/alarm decision is made in
Rust and returned across this boundary. Python's job is to present wire bytes
and actuate whatever twist the core decides.

TWO PARALLEL CONSUMERS
----------------------
`KirraConsumer` (V1)      — 32-byte payload, 128-byte signed frame. Retained as
                            compatibility code for the bench publisher and the
                            V1 smoke test. It carries NO evidence binding.
`BoundKirraConsumer` (V2) — 176-byte payload, 272-byte signed frame, with the
                            perception/profile/proposal identity bound INSIDE
                            the signed bytes. This is the live R2 motor path.

The two are distinct handle types in Rust, so a V2 payload can never reach the
V1 core. The live consumer must use `split_bound_frame` + `BoundKirraConsumer`
only: accepting a V1 frame there would strip the evidence identity while still
producing motion, which is the exact downgrade the V2 format exists to prevent.

Build the shared library first (host or on the robot):
    cargo build -p kirra-consumer-ffi           # target/debug/libkirra_consumer_ffi.so
    cargo build -p kirra-consumer-ffi --release # target/release/... (deploy this)

Point KIRRA_CONSUMER_LIB at the .so, or let this module find it under target/.
"""

from __future__ import annotations

import ctypes
import os
from pathlib import Path

# Wire lengths — MUST match kirra-consumer-ffi (KIRRA_PAYLOAD_LEN / TOKEN / VK).
PAYLOAD_LEN = 32
TOKEN_LEN = 96
VK_LEN = 32

# Evidence-bound V2 wire lengths — MUST match KIRRA_BOUND_PAYLOAD_LEN /
# KIRRA_DIGEST_LEN in the Rust FFI. The V2 payload is the enforced twist PLUS
# the perception/profile/proposal identity it was authored against, so a
# governed frame is 176 + 96 = 272 bytes.
BOUND_PAYLOAD_LEN = 176
DIGEST_LEN = 32
BOUND_FRAME_LEN = BOUND_PAYLOAD_LEN + TOKEN_LEN  # 272

# Refusal-class codes — MUST match the REF_* constants in the Rust FFI.
REFUSAL_NAMES = {
    0: "NO_TOKEN",
    1: "DIGEST_MISMATCH",
    2: "SIGNATURE_INVALID",
    3: "UNDECODABLE",
    4: "STALE",
    5: "SEQUENCE_NOT_ADVANCED",
}

# V2 refusal-class codes — MUST match the BOUND_REF_* constants in the Rust
# FFI. Deliberately a separate numeric range so telemetry can never confuse a
# V2 evidence-binding refusal with a V1 freshness refusal.
BOUND_REFUSAL_NAMES = {
    100: "NO_TOKEN",
    101: "DIGEST_MISMATCH",
    102: "SIGNATURE_INVALID",
    103: "UNDECODABLE",
    104: "FUTURE_ISSUED",
    105: "EXPIRED",
    106: "LIFETIME_TOO_LONG",
    107: "SEQUENCE_NOT_ADVANCED",
    108: "NONCE_NOT_ADVANCED",
    109: "PROFILE_DIGEST_MISMATCH",
    110: "PERCEPTION_FRAME_SUPERSEDED",
}

_LOWERCASE_HEX = frozenset("0123456789abcdef")


def bound_refusal_name(code: int) -> str:
    """Human name for a V2 refusal code, or a stable `code<N>` fallback."""
    return BOUND_REFUSAL_NAMES.get(code, f"code{code}")


def parse_profile_digest_hex(value: "str | None") -> "bytes | None":
    """Decode a pinned profile digest from 64 LOWERCASE hex characters.

    Returns exactly 32 bytes, or None when the input is absent, not a string,
    the wrong length, uppercase, or not hexadecimal.

    Uppercase is refused rather than folded, matching the verifier's
    `decode_lowercase_sha256` and the ROS proposal envelope's digest rule: the
    digest is an identity, and two spellings of one identity would let the same
    profile be pinned under two different-looking values.

    This is the ONLY way the consumer learns its expected profile digest — it
    is trusted configuration, never read from an incoming payload.
    """
    if not isinstance(value, str) or len(value) != DIGEST_LEN * 2:
        return None
    if not all(char in _LOWERCASE_HEX for char in value):
        return None
    try:
        decoded = bytes.fromhex(value)
    except ValueError:  # unreachable given the charset check; belt and braces
        return None
    return decoded if len(decoded) == DIGEST_LEN else None


def split_frame(data: bytes) -> "tuple[bytes, bytes | None] | None":
    """Strict wire parse of a V1 release frame (Copilot #901 review).

    Exactly 32 bytes → (payload, None)          — an unsigned command.
    Exactly 128 bytes → (payload, token[96])    — a signed command.
    ANY other length → None                     — malformed; the caller must
    ignore it (never actuate, never crash). A >128-byte frame must NOT be
    sliced into an oversized token: that raised ValueError inside the ROS
    callback and let hostile input take down the consumer.

    🔴 V1 ONLY. This parser is retained for explicit V1 callers (the bench
    publisher and the V1 half of the FFI smoke test). The live R2 motor
    consumer MUST use `split_bound_frame` — a V1 payload carries no evidence
    binding, so accepting one on the live path would be a silent downgrade.
    """
    if len(data) == PAYLOAD_LEN:
        return data, None
    if len(data) == PAYLOAD_LEN + TOKEN_LEN:
        return data[:PAYLOAD_LEN], data[PAYLOAD_LEN:]
    return None


def split_bound_frame(data: bytes) -> "tuple[bytes, bytes] | None":
    """Strict wire parse of an evidence-bound V2 release frame.

    Exactly 272 bytes → (payload[176], token[96]).
    ANY other length → None — malformed; the caller must ignore it (never
    actuate, never crash).

    There is deliberately NO unsigned V2 form: a V2 payload without its token
    is unverifiable, so the length is exact rather than one-of-two. Notably
    this refuses 0, 32, 96, 128 (a V1 signed frame), 271 and 273 alike — the
    frame is either exactly what the governor minted or it is not a frame.

    Never slices, pads, repairs or reinterprets. An over-long frame must not be
    truncated into a "probably right" prefix: that is precisely the #901 class
    of bug where hostile input took down the callback, and here it would also
    hand the core a payload the governor never signed.
    """
    if len(data) != BOUND_FRAME_LEN:
        return None
    return data[:BOUND_PAYLOAD_LEN], data[BOUND_PAYLOAD_LEN:]


class KirraFrameResult(ctypes.Structure):
    _fields_ = [
        ("kind", ctypes.c_int32),          # 0 Released, 1 SerialError, 2 Refused
        ("refusal_code", ctypes.c_int32),  # meaningful iff kind==2
        ("write", ctypes.c_uint8),         # 1 → actuate (linear, angular)
        ("sequence", ctypes.c_uint64),
        ("linear", ctypes.c_double),
        ("angular", ctypes.c_double),
    ]


class KirraTickResult(ctypes.Structure):
    _fields_ = [
        ("write", ctypes.c_uint8),
        ("linear", ctypes.c_double),
        ("angular", ctypes.c_double),
    ]


class KirraHealth(ctypes.Structure):
    _fields_ = [
        ("releases", ctypes.c_uint64),
        ("refusals_total", ctypes.c_uint64),
        ("no_token", ctypes.c_uint64),
        ("digest_mismatch", ctypes.c_uint64),
        ("signature_invalid", ctypes.c_uint64),
        ("undecodable", ctypes.c_uint64),
        ("stale", ctypes.c_uint64),
        ("sequence_not_advanced", ctypes.c_uint64),
        ("consecutive_signature_invalid", ctypes.c_uint32),
        ("key_mismatch_alarm", ctypes.c_uint8),
        ("silent", ctypes.c_uint8),
    ]


# --- Evidence-bound V2 result/health structs -------------------------------
# Field order and integer widths MUST match `KirraBoundFrameResult` and
# `KirraBoundHealth` in crates/kirra-consumer-ffi/src/lib.rs EXACTLY. Both are
# #[repr(C)] and scalar-only, so ctypes' native alignment reproduces the Rust
# layout; a reordered or resized field here would silently misread the core's
# verdict, which is the one thing this boundary must never do.

class KirraBoundFrameResult(ctypes.Structure):
    _fields_ = [
        ("kind", ctypes.c_int32),          # 0 Released, 1 SerialError, 2 Refused
        ("refusal_code", ctypes.c_int32),  # meaningful iff kind==2
        ("write", ctypes.c_uint8),         # 1 → actuate (linear, angular)
        ("sequence", ctypes.c_uint64),
        ("nonce", ctypes.c_uint64),
        ("scan_sequence", ctypes.c_uint64),
        ("tracker_generation", ctypes.c_uint64),
        ("linear", ctypes.c_double),
        ("angular", ctypes.c_double),
    ]


class KirraBoundHealth(ctypes.Structure):
    _fields_ = [
        ("releases", ctypes.c_uint64),
        ("refusals_total", ctypes.c_uint64),
        ("no_token", ctypes.c_uint64),
        ("digest_mismatch", ctypes.c_uint64),
        ("signature_invalid", ctypes.c_uint64),
        ("undecodable", ctypes.c_uint64),
        ("future_issued", ctypes.c_uint64),
        ("expired", ctypes.c_uint64),
        ("lifetime_too_long", ctypes.c_uint64),
        ("sequence_not_advanced", ctypes.c_uint64),
        ("nonce_not_advanced", ctypes.c_uint64),
        ("profile_digest_mismatch", ctypes.c_uint64),
        ("perception_frame_superseded", ctypes.c_uint64),
        ("consecutive_signature_invalid", ctypes.c_uint32),
        ("key_mismatch_alarm", ctypes.c_uint8),
        ("silent", ctypes.c_uint8),
    ]


def _find_lib() -> str:
    env = os.environ.get("KIRRA_CONSUMER_LIB")
    if env:
        if not Path(env).is_file():
            raise FileNotFoundError(f"KIRRA_CONSUMER_LIB={env} does not exist")
        return env
    # Search the workspace target dirs (release preferred for deployment).
    here = Path(__file__).resolve().parent.parent
    for prof in ("release", "debug"):
        cand = here / "target" / prof / "libkirra_consumer_ffi.so"
        if cand.is_file():
            return str(cand)
    raise FileNotFoundError(
        "libkirra_consumer_ffi.so not found. Build it "
        "(`cargo build -p kirra-consumer-ffi --release`) or set KIRRA_CONSUMER_LIB."
    )


def _load(lib_path: str) -> ctypes.CDLL:
    lib = ctypes.CDLL(lib_path)
    lib.kirra_consumer_new.restype = ctypes.c_void_p
    lib.kirra_consumer_new.argtypes = [
        ctypes.c_char_p,   # vk (32 bytes)
        ctypes.c_uint64,   # freshness_window_ms
        ctypes.c_uint64,   # control_period_ms
        ctypes.c_uint32,   # missed_periods
        ctypes.c_double,   # stop_decel_mps2
        ctypes.c_double,   # vx_max
        ctypes.c_double,   # vz_max
    ]
    lib.kirra_consumer_free.restype = None
    lib.kirra_consumer_free.argtypes = [ctypes.c_void_p]

    lib.kirra_consumer_on_frame.restype = None
    lib.kirra_consumer_on_frame.argtypes = [
        ctypes.c_void_p,
        ctypes.c_char_p,   # payload (32)
        ctypes.c_char_p,   # token (96) or None
        ctypes.c_size_t,   # token_len
        ctypes.c_uint64,   # now_ms
        ctypes.POINTER(KirraFrameResult),
    ]
    lib.kirra_consumer_on_tick.restype = None
    lib.kirra_consumer_on_tick.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint64,
        ctypes.POINTER(KirraTickResult),
    ]
    lib.kirra_consumer_health.restype = None
    lib.kirra_consumer_health.argtypes = [ctypes.c_void_p, ctypes.POINTER(KirraHealth)]

    lib.kirra_consumer_alarm_explanation.restype = ctypes.c_char_p
    lib.kirra_consumer_alarm_explanation.argtypes = []
    return lib


def _bind_bound(lib: ctypes.CDLL) -> ctypes.CDLL:
    """Bind the evidence-bound V2 entry points on an already-loaded library.

    Kept separate from `_load` so a V1-only deployment keeps working against an
    older .so: the V2 symbols are resolved only when a BoundKirraConsumer is
    actually constructed, and a missing symbol becomes a clear error there
    rather than breaking every V1 caller at import.
    """
    lib.kirra_bound_consumer_new.restype = ctypes.c_void_p
    lib.kirra_bound_consumer_new.argtypes = [
        ctypes.c_char_p,   # vk (32 bytes)
        ctypes.c_char_p,   # expected_profile_digest (32 bytes)
        ctypes.c_uint64,   # maximum_token_lifetime_ms
        ctypes.c_uint64,   # control_period_ms
        ctypes.c_uint32,   # missed_periods
        ctypes.c_double,   # stop_decel_mps2
        ctypes.c_double,   # vx_max
        ctypes.c_double,   # vz_max
    ]
    lib.kirra_bound_consumer_free.restype = None
    lib.kirra_bound_consumer_free.argtypes = [ctypes.c_void_p]

    lib.kirra_bound_consumer_on_frame.restype = None
    lib.kirra_bound_consumer_on_frame.argtypes = [
        ctypes.c_void_p,
        ctypes.c_char_p,   # payload (176)
        ctypes.c_char_p,   # token (96) or None
        ctypes.c_size_t,   # token_len
        ctypes.c_uint64,   # now_ms
        ctypes.POINTER(KirraBoundFrameResult),
    ]
    lib.kirra_bound_consumer_on_tick.restype = None
    lib.kirra_bound_consumer_on_tick.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint64,
        ctypes.POINTER(KirraTickResult),
    ]
    lib.kirra_bound_consumer_health.restype = None
    lib.kirra_bound_consumer_health.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(KirraBoundHealth),
    ]
    return lib


class KirraConsumer:
    """The verifying motor consumer, driven from Python. Fail-closed: the
    constructor raises if the core refuses the config (a NULL handle) — the node
    must abort rather than run unverified."""

    def __init__(
        self,
        governor_vk: bytes,
        *,
        freshness_window_ms: int,
        control_period_ms: int,
        missed_periods: int,
        stop_decel_mps2: float,
        vx_max: float,
        vz_max: float,
        lib_path: str | None = None,
    ) -> None:
        if len(governor_vk) != VK_LEN:
            raise ValueError(f"governor_vk must be {VK_LEN} bytes, got {len(governor_vk)}")
        self._lib = _load(lib_path or _find_lib())
        self._h = self._lib.kirra_consumer_new(
            governor_vk,
            ctypes.c_uint64(freshness_window_ms),
            ctypes.c_uint64(control_period_ms),
            ctypes.c_uint32(missed_periods),
            ctypes.c_double(stop_decel_mps2),
            ctypes.c_double(vx_max),
            ctypes.c_double(vz_max),
        )
        if not self._h:
            # NULL = fail-closed: bad key, bad decel/deadline, or bad envelope.
            raise ValueError(
                "kirra_consumer_new refused the configuration (fail-closed NULL handle): "
                "check the pinned key, stop_decel_mps2 (finite > 0), control_period_ms/"
                "missed_periods (non-zero), and vx_max/vz_max (finite > 0)."
            )

    def on_frame(self, payload: bytes, token: bytes | None, now_ms: int) -> KirraFrameResult:
        if len(payload) != PAYLOAD_LEN:
            raise ValueError(f"payload must be {PAYLOAD_LEN} bytes")
        out = KirraFrameResult()
        if token is None:
            self._lib.kirra_consumer_on_frame(
                self._h, payload, None, 0, ctypes.c_uint64(now_ms), ctypes.byref(out)
            )
        else:
            if len(token) != TOKEN_LEN:
                raise ValueError(f"token must be {TOKEN_LEN} bytes")
            self._lib.kirra_consumer_on_frame(
                self._h, payload, token, TOKEN_LEN, ctypes.c_uint64(now_ms), ctypes.byref(out)
            )
        return out

    def on_tick(self, now_ms: int) -> KirraTickResult:
        out = KirraTickResult()
        self._lib.kirra_consumer_on_tick(self._h, ctypes.c_uint64(now_ms), ctypes.byref(out))
        return out

    def health(self) -> KirraHealth:
        out = KirraHealth()
        self._lib.kirra_consumer_health(self._h, ctypes.byref(out))
        return out

    def alarm_explanation(self) -> str:
        return self._lib.kirra_consumer_alarm_explanation().decode("utf-8", "replace")

    def close(self) -> None:
        if getattr(self, "_h", None):
            self._lib.kirra_consumer_free(self._h)
            self._h = None

    def __del__(self) -> None:
        self.close()


class BoundKirraConsumer:
    """The evidence-bound V2 verifying motor consumer, driven from Python.

    Runs in parallel with `KirraConsumer` (V1), which is untouched. The two are
    deliberately separate handle types in Rust, so a V2 payload can never be
    submitted to the V1 core or silently downgraded.

    🔴 NO verification logic lives here. Decoding, Ed25519 signature checks,
    issue/expiry and maximum-lifetime bounds, sequence and nonce replay,
    pinned-profile-digest comparison, perception-frame supersession, proposal
    digest binding and the liveness/decel ramp are ALL decided in Rust and
    returned across this boundary. Python validates its own configuration,
    splits fixed-size wire bytes, calls the core, and obeys `write`.

    Fail-closed: the constructor raises on malformed arguments BEFORE calling C,
    and again if the core returns a NULL handle (bad key, all-zero profile
    digest, zero maximum lifetime, invalid liveness config, invalid envelope).
    """

    def __init__(
        self,
        governor_vk: bytes,
        expected_profile_digest: bytes,
        *,
        maximum_token_lifetime_ms: int,
        control_period_ms: int,
        missed_periods: int,
        stop_decel_mps2: float,
        vx_max: float,
        vz_max: float,
        lib_path: str | None = None,
    ) -> None:
        # Validate BEFORE crossing into C: ctypes would happily read past a
        # short buffer, so a wrong-length key or digest must be refused here.
        if not isinstance(governor_vk, (bytes, bytearray)):
            raise TypeError(
                f"governor_vk must be bytes, got {type(governor_vk).__name__}")
        if len(governor_vk) != VK_LEN:
            raise ValueError(
                f"governor_vk must be {VK_LEN} bytes, got {len(governor_vk)}")
        if not isinstance(expected_profile_digest, (bytes, bytearray)):
            raise TypeError(
                "expected_profile_digest must be bytes, got "
                f"{type(expected_profile_digest).__name__}")
        if len(expected_profile_digest) != DIGEST_LEN:
            raise ValueError(
                f"expected_profile_digest must be {DIGEST_LEN} bytes, "
                f"got {len(expected_profile_digest)}")

        self._h = None  # so close()/__del__ are safe if the load below raises
        self._lib = _bind_bound(_load(lib_path or _find_lib()))
        self._h = self._lib.kirra_bound_consumer_new(
            bytes(governor_vk),
            bytes(expected_profile_digest),
            ctypes.c_uint64(maximum_token_lifetime_ms),
            ctypes.c_uint64(control_period_ms),
            ctypes.c_uint32(missed_periods),
            ctypes.c_double(stop_decel_mps2),
            ctypes.c_double(vx_max),
            ctypes.c_double(vz_max),
        )
        if not self._h:
            # NULL = fail-closed. Never run unverified on a half-configured core.
            raise ValueError(
                "kirra_bound_consumer_new refused the configuration (fail-closed "
                "NULL handle): check the pinned governor key, the pinned profile "
                "digest (32 bytes, not all-zero), maximum_token_lifetime_ms "
                "(non-zero), control_period_ms/missed_periods (non-zero), "
                "stop_decel_mps2 (finite > 0), and vx_max/vz_max (finite > 0)."
            )

    @property
    def lib(self) -> ctypes.CDLL:
        """The loaded library, so sibling bindings (e.g. the R2CP link) reuse
        this handle instead of dlopening a second copy of the same .so."""
        return self._lib

    def _handle(self):
        if not getattr(self, "_h", None):
            # A use-after-free would pass a dangling pointer to Rust. Refuse.
            raise ValueError("BoundKirraConsumer is closed — handle not reusable")
        return self._h

    def on_frame(
        self, payload: bytes, token: bytes | None, now_ms: int
    ) -> KirraBoundFrameResult:
        """Submit one V2 frame. The returned `write` is the ONLY authority on
        whether Python may touch the motor."""
        if len(payload) != BOUND_PAYLOAD_LEN:
            raise ValueError(f"payload must be {BOUND_PAYLOAD_LEN} bytes")
        out = KirraBoundFrameResult()
        handle = self._handle()
        if token is None:
            # The core refuses a tokenless V2 frame (NO_TOKEN); passing it
            # through keeps that decision in Rust rather than guessing here.
            self._lib.kirra_bound_consumer_on_frame(
                handle, payload, None, 0, ctypes.c_uint64(now_ms), ctypes.byref(out)
            )
        else:
            if len(token) != TOKEN_LEN:
                raise ValueError(f"token must be {TOKEN_LEN} bytes")
            self._lib.kirra_bound_consumer_on_frame(
                handle, payload, token, TOKEN_LEN,
                ctypes.c_uint64(now_ms), ctypes.byref(out),
            )
        return out

    def on_tick(self, now_ms: int) -> KirraTickResult:
        """Run the liveness clock (the SS-002 decel-to-zero ramp)."""
        # Resolve the handle FIRST: a closed consumer must fail on its own
        # terms, before any attribute of the library is touched.
        handle = self._handle()
        out = KirraTickResult()
        self._lib.kirra_bound_consumer_on_tick(
            handle, ctypes.c_uint64(now_ms), ctypes.byref(out)
        )
        return out

    def health(self) -> KirraBoundHealth:
        handle = self._handle()
        out = KirraBoundHealth()
        self._lib.kirra_bound_consumer_health(handle, ctypes.byref(out))
        return out

    def alarm_explanation(self) -> str:
        # The alarm sentence is shared with V1 (one static string in the core).
        return self._lib.kirra_consumer_alarm_explanation().decode("utf-8", "replace")

    def close(self) -> None:
        """Free the core handle. Idempotent — a second call is a no-op, and the
        handle is cleared so it can never be reused (double-free / use-after-
        free are both structurally impossible from here)."""
        handle = getattr(self, "_h", None)
        if handle:
            self._h = None          # clear FIRST: a raising free must not re-arm
            self._lib.kirra_bound_consumer_free(handle)

    def __del__(self) -> None:
        self.close()
