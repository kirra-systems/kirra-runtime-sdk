//! Handshake refusals driven through the FULL exported `kirra_r2cp_open`.
//!
//! `r2cp_against_simulator.rs` proves the accept path and the two silence
//! cases. These need a peer that *answers wrongly*, which the simulator will
//! never do — so the test scripts the replies itself.
//!
//! Everything goes through the C entry point rather than the evaluator
//! underneath: the pure refusals are already covered in `kirra-r2cp`, and what
//! is unproven here is that each one survives the FFI's error classification
//! and still leaves the caller with no handle.

use std::ffi::CString;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use kirra_consumer_ffi::r2cp::*;
use kirra_r2cp::handshake::{Hello, SIM_IMPLEMENTATION_ID};
use kirra_r2cp::{decode, encode, FrameReader, MessageType, MAX_PAYLOAD};

/// How the scripted peer answers a probe.
#[derive(Clone, Copy, Debug)]
enum Reply {
    /// Echo a DIFFERENT nonce — a stale or replayed recording.
    WrongNonce,
    /// Advertise a major range with no overlap.
    NoVersionOverlap,
    /// Advertise below the protocol's own floor.
    FrameBelowFloor,
    /// Advertise above what the agreed major permits.
    FrameAboveCeiling,
    /// A HELLO too short to state a version range.
    MalformedHello,
    /// Emit undecodable noise FIRST, then answer correctly. Ordinary UART
    /// corruption must not permanently poison an open attempt.
    GarbageThenValid,
}

struct ScriptedPeer {
    device: PathBuf,
    _thread: std::thread::JoinHandle<()>,
}

impl ScriptedPeer {
    fn spawn(reply: Reply) -> Self {
        let pair = nix::pty::openpty(None, None).expect("openpty");
        // Raw mode on the pair, or the line discipline mangles the binary the
        // same way it would on a real link.
        {
            use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg};
            let mut attrs = tcgetattr(pair.slave.as_fd()).expect("tcgetattr");
            cfmakeraw(&mut attrs);
            tcsetattr(pair.slave.as_fd(), SetArg::TCSANOW, &attrs).expect("tcsetattr");
        }
        let device = nix::unistd::ttyname(pair.slave.as_fd()).expect("ttyname");
        let slave = pair.slave;
        let mut master = std::fs::File::from(pair.master);

        let thread = std::thread::spawn(move || {
            // Hold the slave open for the peer's lifetime so the master does
            // not see EIO the moment the bridge closes.
            let _slave = slave;
            let mut reader = FrameReader::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut chunk = [0u8; 512];
            while std::time::Instant::now() < deadline {
                let Ok(n) = master.read(&mut chunk) else {
                    return;
                };
                if n == 0 {
                    return;
                }
                for candidate in reader.push(&chunk[..n]) {
                    let Ok(frame) = decode(&candidate) else {
                        continue;
                    };
                    if frame.message_type != MessageType::Hello {
                        continue;
                    }
                    let Ok(probe) = Hello::parse(&frame.payload) else {
                        continue;
                    };
                    if master.write_all(&script(reply, probe)).is_err() {
                        return;
                    }
                    let _ = master.flush();
                    // Do NOT return here. Dropping `master` closes the pty
                    // master, which hangs up the slave and can discard data
                    // the bridge has not read yet — the reply would vanish
                    // between the write and the read. A real MCU does not
                    // disappear the instant it answers, so the peer stays
                    // alive until the deadline.
                    while std::time::Instant::now() < deadline {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    return;
                }
            }
        });

        Self {
            device,
            _thread: thread,
        }
    }
}

/// Build the bytes this peer answers with.
fn script(reply: Reply, probe: Hello) -> Vec<u8> {
    let base = Hello {
        nonce: probe.nonce,
        implementation_id: SIM_IMPLEMENTATION_ID,
        firmware_semver: 1,
        minimum_major: 1,
        maximum_major: 1,
        maximum_frame: MAX_PAYLOAD as u16,
    };
    let mut out = Vec::new();
    let hello = match reply {
        Reply::WrongNonce => Hello {
            nonce: [0xEE; 16],
            ..base
        },
        Reply::NoVersionOverlap => Hello {
            minimum_major: 2,
            maximum_major: 3,
            ..base
        },
        Reply::FrameBelowFloor => Hello {
            maximum_frame: 16,
            ..base
        },
        Reply::FrameAboveCeiling => Hello {
            maximum_frame: 4096,
            ..base
        },
        Reply::MalformedHello => {
            // A HELLO frame whose payload is too short to carry a version
            // range. Built by truncating, so the FRAME is well-formed and only
            // the payload is wrong — the refusal must come from the handshake,
            // not the decoder.
            let mut frame = base.to_frame(1, 0, true);
            frame.payload.truncate(20);
            out.extend_from_slice(&encode(&frame).expect("encodes"));
            return out;
        }
        Reply::GarbageThenValid => {
            // Undecodable runs first — a burst of line noise — then the real
            // answer. The bridge must still find it.
            for i in 0..4u8 {
                out.extend_from_slice(&[0x30 + i, 0x31, 0x32, 0x33, 0x00]);
            }
            base
        }
    };
    out.extend_from_slice(&encode(&hello.to_frame(1, 0, true)).expect("encodes"));
    out
}

fn open_link(device: &Path, timeout_ms: u16) -> (i32, *mut KirraR2cpLink) {
    let c = CString::new(device.to_str().unwrap()).unwrap();
    let mut handle: *mut KirraR2cpLink = std::ptr::null_mut();
    let status = unsafe { kirra_r2cp_open(c.as_ptr(), timeout_ms, &mut handle) };
    (status, handle)
}

#[test]
fn every_bad_handshake_reply_refuses_through_the_real_open_call() {
    for (reply, expected) in [
        (Reply::WrongNonce, KIRRA_R2CP_BAD_HANDSHAKE_REPLY),
        (Reply::NoVersionOverlap, KIRRA_R2CP_VERSION_MISMATCH),
        (Reply::FrameBelowFloor, KIRRA_R2CP_FRAME_LIMIT),
        (Reply::FrameAboveCeiling, KIRRA_R2CP_FRAME_LIMIT),
        (Reply::MalformedHello, KIRRA_R2CP_BAD_HANDSHAKE_REPLY),
    ] {
        let peer = ScriptedPeer::spawn(reply);
        let (status, handle) = open_link(&peer.device, 500);
        assert_eq!(status, expected, "{reply:?} classified wrongly");
        assert!(handle.is_null(), "{reply:?} produced a handle");
        assert_eq!(
            kirra_r2cp_status_class(status),
            KIRRA_R2CP_CLASS_PROTOCOL,
            "{reply:?} is a protocol disagreement, not config or availability"
        );
    }
}

#[test]
fn a_valid_reply_after_line_noise_still_opens() {
    // The non-vacuity case for the five refusals above, and the one that
    // matters most operationally: ordinary UART corruption must not
    // permanently poison an open attempt. If this failed, every refusal above
    // could be passing because the scripted peer never works at all.
    let peer = ScriptedPeer::spawn(Reply::GarbageThenValid);
    let (status, handle) = open_link(&peer.device, 1000);
    assert_eq!(
        status, KIRRA_R2CP_OK,
        "noise before a valid HELLO must not prevent identification"
    );
    assert!(!handle.is_null());

    let mut info = KirraR2cpPeerInfo::default();
    assert_eq!(
        unsafe { kirra_r2cp_peer_info(handle, &mut info) },
        KIRRA_R2CP_OK
    );
    assert_eq!(info.identified, 1);
    assert_eq!(info.implementation_id, SIM_IMPLEMENTATION_ID);
    unsafe { kirra_r2cp_close(handle) };
}

#[test]
fn a_protocol_refusal_is_not_an_availability_refusal() {
    // The three classes must stay distinguishable at the FFI boundary, since
    // the consumer's exit code is derived from them: a peer that answered
    // wrongly is not the same event as a peer that is not there.
    let peer = ScriptedPeer::spawn(Reply::NoVersionOverlap);
    let (protocol_status, handle) = open_link(&peer.device, 500);
    assert!(handle.is_null());

    // A silent device, for contrast.
    let pair = nix::pty::openpty(None, None).expect("openpty");
    let quiet_device = nix::unistd::ttyname(pair.slave.as_fd()).expect("ttyname");
    let _keep = (pair.master, pair.slave);
    let (availability_status, handle) = open_link(&quiet_device, 150);
    assert!(handle.is_null());

    assert_ne!(protocol_status, availability_status);
    assert_ne!(
        kirra_r2cp_status_class(protocol_status),
        kirra_r2cp_status_class(availability_status),
        "answered-wrongly and not-there must not collapse into one class"
    );
}
