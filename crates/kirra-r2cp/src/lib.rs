//! **R2CP v1 — the host (Jetson) side of the Kirra MCU link.**
//!
//! `firmware/rosmaster-r2/protocol/src/wire.cpp` is the NORMATIVE decoder. This
//! is a second implementation of the same wire format for the Linux bridge, and
//! the single most important rule about it is:
//!
//! > **This is not a second specification.** Where this disagrees with
//! > `wire.cpp`, `wire.cpp` is right and this is a bug. The differential tests
//! > exist to make that disagreement loud.
//!
//! Why a second implementation at all: the MCU side is `no_std` C++ built for a
//! Cortex-M3, and the bridge is Linux Rust. Sharing one binary is not possible,
//! so the discipline is to share the *corpus* and diff the *behaviour* instead
//! — the same approach `fuzz/corpus/` already takes on the firmware side.
//!
//! ## Frame (from `docs/PROTOCOL.md` §Canonical header)
//!
//! ```text
//! off size field
//!   0    2 magic 0x3252 (LE)
//!   2    1 protocol major
//!   3    1 protocol minor
//!   4    1 message type
//!   5    1 flags
//!   6    2 payload length (LE)
//!   8    4 sequence (LE)
//!  12    8 source monotonic time, µs (LE)
//!  20    N payload
//! 20+N    4 CRC32C over header+payload (LE)
//! ```
//!
//! Then COBS-encoded and terminated with `0x00`.
//!
//! ## What this deliberately does NOT do
//!
//! * **No authorization.** R2CP's `AUTH_TAG` authenticates the LINK — it proves
//!   the bridge sent the bytes. It says nothing about whether the KIRRA checker
//!   authorized the motion they encode. ADR-0033's token-verifying consumer
//!   remains the authorization boundary; see `ROADMAP.md` §Deployment reality.
//! * **No MAC yet.** `encode_authenticated`/`decode_authenticated` are a named
//!   phase gate on the firmware side pending a reviewed, target-benchmarked
//!   HMAC. This crate encodes and decodes the UNAUTHENTICATED path and
//!   *refuses* a tagged frame with [`DecodeError::AuthRequired`], exactly as
//!   `wire.cpp`'s `decode()` does — fail-closed, never a silent pass-through.

#![forbid(unsafe_code)]

pub const MAGIC: u16 = 0x3252;
pub const PROTOCOL_MAJOR: u8 = 1;
pub const PROTOCOL_MINOR: u8 = 0;
pub const HEADER_SIZE: usize = 20;
pub const CRC_SIZE: usize = 4;
pub const MAX_PAYLOAD: usize = 192;
pub const MAX_DECODED_FRAME: usize = HEADER_SIZE + MAX_PAYLOAD + CRC_SIZE;
/// Only bits 0..3 are defined in v1; 4..7 MUST be zero.
pub const KNOWN_FLAG_MASK: u8 = 0x0F;
pub const FLAG_ACK_REQUIRED: u8 = 0x01;
pub const FLAG_RESPONSE: u8 = 0x02;
pub const FLAG_AUTH_TAG: u8 = 0x04;
pub const FLAG_FAULT_LATCHED: u8 = 0x08;

/// The v1 message catalogue (`docs/PROTOCOL.md` §Message catalogue).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Hello = 1,
    Capabilities = 2,
    TimeSyncRequest = 3,
    TimeSyncResponse = 4,
    MotionCommand = 16,
    CommandAcknowledgement = 17,
    Arm = 18,
    Activate = 19,
    Disarm = 20,
    AcknowledgeFault = 21,
    RobotState = 32,
    Odometry = 33,
    Imu = 34,
    Battery = 35,
    Diagnostics = 36,
    FaultEvent = 37,
    ConfigurationGet = 48,
    ConfigurationSet = 49,
    Calibration = 50,
    EnterBootloader = 64,
    FirmwareBlock = 65,
    FirmwareCommit = 66,
}

impl MessageType {
    /// Unknown ids are REJECTED, never coerced — an unrecognised type is a
    /// protocol error, and guessing would let an unsupported command through.
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        use MessageType::*;
        Some(match v {
            1 => Hello,
            2 => Capabilities,
            3 => TimeSyncRequest,
            4 => TimeSyncResponse,
            16 => MotionCommand,
            17 => CommandAcknowledgement,
            18 => Arm,
            19 => Activate,
            20 => Disarm,
            21 => AcknowledgeFault,
            32 => RobotState,
            33 => Odometry,
            34 => Imu,
            35 => Battery,
            36 => Diagnostics,
            37 => FaultEvent,
            48 => ConfigurationGet,
            49 => ConfigurationSet,
            50 => Calibration,
            64 => EnterBootloader,
            65 => FirmwareBlock,
            66 => FirmwareCommit,
            _ => return None,
        })
    }
}

/// Decode failures. Mirrors `r2::protocol::DecodeStatus` name-for-name so a
/// differential test can compare verdicts, not just success/failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Empty,
    Oversized,
    MalformedCobs,
    BadMagic,
    UnsupportedVersion,
    InvalidLength,
    CrcMismatch,
    UnknownMessage,
    InvalidFlags,
    /// The frame carries AUTH_TAG. The unauthenticated path fails closed and
    /// never releases the payload — same contract as `wire.cpp::decode`.
    AuthRequired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub message_type: MessageType,
    pub flags: u8,
    pub sequence: u32,
    pub source_time_us: u64,
    pub payload: Vec<u8>,
}

impl Frame {
    #[must_use]
    pub fn new(message_type: MessageType, sequence: u32, source_time_us: u64) -> Self {
        Self {
            message_type,
            flags: 0,
            sequence,
            source_time_us,
            payload: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_payload(mut self, payload: &[u8]) -> Self {
        self.payload = payload.to_vec();
        self
    }
}

/// CRC32C (Castagnoli), reflected, poly 0x82F63B78 — bit-for-bit the loop in
/// `wire.cpp::crc32c`. Deliberately the same naive implementation rather than a
/// table: it is the shape being differentially tested, and a table would be a
/// second thing to get wrong.
#[must_use]
pub fn crc32c(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0x82F6_3B78 & mask);
        }
    }
    !crc
}

/// COBS encode. Output never contains `0x00`, so the caller's trailing
/// delimiter is unambiguous — that is what bounds resynchronisation to a single
/// delimiter after a corrupt frame.
#[must_use]
pub fn cobs_encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + input.len() / 254 + 2);
    let mut code_index = 0usize;
    out.push(0); // placeholder for the first code byte
    let mut code: u8 = 1;
    for &b in input {
        if b != 0 {
            out.push(b);
            code += 1;
            if code == 0xFF {
                out[code_index] = code;
                code_index = out.len();
                out.push(0);
                code = 1;
            }
        } else {
            out[code_index] = code;
            code_index = out.len();
            out.push(0);
            code = 1;
        }
    }
    out[code_index] = code;
    out
}

/// COBS decode. Returns `None` on a malformed stream (a code byte that runs
/// past the end, or an embedded zero) rather than a truncated best effort.
#[must_use]
pub fn cobs_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0usize;
    while i < input.len() {
        let code = input[i];
        if code == 0 {
            return None; // an embedded delimiter is not valid inside a frame
        }
        i += 1;
        let run = usize::from(code) - 1;
        if i + run > input.len() {
            return None; // code byte overruns the buffer
        }
        for _ in 0..run {
            out.push(input[i]);
            i += 1;
        }
        if code != 0xFF && i < input.len() {
            out.push(0);
        }
    }
    Some(out)
}

/// Encode a frame to wire bytes INCLUDING the trailing `0x00` delimiter.
///
/// # Errors
/// Returns `Err` for an over-long payload or a flag outside `KNOWN_FLAG_MASK` —
/// the same two refusals `wire.cpp::encode` makes.
pub fn encode(frame: &Frame) -> Result<Vec<u8>, DecodeError> {
    if frame.payload.len() > MAX_PAYLOAD {
        return Err(DecodeError::InvalidLength);
    }
    if frame.flags & !KNOWN_FLAG_MASK != 0 {
        return Err(DecodeError::InvalidFlags);
    }
    let n = frame.payload.len();
    let body_len = HEADER_SIZE + n;
    let mut decoded = Vec::with_capacity(body_len + CRC_SIZE);
    decoded.extend_from_slice(&MAGIC.to_le_bytes());
    decoded.push(PROTOCOL_MAJOR);
    decoded.push(PROTOCOL_MINOR);
    decoded.push(frame.message_type as u8);
    decoded.push(frame.flags);
    decoded.extend_from_slice(&(n as u16).to_le_bytes());
    decoded.extend_from_slice(&frame.sequence.to_le_bytes());
    decoded.extend_from_slice(&frame.source_time_us.to_le_bytes());
    decoded.extend_from_slice(&frame.payload);
    let crc = crc32c(&decoded[..body_len]);
    decoded.extend_from_slice(&crc.to_le_bytes());

    let mut out = cobs_encode(&decoded);
    out.push(0);
    Ok(out)
}

/// Decode one COBS-delimited frame (WITHOUT its trailing `0x00`).
///
/// Fail-closed in the order `wire.cpp` checks, so a differential test compares
/// the same verdict for the same bytes.
///
/// # Errors
/// See [`DecodeError`].
pub fn decode(encoded: &[u8]) -> Result<Frame, DecodeError> {
    if encoded.is_empty() {
        return Err(DecodeError::Empty);
    }
    if encoded.len() > MAX_DECODED_FRAME + MAX_DECODED_FRAME / 254 + 2 {
        return Err(DecodeError::Oversized);
    }
    let decoded = cobs_decode(encoded).ok_or(DecodeError::MalformedCobs)?;
    if decoded.len() < HEADER_SIZE + CRC_SIZE {
        return Err(DecodeError::InvalidLength);
    }
    if u16::from_le_bytes([decoded[0], decoded[1]]) != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    if decoded[2] != PROTOCOL_MAJOR {
        return Err(DecodeError::UnsupportedVersion);
    }
    let payload_len = usize::from(u16::from_le_bytes([decoded[6], decoded[7]]));
    if payload_len > MAX_PAYLOAD || decoded.len() != HEADER_SIZE + payload_len + CRC_SIZE {
        return Err(DecodeError::InvalidLength);
    }
    let body_len = HEADER_SIZE + payload_len;
    let want = u32::from_le_bytes([
        decoded[body_len],
        decoded[body_len + 1],
        decoded[body_len + 2],
        decoded[body_len + 3],
    ]);
    if want != crc32c(&decoded[..body_len]) {
        return Err(DecodeError::CrcMismatch);
    }
    let flags = decoded[5];
    if flags & !KNOWN_FLAG_MASK != 0 {
        return Err(DecodeError::InvalidFlags);
    }
    let message_type = MessageType::from_u8(decoded[4]).ok_or(DecodeError::UnknownMessage)?;
    // AUTH_TAG on the unauthenticated path is a REFUSAL, not a downgrade.
    if flags & FLAG_AUTH_TAG != 0 {
        return Err(DecodeError::AuthRequired);
    }
    Ok(Frame {
        message_type,
        flags,
        sequence: u32::from_le_bytes([decoded[8], decoded[9], decoded[10], decoded[11]]),
        source_time_us: u64::from_le_bytes([
            decoded[12],
            decoded[13],
            decoded[14],
            decoded[15],
            decoded[16],
            decoded[17],
            decoded[18],
            decoded[19],
        ]),
        payload: decoded[HEADER_SIZE..body_len].to_vec(),
    })
}

/// Split a byte stream on `0x00` delimiters, yielding candidate frames.
///
/// Resynchronisation is bounded to ONE delimiter: a corrupt frame costs exactly
/// the bytes up to the next `0x00`, never the rest of the stream.
#[must_use]
pub fn split_frames(stream: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut frames = Vec::new();
    let mut current = Vec::new();
    let mut remainder = Vec::new();
    let mut saw_delim = false;
    for &b in stream {
        if b == 0 {
            saw_delim = true;
            if !current.is_empty() {
                frames.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        } else {
            current.push(b);
        }
    }
    if !current.is_empty() {
        remainder = current; // an unterminated tail is NOT a frame yet
    }
    let _ = saw_delim;
    (frames, remainder)
}
