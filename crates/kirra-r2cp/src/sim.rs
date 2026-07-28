//! **Simulated MCU** — R2CP peer for the bridge, with no hardware.
//!
//! Stage 1 of the migration in `ROADMAP.md` §Deployment reality, and the only
//! stage that can fail cheaply. Before a Kirra image is flashed onto anything,
//! the bridge should have driven a peer that speaks the real protocol and
//! refuses the same things the firmware refuses.
//!
//! ## What this is, and is not
//!
//! It mirrors the firmware's *protocol-visible* behaviour: the `SafetyState`
//! machine from `safety/include/r2/safety/safety_manager.hpp`, the replay rule,
//! command freshness, and the envelope-tightening rule from `PROTOCOL.md`.
//!
//! It is **not** a model of the firmware's internals, and passing against it is
//! **not** evidence about the firmware. It is evidence about the *bridge* —
//! that the host sends well-formed, correctly-sequenced, fresh commands and
//! handles refusals. HIL is what tests the firmware.
//!
//! Deliberately excluded, because simulation cannot honestly cover them:
//! real UART timing, brown-out, cable pull, watchdog expiry under load, and
//! on-target WCET. Those are stage 2.
//!
//! ## Safety rules it enforces (all fail-closed)
//!
//! * **Replay**: `sequence <= last_accepted` is REJECTED. Equal counts as a
//!   replay — the same rule as the QNX judge and the iceoryx2 carrier, so all
//!   three refuse identically.
//! * **Freshness**: a command whose `source_time_us` is older than
//!   `valid_for_us` is stale and refused. A stale command is not a liveness
//!   event either — it cannot feed the watchdog.
//! * **State**: motion is honoured ONLY in `Active`. `Standby`/`Armed` accept
//!   the frame and refuse the motion.
//! * **DISARM from Active requests a CONTROLLED STOP**, never a jump to
//!   Standby (`PROTOCOL.md` §Message catalogue).
//! * **`ACKNOWLEDGE_FAULT` only requests a clear.** A network packet is never
//!   itself physical acknowledgement, so a latched fault stays latched unless
//!   the simulated local input is also asserted.
//! * **Watchdog**: no fresh motion within the command window → controlled stop.

use crate::{DecodeError, Frame, MessageType};

/// The firmware's protocol-visible states (`safety_manager.hpp`). `Boot`,
/// `SelfTest` and `FirmwareUpdate` exist so the names line up one-to-one; the
/// sim starts in `Standby` because POST is not simulated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafetyState {
    Standby,
    Armed,
    Active,
    ControlledStop,
    FaultLatched,
}

impl SafetyState {
    /// The byte the firmware puts on the wire. MIRRORED from the discriminants
    /// in `safety_manager.hpp` — `boot`/`self_test` occupy 0/1 and
    /// `firmware_update` occupies 7, which is why the numbers here start at 2
    /// and are written out rather than derived from this enum's own order.
    #[must_use]
    pub fn to_wire(self) -> u8 {
        match self {
            SafetyState::Standby => 2,
            SafetyState::Armed => 3,
            SafetyState::Active => 4,
            SafetyState::ControlledStop => 5,
            SafetyState::FaultLatched => 6,
        }
    }
}

/// COMMAND_ACK `result` codes.
///
/// ⚠ **PROVISIONAL — this crate is defining them, not mirroring them.**
/// `PROTOCOL.md` §COMMAND_ACK names the eight results in prose ("accepted,
/// clamped, stale, replay, unauthenticated, invalid, disarmed and faulted") but
/// binds no numbers, and the firmware does not yet emit an ACK. The values below
/// take the prose order.
///
/// The obligation this creates is recorded in `firmware/rosmaster-r2/docs/
/// ROADMAP.md`: when the firmware implements COMMAND_ACK it must either adopt
/// these values or change them here, and the differential harness is what will
/// catch a silent disagreement. Until then, a bridge test asserting on a result
/// code is asserting against a proposal.
pub mod ack_result {
    pub const ACCEPTED: u16 = 0;
    /// The sim never emits this: it refuses out-of-envelope commands rather
    /// than tightening them. Reserved so the numbering matches the prose.
    pub const CLAMPED: u16 = 1;
    pub const STALE: u16 = 2;
    pub const REPLAY: u16 = 3;
    pub const UNAUTHENTICATED: u16 = 4;
    pub const INVALID: u16 = 5;
    pub const DISARMED: u16 = 6;
    pub const FAULTED: u16 = 7;
}

/// Why a command was refused. Returned to the caller AND encoded into the
/// COMMAND_ACK, so a bridge test can assert the reason, not just the failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Replay,
    Stale,
    NotActive,
    FaultLatched,
    MalformedPayload,
    /// The frame decoded, but the type is not one the MCU accepts here.
    UnexpectedType,
}

impl Refusal {
    #[must_use]
    pub fn to_ack_result(self) -> u16 {
        match self {
            Refusal::Replay => ack_result::REPLAY,
            Refusal::Stale => ack_result::STALE,
            Refusal::NotActive => ack_result::DISARMED,
            Refusal::FaultLatched => ack_result::FAULTED,
            // A payload the MCU could not parse and a type it does not accept
            // are both "the frame was well-formed on the wire and wrong above
            // it" — the operator-visible distinction is in the log, not here.
            Refusal::MalformedPayload | Refusal::UnexpectedType => ack_result::INVALID,
        }
    }
}

// No `Eq`: `Accepted` carries f32, which is only `PartialEq`. Comparing
// accepted motion by value is exactly what the tests want, and derived Eq would
// be a lie about float equality anyway.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    Accepted {
        velocity_mps: f32,
        curvature_per_m: f32,
    },
    /// Accepted as a frame, but commanded motion was refused — the distinction
    /// matters: the link is healthy, the command is not.
    Refused(Refusal),
    StateChanged(SafetyState),
    Ignored,
}

/// A decoded MOTION_COMMAND (`PROTOCOL.md` §MOTION_COMMAND).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionCommand {
    pub command_id: u32,
    pub valid_for_us: u32,
    pub velocity_mps: f32,
    pub curvature_per_m: f32,
    pub acceleration_limit_mps2: f32,
    pub jerk_limit_mps3: f32,
    pub mode: u8,
}

pub const MODE_STOP: u8 = 0;
pub const MODE_TRACK: u8 = 1;
pub const MODE_HOLD_ZERO: u8 = 2;

/// `valid_for_us` is bounded to 20–1000 ms by configuration.
pub const MIN_VALID_FOR_US: u32 = 20_000;
pub const MAX_VALID_FOR_US: u32 = 1_000_000;

impl MotionCommand {
    /// Parse the payload. Non-finite floats are REFUSED, not clamped: a NaN
    /// velocity is a broken sender, and silently substituting zero would hide
    /// it. `PROTOCOL.md`: "All command values are SI units and finite."
    pub fn parse(payload: &[u8]) -> Result<Self, Refusal> {
        if payload.len() < 28 {
            return Err(Refusal::MalformedPayload);
        }
        let f32_at = |o: usize| f32::from_le_bytes(payload[o..o + 4].try_into().unwrap());
        let cmd = Self {
            command_id: u32::from_le_bytes(payload[0..4].try_into().unwrap()),
            valid_for_us: u32::from_le_bytes(payload[4..8].try_into().unwrap()),
            velocity_mps: f32_at(8),
            curvature_per_m: f32_at(12),
            acceleration_limit_mps2: f32_at(16),
            jerk_limit_mps3: f32_at(20),
            mode: payload[24],
        };
        let finite = [
            cmd.velocity_mps,
            cmd.curvature_per_m,
            cmd.acceleration_limit_mps2,
            cmd.jerk_limit_mps3,
        ];
        if finite.iter().any(|v| !v.is_finite()) {
            return Err(Refusal::MalformedPayload);
        }
        if !(MIN_VALID_FOR_US..=MAX_VALID_FOR_US).contains(&cmd.valid_for_us) {
            return Err(Refusal::MalformedPayload);
        }
        Ok(cmd)
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(28);
        p.extend_from_slice(&self.command_id.to_le_bytes());
        p.extend_from_slice(&self.valid_for_us.to_le_bytes());
        p.extend_from_slice(&self.velocity_mps.to_le_bytes());
        p.extend_from_slice(&self.curvature_per_m.to_le_bytes());
        p.extend_from_slice(&self.acceleration_limit_mps2.to_le_bytes());
        p.extend_from_slice(&self.jerk_limit_mps3.to_le_bytes());
        p.push(self.mode);
        p.extend_from_slice(&[0u8; 3]); // reserved
        p
    }
}

/// The simulated MCU. Pure: time is injected, so every rule below is testable
/// without sleeping.
#[derive(Debug)]
pub struct SimulatedMcu {
    pub state: SafetyState,
    /// Highest accepted sequence. `<=` is a replay (equal included).
    pub last_accepted_sequence: Option<u32>,
    pub last_motion_at_us: Option<u64>,
    pub last_valid_for_us: u32,
    pub physical_ack_asserted: bool,
    pub accepted_commands: u64,
    pub refusals: u64,
    /// The MCU's OWN sequence space for the frames it sends. It is separate from
    /// `last_accepted_sequence` (the host's) because the two directions are
    /// independently replay-checked — sharing one counter would let traffic in
    /// one direction advance the other's watermark.
    pub next_ack_sequence: u32,
}

impl Default for SimulatedMcu {
    fn default() -> Self {
        Self::new()
    }
}

impl SimulatedMcu {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: SafetyState::Standby,
            last_accepted_sequence: None,
            last_motion_at_us: None,
            last_valid_for_us: 0,
            physical_ack_asserted: false,
            accepted_commands: 0,
            refusals: 0,
            next_ack_sequence: 1,
        }
    }

    /// The frame to send back, if any.
    ///
    /// One dispatcher rather than an ACK-for-everything rule, because a HELLO
    /// must be answered with a HELLO: a peer probing for "is anything there
    /// that speaks R2CP" learns nothing from a COMMAND_ACK, and the whole point
    /// of the probe is that a NON-peer cannot accidentally satisfy it.
    ///
    /// An undecodable frame never reaches here — the MCU cannot echo a
    /// sequence it never read, so silence is the only honest answer.
    pub fn reply_for(&mut self, to: &Frame, outcome: Outcome, now_us: u64) -> Option<Frame> {
        if to.message_type == MessageType::Hello {
            // A HELLO already marked as a response is someone else's answer,
            // not a probe; replying would start a loop between two peers.
            if to.flags & crate::FLAG_RESPONSE != 0 {
                return None;
            }
            let probe = crate::handshake::Hello::parse(&to.payload).ok()?;
            let seq = self.next_ack_sequence;
            self.next_ack_sequence = self.next_ack_sequence.wrapping_add(1);
            return Some(
                crate::handshake::Hello {
                    // The nonce is ECHOED, never regenerated: it is what ties
                    // this answer to that probe.
                    nonce: probe.nonce,
                    implementation_id: crate::handshake::SIM_IMPLEMENTATION_ID,
                    firmware_semver: 0,
                    minimum_major: crate::PROTOCOL_MAJOR,
                    maximum_major: crate::PROTOCOL_MAJOR,
                    maximum_frame: crate::MAX_PAYLOAD as u16,
                }
                .to_frame(seq, now_us, true),
            );
        }
        Some(self.ack_for(to, outcome, now_us))
    }

    /// Build the COMMAND_ACK for `outcome`, in reply to `to`.
    ///
    /// "ACK proves protocol handling, not physical movement" (`PROTOCOL.md`
    /// §COMMAND_ACK) — an `ACCEPTED` here says the frame was well-formed,
    /// in-sequence, fresh and admissible in the current state. It says nothing
    /// about wheels turning, and a bridge must not read it as motion confirmed.
    pub fn ack_for(&mut self, to: &Frame, outcome: Outcome, now_us: u64) -> Frame {
        let result = match outcome {
            Outcome::Accepted { .. } | Outcome::StateChanged(_) | Outcome::Ignored => {
                ack_result::ACCEPTED
            }
            Outcome::Refused(r) => r.to_ack_result(),
        };
        // command_id is the sender's own correlation id, carried in the motion
        // payload. A frame whose payload did not parse has none to echo, so 0
        // is sent — the received_sequence field is what correlates then.
        let command_id = MotionCommand::parse(&to.payload).map_or(0, |c| c.command_id);

        let mut p = Vec::with_capacity(28);
        p.extend_from_slice(&command_id.to_le_bytes());
        p.extend_from_slice(&to.sequence.to_le_bytes());
        p.extend_from_slice(&now_us.to_le_bytes());
        p.extend_from_slice(&result.to_le_bytes());
        p.push(self.state.to_wire());
        p.push(0); // reserved
        let active_faults: u64 = u64::from(self.state == SafetyState::FaultLatched);
        p.extend_from_slice(&active_faults.to_le_bytes());

        let seq = self.next_ack_sequence;
        self.next_ack_sequence = self.next_ack_sequence.wrapping_add(1);
        let mut frame =
            Frame::new(MessageType::CommandAcknowledgement, seq, now_us).with_payload(&p);
        frame.flags = crate::FLAG_RESPONSE;
        if self.state == SafetyState::FaultLatched {
            frame.flags |= crate::FLAG_FAULT_LATCHED;
        }
        frame
    }

    /// The replay rule, shared with the QNX judge and the iceoryx2 carrier:
    /// `sequence <= last_accepted` is a replay. Equal is NOT "the same command
    /// again, harmlessly" — it is the signature of a captured frame replayed.
    #[must_use]
    pub fn is_replay(&self, sequence: u32) -> bool {
        matches!(self.last_accepted_sequence, Some(last) if sequence <= last)
    }

    /// Has the command window elapsed with no fresh motion? → controlled stop.
    #[must_use]
    pub fn watchdog_expired(&self, now_us: u64) -> bool {
        match self.last_motion_at_us {
            None => false,
            Some(t) => now_us.saturating_sub(t) > u64::from(self.last_valid_for_us),
        }
    }

    /// Advance time. Returns a state change if the watchdog fired.
    pub fn tick(&mut self, now_us: u64) -> Option<SafetyState> {
        if self.state == SafetyState::Active && self.watchdog_expired(now_us) {
            self.state = SafetyState::ControlledStop;
            self.last_motion_at_us = None;
            return Some(self.state);
        }
        None
    }

    /// Handle one decoded frame at `now_us`.
    pub fn handle(&mut self, frame: &Frame, now_us: u64) -> Outcome {
        // Replay is checked BEFORE anything else and before any state change,
        // so a replayed ARM cannot move the machine either.
        if self.is_replay(frame.sequence) {
            self.refusals += 1;
            return Outcome::Refused(Refusal::Replay);
        }

        let outcome = match frame.message_type {
            MessageType::MotionCommand => self.handle_motion(frame, now_us),
            MessageType::Arm => self.transition(SafetyState::Armed, &[SafetyState::Standby]),
            MessageType::Activate => self.transition(SafetyState::Active, &[SafetyState::Armed]),
            MessageType::Disarm => {
                // From Active this is a REQUEST for a controlled stop, never a
                // jump to Standby (PROTOCOL.md §Message catalogue).
                let next = if self.state == SafetyState::Active {
                    SafetyState::ControlledStop
                } else {
                    SafetyState::Standby
                };
                self.state = next;
                self.last_motion_at_us = None;
                Outcome::StateChanged(next)
            }
            MessageType::AcknowledgeFault => {
                // A network packet is never itself physical acknowledgement.
                if self.state == SafetyState::FaultLatched && self.physical_ack_asserted {
                    self.state = SafetyState::Standby;
                    Outcome::StateChanged(self.state)
                } else {
                    self.refusals += 1;
                    Outcome::Refused(Refusal::FaultLatched)
                }
            }
            // A HELLO is answered (see `reply_for`), not acted on: it changes
            // no state, so it is `Ignored` by the state machine specifically.
            MessageType::Hello | MessageType::TimeSyncRequest => Outcome::Ignored,
            _ => {
                self.refusals += 1;
                Outcome::Refused(Refusal::UnexpectedType)
            }
        };

        // Only a frame that was not refused advances the watermark — a refused
        // command must never consume a sequence number, or an attacker could
        // burn the space with garbage.
        if !matches!(outcome, Outcome::Refused(_)) {
            self.last_accepted_sequence = Some(frame.sequence);
        }
        outcome
    }

    fn transition(&mut self, to: SafetyState, from: &[SafetyState]) -> Outcome {
        if self.state == SafetyState::FaultLatched {
            self.refusals += 1;
            return Outcome::Refused(Refusal::FaultLatched);
        }
        if from.contains(&self.state) {
            self.state = to;
            Outcome::StateChanged(to)
        } else {
            self.refusals += 1;
            Outcome::Refused(Refusal::NotActive)
        }
    }

    fn handle_motion(&mut self, frame: &Frame, now_us: u64) -> Outcome {
        let cmd = match MotionCommand::parse(&frame.payload) {
            Ok(c) => c,
            Err(r) => {
                self.refusals += 1;
                return Outcome::Refused(r);
            }
        };
        if self.state == SafetyState::FaultLatched {
            self.refusals += 1;
            return Outcome::Refused(Refusal::FaultLatched);
        }
        // Freshness: a command older than its own validity window is stale.
        // Checked BEFORE the state test so a stale command can never feed the
        // watchdog even in Active.
        let age = now_us.saturating_sub(frame.source_time_us);
        if age > u64::from(cmd.valid_for_us) {
            self.refusals += 1;
            return Outcome::Refused(Refusal::Stale);
        }
        if self.state != SafetyState::Active {
            self.refusals += 1;
            return Outcome::Refused(Refusal::NotActive);
        }
        self.last_motion_at_us = Some(now_us);
        self.last_valid_for_us = cmd.valid_for_us;
        self.accepted_commands += 1;
        let (v, k) = if cmd.mode == MODE_TRACK {
            (cmd.velocity_mps, cmd.curvature_per_m)
        } else {
            (0.0, 0.0) // STOP and HOLD_ZERO command no motion
        };
        Outcome::Accepted {
            velocity_mps: v,
            curvature_per_m: k,
        }
    }

    /// Force a latched fault (the simulated e-stop / hardware fault input).
    pub fn raise_latched_fault(&mut self) {
        self.state = SafetyState::FaultLatched;
        self.last_motion_at_us = None;
    }
}

/// Byte-level faults the sim can INJECT into its own output, so the bridge's
/// decoder is exercised against a hostile peer rather than a polite one.
///
/// Everything here is a mutation of ALREADY-ENCODED bytes. Faults that cannot be
/// expressed that way — a reserved flag bit lives in the header, before COBS —
/// get their own constructor ([`encode_with_raw_flags`]) rather than a byte
/// smear that would fail for the wrong reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InjectedFault {
    None,
    /// Drop the last byte before the delimiter.
    Truncate,
    /// Flip a bit in the CRC field.
    CorruptCrc,
    /// Flip a bit in the frame body — the CRC must catch it.
    CorruptPayload,
    /// Emit a frame longer than the maximum.
    Oversize,
}

/// Apply a byte-level fault to an encoded frame (delimiter included).
#[must_use]
pub fn inject(frame_bytes: &[u8], fault: InjectedFault) -> Vec<u8> {
    let mut b = frame_bytes.to_vec();
    match fault {
        InjectedFault::None => {}
        InjectedFault::Truncate => {
            if b.len() > 2 {
                b.remove(b.len() - 2);
            }
        }
        InjectedFault::CorruptCrc | InjectedFault::CorruptPayload => {
            // Both land inside the COBS body; the CRC is what must catch them.
            let idx = if fault == InjectedFault::CorruptCrc {
                b.len() - 2
            } else {
                b.len() / 2
            };
            b[idx] ^= 0x01;
            if b[idx] == 0 {
                b[idx] = 0x02; // keep it a legal COBS byte so the CRC is the gate
            }
        }
        InjectedFault::Oversize => {
            b = vec![0x01; crate::MAX_DECODED_FRAME * 2];
            b.push(0);
        }
    }
    b
}

/// The HOSTILE-PEER encoder: emits a frame carrying `flags` verbatim, including
/// bit patterns [`crate::encode`] refuses to produce.
///
/// A real misbehaving or downgraded peer can put anything in that byte; the
/// bridge's decoder must refuse a reserved bit (`InvalidFlags`) rather than
/// ignore it, and this is the only way to hand it one.
///
/// # Errors
/// Only for an over-long payload — flags are deliberately unchecked here.
pub fn encode_with_raw_flags(frame: &Frame, flags: u8) -> Result<Vec<u8>, DecodeError> {
    if frame.payload.len() > crate::MAX_PAYLOAD {
        return Err(DecodeError::InvalidLength);
    }
    let n = frame.payload.len();
    let body_len = crate::HEADER_SIZE + n;
    let mut decoded = Vec::with_capacity(body_len + crate::CRC_SIZE);
    decoded.extend_from_slice(&crate::MAGIC.to_le_bytes());
    decoded.push(crate::PROTOCOL_MAJOR);
    decoded.push(crate::PROTOCOL_MINOR);
    decoded.push(frame.message_type as u8);
    decoded.push(flags);
    decoded.extend_from_slice(&(n as u16).to_le_bytes());
    decoded.extend_from_slice(&frame.sequence.to_le_bytes());
    decoded.extend_from_slice(&frame.source_time_us.to_le_bytes());
    decoded.extend_from_slice(&frame.payload);
    let crc = crate::crc32c(&decoded[..body_len]);
    decoded.extend_from_slice(&crc.to_le_bytes());
    let mut out = crate::cobs_encode(&decoded);
    out.push(0);
    Ok(out)
}

/// Decode a byte stream into outcomes, refusing anything malformed. This is the
/// bridge-facing entry point the PTY loop drives.
pub fn feed(
    mcu: &mut SimulatedMcu,
    stream: &[u8],
    now_us: u64,
) -> Vec<Result<Outcome, DecodeError>> {
    let (frames, _rest) = crate::split_frames(stream);
    frames
        .iter()
        .map(|f| crate::decode(f).map(|frame| mcu.handle(&frame, now_us)))
        .collect()
}
