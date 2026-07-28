//! The streaming framer. Small, and load-bearing: everything the bridge decides
//! rests on it having cut the byte stream in the right places.

use kirra_r2cp::sim::{MotionCommand, MODE_TRACK};
use kirra_r2cp::{decode, encode, Frame, FrameReader, MessageType, MAX_ENCODED_FRAME};

fn frame(seq: u32) -> Frame {
    let cmd = MotionCommand {
        command_id: seq,
        valid_for_us: 100_000,
        velocity_mps: 0.25,
        curvature_per_m: 0.0,
        acceleration_limit_mps2: 1.0,
        jerk_limit_mps3: 5.0,
        mode: MODE_TRACK,
    };
    Frame::new(MessageType::MotionCommand, seq, 0).with_payload(&cmd.encode())
}

#[test]
fn feeding_one_byte_at_a_time_yields_the_same_frames_as_one_bulk_push() {
    // The property that makes read-boundary independence real: the framer's
    // output must depend on the byte SEQUENCE, never on how it was chunked.
    let mut stream = Vec::new();
    for seq in 1..6u32 {
        stream.extend_from_slice(&encode(&frame(seq)).unwrap());
    }

    let mut bulk = FrameReader::new();
    let bulk_frames = bulk.push(&stream);

    let mut drip = FrameReader::new();
    let mut drip_frames = Vec::new();
    for &b in &stream {
        drip_frames.extend(drip.push(&[b]));
    }

    assert_eq!(bulk_frames, drip_frames);
    assert_eq!(bulk_frames.len(), 5);
    for (i, candidate) in bulk_frames.iter().enumerate() {
        let decoded = decode(candidate).expect("each candidate decodes");
        assert_eq!(decoded.sequence, i as u32 + 1);
    }
}

#[test]
fn an_arbitrary_chunking_yields_the_same_frames_too() {
    let mut stream = Vec::new();
    for seq in 1..4u32 {
        stream.extend_from_slice(&encode(&frame(seq)).unwrap());
    }
    let reference = FrameReader::new().push(&stream);

    // Deterministic, awkward chunk sizes — including ones that land mid-frame
    // and ones that straddle a delimiter.
    for size in [1usize, 3, 7, 13, 64, 255, 1024] {
        let mut r = FrameReader::new();
        let mut got = Vec::new();
        for part in stream.chunks(size) {
            got.extend(r.push(part));
        }
        assert_eq!(got, reference, "chunk size {size} changed the framing");
        assert_eq!(r.pending_len(), 0, "chunk size {size} left a tail behind");
    }
}

#[test]
fn a_partial_tail_is_held_and_reported_never_returned() {
    let bytes = encode(&frame(1)).unwrap();
    let mut r = FrameReader::new();
    let head = &bytes[..bytes.len() - 1]; // everything but the delimiter

    assert!(
        r.push(head).is_empty(),
        "an unterminated frame is not a frame"
    );
    assert_eq!(r.pending_len(), head.len());

    assert_eq!(r.push(&[0]).len(), 1, "the delimiter completes it");
    assert_eq!(r.pending_len(), 0);
}

#[test]
fn idle_line_between_frames_is_not_a_frame() {
    // Consecutive delimiters are what a quiet line looks like; emitting an empty
    // candidate for each would hand the decoder work that can only ever fail.
    let mut stream = vec![0u8; 8];
    stream.extend_from_slice(&encode(&frame(1)).unwrap());
    stream.extend_from_slice(&[0, 0, 0]);
    stream.extend_from_slice(&encode(&frame(2)).unwrap());

    let mut r = FrameReader::new();
    let frames = r.push(&stream);
    assert_eq!(frames.len(), 2);
    assert!(frames.iter().all(|f| !f.is_empty()));
    assert_eq!(r.discarded_runs, 0, "idle line is not a desync");
}

#[test]
fn an_over_long_run_is_discarded_so_memory_stays_bounded() {
    // A sender that never emits a delimiter must not be able to grow the host's
    // buffer without limit. The bound comes from the FORMAT — nothing longer
    // than MAX_ENCODED_FRAME can still become a valid frame — so discarding is
    // safe rather than lossy.
    let mut r = FrameReader::new();
    let flood = vec![0x5Au8; MAX_ENCODED_FRAME * 4];
    assert!(r.push(&flood).is_empty());
    assert!(r.discarded_runs >= 3, "got {}", r.discarded_runs);
    assert!(
        r.pending_len() <= MAX_ENCODED_FRAME,
        "buffer grew past the bound: {}",
        r.pending_len()
    );

    // Positive control: the link recovers on the next delimiter, and the frame
    // after it is intact. Bounded resync, not a poisoned reader.
    let mut after = vec![0u8];
    after.extend_from_slice(&encode(&frame(9)).unwrap());
    let frames = r.push(&after);
    let good: Vec<_> = frames.iter().filter_map(|f| decode(f).ok()).collect();
    assert_eq!(good.len(), 1);
    assert_eq!(good[0].sequence, 9);
}

#[test]
fn a_frame_of_exactly_the_maximum_length_still_gets_through() {
    // Non-vacuity for the bound above: it must sit strictly above the largest
    // LEGAL frame, or the discard would be silently dropping valid traffic.
    let big = Frame::new(MessageType::ConfigurationSet, 1, 0)
        .with_payload(&vec![0xABu8; kirra_r2cp::MAX_PAYLOAD]);
    let bytes = encode(&big).expect("a maximum-payload frame encodes");
    assert!(bytes.len() <= MAX_ENCODED_FRAME);

    let mut r = FrameReader::new();
    let frames = r.push(&bytes);
    assert_eq!(frames.len(), 1);
    assert_eq!(
        r.discarded_runs, 0,
        "a legal maximum frame must not be discarded"
    );
    assert_eq!(
        decode(&frames[0]).unwrap().payload.len(),
        kirra_r2cp::MAX_PAYLOAD
    );
}
