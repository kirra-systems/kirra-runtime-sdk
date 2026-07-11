"""ctypes binding to libkirra_consumer_ffi (ADR-0033 decision (c)).

This is the ONLY place Python talks to the verify core. It exposes the C ABI of
`crates/kirra-consumer-ffi` as a small Pythonic `KirraConsumer` wrapper. There is
NO verification logic here: every gate/watermark/freshness/liveness/alarm
decision is made in Rust and returned across this boundary. Python's job is to
present wire bytes and actuate whatever twist the core decides.

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

# Refusal-class codes — MUST match the REF_* constants in the Rust FFI.
REFUSAL_NAMES = {
    0: "NO_TOKEN",
    1: "DIGEST_MISMATCH",
    2: "SIGNATURE_INVALID",
    3: "UNDECODABLE",
    4: "STALE",
    5: "SEQUENCE_NOT_ADVANCED",
}


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
