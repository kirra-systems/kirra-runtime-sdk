//! The odd/even generation seqlock (HVCHAN-001 §3 steps 2-3).
//!
//! The publisher marks `generation` **odd** while a write is in progress and
//! **even** on commit; the governor obtains a **coherent owned snapshot** by
//! reading the generation, copying the whole struct into partition-local memory,
//! and re-reading the generation — accepting only if it is **unchanged and even**.
//! The governor validates **only its local copy**, never the live shared region
//! (this, with the read-only mapping of §5, is the torn-read defense). Retries
//! are **bounded**; retry-exhaustion is **fail-closed reject**.
//!
//! Both directions are generic over the region traits so the **target** binds
//! them to the hypervisor-mapped region (where the mapping `unsafe` lives — the
//! ADR-0006 Clause 3 integration shim, *outside* this crate), while the protocol
//! itself stays `#![forbid(unsafe_code)]` and host-testable against atomics.

use crate::view::GovernorContractView;

/// Read access to the shared contract region (the governor side).
///
/// Implementors MUST make [`load_generation`](Self::load_generation) an
/// acquire-ordered read of the seqlock counter, so that a successful
/// generation re-read also makes the copied body bytes visible.
pub trait ContractReader {
    /// Acquire-load the seqlock `generation` counter.
    fn load_generation(&self) -> u64;
    /// Copy the current region bytes into a partition-local [`GovernorContractView`].
    /// The `generation` field of the returned value is not trusted; the caller
    /// uses the separately-loaded counter for coherence.
    fn copy_view(&self) -> GovernorContractView;
}

/// Write access to the shared contract region (the guest publisher side).
pub trait ContractWriter {
    /// Release-store the seqlock `generation` counter.
    fn store_generation(&self, generation: u64);
    /// Store every body field **except** `generation` (the [`publish`] driver
    /// owns the generation transitions). Relaxed stores are sufficient — the
    /// subsequent release-store of the even generation publishes them.
    fn store_body(&self, view: &GovernorContractView);
}

/// Why a coherent snapshot could not be obtained within the retry budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotFault {
    /// The generation was persistently odd, or churned across every copy attempt,
    /// up to `max_retries`. **Fail-closed reject** (HVCHAN-001 §4,
    /// contract-discipline) — never a stale-data accept.
    RetryExhausted,
}

/// Obtain a coherent owned snapshot (HVCHAN-001 §3 step 3).
///
/// Returns the local copy iff the generation was **even before the copy and
/// unchanged after** it. An odd generation (write in progress) or a generation
/// that moved across the copy (torn) costs a retry; exhausting `max_retries`
/// fails closed with [`SnapshotFault::RetryExhausted`].
pub fn read_coherent_snapshot<R: ContractReader>(
    reader: &R,
    max_retries: u32,
) -> Result<GovernorContractView, SnapshotFault> {
    let mut failures: u32 = 0;
    loop {
        let g1 = reader.load_generation();
        if g1 & 1 == 0 {
            // Even: no write in progress as of this read. Copy, then re-check.
            let view = reader.copy_view();
            let g2 = reader.load_generation();
            if g2 == g1 {
                // Unchanged and even ⇒ the copy did not race a writer.
                return Ok(view);
            }
        }
        // Odd, or the generation moved across the copy ⇒ torn / in-progress.
        if failures >= max_retries {
            return Err(SnapshotFault::RetryExhausted);
        }
        failures += 1;
    }
}

/// Publish `body` under the odd/even seqlock (HVCHAN-001 §3 step 2).
///
/// `committed_gen` is the current committed (even) generation; returns the new
/// committed (even) generation. The body's own `generation` field is ignored —
/// this driver owns the counter.
///
/// Sequence: `generation := committed_gen+1` (odd, write begins) → body →
/// `generation := committed_gen+2` (even, commit). A reader that observes the
/// odd value, or a generation change across its copy, retries.
pub fn publish<W: ContractWriter>(
    writer: &W,
    committed_gen: u64,
    body: &GovernorContractView,
) -> u64 {
    let writing = committed_gen.wrapping_add(1); // odd: write in progress
    writer.store_generation(writing);
    writer.store_body(body);
    let next = committed_gen.wrapping_add(2); // even: commit
    writer.store_generation(next);
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{GovernorContractView, MAX_COMMAND_BYTES};
    use core::cell::Cell;
    use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
    use std::sync::Arc;
    use std::{thread, vec::Vec};

    // ---- deterministic mocks for the snapshot loop ------------------------

    /// A reader whose `load_generation` replays a scripted sequence; `copy_view`
    /// returns a fixed view. Lets us drive the seqlock loop precisely.
    struct ScriptedReader {
        gens: Vec<u64>,
        idx: Cell<usize>,
        view: GovernorContractView,
    }
    impl ContractReader for ScriptedReader {
        fn load_generation(&self) -> u64 {
            let i = self.idx.get();
            let g = self.gens[i.min(self.gens.len() - 1)];
            self.idx.set(i + 1);
            g
        }
        fn copy_view(&self) -> GovernorContractView {
            self.view
        }
    }

    fn any_view() -> GovernorContractView {
        GovernorContractView::new_command(2, 1, 0, 0, b"x").unwrap()
    }

    #[test]
    fn stable_even_generation_yields_a_snapshot() {
        // g1 even, g2 == g1 ⇒ coherent on the first attempt.
        let r = ScriptedReader { gens: std::vec![4, 4], idx: Cell::new(0), view: any_view() };
        assert_eq!(read_coherent_snapshot(&r, 8), Ok(any_view()));
    }

    #[test]
    fn persistently_odd_generation_exhausts_and_fails_closed() {
        // Always odd ⇒ a write never commits ⇒ fail-closed after the budget.
        let r = ScriptedReader { gens: std::vec![3], idx: Cell::new(0), view: any_view() };
        assert_eq!(read_coherent_snapshot(&r, 4), Err(SnapshotFault::RetryExhausted));
    }

    #[test]
    fn generation_change_across_copy_is_treated_as_torn() {
        // First attempt: g1=4 (even), g2=6 (writer committed during copy) ⇒ torn,
        // retry. Second attempt: g1=6, g2=6 ⇒ coherent.
        let r = ScriptedReader {
            gens: std::vec![4, 6, 6, 6],
            idx: Cell::new(0),
            view: any_view(),
        };
        assert_eq!(read_coherent_snapshot(&r, 8), Ok(any_view()));
    }

    #[test]
    fn odd_then_even_recovers_within_budget() {
        // g1=3 (odd, skip+retry), then g1=8,g2=8 (coherent).
        let r = ScriptedReader { gens: std::vec![3, 8, 8], idx: Cell::new(0), view: any_view() };
        assert_eq!(read_coherent_snapshot(&r, 8), Ok(any_view()));
    }

    // ---- a real concurrent region backed by atomics -----------------------

    /// Host stand-in for the hypervisor-mapped region: every field is an atomic,
    /// so concurrent reader/writer access is well-defined with no `unsafe`. The
    /// generation carries the ordering (Acquire/Release); body fields are Relaxed.
    struct AtomicRegion {
        generation: AtomicU64,
        layout_version: AtomicU32,
        magic: AtomicU32,
        sequence: AtomicU64,
        publication_nanos: AtomicU64,
        deadline_nanos: AtomicU64,
        crc32: AtomicU32,
        command_len: AtomicU32,
        command: [AtomicU8; MAX_COMMAND_BYTES],
    }

    impl AtomicRegion {
        fn new() -> Self {
            Self {
                generation: AtomicU64::new(0),
                layout_version: AtomicU32::new(0),
                magic: AtomicU32::new(0),
                sequence: AtomicU64::new(0),
                publication_nanos: AtomicU64::new(0),
                deadline_nanos: AtomicU64::new(0),
                crc32: AtomicU32::new(0),
                command_len: AtomicU32::new(0),
                command: core::array::from_fn(|_| AtomicU8::new(0)),
            }
        }
    }

    impl ContractReader for AtomicRegion {
        fn load_generation(&self) -> u64 {
            self.generation.load(Ordering::Acquire)
        }
        fn copy_view(&self) -> GovernorContractView {
            let mut command = [0u8; MAX_COMMAND_BYTES];
            for (i, slot) in command.iter_mut().enumerate() {
                *slot = self.command[i].load(Ordering::Relaxed);
            }
            GovernorContractView {
                layout_version: self.layout_version.load(Ordering::Relaxed),
                magic: self.magic.load(Ordering::Relaxed),
                generation: self.generation.load(Ordering::Relaxed),
                sequence: self.sequence.load(Ordering::Relaxed),
                publication_nanos: self.publication_nanos.load(Ordering::Relaxed),
                deadline_nanos: self.deadline_nanos.load(Ordering::Relaxed),
                crc32: self.crc32.load(Ordering::Relaxed),
                command_len: self.command_len.load(Ordering::Relaxed),
                command,
            }
        }
    }

    impl ContractWriter for AtomicRegion {
        fn store_generation(&self, generation: u64) {
            self.generation.store(generation, Ordering::Release);
        }
        fn store_body(&self, view: &GovernorContractView) {
            // Every field EXCEPT generation. Written field-by-field so a reader
            // that ignored the seqlock could observe a torn mix — which the
            // seqlock is precisely what prevents.
            self.layout_version.store(view.layout_version, Ordering::Relaxed);
            self.magic.store(view.magic, Ordering::Relaxed);
            self.sequence.store(view.sequence, Ordering::Relaxed);
            self.publication_nanos.store(view.publication_nanos, Ordering::Relaxed);
            self.deadline_nanos.store(view.deadline_nanos, Ordering::Relaxed);
            self.crc32.store(view.crc32, Ordering::Relaxed);
            self.command_len.store(view.command_len, Ordering::Relaxed);
            for (i, b) in view.command.iter().enumerate() {
                self.command[i].store(*b, Ordering::Relaxed);
            }
        }
    }

    #[test]
    fn concurrent_writer_and_reader_never_yield_a_torn_snapshot() {
        // The publisher writes, for sequence s, a view whose fields satisfy a
        // cross-field invariant: deadline == s*1000, publication == s, and the
        // command's CRC matches `crc32`. A torn snapshot (a mix of two writes)
        // would break the invariant; the seqlock must guarantee every coherent
        // snapshot the reader accepts is internally consistent.
        let region = Arc::new(AtomicRegion::new());
        const N: u64 = 20_000;

        let w = Arc::clone(&region);
        let writer = thread::spawn(move || {
            let mut committed = 0u64;
            for s in 1..=N {
                let cmd = s.to_le_bytes();
                let body =
                    GovernorContractView::new_command(0, s, s, s * 1000, &cmd).unwrap();
                committed = publish(&*w, committed, &body);
            }
        });

        let r = Arc::clone(&region);
        let reader = thread::spawn(move || {
            let mut seen = 0u64;
            // Read until the writer has published the last sequence.
            loop {
                if let Ok(v) = read_coherent_snapshot(&*r, 64) {
                    if v.sequence != 0 {
                        // Cross-field consistency: no torn mix of two writes.
                        assert_eq!(v.deadline_nanos, v.sequence * 1000, "torn: deadline");
                        assert_eq!(v.publication_nanos, v.sequence, "torn: publication");
                        assert_eq!(
                            v.crc32,
                            crate::crc::crc32_ieee(&v.sequence.to_le_bytes()),
                            "torn: command vs crc"
                        );
                        seen = v.sequence;
                    }
                }
                if seen == N {
                    break;
                }
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }
}
