"""Thin ctypes binding over the Rust R2CP link (`libkirra_consumer_ffi.so`).

🔴 THIS FILE MUST NOT LEARN THE PROTOCOL.

Same rule that keeps `kirra_ffi.py` free of keys, signatures and watermarks.
There is exactly ONE R2CP implementation on the host — `crates/kirra-r2cp`,
differentially tested against the firmware's normative `wire.cpp` — and a
second one here would be a third dialect that nothing tests against either of
the others. So this module contains no:

    message IDs · frame sizes · ACK numeric codes · COBS · CRC
    HELLO fields · nonce generation · version comparison · sequence numbers

It contains a handle, four verbs, and typed errors. `robot/r2cp_purity_test.py`
asserts that mechanically, because a convention nobody checks is a convention
that erodes.

## The handle only exists for an identified peer

`open()` performs the HELLO handshake inside Rust and either returns a live
link or raises. There is no object representing "a device we have not
identified", so commanding one is not a mistake this API can express.

That matters concretely on the bring-up robot: `/dev/myserial` is a stock
vendor motor board that does not speak R2CP, and writing R2CP frames into a
vendor command parser attached to motors is undefined behaviour.

## Failure CLASSES, not just failures

`R2cpError.is_config` / `.is_availability` / `.is_protocol` exist because the
consumer's exit code depends on the difference and `kirra-consumer.service`
carries `RestartPreventExitStatus=2`. A missing device is deterministic
configuration and should stop the unit; an MCU that was merely unplugged at
boot must NOT, or reconnecting it would leave the unit dead until someone runs
`systemctl reset-failed`.
"""

from __future__ import annotations

import ctypes
from dataclasses import dataclass

# Status classes, mirrored from the Rust side. These are CLASSES, not protocol
# values — they say what KIND of failure occurred so systemd can behave
# correctly, and carry no wire meaning.
CLASS_OK = 0
CLASS_CONFIG = 1
CLASS_AVAILABILITY = 2
CLASS_PROTOCOL = 3
CLASS_RUNTIME = 4


class R2cpError(RuntimeError):
    """A link failure, carrying the Rust side's status and its class.

    The message is the Rust side's operator-facing sentence — not composed
    here, so there is one wording per failure across every caller.
    """

    def __init__(self, status: int, klass: int, message: str, device: str) -> None:
        super().__init__(f"{message} (device={device})")
        self.status = status
        self.klass = klass
        self.device = device

    @property
    def is_config(self) -> bool:
        """Deterministic configuration: restarting will only repeat it."""
        return self.klass == CLASS_CONFIG

    @property
    def is_availability(self) -> bool:
        """Hardware availability: may resolve without a configuration change.

        A bounded restart is the right response. Mapping this to the
        consumer's config exit would make a reconnected MCU unrecoverable.
        """
        return self.klass == CLASS_AVAILABILITY

    @property
    def is_protocol(self) -> bool:
        """The peer answered and we cannot work with it."""
        return self.klass == CLASS_PROTOCOL


@dataclass(frozen=True)
class PeerInfo:
    """What the peer said about itself. For the log, not for decisions."""

    implementation_id: int
    firmware_semver: int
    maximum_frame: int
    agreed_major: int
    identified: bool
    exclusive: bool

    def describe(self) -> str:
        # Rendered as ASCII when the id is four printable bytes (the
        # convention the Rust side uses: "KSIM"), else hex. Presentation only.
        raw = self.implementation_id.to_bytes(4, "big")
        name = raw.decode("ascii") if all(0x20 <= b < 0x7F for b in raw) else raw.hex()
        return (
            f"mcu_id={name} version={self.agreed_major} "
            f"max_frame={self.maximum_frame} semver={self.firmware_semver}"
        )


@dataclass(frozen=True)
class Ack:
    """One command's outcome.

    `accepted` is the field to branch on. `result` and `safety_state` are
    DIAGNOSTICS: the numeric ACK result codes are provisional until the
    firmware binds them, so no control flow here may depend on their values.
    """

    accepted: bool
    safety_state: int
    result: int
    timed_out: bool


class _PeerInfoStruct(ctypes.Structure):
    _fields_ = [
        ("implementation_id", ctypes.c_uint32),
        ("firmware_semver", ctypes.c_uint32),
        ("maximum_frame", ctypes.c_uint16),
        ("agreed_major", ctypes.c_uint8),
        ("identified", ctypes.c_uint8),
        ("exclusive", ctypes.c_uint8),
    ]


class _AckStruct(ctypes.Structure):
    _fields_ = [
        ("accepted", ctypes.c_uint8),
        ("safety_state", ctypes.c_uint8),
        ("result", ctypes.c_uint16),
        ("timed_out", ctypes.c_uint8),
    ]


def bind(lib: ctypes.CDLL) -> ctypes.CDLL:
    """Resolve the R2CP entry points on an already-loaded library.

    Separate from `kirra_ffi._load` so a deployment whose `.so` predates R2CP
    keeps working: the symbols resolve when a link is actually opened, and a
    missing one becomes a clear error there rather than breaking every
    existing caller at import.
    """
    lib.kirra_r2cp_open.restype = ctypes.c_int32
    lib.kirra_r2cp_open.argtypes = [
        ctypes.c_char_p,  # device path
        ctypes.c_uint16,  # handshake timeout ms
        ctypes.POINTER(ctypes.c_void_p),
    ]
    lib.kirra_r2cp_activate.restype = ctypes.c_int32
    lib.kirra_r2cp_activate.argtypes = [ctypes.c_void_p, ctypes.c_uint16]

    lib.kirra_r2cp_peer_info.restype = ctypes.c_int32
    lib.kirra_r2cp_peer_info.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(_PeerInfoStruct),
    ]
    lib.kirra_r2cp_command.restype = ctypes.c_int32
    lib.kirra_r2cp_command.argtypes = [
        ctypes.c_void_p,
        ctypes.c_float,  # velocity_mps
        ctypes.c_float,  # curvature_per_m
        ctypes.c_float,  # acceleration_limit_mps2
        ctypes.c_float,  # jerk_limit_mps3
        ctypes.c_uint32,  # valid_for_us
        ctypes.c_uint32,  # command_id
        ctypes.c_uint64,  # source_time_us
        ctypes.c_uint16,  # timeout_ms
        ctypes.POINTER(_AckStruct),
    ]
    lib.kirra_r2cp_stop.restype = ctypes.c_int32
    lib.kirra_r2cp_stop.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint64]

    lib.kirra_r2cp_reset.restype = ctypes.c_int32
    lib.kirra_r2cp_reset.argtypes = [ctypes.c_void_p]

    lib.kirra_r2cp_close.restype = None
    lib.kirra_r2cp_close.argtypes = [ctypes.c_void_p]

    lib.kirra_r2cp_status_class.restype = ctypes.c_int32
    lib.kirra_r2cp_status_class.argtypes = [ctypes.c_int32]

    lib.kirra_r2cp_status_message.restype = ctypes.c_char_p
    lib.kirra_r2cp_status_message.argtypes = [ctypes.c_int32]
    return lib


def _raise(lib: ctypes.CDLL, status: int, device: str) -> None:
    raise R2cpError(
        status,
        int(lib.kirra_r2cp_status_class(status)),
        (lib.kirra_r2cp_status_message(status) or b"").decode("utf-8", "replace"),
        device,
    )


class R2cpLink:
    """A link to an IDENTIFIED R2CP peer.

    Constructed only by `open()`, which raises rather than returning an
    unidentified link — so holding one of these is itself the evidence that a
    peer answered.
    """

    def __init__(self, lib: ctypes.CDLL, handle: ctypes.c_void_p, device: str) -> None:
        self._lib = lib
        self._handle = handle
        self.device = device

    def peer_info(self) -> PeerInfo:
        out = _PeerInfoStruct()
        self._lib.kirra_r2cp_peer_info(self._handle, ctypes.byref(out))
        return PeerInfo(
            implementation_id=int(out.implementation_id),
            firmware_semver=int(out.firmware_semver),
            maximum_frame=int(out.maximum_frame),
            agreed_major=int(out.agreed_major),
            identified=bool(out.identified),
            exclusive=bool(out.exclusive),
        )

    def activate(self, timeout_ms: int) -> None:
        """ARM then ACTIVATE. Raises on refusal — never a silent no-op."""
        status = self._lib.kirra_r2cp_activate(self._handle, ctypes.c_uint16(timeout_ms))
        if status != 0:
            _raise(self._lib, status, self.device)

    def command(
        self,
        velocity_mps: float,
        curvature_per_m: float,
        acceleration_limit_mps2: float,
        jerk_limit_mps3: float,
        valid_for_us: int,
        command_id: int,
        source_time_us: int,
        timeout_ms: int,
    ) -> Ack:
        """Transport one already-authorized motion command.

        Nothing here decides, clamps or authorizes: the governor released
        these values upstream and this is the wire. A refusal comes back as an
        `Ack` with `accepted=False` rather than an exception, because a peer
        refusing a command is an expected operating condition (disarmed,
        stale, replayed) and not a link failure.
        """
        out = _AckStruct()
        self._lib.kirra_r2cp_command(
            self._handle,
            ctypes.c_float(velocity_mps),
            ctypes.c_float(curvature_per_m),
            ctypes.c_float(acceleration_limit_mps2),
            ctypes.c_float(jerk_limit_mps3),
            ctypes.c_uint32(valid_for_us),
            ctypes.c_uint32(command_id),
            ctypes.c_uint64(source_time_us),
            ctypes.c_uint16(timeout_ms),
            ctypes.byref(out),
        )
        return Ack(
            accepted=bool(out.accepted),
            safety_state=int(out.safety_state),
            result=int(out.result),
            timed_out=bool(out.timed_out),
        )

    def send_stop(self, valid_for_us: int, source_time_us: int) -> bool:
        """Attempt a stop. Returns whether the BYTES WERE WRITTEN.

        🔴 True is not proof that anything stopped. Nothing is read back, and
        if the peer has been de-identified this may be writing to a device
        that is not an R2CP peer at all. What actually stops a governed robot
        is the peer's own watchdog: commands cease, the command window lapses,
        the MCU enters a controlled stop by itself. This makes that immediate
        when the peer is real.

        Log it as "stop sent", never as "stopped".
        """
        return self._lib.kirra_r2cp_stop(
            self._handle, ctypes.c_uint32(valid_for_us), ctypes.c_uint64(source_time_us)
        ) == 0

    def reset(self) -> None:
        """Drop the identified peer; a fresh `open()` is then required."""
        self._lib.kirra_r2cp_reset(self._handle)

    def close(self) -> None:
        if self._handle:
            self._lib.kirra_r2cp_close(self._handle)
            self._handle = None

    def __enter__(self) -> R2cpLink:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def __del__(self) -> None:
        try:
            self.close()
        except Exception:  # noqa: BLE001 — never raise from a finalizer
            pass


def open(lib: ctypes.CDLL, device: str, handshake_timeout_ms: int) -> R2cpLink:  # noqa: A001
    """Open `device` and identify the peer, or raise `R2cpError`.

    The HELLO handshake — including the nonce, which is drawn from the OS
    entropy pool inside Rust — happens here. On failure there is no handle and
    no link object: an unidentified device is not representable.
    """
    handle = ctypes.c_void_p()
    status = lib.kirra_r2cp_open(
        device.encode("utf-8"),
        ctypes.c_uint16(handshake_timeout_ms),
        ctypes.byref(handle),
    )
    if status != 0 or not handle:
        _raise(lib, status, device)
    return R2cpLink(lib, handle, device)
