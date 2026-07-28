//! Bind the simulated MCU to a real PTY, so the bridge opens a **device path**.
//!
//! [`crate::sim`] is pure and testable in memory, which is exactly why it is not
//! enough on its own: an in-memory test lets the bridge hand the MCU a `Vec` of
//! whole frames, and the two failure modes that actually bite on a serial link
//! never occur.
//!
//! * **A read boundary is not a frame boundary.** A UART read returns whatever
//!   arrived, routinely splitting one frame across two reads and packing three
//!   into one. Code that works on `Vec<Frame>` and breaks on `/dev/ttyTHS1` is
//!   almost always code that assumed otherwise.
//! * **A tty is not a pipe.** The line discipline sits between the two ends and,
//!   left at its defaults, will eat and rewrite binary: `ICRNL`/`ONLCR` rewrite
//!   0x0D/0x0A, `IXON` swallows 0x11 and 0x13 as flow control, `ISIG` turns 0x03
//!   into SIGINT, and canonical mode buffers by line — for a frame format whose
//!   bytes are uniformly distributed, that is silent corruption on roughly every
//!   frame. [`SimulatedMcuPty::open`] puts BOTH ends in raw mode, and that is
//!   load-bearing, not hygiene.
//!
//! The bridge under test therefore does the same thing it will do on the robot:
//! open a path, read bytes, frame them itself.
//!
//! ## What a PTY still cannot tell you
//!
//! Baud rate, framing/parity errors, line noise, cable pull, brown-out, and the
//! MCU's real timing. A PTY is a kernel buffer with a tty interface on it: it
//! never drops a byte and never delays one. Passing here means the bridge's
//! FRAMING and PROTOCOL handling are right. HIL is what tests the link.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

use nix::poll::{PollFd, PollFlags, PollTimeout};
use nix::pty::openpty;
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg};

use crate::sim::{Outcome, SimulatedMcu};
use crate::{decode, encode, DecodeError, FrameReader};

/// One handled frame: what arrived, and what the MCU did about it.
#[derive(Debug)]
pub struct Handled {
    /// `Err` means the bytes never became a frame — a decoder refusal, which is
    /// reported rather than swallowed so a bridge test can assert on it.
    pub outcome: Result<Outcome, DecodeError>,
    /// Whether a COMMAND_ACK was written back. A frame that did not decode gets
    /// no ACK: the MCU cannot echo a sequence it never read.
    pub acknowledged: bool,
}

/// A simulated MCU sitting on the far end of a pseudo-terminal.
#[derive(Debug)]
pub struct SimulatedMcuPty {
    /// The MCU's end. The bridge never sees this.
    master: File,
    /// Held open for the object's lifetime. Two reasons, both load-bearing:
    /// the path stays valid, and reads on the master do not start returning
    /// `EIO` the moment the bridge closes its end — a real MCU does not vanish
    /// because the host restarted.
    _slave: OwnedFd,
    device: PathBuf,
    reader: FrameReader,
}

impl SimulatedMcuPty {
    /// Allocate a PTY pair and put both ends in raw mode.
    ///
    /// # Errors
    /// If the PTY cannot be allocated, its name cannot be resolved, or raw mode
    /// cannot be set. Raw-mode failure is fatal ON PURPOSE: continuing would
    /// hand the bridge a link that corrupts binary frames, and a corrupted-frame
    /// test result is worse than no test.
    pub fn open() -> io::Result<Self> {
        let pair = openpty(None, None).map_err(io::Error::from)?;

        for fd in [pair.master.as_fd(), pair.slave.as_fd()] {
            let mut attrs = tcgetattr(fd).map_err(io::Error::from)?;
            cfmakeraw(&mut attrs);
            tcsetattr(fd, SetArg::TCSANOW, &attrs).map_err(io::Error::from)?;
        }

        let device = nix::unistd::ttyname(pair.slave.as_fd()).map_err(io::Error::from)?;

        Ok(Self {
            master: File::from(pair.master),
            _slave: pair.slave,
            device,
            reader: FrameReader::new(),
        })
    }

    /// The path the bridge opens, e.g. `/dev/pts/7`.
    #[must_use]
    pub fn device_path(&self) -> &Path {
        &self.device
    }

    /// Runs of bytes discarded for exceeding the maximum frame size. Non-zero
    /// means the link is desynchronised, not merely noisy — see
    /// [`FrameReader`].
    #[must_use]
    pub fn discarded_runs(&self) -> u64 {
        self.reader.discarded_runs
    }

    /// Wait up to `timeout_ms` for bytes, then handle every frame they complete.
    ///
    /// Returns an empty vec on timeout, or when the bytes that arrived did not
    /// complete a frame — a partial frame is HELD, never acted on.
    ///
    /// # Errors
    /// Propagates read/write failures on the PTY.
    pub fn pump(
        &mut self,
        mcu: &mut SimulatedMcu,
        now_us: u64,
        timeout_ms: u16,
    ) -> io::Result<Vec<Handled>> {
        let mut poll_fds = [PollFd::new(self.master.as_fd(), PollFlags::POLLIN)];
        let ready = nix::poll::poll(&mut poll_fds, PollTimeout::from(timeout_ms))
            .map_err(io::Error::from)?;
        if ready == 0 {
            return Ok(Vec::new());
        }

        let mut chunk = [0u8; 512];
        let n = match self.master.read(&mut chunk) {
            Ok(n) => n,
            // A tty master reports EIO when the last slave holder closes. We
            // keep one open, so this is "no data" rather than a fault; treat it
            // as such instead of failing a long-running loop.
            Err(e) if e.raw_os_error() == Some(nix::libc::EIO) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        if n == 0 {
            return Ok(Vec::new());
        }

        let mut handled = Vec::new();
        for candidate in self.reader.push(&chunk[..n]) {
            match decode(&candidate) {
                Ok(frame) => {
                    let outcome = mcu.handle(&frame, now_us);
                    let ack = mcu.ack_for(&frame, outcome, now_us);
                    // encode() only fails on an over-long payload or an illegal
                    // flag, neither of which ack_for can produce; an error here
                    // would be a bug in this crate, not a link fault.
                    let bytes = encode(&ack).map_err(|e| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("unencodable ACK: {e:?}"),
                        )
                    })?;
                    self.master.write_all(&bytes)?;
                    self.master.flush()?;
                    handled.push(Handled {
                        outcome: Ok(outcome),
                        acknowledged: true,
                    });
                }
                Err(e) => handled.push(Handled {
                    outcome: Err(e),
                    acknowledged: false,
                }),
            }
        }
        Ok(handled)
    }

    /// Advance the MCU's clock, writing an unsolicited ACK if the watchdog
    /// fired. A controlled stop the host did not ask for is exactly the event a
    /// bridge must not learn about by inference.
    ///
    /// # Errors
    /// Propagates write failures on the PTY.
    pub fn tick(&mut self, mcu: &mut SimulatedMcu, now_us: u64) -> io::Result<bool> {
        let Some(_state) = mcu.tick(now_us) else {
            return Ok(false);
        };
        // Sequence 0 in the correlation field: this ACK answers no frame.
        let trigger = crate::Frame::new(crate::MessageType::CommandAcknowledgement, 0, now_us);
        let ack = mcu.ack_for(&trigger, Outcome::StateChanged(mcu.state), now_us);
        let bytes = encode(&ack)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;
        self.master.write_all(&bytes)?;
        self.master.flush()?;
        Ok(true)
    }
}
