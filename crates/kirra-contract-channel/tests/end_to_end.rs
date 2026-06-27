//! End-to-end trust chain (HVCHAN-001 §3): publish under the seqlock → obtain a
//! coherent snapshot → validate → advance the watermark → expose the canonical
//! image the digest signs. Single-threaded, deterministic — it exercises the
//! protocol wiring, complementing the concurrency test in `src/seqlock.rs`.

use core::cell::{Cell, RefCell};

use kirra_contract_channel::{
    publish, read_coherent_snapshot, validate, AcceptedWatermark, ContractFault, ContractReader,
    ContractWriter, GovernorContractView, MAX_SNAPSHOT_RETRIES,
};

/// A single-threaded stand-in for the shared region: the seqlock generation lives
/// in `gen`, the body in `view`. `copy_view` overlays the authoritative counter
/// onto the snapshot's `generation`, mirroring the real region where both sit at
/// fixed offsets in one mapping.
struct LocalRegion {
    gen: Cell<u64>,
    view: RefCell<GovernorContractView>,
}

impl LocalRegion {
    fn new() -> Self {
        Self {
            gen: Cell::new(0),
            view: RefCell::new(GovernorContractView::new_command(0, 0, 0, 0, b"").unwrap()),
        }
    }
    /// Corrupt the stored CRC field, simulating an integrity fault in the region.
    fn corrupt_crc(&self) {
        self.view.borrow_mut().crc32 ^= 0xFFFF_FFFF;
    }
}

impl ContractReader for LocalRegion {
    fn load_generation(&self) -> u64 {
        self.gen.get()
    }
    fn copy_view(&self) -> GovernorContractView {
        let mut v = *self.view.borrow();
        v.generation = self.gen.get();
        v
    }
}

impl ContractWriter for LocalRegion {
    fn store_generation(&self, generation: u64) {
        self.gen.set(generation);
    }
    fn store_body(&self, view: &GovernorContractView) {
        let mut cur = self.view.borrow_mut();
        let keep_gen = cur.generation;
        *cur = *view;
        cur.generation = keep_gen; // the seqlock counter is owned by `gen`
    }
}

#[test]
fn full_trust_chain_accepts_monotonic_stream_and_rejects_replay() {
    let region = LocalRegion::new();
    let mut committed = 0u64;
    let mut watermark = AcceptedWatermark::new();

    // A strictly-increasing stream of commands, each published then validated.
    for s in 1..=8u64 {
        let cmd = std::format!("steer:{s}");
        let body =
            GovernorContractView::new_command(0, s, s * 10, 1_000_000, cmd.as_bytes()).unwrap();
        committed = publish(&region, committed, &body);

        let snap = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES)
            .expect("a quiescent region yields a coherent snapshot");

        // The snapshot carries the committed (even) generation.
        assert_eq!(snap.generation, committed);
        assert_eq!(snap.generation % 2, 0);

        // Validates against the advancing watermark; record only on Ok (§3.1).
        assert_eq!(validate(&snap, 500_000, &watermark), Ok(()));
        watermark.record(&snap);

        // Digest hook (§3 step 5): the canonical image is the exact validated
        // bytes, and it round-trips — what the existing Ed25519/SHA-256 machinery
        // signs is precisely the snapshot the judge approved.
        let image = snap.canonical_image();
        assert_eq!(GovernorContractView::from_canonical_image(&image), Some(snap));
        assert_eq!(snap.validated_command(), Some(cmd.as_bytes()));
    }

    // No new publish: re-reading the same committed snapshot is a replay — the
    // monotonic sequence gate rejects it (equal == replay).
    let stale = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
    assert!(matches!(
        validate(&stale, 500_000, &watermark),
        Err(ContractFault::SequenceRegressOrReplay { .. })
    ));
}

#[test]
fn integrity_fault_in_the_region_is_caught_on_the_validated_copy() {
    let region = LocalRegion::new();
    let body = GovernorContractView::new_command(0, 1, 0, 1_000_000, b"go").unwrap();
    publish(&region, 0, &body);
    region.corrupt_crc();

    let snap = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
    assert!(matches!(
        validate(&snap, 0, &AcceptedWatermark::new()),
        Err(ContractFault::CrcMismatch { .. })
    ));
}
