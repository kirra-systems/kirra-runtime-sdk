//! End-to-end: a bridge-shaped client opens the simulated MCU's DEVICE PATH.
//!
//! Everything here goes through a real pty — real `write(2)`, a real line
//! discipline, real read boundaries. The in-memory suite (`simulated_mcu.rs`)
//! proves the RULES; this proves they survive contact with a serial-shaped
//! transport, which is where framing bugs actually live.

#![cfg(feature = "pty")]

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::time::{Duration, Instant};

use kirra_r2cp::command_ack::AckResult;
use kirra_r2cp::pty::{Handled, SimulatedMcuPty};
use kirra_r2cp::sim::{MotionCommand, Outcome, SafetyState, SimulatedMcu, MODE_TRACK};
use kirra_r2cp::{decode, encode, Frame, FrameReader, MessageType, MAX_ENCODED_FRAME};

const VALID_FOR: u32 = 100_000;

/// The test's half of the link: what the real bridge does — open a path, write
/// bytes, frame whatever comes back.
struct BridgeEnd {
    port: std::fs::File,
    reader: FrameReader,
}

impl BridgeEnd {
    fn open(device: &Path) -> Self {
        let port = OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .expect("bridge can open the simulated MCU's device path");
        Self {
            port,
            reader: FrameReader::new(),
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        self.port.write_all(bytes).expect("write to pty");
        self.port.flush().expect("flush pty");
    }

    fn send(&mut self, frame: &Frame) {
        let bytes = encode(frame).expect("test frames encode");
        self.write_bytes(&bytes);
    }

    /// Read whatever is available within `timeout_ms`, returning completed
    /// frames. Bounded so a regression FAILS the suite instead of hanging it.
    fn read_frames(&mut self, timeout_ms: u16) -> Vec<Frame> {
        use nix::poll::{PollFd, PollFlags, PollTimeout};
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        while Instant::now() < deadline {
            let left = deadline
                .saturating_duration_since(Instant::now())
                .as_millis();
            let mut fds = [PollFd::new(self.port.as_fd(), PollFlags::POLLIN)];
            let ms = u16::try_from(left).unwrap_or(u16::MAX);
            if nix::poll::poll(&mut fds, PollTimeout::from(ms)).expect("poll") == 0 {
                break;
            }
            let mut chunk = [0u8; 512];
            let n = self.port.read(&mut chunk).expect("read from pty");
            if n == 0 {
                break;
            }
            for candidate in self.reader.push(&chunk[..n]) {
                out.push(decode(&candidate).expect("the MCU emits decodable frames"));
            }
            if !out.is_empty() {
                break;
            }
        }
        out
    }
}

/// The COMMAND_ACK fields this suite asserts on (`PROTOCOL.md` §COMMAND_ACK).
fn ack_fields(frame: &Frame) -> (u32, u16, u8) {
    assert_eq!(frame.message_type, MessageType::CommandAcknowledgement);
    assert_eq!(frame.payload.len(), 28, "ACK payload is 28 bytes");
    let received_sequence = u32::from_le_bytes(frame.payload[4..8].try_into().unwrap());
    let result = u16::from_le_bytes(frame.payload[16..18].try_into().unwrap());
    let safety_state = frame.payload[18];
    (received_sequence, result, safety_state)
}

fn motion(seq: u32, t_us: u64, v: f32) -> Frame {
    let cmd = MotionCommand {
        command_id: 1000 + seq,
        valid_for_us: VALID_FOR,
        velocity_mps: v,
        curvature_per_m: 0.0,
        acceleration_limit_mps2: 1.0,
        jerk_limit_mps3: 5.0,
        mode: MODE_TRACK,
    };
    Frame::new(MessageType::MotionCommand, seq, t_us).with_payload(&cmd.encode())
}

/// How long a test will wait for bytes it has already written to come back as a
/// handled frame. Generous on purpose: it bounds a hang, it is not a timing
/// assertion, and no passing run spends it.
const PUMP_DEADLINE: Duration = Duration::from_secs(2);

/// Pump until at least one frame is handled, or `deadline` elapses.
///
/// [`SimulatedMcuPty::pump`] is deliberately single-shot — one poll, one read —
/// because the reader it models must HOLD a partial frame rather than block
/// waiting for the rest of one. That is the right contract for the reader and
/// the wrong one for a test that has written a whole frame and wants the verdict
/// on it: the kernel may split those bytes across reads, and under CI load it
/// does (#1247). The wait belongs here, in the test, rather than in `pump` —
/// widening `pump`'s timeout would make the flake rarer without making it
/// impossible, and loosening its single-read semantics would erase the very
/// hold-a-partial-frame property the suite exists to prove.
///
/// Returns the first non-empty batch, so a caller asserting "exactly one frame"
/// still asserts it; an empty vec on deadline, so a caller asserting silence
/// still fails rather than hangs.
fn pump_until_handled(
    pty: &mut SimulatedMcuPty,
    mcu: &mut SimulatedMcu,
    now_us: u64,
    deadline: Duration,
) -> Vec<Handled> {
    let start = Instant::now();
    loop {
        let handled = pty.pump(mcu, now_us, 50).expect("pump");
        if !handled.is_empty() {
            return handled;
        }
        if start.elapsed() >= deadline {
            return Vec::new();
        }
    }
}

/// Drive the pair to Active over the wire, asserting each ACK.
fn arm_and_activate(pty: &mut SimulatedMcuPty, mcu: &mut SimulatedMcu, bridge: &mut BridgeEnd) {
    for (seq, ty, expect_state) in [
        (1u32, MessageType::Arm, SafetyState::Armed),
        (2, MessageType::Activate, SafetyState::Active),
    ] {
        bridge.send(&Frame::new(ty, seq, 0));
        let handled = pump_until_handled(pty, mcu, 0, PUMP_DEADLINE);
        assert_eq!(
            handled.len(),
            1,
            "{ty:?} should produce exactly one outcome"
        );
        assert!(handled[0].acknowledged);
        let acks = bridge.read_frames(200);
        assert_eq!(acks.len(), 1, "{ty:?} must be acknowledged");
        let (recv, result, state) = ack_fields(&acks[0]);
        assert_eq!(recv, seq);
        assert_eq!(result, AckResult::Accepted.as_u16());
        assert_eq!(state, expect_state.to_wire());
    }
}

#[test]
fn a_bridge_opening_the_device_path_is_acknowledged_over_the_wire() {
    let mut pty = SimulatedMcuPty::open().expect("allocate a pty");
    let mut mcu = SimulatedMcu::new();
    let mut bridge = BridgeEnd::open(pty.device_path());

    arm_and_activate(&mut pty, &mut mcu, &mut bridge);

    bridge.send(&motion(3, 0, 0.4));
    let handled = pump_until_handled(&mut pty, &mut mcu, 0, PUMP_DEADLINE);
    assert!(matches!(handled[0].outcome, Ok(Outcome::Accepted { .. })));
    let (recv, result, state) = ack_fields(&bridge.read_frames(200)[0]);
    assert_eq!(
        (recv, result, state),
        (
            3,
            AckResult::Accepted.as_u16(),
            SafetyState::Active.to_wire()
        )
    );
}

#[test]
fn a_refusal_comes_back_as_a_result_code_rather_than_silence() {
    // Standby: motion is refused. The bridge must LEARN that, not infer it from
    // an absent reply — silence is indistinguishable from a dead link.
    let mut pty = SimulatedMcuPty::open().expect("allocate a pty");
    let mut mcu = SimulatedMcu::new();
    let mut bridge = BridgeEnd::open(pty.device_path());

    bridge.send(&motion(1, 0, 0.4));
    let handled = pump_until_handled(&mut pty, &mut mcu, 0, PUMP_DEADLINE);
    assert!(handled[0].acknowledged, "a refusal is still acknowledged");
    let (recv, result, state) = ack_fields(&bridge.read_frames(200)[0]);
    assert_eq!(recv, 1);
    assert_eq!(result, AckResult::Disarmed.as_u16());
    assert_eq!(state, SafetyState::Standby.to_wire());

    // A replay of the same frame answers REPLAY, so the two refusals are
    // distinguishable on the wire and not merged into one "no".
    bridge.send(&Frame::new(MessageType::Arm, 5, 0));
    pump_until_handled(&mut pty, &mut mcu, 0, PUMP_DEADLINE);
    let _ = bridge.read_frames(200);
    bridge.send(&Frame::new(MessageType::Arm, 5, 0));
    pump_until_handled(&mut pty, &mut mcu, 0, PUMP_DEADLINE);
    let (_, result, _) = ack_fields(&bridge.read_frames(200)[0]);
    assert_eq!(result, AckResult::Replay.as_u16());
}

#[test]
fn a_frame_split_across_writes_is_held_until_it_is_whole() {
    // The failure this catches: a bridge (or MCU) that treats one read as one
    // frame. A UART splits frames routinely; a pty lets us do it deliberately.
    let mut pty = SimulatedMcuPty::open().expect("allocate a pty");
    let mut mcu = SimulatedMcu::new();
    let mut bridge = BridgeEnd::open(pty.device_path());
    arm_and_activate(&mut pty, &mut mcu, &mut bridge);

    let bytes = encode(&motion(3, 0, 0.4)).unwrap();
    let (a, rest) = bytes.split_at(5);
    let (b, c) = rest.split_at(rest.len() - 3);

    for part in [a, b] {
        bridge.write_bytes(part);
        assert!(
            pty.pump(&mut mcu, 0, 50).expect("pump").is_empty(),
            "a partial frame must never be acted on"
        );
    }
    assert_eq!(mcu.accepted_commands, 0);

    bridge.write_bytes(c);
    let handled = pump_until_handled(&mut pty, &mut mcu, 0, PUMP_DEADLINE);
    assert_eq!(
        handled.len(),
        1,
        "the final byte completes exactly one frame"
    );
    assert!(matches!(handled[0].outcome, Ok(Outcome::Accepted { .. })));
    assert_eq!(mcu.accepted_commands, 1);
}

#[test]
fn a_frame_the_kernel_delivers_in_pieces_is_still_recovered_by_the_wait() {
    // Regression for #1247. `pump` is single-shot by design — one poll, one
    // read — because the reader it models must HOLD a partial frame rather than
    // block for the rest of one. The consequence is that a test which writes a
    // whole frame and then pumps ONCE is asserting on a race: when the tty hands
    // those bytes over across two reads, which it does intermittently and did on
    // CI, that one pump comes back empty and the test fails on timing rather
    // than on behaviour. Waiting is the test's job, so it lives in the helper.
    //
    // The split is FORCED here rather than waited for, so this pins the property
    // instead of sampling it.
    let mut pty = SimulatedMcuPty::open().expect("allocate a pty");
    let mut mcu = SimulatedMcu::new();
    // Held for its side effect as much as its fd: the pty master reports EIO
    // once the last slave holder closes.
    let bridge = BridgeEnd::open(pty.device_path());

    let wire = encode(&Frame::new(MessageType::Hello, 1, 0).with_payload(&[1, 2, 3])).unwrap();
    let (head, tail) = wire.split_at(wire.len() - 2);
    let (head, tail) = (head.to_vec(), tail.to_vec());

    // A writer that delivers the frame in two pieces, the second far enough
    // behind the first that no single read can span them. The delays are
    // ROBUSTNESS margins, not the property: the assertions below hold for any
    // schedule in which the head lands before the tail.
    let mut split_port = bridge.port.try_clone().expect("clone the bridge fd");
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        split_port.write_all(&head).expect("write the head");
        split_port.flush().expect("flush the head");
        std::thread::sleep(Duration::from_millis(900));
        split_port.write_all(&tail).expect("write the tail");
        split_port.flush().expect("flush the tail");
    });

    // Non-vacuity, and the failure #1247 actually reports: ONE pump wakes on the
    // head, reads it, finds no whole frame, and returns empty — even though a
    // frame is in flight and will complete. A test that pumps once here calls
    // that a failure. This is the single-shot contract working as intended.
    assert!(
        pty.pump(&mut mcu, 0, 300).expect("pump").is_empty(),
        "one pump cannot complete a frame whose bytes have not all arrived"
    );

    // The wait, by contrast, keeps reading until the frame is whole.
    let handled = pump_until_handled(&mut pty, &mut mcu, 0, PUMP_DEADLINE);
    writer.join().expect("the split writer finishes");

    assert_eq!(
        handled.len(),
        1,
        "the wait must recover the frame the split delivery deferred"
    );
    assert!(
        matches!(handled[0].outcome, Ok(Outcome::Ignored)),
        "and recover it INTACT — a split must not become a torn frame: {:?}",
        handled[0].outcome
    );
}

#[test]
fn several_frames_in_one_write_are_all_handled_in_order() {
    let mut pty = SimulatedMcuPty::open().expect("allocate a pty");
    let mut mcu = SimulatedMcu::new();
    let mut bridge = BridgeEnd::open(pty.device_path());
    arm_and_activate(&mut pty, &mut mcu, &mut bridge);

    let mut batch = Vec::new();
    for seq in 3..8u32 {
        batch.extend_from_slice(&encode(&motion(seq, 0, 0.1 * seq as f32)).unwrap());
    }
    bridge.write_bytes(&batch);

    let mut handled = Vec::new();
    let deadline = Instant::now() + PUMP_DEADLINE;
    while handled.len() < 5 && Instant::now() < deadline {
        handled.extend(pty.pump(&mut mcu, 0, 100).expect("pump"));
    }
    assert_eq!(handled.len(), 5, "all five frames in one write are handled");
    assert!(handled
        .iter()
        .all(|h| matches!(h.outcome, Ok(Outcome::Accepted { .. }))));
    assert_eq!(
        mcu.last_accepted_sequence,
        Some(7),
        "handled in sequence order"
    );
}

#[test]
fn raw_mode_carries_bytes_the_line_discipline_would_otherwise_rewrite() {
    // The payload deliberately contains the bytes a cooked tty mangles:
    // 0x0D/0x0A (ICRNL/ONLCR), 0x11/0x13 (IXON flow control), 0x03 (ISIG INTR),
    // 0x7F (ERASE), 0x1A (SUSP).
    let hostile = [0x0Du8, 0x0A, 0x11, 0x13, 0x03, 0x7F, 0x1A, 0x0D, 0x0A];
    // HELLO is host->MCU and carries an opaque payload, so the frame reaching
    // the MCU intact is the whole assertion — acceptance is not the point.
    let frame = Frame::new(MessageType::Hello, 1, 0).with_payload(&hostile);
    let wire = encode(&frame).unwrap();
    assert!(
        hostile.iter().all(|b| wire.contains(b)),
        "COBS leaves these bytes literal, so they really are on the wire"
    );

    let mut pty = SimulatedMcuPty::open().expect("allocate a pty");
    let mut mcu = SimulatedMcu::new();
    let mut bridge = BridgeEnd::open(pty.device_path());

    bridge.write_bytes(&wire);
    let handled = pump_until_handled(&mut pty, &mut mcu, 0, PUMP_DEADLINE);
    assert_eq!(handled.len(), 1);
    assert!(
        matches!(handled[0].outcome, Ok(Outcome::Ignored)),
        "the frame must arrive intact: {:?}",
        handled[0].outcome
    );
}

#[test]
fn without_raw_mode_the_same_frame_is_corrupted() {
    // Non-vacuity for the test above: cfmakeraw is load-bearing, not hygiene.
    // Re-enable output post-processing on the pty and the identical bytes stop
    // arriving intact — which is what would happen to every frame on a link
    // opened at tty defaults.
    use nix::sys::termios::{tcgetattr, tcsetattr, OutputFlags, SetArg};

    let hostile = [0x0Du8, 0x0A, 0x11, 0x13, 0x03, 0x7F, 0x1A, 0x0D, 0x0A];
    let frame = Frame::new(MessageType::Hello, 1, 0).with_payload(&hostile);
    let wire = encode(&frame).unwrap();

    let mut pty = SimulatedMcuPty::open().expect("allocate a pty");
    let mut mcu = SimulatedMcu::new();
    let mut bridge = BridgeEnd::open(pty.device_path());

    let fd = bridge.port.as_fd();
    let mut attrs = tcgetattr(fd).expect("tcgetattr");
    attrs.output_flags |= OutputFlags::OPOST | OutputFlags::ONLCR;
    tcsetattr(fd, SetArg::TCSANOW, &attrs).expect("tcsetattr");

    bridge.write_bytes(&wire);
    let handled = pump_until_handled(&mut pty, &mut mcu, 0, PUMP_DEADLINE);

    // ONLCR inserts a 0x0D before each 0x0A, so more bytes arrive than were
    // sent and the COBS run lengths no longer describe them. The observed
    // verdict is MalformedCobs; the assertion is deliberately the CLASS (some
    // decode error) rather than that one variant, since which error a given
    // corruption trips is not the property under test.
    //
    // Requiring a NON-EMPTY result matters: an empty `handled` would also
    // satisfy "not intact" while proving nothing, so the bytes must be shown to
    // have arrived AND to have arrived broken.
    assert_eq!(
        handled.len(),
        1,
        "the corrupted bytes must reach the decoder, not vanish: {handled:?}"
    );
    assert!(
        handled[0].outcome.is_err(),
        "a cooked tty must NOT deliver this frame intact — if it does, the raw-mode \
         test above proves nothing. got {:?}",
        handled[0].outcome
    );
    assert!(!handled[0].acknowledged, "an undecodable frame gets no ACK");
    assert_eq!(mcu.accepted_commands, 0);
}

#[test]
fn an_undelimited_flood_is_discarded_and_the_link_resynchronises() {
    // A stuck line or a desynchronised sender must not grow host memory without
    // limit, and must not cost more than the garbage itself.
    let mut pty = SimulatedMcuPty::open().expect("allocate a pty");
    let mut mcu = SimulatedMcu::new();
    let mut bridge = BridgeEnd::open(pty.device_path());
    arm_and_activate(&mut pty, &mut mcu, &mut bridge);

    let flood = vec![0x42u8; MAX_ENCODED_FRAME * 3];
    bridge.write_bytes(&flood);
    let deadline = Instant::now() + PUMP_DEADLINE;
    while pty.discarded_runs() == 0 && Instant::now() < deadline {
        let handled = pty.pump(&mut mcu, 0, 100).expect("pump");
        assert!(handled.is_empty(), "undelimited garbage is not a frame");
    }
    assert!(
        pty.discarded_runs() > 0,
        "an over-long run must be discarded, not buffered"
    );

    // Positive control: the link still works. The delimiter ends the flood and
    // the next real frame is accepted.
    bridge.write_bytes(&[0]);
    bridge.send(&motion(3, 0, 0.4));

    let mut handled = Vec::new();
    let deadline = Instant::now() + PUMP_DEADLINE;
    while Instant::now() < deadline
        && !handled
            .iter()
            .any(|h: &Handled| matches!(h.outcome, Ok(Outcome::Accepted { .. })))
    {
        handled.extend(pty.pump(&mut mcu, 0, 100).expect("pump"));
    }

    // The flood's cost is bounded to the garbage itself plus at most ONE bogus
    // candidate — the sub-cap remainder the closing delimiter finally ends. It
    // is refused by the decoder, never handed to the state machine.
    let accepted = handled
        .iter()
        .filter(|h| matches!(h.outcome, Ok(Outcome::Accepted { .. })))
        .count();
    assert_eq!(accepted, 1, "the frame after the flood must be accepted");
    assert!(
        handled.len() <= 2,
        "resync must cost at most one bogus candidate, got {handled:?}"
    );
    assert!(
        handled
            .iter()
            .all(|h| h.outcome.is_err() || matches!(h.outcome, Ok(Outcome::Accepted { .. }))),
        "the garbage must be refused by the DECODER, never reach the state machine"
    );
    assert_eq!(mcu.accepted_commands, 1);
}

#[test]
fn the_watchdog_announces_its_controlled_stop_on_the_wire() {
    // A stop the host did not ask for is exactly the event a bridge must not
    // have to infer from absent telemetry.
    let mut pty = SimulatedMcuPty::open().expect("allocate a pty");
    let mut mcu = SimulatedMcu::new();
    let mut bridge = BridgeEnd::open(pty.device_path());
    arm_and_activate(&mut pty, &mut mcu, &mut bridge);

    bridge.send(&motion(3, 0, 0.4));
    pump_until_handled(&mut pty, &mut mcu, 0, PUMP_DEADLINE);
    let _ = bridge.read_frames(200);

    // Inside the window: nothing to announce.
    assert!(!pty.tick(&mut mcu, u64::from(VALID_FOR) / 2).expect("tick"));
    assert!(bridge.read_frames(50).is_empty());

    // Past it: an unsolicited ACK carrying controlled_stop.
    assert!(pty.tick(&mut mcu, u64::from(VALID_FOR) + 1).expect("tick"));
    let acks = bridge.read_frames(200);
    assert_eq!(acks.len(), 1, "the stop is announced");
    let (recv, _, state) = ack_fields(&acks[0]);
    assert_eq!(recv, 0, "sequence 0: this ACK answers no frame");
    assert_eq!(state, SafetyState::ControlledStop.to_wire());
    assert_eq!(mcu.state, SafetyState::ControlledStop);
}

#[test]
fn the_device_path_is_a_real_pts_node() {
    let pty = SimulatedMcuPty::open().expect("allocate a pty");
    let path = pty.device_path();
    assert!(path.exists(), "{} should exist", path.display());
    assert!(
        path.starts_with("/dev/pts/") || path.starts_with("/dev/tty"),
        "expected a pts node, got {}",
        path.display()
    );
}
