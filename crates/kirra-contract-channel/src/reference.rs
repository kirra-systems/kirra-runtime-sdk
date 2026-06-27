//! Host / in-process reference carrier (the Ideal-B seam made concrete for tests
//! and demos). See the crate-level "carrier-agnostic seam" section.
//!
//! [`InProcessRegion`] is an atomics-backed shared region implementing both
//! [`ContractReader`] and [`ContractWriter`] with **no `unsafe` and no
//! allocation**. It exercises the full publish/coherent-read protocol in one
//! process (e.g. two threads sharing an `Arc<InProcessRegion>`). The generation
//! counter carries the ordering (Acquire/Release); body fields are Relaxed (the
//! seqlock generation fences them).
//!
//! The **target** swaps this out: a raw hypervisor-SHM binding (Ideal-B) or a
//! future minimal-footprint iceoryx2 consumer (Ideal-A) implements the same two
//! traits over real cross-partition memory. The contract and trust chain are
//! identical regardless — this is the only thing that changes.

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::view::MAX_COMMAND_BYTES;
use crate::{ContractReader, ContractWriter, GovernorContractView};

/// An atomics-backed, in-process stand-in for the shared contract region. Every
/// field is an atomic so concurrent reader/writer access is well-defined without
/// `unsafe`. Share it across threads via `Arc<InProcessRegion>`.
pub struct InProcessRegion {
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

impl InProcessRegion {
    /// A zeroed region (generation 0 = even/quiescent, no command published yet).
    pub fn new() -> Self {
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

impl Default for InProcessRegion {
    fn default() -> Self {
        Self::new()
    }
}

impl ContractReader for InProcessRegion {
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

impl ContractWriter for InProcessRegion {
    fn store_generation(&self, generation: u64) {
        self.generation.store(generation, Ordering::Release);
    }
    fn store_body(&self, view: &GovernorContractView) {
        // Every field EXCEPT generation (the publish() driver owns the counter).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{publish, read_coherent_snapshot, MAX_SNAPSHOT_RETRIES};

    #[test]
    fn publish_then_read_roundtrips_through_the_region() {
        let region = InProcessRegion::new();
        let body = GovernorContractView::new_command(0, 7, 100, 10_000, b"steer").unwrap();
        let committed = publish(&region, 0, &body);

        let snap = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
        assert_eq!(snap.generation, committed);
        assert_eq!(snap.generation % 2, 0);
        assert_eq!(snap.sequence, 7);
        assert_eq!(snap.validated_command(), Some(&b"steer"[..]));
    }
}
