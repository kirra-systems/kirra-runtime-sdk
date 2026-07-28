//! HELLO handshake — proving the thing on the other end of the cable is an
//! R2CP peer *before* anything arms.
//!
//! ## Why this exists, concretely
//!
//! The bring-up robot's motor board today is a stock Yahboom/Rosmaster MCU
//! running vendor firmware. It does not speak R2CP and never will. If the
//! consumer's R2CP drive mode were a bare last-hop swap, pointing it at
//! `/dev/myserial` would not merely produce no motion — it would write R2CP
//! frames into a **vendor command parser**, where some byte sequence may well
//! be a valid vendor opcode. "It just won't move" is the optimistic reading;
//! the honest one is that the behaviour is undefined and the board is attached
//! to motors.
//!
//! So the R2CP mode refuses to arm until a peer has ANSWERED. Silence, garbage,
//! or an answer that does not match the probe are all refusals. This is the
//! same fail-closed shape as the rest of the stack: the absence of evidence is
//! never read as evidence of absence of a problem.
//!
//! ## Why HELLO and not CAPABILITIES
//!
//! `PROTOCOL.md` specifies HELLO's payload byte-for-byte. CAPABILITIES it
//! describes only in prose ("fixed bitset plus board revision, MCU ID, IMU
//! type, …") with no layout, so implementing it here would mean inventing a
//! second, larger wire format with no normative definition to hold it to.
//! COMMAND_ACK earned one (`PROTOCOL.md` §COMMAND_ACK, differentially tested);
//! CAPABILITIES has not, and inventing a layout the firmware never agreed to
//! is how a host-only dialect starts.
//!
//! HELLO is sufficient for the job: it carries a nonce (so a reply can be tied
//! to *this* probe rather than a stale or replayed one) and the peer's
//! supported major-version range and maximum frame size — which is exactly the
//! set of facts that decides whether it is safe to send this peer a command.
//!
//! ## What it does NOT establish
//!
//! Nothing about authorization. A successful handshake says "an R2CP peer is
//! there and we agree on the wire format". It says nothing about whether the
//! KIRRA checker authorized any motion — ADR-0033's token-verifying consumer
//! remains that boundary — and nothing about the peer being genuine, since v1
//! ships no link authentication (`FLAG_AUTH_TAG` is refused, not verified).

use crate::{Frame, MessageType, MAX_PAYLOAD, PROTOCOL_MAJOR};

/// HELLO's payload (`PROTOCOL.md` §HELLO): `nonce[16], implementation_id:u32,
/// firmware_semver:u32, minimum_major:u8, maximum_major:u8, maximum_frame:u16`.
pub const HELLO_PAYLOAD_LEN: usize = 28;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hello {
    pub nonce: [u8; 16],
    pub implementation_id: u32,
    pub firmware_semver: u32,
    pub minimum_major: u8,
    pub maximum_major: u8,
    pub maximum_frame: u16,
}

impl Hello {
    /// Parse a HELLO payload.
    ///
    /// # Errors
    /// [`HandshakeError::MalformedHello`] if the payload is not exactly
    /// [`HELLO_PAYLOAD_LEN`] bytes. A short payload is refused rather than
    /// zero-filled: a peer that cannot state its version range has not
    /// answered the question that was asked.
    pub fn parse(payload: &[u8]) -> Result<Self, HandshakeError> {
        if payload.len() != HELLO_PAYLOAD_LEN {
            return Err(HandshakeError::MalformedHello);
        }
        let mut nonce = [0u8; 16];
        nonce.copy_from_slice(&payload[0..16]);
        Ok(Self {
            nonce,
            implementation_id: u32::from_le_bytes(payload[16..20].try_into().unwrap()),
            firmware_semver: u32::from_le_bytes(payload[20..24].try_into().unwrap()),
            minimum_major: payload[24],
            maximum_major: payload[25],
            maximum_frame: u16::from_le_bytes(payload[26..28].try_into().unwrap()),
        })
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(HELLO_PAYLOAD_LEN);
        p.extend_from_slice(&self.nonce);
        p.extend_from_slice(&self.implementation_id.to_le_bytes());
        p.extend_from_slice(&self.firmware_semver.to_le_bytes());
        p.push(self.minimum_major);
        p.push(self.maximum_major);
        p.extend_from_slice(&self.maximum_frame.to_le_bytes());
        p
    }

    /// The host's probe. `nonce` must be unpredictable — it is what ties a
    /// reply to this probe, so a caller passing a constant gets a handshake
    /// that a recorded reply would satisfy.
    #[must_use]
    pub fn probe(nonce: [u8; 16]) -> Self {
        Self {
            nonce,
            implementation_id: HOST_IMPLEMENTATION_ID,
            firmware_semver: 0,
            minimum_major: PROTOCOL_MAJOR,
            maximum_major: PROTOCOL_MAJOR,
            maximum_frame: MAX_PAYLOAD as u16,
        }
    }

    #[must_use]
    pub fn to_frame(self, sequence: u32, source_time_us: u64, response: bool) -> Frame {
        let mut frame =
            Frame::new(MessageType::Hello, sequence, source_time_us).with_payload(&self.encode());
        if response {
            frame.flags = crate::FLAG_RESPONSE;
        }
        frame
    }
}

/// `kirra-r2cp`, the host bridge. Identifies the SPEAKER, not the peer.
pub const HOST_IMPLEMENTATION_ID: u32 = 0x4B52_4231; // "KRB1"
/// The simulated MCU. Deliberately distinct from any firmware id, so a log or
/// a test can never mistake the simulator for a board.
pub const SIM_IMPLEMENTATION_ID: u32 = 0x4B53_494D; // "KSIM"

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandshakeError {
    /// Nothing answered within the timeout. The overwhelmingly likely cause on
    /// this robot is a peer that does not speak R2CP at all — a vendor board.
    NoResponse,
    /// Something answered, but not with a HELLO.
    NotAHello,
    /// A HELLO that was not marked as a response. A peer's own unsolicited
    /// probe is not an answer to ours.
    NotAResponse,
    MalformedHello,
    /// The nonce did not match the probe. Either a stale reply from a previous
    /// session or a replayed recording — never accepted as this probe's answer.
    NonceMismatch,
    /// No overlap between our supported major range and the peer's.
    VersionMismatch {
        peer_min: u8,
        peer_max: u8,
    },
    /// The peer cannot receive frames as large as we may send. Refused up
    /// front rather than discovered when a long command is silently dropped.
    FrameTooSmall {
        peer_maximum: u16,
        required: u16,
    },
    /// The peer advertises a limit below the protocol's own floor — it could
    /// not accept a fully-specified MOTION_COMMAND, so it cannot be a
    /// conforming v1 peer whatever else it claims.
    FrameBelowProtocolMinimum {
        peer_maximum: u16,
        minimum: u16,
    },
    /// The peer advertises a limit ABOVE what this major version permits.
    /// Not harmless: within an agreed major the maximum is fixed, so a larger
    /// claim means the two ends do not agree on the format they just agreed
    /// on. Refused rather than clamped — a peer whose framing assumptions we
    /// cannot pin down is not one to command motion on.
    FrameAboveProtocolMaximum {
        peer_maximum: u16,
        maximum: u16,
    },
}

/// The smallest maximum-frame a conforming v1 peer may advertise.
///
/// Derived, not chosen: `PROTOCOL.md` §MOTION_COMMAND is
/// `command_id:u32, valid_for_us:u32, velocity_mps:f32, curvature_per_m:f32,
/// acceleration_limit_mps2:f32, jerk_limit_mps3:f32, mode:u8, reserved[3],
/// auth_tag[16]` — 44 bytes. A peer that cannot receive 44 cannot receive a
/// fully-specified motion command.
///
/// This crate currently emits the 28-byte UNAUTHENTICATED form (the
/// `auth_tag` path is a named phase gate, not implemented), so the floor is
/// deliberately stricter than what we send today: a peer that cannot take an
/// authenticated command should be rejected now rather than at the point
/// authentication lands and motion silently stops.
pub const MIN_ADVERTISED_FRAME: u16 = 44;

/// A peer that answered acceptably.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    pub implementation_id: u32,
    pub firmware_semver: u32,
    pub maximum_frame: u16,
    /// The major version both ends agreed on.
    pub agreed_major: u8,
}

/// Decide whether `response` is an acceptable answer to `probe`.
///
/// Pure, so every refusal below is testable without a link. `required_frame` is
/// the largest payload this bridge intends to send.
///
/// # Errors
/// See [`HandshakeError`]. Every arm is a refusal to arm.
pub fn evaluate_hello_response(
    probe: &Hello,
    response: &Frame,
    required_frame: u16,
) -> Result<Peer, HandshakeError> {
    if response.message_type != MessageType::Hello {
        return Err(HandshakeError::NotAHello);
    }
    if response.flags & crate::FLAG_RESPONSE == 0 {
        return Err(HandshakeError::NotAResponse);
    }
    let peer = Hello::parse(&response.payload)?;
    // Checked BEFORE the version fields: an answer we cannot tie to this probe
    // tells us nothing about the peer, so its claims are not evidence yet.
    if peer.nonce != probe.nonce {
        return Err(HandshakeError::NonceMismatch);
    }
    if peer.minimum_major > peer.maximum_major {
        return Err(HandshakeError::MalformedHello);
    }
    // Range overlap, not equality: a peer supporting 1..2 can talk to a host
    // supporting 1..1.
    let agreed = probe.maximum_major.min(peer.maximum_major);
    if agreed < probe.minimum_major.max(peer.minimum_major) {
        return Err(HandshakeError::VersionMismatch {
            peer_min: peer.minimum_major,
            peer_max: peer.maximum_major,
        });
    }
    // Three separate frame checks, because they mean three different things
    // and collapsing them would hide which one an operator has to fix.
    //
    //   below the protocol floor → the peer is not a conforming v1 peer
    //   above the protocol ceiling → the peer disagrees about the format
    //   below what WE need → the peer is conforming but we cannot use it here
    if peer.maximum_frame < MIN_ADVERTISED_FRAME {
        return Err(HandshakeError::FrameBelowProtocolMinimum {
            peer_maximum: peer.maximum_frame,
            minimum: MIN_ADVERTISED_FRAME,
        });
    }
    if peer.maximum_frame > MAX_PAYLOAD as u16 {
        return Err(HandshakeError::FrameAboveProtocolMaximum {
            peer_maximum: peer.maximum_frame,
            maximum: MAX_PAYLOAD as u16,
        });
    }
    if peer.maximum_frame < required_frame {
        return Err(HandshakeError::FrameTooSmall {
            peer_maximum: peer.maximum_frame,
            required: required_frame,
        });
    }
    Ok(Peer {
        implementation_id: peer.implementation_id,
        firmware_semver: peer.firmware_semver,
        maximum_frame: peer.maximum_frame,
        agreed_major: agreed,
    })
}
