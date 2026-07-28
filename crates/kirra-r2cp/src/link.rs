//! `SerialLink` — the BRIDGE's side of the wire.
//!
//! [`crate::pty`] is the simulated MCU's end; this is the end the consumer
//! holds. It opens a device path (a `/dev/pts/N` from the simulator, or a real
//! `/dev/ttyTHS1`/`/dev/myserial`), puts it in raw mode, and speaks frames.
//!
//! It is deliberately the SAME code in both cases. A test that drives the
//! simulator through a bespoke helper proves things about the helper; pointing
//! the production link at the simulator is what makes the simulator a useful
//! integration target.
//!
//! ## Raw mode is not optional here either
//!
//! Same reasoning as `pty.rs`: at tty defaults the line discipline rewrites
//! binary (`ONLCR`/`ICRNL`, `IXON` flow control, `ISIG`), which for a frame
//! format with uniformly distributed bytes is corruption on roughly every
//! frame. On a real UART this also sets the baud rate; on a pty the kernel
//! ignores speed, which is one of the several reasons a pty is not a link test.
//!
//! ## The handshake gate
//!
//! [`SerialLink::handshake`] must succeed before anything sends a command. See
//! [`crate::handshake`] for why: on the bring-up robot the thing at the other
//! end of `/dev/myserial` is a *vendor* MCU that does not speak R2CP, and
//! writing R2CP frames into a vendor command parser is undefined behaviour on
//! a board wired to motors.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, PollTimeout};
use nix::sys::termios::{cfmakeraw, cfsetspeed, tcgetattr, tcsetattr, BaudRate, SetArg};

use crate::handshake::{evaluate_hello_response, HandshakeError, Hello, Peer};
use crate::sim::{MotionCommand, MODE_STOP, MODE_TRACK};
use crate::{decode, encode, Frame, FrameReader, MessageType, MAX_PAYLOAD};

/// Frames read from the peer, and the runs the framer had to discard.
#[derive(Debug, Default)]
pub struct Received {
    pub frames: Vec<Frame>,
    /// Bytes that arrived but could not be decoded. Surfaced, not swallowed:
    /// on a real link this is the signal that something is wrong with the
    /// cable, the baud rate, or the peer.
    pub undecodable: usize,
}

#[derive(Debug)]
pub struct SerialLink {
    port: File,
    reader: FrameReader,
    /// The bridge's own sequence space, strictly advancing. The peer refuses
    /// `sequence <= last_accepted`, so this must never repeat or go backwards
    /// within a session.
    next_sequence: u32,
    peer: Option<Peer>,
}

impl SerialLink {
    /// Open `device` and put it in raw mode. `baud` is applied when `Some`;
    /// a pty ignores it, a real UART does not.
    ///
    /// # Errors
    /// If the device cannot be opened or raw mode cannot be set. Raw-mode
    /// failure is fatal on purpose — a link that corrupts frames must not be
    /// handed to a caller that is about to command motion on it.
    pub fn open(device: &Path, baud: Option<BaudRate>) -> io::Result<Self> {
        let port = OpenOptions::new().read(true).write(true).open(device)?;
        let fd = port.as_fd();
        let mut attrs = tcgetattr(fd).map_err(io::Error::from)?;
        cfmakeraw(&mut attrs);
        if let Some(b) = baud {
            cfsetspeed(&mut attrs, b).map_err(io::Error::from)?;
        }
        tcsetattr(fd, SetArg::TCSANOW, &attrs).map_err(io::Error::from)?;
        Ok(Self {
            port,
            reader: FrameReader::new(),
            next_sequence: 1,
            peer: None,
        })
    }

    /// The peer established by a successful [`Self::handshake`]. `None` until
    /// then — and a caller must treat `None` as "do not send commands".
    #[must_use]
    pub fn peer(&self) -> Option<Peer> {
        self.peer
    }

    fn take_sequence(&mut self) -> u32 {
        let s = self.next_sequence;
        // Saturating, not wrapping: wrapping would hand the peer a sequence it
        // has already accepted, which its replay rule correctly refuses — the
        // link would go dead at 2^32 with no explanation. Saturating makes it
        // stop advancing, which the peer also refuses, but the cause is then
        // visible in this counter rather than hidden in a wrap.
        self.next_sequence = self.next_sequence.saturating_add(1);
        s
    }

    /// # Errors
    /// Propagates write failures.
    pub fn send(&mut self, mut frame: Frame) -> io::Result<u32> {
        let seq = self.take_sequence();
        frame.sequence = seq;
        let bytes = encode(&frame)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;
        self.port.write_all(&bytes)?;
        self.port.flush()?;
        Ok(seq)
    }

    /// Read for up to `timeout_ms`, returning whatever frames completed.
    ///
    /// # Errors
    /// Propagates read failures.
    pub fn receive(&mut self, timeout_ms: u16) -> io::Result<Received> {
        let mut out = Received::default();
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        while Instant::now() < deadline {
            let left = deadline
                .saturating_duration_since(Instant::now())
                .as_millis();
            let mut fds = [PollFd::new(self.port.as_fd(), PollFlags::POLLIN)];
            let ms = u16::try_from(left).unwrap_or(u16::MAX);
            if nix::poll::poll(&mut fds, PollTimeout::from(ms)).map_err(io::Error::from)? == 0 {
                break;
            }
            let mut chunk = [0u8; 512];
            let n = match self.port.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.raw_os_error() == Some(nix::libc::EIO) => break,
                Err(e) => return Err(e),
            };
            for candidate in self.reader.push(&chunk[..n]) {
                match decode(&candidate) {
                    Ok(f) => out.frames.push(f),
                    Err(_) => out.undecodable += 1,
                }
            }
            if !out.frames.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    /// Probe the peer and refuse unless it answers acceptably.
    ///
    /// `nonce` must be unpredictable — it is the only thing tying the reply to
    /// this probe, so a constant would let a recorded reply satisfy the gate.
    ///
    /// # Errors
    /// [`LinkError::Handshake`] for every refusal; see [`HandshakeError`]. The
    /// case that matters most on the bring-up robot is `NoResponse`: a vendor
    /// board is silent, and silence must never be read as agreement.
    pub fn handshake(&mut self, nonce: [u8; 16], timeout_ms: u16) -> Result<Peer, LinkError> {
        let probe = Hello::probe(nonce);
        self.send(probe.to_frame(0, 0, false))?;

        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        let mut last = HandshakeError::NoResponse;
        while Instant::now() < deadline {
            let left = deadline
                .saturating_duration_since(Instant::now())
                .as_millis();
            let got = self.receive(u16::try_from(left).unwrap_or(u16::MAX))?;
            for frame in &got.frames {
                match evaluate_hello_response(&probe, frame, MAX_PAYLOAD as u16) {
                    Ok(peer) => {
                        self.peer = Some(peer);
                        return Ok(peer);
                    }
                    // Keep looking within the window rather than failing on the
                    // first non-answer: a peer may still be emitting telemetry
                    // from before the probe, and that traffic is not a refusal.
                    // The LAST refusal is reported, so the operator sees why
                    // the frames that did arrive were not acceptable.
                    Err(e) => last = e,
                }
            }
        }
        Err(LinkError::Handshake(last))
    }

    /// Send ARM then ACTIVATE, requiring each to be acknowledged.
    ///
    /// # Errors
    /// [`LinkError::NotHandshaken`] if called before a successful handshake —
    /// the ordering is enforced here rather than left to the caller, because
    /// "arm a peer we have not identified" is exactly the mistake this whole
    /// module exists to prevent.
    pub fn arm_and_activate(&mut self, timeout_ms: u16) -> Result<(), LinkError> {
        if self.peer.is_none() {
            return Err(LinkError::NotHandshaken);
        }
        for ty in [MessageType::Arm, MessageType::Activate] {
            let seq = self.send(Frame::new(ty, 0, 0))?;
            let got = self.receive(timeout_ms)?;
            if !got.frames.iter().any(|f| {
                f.message_type == MessageType::CommandAcknowledgement
                    && ack_received_sequence(f) == Some(seq)
                    && ack_result(f) == Some(crate::sim::ack_result::ACCEPTED)
            }) {
                return Err(LinkError::NotAcknowledged(ty));
            }
        }
        Ok(())
    }

    /// Send one MOTION_COMMAND. `velocity_mps`/`curvature_per_m` are the
    /// values the governor already released — this transports them, it does
    /// not decide them.
    ///
    /// # Errors
    /// [`LinkError::NotHandshaken`] before a handshake; write failures.
    pub fn send_motion(
        &mut self,
        cmd: &MotionCommand,
        source_time_us: u64,
    ) -> Result<u32, LinkError> {
        if self.peer.is_none() {
            return Err(LinkError::NotHandshaken);
        }
        let frame =
            Frame::new(MessageType::MotionCommand, 0, source_time_us).with_payload(&cmd.encode());
        Ok(self.send(frame)?)
    }

    /// The stop primitive: a MODE_STOP command, which the peer honours as zero
    /// motion whatever the numeric fields say.
    ///
    /// Deliberately still a MOTION_COMMAND rather than a DISARM: it must be
    /// safe to call repeatedly and from a shutdown path, and it keeps the peer
    /// in Active where the next command can be honoured. DISARM is the
    /// heavier, one-way request.
    ///
    /// # Errors
    /// Write failures. Unlike the others this does NOT require a handshake:
    /// a stop must be attemptable on a link whose state is uncertain.
    pub fn send_stop(&mut self, valid_for_us: u32, source_time_us: u64) -> io::Result<u32> {
        let cmd = MotionCommand {
            command_id: 0,
            valid_for_us,
            velocity_mps: 0.0,
            curvature_per_m: 0.0,
            acceleration_limit_mps2: 0.0,
            jerk_limit_mps3: 0.0,
            mode: MODE_STOP,
        };
        let frame =
            Frame::new(MessageType::MotionCommand, 0, source_time_us).with_payload(&cmd.encode());
        self.send(frame)
    }
}

/// `received_sequence` from a COMMAND_ACK payload, or `None` if the frame is
/// not a well-formed ACK.
#[must_use]
pub fn ack_received_sequence(frame: &Frame) -> Option<u32> {
    if frame.message_type != MessageType::CommandAcknowledgement || frame.payload.len() < 28 {
        return None;
    }
    Some(u32::from_le_bytes(frame.payload[4..8].try_into().ok()?))
}

/// `result` from a COMMAND_ACK payload. See `sim::ack_result` — these values
/// are PROVISIONAL until the firmware binds them.
#[must_use]
pub fn ack_result(frame: &Frame) -> Option<u16> {
    if frame.message_type != MessageType::CommandAcknowledgement || frame.payload.len() < 28 {
        return None;
    }
    Some(u16::from_le_bytes(frame.payload[16..18].try_into().ok()?))
}

/// `safety_state` from a COMMAND_ACK payload. Mirrors the firmware's
/// `safety_manager.hpp` discriminants — NOT provisional.
#[must_use]
pub fn ack_safety_state(frame: &Frame) -> Option<u8> {
    if frame.message_type != MessageType::CommandAcknowledgement || frame.payload.len() < 28 {
        return None;
    }
    Some(frame.payload[18])
}

#[derive(Debug)]
pub enum LinkError {
    Io(io::Error),
    Handshake(HandshakeError),
    /// A command was attempted before the peer was identified.
    NotHandshaken,
    NotAcknowledged(MessageType),
}

impl From<io::Error> for LinkError {
    fn from(e: io::Error) -> Self {
        LinkError::Io(e)
    }
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Io(e) => write!(f, "link I/O error: {e}"),
            LinkError::Handshake(HandshakeError::NoResponse) => write!(
                f,
                "no R2CP peer answered. On this robot the most likely cause is that \
                 the device is a VENDOR motor board, which does not speak R2CP — \
                 refusing to send it frames"
            ),
            LinkError::Handshake(e) => write!(f, "R2CP handshake refused: {e:?}"),
            LinkError::NotHandshaken => write!(
                f,
                "command attempted before a successful handshake (fail-closed)"
            ),
            LinkError::NotAcknowledged(ty) => write!(f, "{ty:?} was not acknowledged"),
        }
    }
}

impl std::error::Error for LinkError {}

/// Re-exported so callers do not need `MODE_TRACK` from two places.
pub use crate::sim::MODE_TRACK as MOTION_MODE_TRACK;
const _: () = assert!(MODE_TRACK == MOTION_MODE_TRACK);
