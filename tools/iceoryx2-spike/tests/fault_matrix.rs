// Integration test — the fault matrix over a REAL iceoryx2 channel, asserted.
// Runs identically in both feature configs:
//   cargo test                        (full:    iceoryx2 std + console)
//   cargo test --no-default-features  (minimal: iceoryx2 no features)
//
// Gate = verdict correctness only.

use iceoryx2_spike::harness::{run_matrix, FaultClass, Outcome};
use iceoryx2_spike::judge::{JudgeState, RejectReason, Verdict};
use iceoryx2_spike::wire::{edge_validate, CommandFrame, EdgeReject, FRAME_MAGIC, MAX_PAYLOAD_LEN};

/// Every fault class produces its expected verdict across many samples over the
/// real transport.
#[test]
fn fault_matrix_all_classes_correct() {
    let rows = run_matrix("kirra/spike/test-matrix", 50, 200).expect("matrix run");
    assert_eq!(rows.len(), FaultClass::ALL.len());
    for r in &rows {
        assert!(
            r.all_correct,
            "class {} wrong: expected {:?}, observed {:?}",
            r.class.name(),
            r.expected,
            r.observed
        );
    }
}

/// The Replay row specifically: an EQUAL sequence must reject (the corrected
/// `<=` rule). This is the red/green proof that the strict-`<` replay hole is
/// not present.
#[test]
fn replay_equal_sequence_rejects() {
    let mut state = JudgeState {
        last_accepted: 7,
        have_accepted: true,
    };
    // Equal sequence ⇒ Replay.
    let equal = CommandFrame::well_formed(7, u64::MAX / 2, 1.0, 0.0);
    assert_eq!(
        state.judge(&equal, 1_000),
        Verdict::Reject(RejectReason::Replay),
        "equal sequence must be rejected as Replay (the corrected <= check)"
    );
    // Strictly-newer ⇒ accepted (the legitimate path still works).
    let newer = CommandFrame::well_formed(8, u64::MAX / 2, 1.0, 0.0);
    assert_eq!(state.judge(&newer, 1_000), Verdict::Accept);
}

/// Lower sequence is a regress (distinct from replay).
#[test]
fn lower_sequence_is_regress() {
    let mut state = JudgeState {
        last_accepted: 7,
        have_accepted: true,
    };
    let lower = CommandFrame::well_formed(6, u64::MAX / 2, 1.0, 0.0);
    assert_eq!(
        state.judge(&lower, 1_000),
        Verdict::Reject(RejectReason::SequenceRegress)
    );
}

/// The subscriber edge rejects oversize before the judge sees the frame.
#[test]
fn edge_rejects_oversize() {
    let mut f = CommandFrame::well_formed(1, u64::MAX / 2, 1.0, 0.0);
    f.declared_len = (MAX_PAYLOAD_LEN + 1) as u16;
    assert_eq!(edge_validate(&f), Err(EdgeReject::Oversize));
}

/// The subscriber edge rejects a CRC mismatch.
#[test]
fn edge_rejects_crc_mismatch() {
    let mut f = CommandFrame::well_formed(1, u64::MAX / 2, 1.0, 0.0);
    f.payload[0] ^= 0xFF; // corrupt without recomputing CRC
    assert_eq!(edge_validate(&f), Err(EdgeReject::CrcMismatch));
}

/// Bad magic, missed deadline, integrity flag, and the kinematic envelope all
/// reject through the judge; a clean frame accepts.
#[test]
fn judge_contract_checks() {
    // Clean accept.
    let mut s = JudgeState::new();
    let ok = CommandFrame::well_formed(1, u64::MAX / 2, 5.0, 0.2);
    assert_eq!(s.judge(&ok, 1_000), Verdict::Accept);

    // Bad magic.
    let mut s = JudgeState::new();
    let mut bad = CommandFrame::well_formed(1, u64::MAX / 2, 5.0, 0.2);
    bad.magic = !FRAME_MAGIC;
    assert_eq!(
        s.judge(&bad, 1_000),
        Verdict::Reject(RejectReason::BadMagic)
    );

    // Missed deadline (now > deadline).
    let mut s = JudgeState::new();
    let stale = CommandFrame::well_formed(1, 100, 5.0, 0.2);
    assert_eq!(
        s.judge(&stale, 1_000),
        Verdict::Reject(RejectReason::DeadlineMissed)
    );

    // Integrity flag not set.
    let mut s = JudgeState::new();
    let mut noint = CommandFrame::well_formed(1, u64::MAX / 2, 5.0, 0.2);
    noint.integrity_flag = 0;
    assert_eq!(
        s.judge(&noint, 1_000),
        Verdict::Reject(RejectReason::IntegrityFlag)
    );

    // Kinematic envelope (over the PROXY limit) and NaN both reject.
    let mut s = JudgeState::new();
    let fast = CommandFrame::well_formed(1, u64::MAX / 2, 999.0, 0.0);
    assert_eq!(
        s.judge(&fast, 1_000),
        Verdict::Reject(RejectReason::KinematicLimit)
    );
    let mut s = JudgeState::new();
    let nan = CommandFrame::well_formed(1, u64::MAX / 2, f64::NAN, 0.0);
    assert_eq!(
        s.judge(&nan, 1_000),
        Verdict::Reject(RejectReason::KinematicLimit)
    );
}

/// Sanity: a valid frame survives the zero-copy round-trip and is accepted
/// (a single-class smoke over the transport).
#[test]
fn valid_round_trips_and_accepts() {
    let rows = run_matrix("kirra/spike/test-valid", 10, 20).expect("matrix run");
    let valid = rows.iter().find(|r| r.class == FaultClass::Valid).unwrap();
    assert!(valid.all_correct);
    assert_eq!(valid.observed, Outcome::Judged(Verdict::Accept));
}
