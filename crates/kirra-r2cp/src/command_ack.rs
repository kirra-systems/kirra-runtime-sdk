//! COMMAND_ACK payload codec — the host half of `command_ack.cpp`.
//!
//! `PROTOCOL.md` §COMMAND_ACK now BINDS the `result` values normatively, and
//! `firmware/rosmaster-r2/protocol/src/command_ack.cpp` is the normative
//! implementation. This is the second implementation for the Linux bridge, held
//! to it by the differential harness — same discipline as the frame codec, and
//! for the same reason: where the two disagree, the firmware is right.
//!
//! ## What an ACK does and does not say
//!
//! It proves protocol HANDLING. `Accepted` means the command was well-formed,
//! in-sequence, fresh and admissible in the current state. It says nothing
//! about wheels turning, and a bridge that logs it as "moving" is overclaiming.
//!
//! ## Unassigned values are refused, never coerced
//!
//! A `result` this build does not recognise is not evidence that a command
//! succeeded. Mapping it to the nearest known code would turn "I do not know
//! what happened" into "accepted" — which is the single most dangerous
//! direction for this particular field to fail in.

/// `PROTOCOL.md` §COMMAND_ACK: 28 bytes, little-endian.
pub const COMMAND_ACK_PAYLOAD_LEN: usize = 28;

/// The normative result codes. Values 8–65535 are unassigned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum AckResult {
    Accepted = 0,
    /// Honoured in MODIFIED form, tightened against the calibrated envelope.
    ///
    /// Success-with-derate, NOT a refusal: `effective = min(commanded,
    /// calibrated_hard_limit)`, so a clamp means the MCU's envelope was
    /// tighter than what was asked for — the command still took effect. A
    /// caller that treats this as a failure will retry a command that is
    /// already being obeyed.
    Clamped = 1,
    Stale = 2,
    Replay = 3,
    Unauthenticated = 4,
    Invalid = 5,
    Disarmed = 6,
    Faulted = 7,
}

/// The largest assigned `result`. Anything above is refused on decode.
pub const MAX_ASSIGNED_ACK_RESULT: u16 = 7;

impl AckResult {
    /// Unassigned values return `None` — see the module docs on why this must
    /// not coerce.
    #[must_use]
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0 => AckResult::Accepted,
            1 => AckResult::Clamped,
            2 => AckResult::Stale,
            3 => AckResult::Replay,
            4 => AckResult::Unauthenticated,
            5 => AckResult::Invalid,
            6 => AckResult::Disarmed,
            7 => AckResult::Faulted,
            _ => return None,
        })
    }

    #[must_use]
    pub fn as_u16(self) -> u16 {
        self as u16
    }

    /// Did the command take effect? `Clamped` counts — it was honoured, just
    /// tightened. Everything else is a refusal.
    #[must_use]
    pub fn is_honoured(self) -> bool {
        matches!(self, AckResult::Accepted | AckResult::Clamped)
    }

    /// A short operator-facing phrase. Presentation only; no control flow may
    /// depend on the wording.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            AckResult::Accepted => "accepted",
            AckResult::Clamped => "accepted, clamped to the calibrated envelope",
            AckResult::Stale => "refused: older than its own validity window",
            AckResult::Replay => "refused: sequence did not advance",
            AckResult::Unauthenticated => "refused: missing or invalid authentication tag",
            AckResult::Invalid => "refused: payload did not parse, or unexpected type",
            AckResult::Disarmed => "refused: motion is not honoured in the current state",
            AckResult::Faulted => "refused: a fault is latched",
        }
    }
}

/// `r2::safety::SafetyState` discriminants verbatim (`safety_manager.hpp`).
/// `boot`/`self_test`/`firmware_update` are included so the numbering matches
/// the header — a decoder that invented its own could not be checked against it.
pub const MAX_ASSIGNED_SAFETY_STATE: u8 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandAck {
    pub command_id: u32,
    /// The frame sequence being acknowledged. Zero means UNSOLICITED — a
    /// watchdog-driven controlled stop answers no frame.
    pub received_sequence: u32,
    pub applied_at_us: u64,
    pub result: AckResult,
    pub safety_state: u8,
    pub active_faults: u64,
}

/// Mirrors `AckDecodeStatus` name-for-name so the differential harness can
/// compare VERDICTS, not merely success/failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckDecodeError {
    /// Not exactly [`COMMAND_ACK_PAYLOAD_LEN`]. Not "at least": a longer
    /// payload means the sender believes in a field this decoder does not
    /// have, which is a version disagreement rather than extra data to ignore.
    InvalidLength,
    UnknownResult,
    UnknownSafetyState,
    /// v1 defines the reserved byte as zero. Accepting a non-zero value would
    /// silently consume the escape hatch a future version needs.
    ReservedNotZero,
}

/// Encoding refusals. Mirrors the C++ side's `false` return, split by cause so
/// a caller learns which field it got wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckEncodeError {
    UnknownSafetyState,
}

impl CommandAck {
    /// # Errors
    /// [`AckEncodeError::UnknownSafetyState`] for a state outside the header's
    /// discriminants. Refusing to ENCODE keeps a malformed ack off the wire
    /// rather than leaving the receiver to catch it — the same choice the
    /// firmware makes.
    ///
    /// `result` cannot be out of range: it is an enum, so the unrepresentable
    /// case the C++ side must check for cannot be constructed here.
    pub fn encode(&self) -> Result<Vec<u8>, AckEncodeError> {
        if self.safety_state > MAX_ASSIGNED_SAFETY_STATE {
            return Err(AckEncodeError::UnknownSafetyState);
        }
        let mut p = Vec::with_capacity(COMMAND_ACK_PAYLOAD_LEN);
        p.extend_from_slice(&self.command_id.to_le_bytes());
        p.extend_from_slice(&self.received_sequence.to_le_bytes());
        p.extend_from_slice(&self.applied_at_us.to_le_bytes());
        p.extend_from_slice(&self.result.as_u16().to_le_bytes());
        p.push(self.safety_state);
        p.push(0); // reserved
        p.extend_from_slice(&self.active_faults.to_le_bytes());
        debug_assert_eq!(p.len(), COMMAND_ACK_PAYLOAD_LEN);
        Ok(p)
    }

    /// # Errors
    /// See [`AckDecodeError`]. Every field is validated BEFORE any is used, so
    /// a refusal never yields a partially-parsed ack.
    pub fn parse(payload: &[u8]) -> Result<Self, AckDecodeError> {
        if payload.len() != COMMAND_ACK_PAYLOAD_LEN {
            return Err(AckDecodeError::InvalidLength);
        }
        let raw_result = u16::from_le_bytes([payload[16], payload[17]]);
        let result = AckResult::from_u16(raw_result).ok_or(AckDecodeError::UnknownResult)?;
        let safety_state = payload[18];
        if safety_state > MAX_ASSIGNED_SAFETY_STATE {
            return Err(AckDecodeError::UnknownSafetyState);
        }
        if payload[19] != 0 {
            return Err(AckDecodeError::ReservedNotZero);
        }
        Ok(Self {
            command_id: u32::from_le_bytes(payload[0..4].try_into().unwrap()),
            received_sequence: u32::from_le_bytes(payload[4..8].try_into().unwrap()),
            applied_at_us: u64::from_le_bytes(payload[8..16].try_into().unwrap()),
            result,
            safety_state,
            active_faults: u64::from_le_bytes(payload[20..28].try_into().unwrap()),
        })
    }
}
