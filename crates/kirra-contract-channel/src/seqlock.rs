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

use core::sync::atomic::{fence, Ordering};

use crate::view::GovernorContractView;

// Memory-ordering contract (the four edges that make the seqlock sound on
// weakly-ordered targets — aarch64 is the deployment ISA, so TSO intuition
// does not apply; this is the canonical seqlock shape, cf. Boehm, "Can
// seqlocks get along with programming language memory models?", MSPC 2012):
//
//   1. writer even-commit store  = Release   (impl: `store_generation`)
//   2. reader first gen load g1  = Acquire   (impl: `load_generation`)
//      → edges 1+2 make a committed body visible when g1 observes it.
//   3. writer `fence(Release)` AFTER the odd store, BEFORE the body stores
//      (owned by [`publish`]) — without it the Relaxed body stores may become
//      visible BEFORE the odd marker, so a reader could copy new bytes while
//      both its generation reads still see the old even value.
//   4. reader `fence(Acquire)` AFTER the body copy, BEFORE the g2 re-read
//      (owned by [`read_coherent_snapshot`]) — an Acquire *load* of g2 alone
//      is a one-way barrier that does NOT stop the earlier Relaxed body loads
//      from being observed after it; the fence pairs with edge 3 so that if
//      the copy overlapped a write session, g2 is forced to observe the odd
//      (or later) generation and the snapshot is rejected.
//
// Edges 3 and 4 live HERE, in the shared driver, so every region binding
// (in-process reference, POSIX-SHM read-write and read-only, future
// hypervisor/iceoryx2 shims) inherits them; implementations only owe edges
// 1 and 2 plus Relaxed (data-race-free) body accesses.

/// Read access to the shared contract region (the governor side).
///
/// Implementors MUST make [`load_generation`](Self::load_generation) an
/// acquire-ordered read of the seqlock counter (ordering edge 2 above; the
/// driver supplies the post-copy acquire fence, edge 4).
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
            // Ordering edge 4: the body copy above uses Relaxed loads; without
            // this fence a weakly-ordered CPU may satisfy those loads AFTER the
            // g2 re-read below (an Acquire load only bars later ops from moving
            // up, not earlier ones from moving down), admitting a torn snapshot
            // that g2 == g1 cannot detect. Pairs with the writer's release
            // fence in [`publish`].
            fence(Ordering::Acquire);
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
    // Ordering edge 3: the body stores below are Relaxed; without this fence a
    // weakly-ordered CPU may make them visible BEFORE the odd marker above (a
    // Release store only orders EARLIER accesses before itself), so a reader
    // could copy new body bytes while both its generation reads still return
    // the old even value. Pairs with the reader's acquire fence in
    // [`read_coherent_snapshot`].
    fence(Ordering::Release);
    writer.store_body(body);
    let next = committed_gen.wrapping_add(2); // even: commit
    writer.store_generation(next);
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reference::InProcessRegion;
    use crate::view::GovernorContractView;
    use core::cell::Cell;
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

    #[test]
    fn concurrent_writer_and_reader_never_yield_a_torn_snapshot() {
        // The publisher writes, for sequence s, a view whose fields satisfy a
        // cross-field invariant: deadline == s*1000, publication == s, and the
        // command's CRC matches `crc32`. A torn snapshot (a mix of two writes)
        // would break the invariant; the seqlock must guarantee every coherent
        // snapshot the reader accepts is internally consistent.
        let region = Arc::new(InProcessRegion::new());
        // Miri interprets ~3 orders of magnitude slower; a reduced count still
        // exercises the same acquire/release protocol (Miri's value is the
        // memory-model checking, not the iteration volume).
        const N: u64 = if cfg!(miri) { 200 } else { 20_000 };

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
