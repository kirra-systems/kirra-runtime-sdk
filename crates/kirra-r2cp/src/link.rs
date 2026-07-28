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

/// Claim `TIOCEXCL` — kernel-enforced exclusive access for this descriptor's
/// lifetime. While it is held, every further non-root `open(2)` of the port
/// fails `EBUSY`.
///
/// This is ADR-0033's Tier-3 sole-writer guarantee for the R2CP path. The boot
/// sentinel (owner, mode 0600, no other holder) proves nobody held the port
/// *before* we started; this is what stops anyone taking it *after*.
///
/// The one `unsafe` in this crate. There is no safe wrapper for `TIOCEXCL` in
/// `nix`, and the call is a plain `ioctl` with no arguments on a descriptor we
/// own — the borrow of `port` is what keeps it valid across the call.
#[allow(unsafe_code)]
fn claim_exclusive(port: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: `port` is an open descriptor owned by the caller and borrowed
    // for the duration of this call; TIOCEXCL takes no argument and writes
    // nothing back through the pointer-free varargs slot.
    let rc = unsafe { nix::libc::ioctl(port.as_raw_fd(), nix::libc::TIOCEXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Read back whether the tty is in exclusive mode (`TIOCGEXCL`, Linux 3.8+).
///
/// The claim is only worth logging if it can be CONFIRMED, and `TIOCEXCL`
/// returning 0 is not confirmation of much — so the startup path reports
/// exclusivity from this, not from the fact that the claim call did not error.
///
/// It is also the only way to observe the claim from a privileged process:
/// `tty_ioctl(4)` exempts `CAP_SYS_ADMIN` from the `EBUSY` that `TIOCEXCL`
/// imposes on everyone else, so a root test can verify the STATE even though
/// it cannot be blocked by it.
#[allow(unsafe_code)]
fn read_exclusive(port: &File) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut set: i32 = 0;
    // SAFETY: `port` is an open descriptor borrowed for this call, and
    // TIOCGEXCL writes exactly one `int` through the pointer we supply.
    let rc = unsafe {
        nix::libc::ioctl(
            port.as_raw_fd(),
            nix::libc::TIOCGEXCL,
            std::ptr::addr_of_mut!(set),
        )
    };
    if rc == 0 {
        Ok(set != 0)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Release exclusive mode (`TIOCNXCL`).
///
/// Closing the descriptor is NOT reliably enough. The kernel clears the tty's
/// exclusive flag when the tty itself is released — which happens on the last
/// close of a real UART, but NOT while any other descriptor keeps the tty
/// alive. I confirmed that here: dropping the link's fd left the flag set
/// because the simulator's pty master was still open.
///
/// On the robot the consumer is normally the sole opener, so the flag would
/// usually clear anyway. "Usually" is the problem: if anything else is holding
/// the tty when the consumer exits, the port stays exclusive, and the
/// consumer's own unprivileged restart is then refused `EBUSY` — permanently,
/// with no configuration wrong. Releasing explicitly costs one ioctl.
#[allow(unsafe_code)]
fn release_exclusive(port: &File) {
    use std::os::fd::AsRawFd;
    // SAFETY: `port` is an open descriptor borrowed for this call; TIOCNXCL
    // takes no argument. The result is deliberately ignored — this runs in
    // `Drop`, where there is nobody left to report to.
    let _ = unsafe { nix::libc::ioctl(port.as_raw_fd(), nix::libc::TIOCNXCL) };
}

impl Drop for SerialLink {
    fn drop(&mut self) {
        release_exclusive(&self.port);
    }
}

/// Consecutive undecodable runs that end the session's trust in its peer.
///
/// Not zero: a single corrupt frame is ordinary on a real UART (that is what
/// the CRC is for) and re-handshaking on every one would make the link
/// unusable. Not large either: sustained framing failure means the two ends
/// have lost agreement about where frames begin, and everything decoded after
/// that point is guesswork. The streak RESETS on any good frame, so this
/// counts *sustained* failure rather than a total.
pub const FRAMING_FAILURE_THRESHOLD: usize = 8;

/// Why the link stopped trusting its peer. Kept so the caller can distinguish
/// "never handshaken" from "was fine, then something broke" — the second is a
/// hardware-availability event, the first is usually configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkFault {
    /// Sustained framing failure: the ends disagree about frame boundaries.
    FramingLost { consecutive: usize },
    /// The device went away — EOF or EIO on read. A cable pull, a USB
    /// re-enumeration, an MCU reset.
    Disconnected,
    /// A caller explicitly reset the link (e.g. after a watchdog stop).
    Reset,
}

#[derive(Debug)]
pub struct SerialLink {
    port: File,
    reader: FrameReader,
    /// The bridge's own sequence space, strictly advancing. The peer refuses
    /// `sequence <= last_accepted`, so this must never repeat or go backwards
    /// within a session.
    next_sequence: u32,
    /// The identified peer. Lives HERE, on the instance — not in a global and
    /// not as a boolean the caller maintains. A new `SerialLink` (a reopen)
    /// therefore starts unidentified by construction rather than by the
    /// caller remembering to reset something.
    peer: Option<Peer>,
    consecutive_undecodable: usize,
    fault: Option<LinkFault>,
    /// Kernel-CONFIRMED exclusive mode, read back after the claim. A caller
    /// may only tell an operator the port is exclusive when this is true.
    exclusive: bool,
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

        // Construct FIRST, then claim. This ordering is deliberate and is the
        // difference between "nothing fallible happens to follow the claim
        // today" and "a leaked claim is impossible".
        //
        // Once `Self` exists, `Drop` covers it: any `?` below unwinds through
        // `release_exclusive` and then closes the descriptor. Claiming before
        // construction would mean that a fallible step added later — a second
        // ioctl, a probe, a timeout — silently leaks exclusive mode on the
        // error path, and on a tty another process is keeping alive that leak
        // is permanent (see `release_exclusive`).
        let mut link = Self {
            port,
            reader: FrameReader::new(),
            next_sequence: 1,
            peer: None,
            consecutive_undecodable: 0,
            fault: None,
            exclusive: false,
        };

        // Exclusivity is claimed HERE — after the descriptor exists and the
        // line is configured, but BEFORE a single handshake byte goes out.
        // A second writer appearing between the open and the claim would be
        // invisible, and the whole reason this link exists is that exactly one
        // process may reach the motors.
        claim_exclusive(&link.port)?;

        // Confirmed, not assumed: a caller may only report exclusivity if the
        // kernel says it holds. An error reading it back is "cannot confirm",
        // not a failed open — the claim already succeeded, and refusing to
        // start because the read-back was unavailable would be stricter than
        // the guarantee requires.
        link.exclusive = read_exclusive(&link.port).unwrap_or(false);
        Ok(link)
    }

    /// The peer established by a successful [`Self::handshake`]. `None` until
    /// then, and `None` again after any [`LinkFault`] — a caller must treat
    /// `None` as "do not send commands", which is enforced by
    /// [`Self::arm_and_activate`] and [`Self::send_motion`] rather than left
    /// to the caller's discipline.
    #[must_use]
    pub fn peer(&self) -> Option<Peer> {
        self.peer
    }

    /// Whether the kernel CONFIRMS this descriptor holds the tty exclusively.
    ///
    /// Not "we called TIOCEXCL and it returned 0" — this is the state read
    /// back with `TIOCGEXCL`. Report exclusivity to an operator only when it
    /// is true; claiming it otherwise is exactly the kind of unearned
    /// assurance ADR-0033 Tier-3 exists to remove.
    #[must_use]
    pub fn is_exclusive(&self) -> bool {
        self.exclusive
    }

    /// Why the peer was dropped, if it was. `None` means either "still
    /// identified" or "never was" — [`Self::peer`] distinguishes those.
    #[must_use]
    pub fn fault(&self) -> Option<LinkFault> {
        self.fault
    }

    /// Drop the identified peer, requiring a fresh handshake before any
    /// further command.
    ///
    /// Call this after a watchdog-triggered stop or any other event that makes
    /// the peer's state uncertain. Re-identifying is cheap; commanding a peer
    /// whose state we are guessing at is not.
    pub fn reset(&mut self) {
        self.invalidate(LinkFault::Reset);
    }

    fn invalidate(&mut self, fault: LinkFault) {
        self.peer = None;
        self.fault = Some(fault);
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
                // EOF and EIO both mean the device went away. On a real link
                // that is a cable pull, a USB re-enumeration or an MCU reset —
                // in every case the peer's state is now unknown, so the
                // identification is dropped and a fresh handshake is required.
                Ok(0) => {
                    self.invalidate(LinkFault::Disconnected);
                    break;
                }
                Ok(n) => n,
                Err(e) if e.raw_os_error() == Some(nix::libc::EIO) => {
                    self.invalidate(LinkFault::Disconnected);
                    break;
                }
                Err(e) => return Err(e),
            };
            for candidate in self.reader.push(&chunk[..n]) {
                match decode(&candidate) {
                    Ok(f) => {
                        // A good frame proves the ends still agree on where
                        // frames begin, so the streak is sustained failure,
                        // not a lifetime total.
                        self.consecutive_undecodable = 0;
                        out.frames.push(f);
                    }
                    Err(_) => {
                        out.undecodable += 1;
                        self.consecutive_undecodable += 1;
                        if self.consecutive_undecodable >= FRAMING_FAILURE_THRESHOLD {
                            self.invalidate(LinkFault::FramingLost {
                                consecutive: self.consecutive_undecodable,
                            });
                        }
                    }
                }
            }
            if !out.frames.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    /// [`Self::handshake`] with a nonce drawn from the OS entropy pool.
    ///
    /// This is the entry point every non-test caller should use. Taking the
    /// nonce as a parameter is right for tests, which need a known value, but
    /// wrong as the only option: it pushes "must be unpredictable" out to every
    /// caller, and a caller that gets it wrong produces a gate a recorded reply
    /// can satisfy — with no visible symptom. One audited entropy source is the
    /// safer default.
    ///
    /// # Errors
    /// [`LinkError::Io`] if `/dev/urandom` cannot be read. FAIL-CLOSED on
    /// purpose: there is no weaker fallback nonce, because a predictable one
    /// silently converts the gate into theatre.
    pub fn handshake_fresh(&mut self, timeout_ms: u16) -> Result<Peer, LinkError> {
        let mut nonce = [0u8; 16];
        let mut urandom = File::open("/dev/urandom")?;
        urandom.read_exact(&mut nonce)?;
        self.handshake(nonce, timeout_ms)
    }

    /// Probe the peer and refuse unless it answers acceptably.
    ///
    /// `nonce` must be unpredictable — it is the only thing tying the reply to
    /// this probe, so a constant would let a recorded reply satisfy the gate.
    /// Prefer [`Self::handshake_fresh`] outside tests.
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
    /// 🔴 **BEST-EFFORT WHEN THE PEER IS UNIDENTIFIED, AND THE RETURN VALUE IS
    /// NOT A STOP.** Unlike the others this does not require a handshake — a
    /// shutdown path must not be blocked by the gate that protects startup —
    /// but that means it may be writing to a device that is not an R2CP peer
    /// at all. `Ok` here means *the bytes were written to the file
    /// descriptor*. It is NOT evidence that:
    ///
    /// * a vendor board understood the frame (it did not — it does not speak
    ///   R2CP, which is the entire reason [`Self::handshake`] exists);
    /// * an R2CP peer received it (nothing is read back);
    /// * the motors stopped.
    ///
    /// The thing that actually stops a governed robot is the peer's own
    /// watchdog: commands cease, the command window lapses, the MCU enters
    /// ControlledStop on its own. This call is an attempt to make that
    /// immediate, not a substitute for it. A caller must not log it as
    /// "stopped", only as "stop sent".
    ///
    /// # Errors
    /// Write failures only.
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
