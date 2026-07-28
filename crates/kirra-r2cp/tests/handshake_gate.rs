//! The gate that decides whether it is safe to send this peer a command.
//!
//! The load-bearing case is the FIRST end-to-end test below: the bring-up
//! robot's `/dev/myserial` is a stock vendor motor board that does not speak
//! R2CP. Pointing the R2CP drive mode at it must refuse, not "produce no
//! motion" — writing R2CP frames into a vendor command parser attached to
//! motors is undefined behaviour, not a no-op.

#![cfg(feature = "pty")]

use std::io::Write;
use std::os::fd::AsFd;
use std::path::PathBuf;

use kirra_r2cp::handshake::{
    evaluate_hello_response, HandshakeError, Hello, HOST_IMPLEMENTATION_ID, SIM_IMPLEMENTATION_ID,
};
use kirra_r2cp::link::{LinkError, SerialLink};
use kirra_r2cp::pty::SimulatedMcuPty;
use kirra_r2cp::sim::{MotionCommand, Outcome, SimulatedMcu, MODE_TRACK};
use kirra_r2cp::{encode, Frame, MessageType, MAX_PAYLOAD};

const NONCE: [u8; 16] = [7u8; 16];

fn peer_hello(nonce: [u8; 16]) -> Hello {
    Hello {
        nonce,
        implementation_id: SIM_IMPLEMENTATION_ID,
        firmware_semver: 1,
        minimum_major: 1,
        maximum_major: 1,
        maximum_frame: MAX_PAYLOAD as u16,
    }
}

// ------------------------------------------------------------ pure evaluator

#[test]
fn a_matching_response_is_accepted_and_names_the_peer() {
    let probe = Hello::probe(NONCE);
    let peer = evaluate_hello_response(
        &probe,
        &peer_hello(NONCE).to_frame(1, 0, true),
        MAX_PAYLOAD as u16,
    )
    .expect("a well-formed matching response is accepted");
    assert_eq!(peer.implementation_id, SIM_IMPLEMENTATION_ID);
    assert_eq!(peer.agreed_major, 1);
    assert_ne!(
        peer.implementation_id, HOST_IMPLEMENTATION_ID,
        "the peer must be distinguishable from ourselves in a log"
    );
}

#[test]
fn a_reply_carrying_someone_elses_nonce_is_refused() {
    // This is the nonce doing its job: a recorded reply from an earlier session
    // must not satisfy a fresh probe.
    let probe = Hello::probe(NONCE);
    let stale = peer_hello([9u8; 16]).to_frame(1, 0, true);
    assert_eq!(
        evaluate_hello_response(&probe, &stale, MAX_PAYLOAD as u16),
        Err(HandshakeError::NonceMismatch)
    );
    // Positive control: identical in every other respect, with the right nonce.
    assert!(evaluate_hello_response(
        &probe,
        &peer_hello(NONCE).to_frame(1, 0, true),
        MAX_PAYLOAD as u16
    )
    .is_ok());
}

#[test]
fn only_a_hello_marked_as_a_response_answers_a_probe() {
    let probe = Hello::probe(NONCE);

    // A COMMAND_ACK is not an answer to a HELLO.
    let ack = Frame::new(MessageType::CommandAcknowledgement, 1, 0).with_payload(&[0u8; 28]);
    assert_eq!(
        evaluate_hello_response(&probe, &ack, MAX_PAYLOAD as u16),
        Err(HandshakeError::NotAHello)
    );

    // A peer's own unsolicited HELLO is not an answer either.
    let unsolicited = peer_hello(NONCE).to_frame(1, 0, false);
    assert_eq!(
        evaluate_hello_response(&probe, &unsolicited, MAX_PAYLOAD as u16),
        Err(HandshakeError::NotAResponse)
    );
}

#[test]
fn version_ranges_must_overlap_and_overlap_is_enough() {
    let probe = Hello::probe(NONCE); // host supports exactly major 1

    for (min, max) in [(2u8, 3u8), (0, 0)] {
        let mut h = peer_hello(NONCE);
        h.minimum_major = min;
        h.maximum_major = max;
        assert_eq!(
            evaluate_hello_response(&probe, &h.to_frame(1, 0, true), MAX_PAYLOAD as u16),
            Err(HandshakeError::VersionMismatch {
                peer_min: min,
                peer_max: max
            }),
            "peer range {min}..{max} does not overlap the host's"
        );
    }

    // Positive control: a peer supporting 1..2 CAN talk to a host on 1 — the
    // check is overlap, not equality, or every firmware bump would break it.
    let mut wide = peer_hello(NONCE);
    wide.minimum_major = 1;
    wide.maximum_major = 2;
    let peer = evaluate_hello_response(&probe, &wide.to_frame(1, 0, true), MAX_PAYLOAD as u16)
        .expect("overlapping ranges are acceptable");
    assert_eq!(peer.agreed_major, 1, "agree on the highest common major");

    // An inverted range is malformed, not merely non-overlapping.
    let mut inverted = peer_hello(NONCE);
    inverted.minimum_major = 3;
    inverted.maximum_major = 1;
    assert_eq!(
        evaluate_hello_response(&probe, &inverted.to_frame(1, 0, true), MAX_PAYLOAD as u16),
        Err(HandshakeError::MalformedHello)
    );
}

#[test]
fn a_peer_that_cannot_receive_our_largest_frame_is_refused_up_front() {
    // Better than discovering it when a long command is silently dropped.
    let probe = Hello::probe(NONCE);
    let mut small = peer_hello(NONCE);
    small.maximum_frame = 32;
    assert_eq!(
        evaluate_hello_response(&probe, &small.to_frame(1, 0, true), 64),
        Err(HandshakeError::FrameTooSmall {
            peer_maximum: 32,
            required: 64
        })
    );
    // Positive control: exactly enough is enough.
    assert!(evaluate_hello_response(&probe, &small.to_frame(1, 0, true), 32).is_ok());
}

#[test]
fn a_short_hello_payload_is_refused_not_zero_filled() {
    let probe = Hello::probe(NONCE);
    let mut truncated = peer_hello(NONCE).to_frame(1, 0, true);
    truncated.payload.truncate(20);
    assert_eq!(
        evaluate_hello_response(&probe, &truncated, MAX_PAYLOAD as u16),
        Err(HandshakeError::MalformedHello),
        "a peer that cannot state its version range has not answered"
    );
}

#[test]
fn hello_round_trips_through_its_own_encoding() {
    let h = peer_hello([0xABu8; 16]);
    assert_eq!(Hello::parse(&h.encode()), Ok(h));
}

// ------------------------------------------------------------- over the wire

/// A pty whose master nobody services — the shape of a device that is present
/// and openable but is not an R2CP peer.
struct SilentPeer {
    _master: std::fs::File,
    _slave: std::os::fd::OwnedFd,
    device: PathBuf,
}

impl SilentPeer {
    fn open() -> Self {
        let pair = nix::pty::openpty(None, None).expect("openpty");
        let device = nix::unistd::ttyname(pair.slave.as_fd()).expect("ttyname");
        Self {
            _master: std::fs::File::from(pair.master),
            _slave: pair.slave,
            device,
        }
    }
}

#[test]
fn a_device_that_is_not_an_r2cp_peer_refuses_the_handshake() {
    // 🔴 The case this whole module exists for. On the bring-up robot
    // /dev/myserial is a stock vendor motor board: present, openable, and
    // silent in R2CP terms. It must REFUSE, not fall through to sending
    // frames into a vendor command parser attached to motors.
    let quiet = SilentPeer::open();
    let mut link = SerialLink::open(&quiet.device, None).expect("the device opens");

    let err = link
        .handshake(NONCE, 150)
        .expect_err("a non-R2CP device must not satisfy the gate");
    assert!(matches!(
        err,
        LinkError::Handshake(HandshakeError::NoResponse)
    ));
    assert!(link.peer().is_none());

    // The operator-facing message must name the likely cause, because "no
    // response" alone sends someone looking for a cable fault.
    let msg = err.to_string();
    assert!(
        msg.contains("VENDOR"),
        "the refusal should name the likely cause, got: {msg}"
    );
}

#[test]
fn a_device_that_chatters_a_foreign_protocol_still_refuses() {
    // A vendor board is not necessarily silent — it may be emitting its own
    // telemetry. Bytes arriving must not be mistaken for agreement.
    let mut noisy = SilentPeer::open();
    let mut link = SerialLink::open(&noisy.device, None).expect("the device opens");

    // Yahboom-shaped framing: 0xFF 0xFE header, length, payload. Nothing here
    // is valid R2CP, and none of it is an answer to our probe.
    for _ in 0..8 {
        noisy
            ._master
            .write_all(&[0xFF, 0xFE, 0x04, 0x01, 0x02, 0x03, 0x00])
            .expect("write vendor chatter");
    }
    noisy._master.flush().expect("flush");

    let err = link
        .handshake(NONCE, 150)
        .expect_err("foreign traffic is not a handshake");
    assert!(matches!(err, LinkError::Handshake(_)));
    assert!(link.peer().is_none());
}

/// Run the simulated MCU on a background thread so the link talks to a live
/// peer, exactly as the consumer will.
fn spawn_sim() -> (PathBuf, std::sync::mpsc::Sender<()>) {
    let pty = SimulatedMcuPty::open().expect("allocate a pty");
    let device = pty.device_path().to_path_buf();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut pty = pty;
        let mut mcu = SimulatedMcu::new();
        let started = std::time::Instant::now();
        loop {
            if stop_rx.try_recv().is_ok() {
                return;
            }
            let now_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            if pty.pump(&mut mcu, now_us, 20).is_err() {
                return;
            }
        }
    });
    (device, stop_tx)
}

#[test]
fn a_live_simulator_satisfies_the_gate_and_identifies_itself() {
    // Positive control for the two refusals above: the identical code path,
    // against a peer that does speak R2CP, succeeds.
    let (device, stop) = spawn_sim();
    let mut link = SerialLink::open(&device, None).expect("open");

    let peer = link.handshake(NONCE, 1000).expect("the simulator answers");
    assert_eq!(peer.implementation_id, SIM_IMPLEMENTATION_ID);
    assert_eq!(peer.agreed_major, 1);
    assert!(link.peer().is_some());
    let _ = stop.send(());
}

#[test]
fn commands_are_refused_before_the_handshake_and_accepted_after() {
    let (device, stop) = spawn_sim();
    let mut link = SerialLink::open(&device, None).expect("open");

    let cmd = MotionCommand {
        command_id: 1,
        valid_for_us: 100_000,
        velocity_mps: 0.3,
        curvature_per_m: 0.0,
        acceleration_limit_mps2: 1.0,
        jerk_limit_mps3: 5.0,
        mode: MODE_TRACK,
    };

    // Ordering is enforced by the link, not left to the caller.
    assert!(matches!(
        link.arm_and_activate(200),
        Err(LinkError::NotHandshaken)
    ));
    assert!(matches!(
        link.send_motion(&cmd, 0),
        Err(LinkError::NotHandshaken)
    ));

    link.handshake(NONCE, 1000).expect("handshake");
    link.arm_and_activate(1000).expect("arm and activate");
    link.send_motion(&cmd, 0).expect("motion is sent");

    let got = link.receive(1000).expect("receive");
    let acked = got
        .frames
        .iter()
        .find(|f| f.message_type == MessageType::CommandAcknowledgement)
        .expect("the motion command is acknowledged");
    assert_eq!(
        kirra_r2cp::link::ack_result(acked),
        Some(kirra_r2cp::sim::ack_result::ACCEPTED)
    );
    let _ = stop.send(());
}

#[test]
fn a_stop_is_sendable_without_a_handshake() {
    // Deliberate asymmetry: commanding motion requires an identified peer, but
    // a stop must be attemptable on a link whose state is uncertain — a
    // shutdown path must not be blocked by the gate that protects startup.
    let quiet = SilentPeer::open();
    let mut link = SerialLink::open(&quiet.device, None).expect("open");
    assert!(link.peer().is_none());
    link.send_stop(100_000, 0)
        .expect("a stop is always sendable");
}

#[test]
fn the_simulator_does_not_answer_its_own_kind_of_reply() {
    // Two peers each answering the other's HELLO response would loop forever.
    // A HELLO already flagged as a response gets no reply.
    let mut mcu = SimulatedMcu::new();
    let response = peer_hello(NONCE).to_frame(1, 0, true);
    let outcome = mcu.handle(&response, 0);
    assert_eq!(outcome, Outcome::Ignored);
    assert!(mcu.reply_for(&response, outcome, 0).is_none());

    // Positive control: a PROBE does get answered.
    let probe = Hello::probe(NONCE).to_frame(2, 0, false);
    let outcome = mcu.handle(&probe, 0);
    let reply = mcu
        .reply_for(&probe, outcome, 0)
        .expect("a probe is answered");
    assert_eq!(reply.message_type, MessageType::Hello);
    assert_ne!(reply.flags & kirra_r2cp::FLAG_RESPONSE, 0);
    assert!(encode(&reply).is_ok());
}
