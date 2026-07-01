//! # kirra-contract-channel — the Clause 2 cross-partition boundary contract
//!
//! This crate realizes **ADR-0006 Clause 2** and the **HVCHAN-001** specification
//! (`docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md`): the guest↔host governor
//! contract channel is **not a transport library** but a **frozen, versioned,
//! fixed-size `#[repr(C)]` layout over hypervisor shared memory**, written by an
//! untrusted guest publisher and validated byte-by-byte by the governor, which
//! **fails closed** on every fault.
//!
//! ## What's here
//!
//! - [`GovernorContractView`] — the frozen `#[repr(C)]`, pointer-free layout
//!   (HVCHAN-001 §2). Its byte layout is pinned by compile-time assertions — the
//!   **freeze IS the safety claim** (the `kinematics_contract.rs` talisman
//!   discipline). Any change to a field, size, order, or [`MAX_COMMAND_BYTES`]
//!   requires a new [`LAYOUT_VERSION`], never an in-place edit.
//! - [`ContractReader`] / [`ContractWriter`] + [`read_coherent_snapshot`] /
//!   [`publish`] — the **odd/even generation seqlock** (HVCHAN-001 §3 steps 2-3).
//!   These are generic over the region traits so the **target binds them to the
//!   hypervisor-mapped region** (the only place memory-mapping `unsafe` lives —
//!   the ADR-0006 Clause 3 integration shim, *outside* this crate) while the
//!   protocol logic stays `no_std` + `#![forbid(unsafe_code)]` and host-testable.
//! - [`validate`] + [`ContractFault`] — the snapshot validation pipeline
//!   (bounds → CRC → judge) and the fail-closed failure taxonomy (HVCHAN-001 §4).
//! - [`AcceptedWatermark`] — the monotonic generation/sequence gate
//!   (`<= last_accepted ⇒ reject`; HVCHAN-001 §3.1), the same rule the #273
//!   iceoryx2 spike judge and the #79 epoch fence use.
//! - [`VehicleCommandPayload`] — what the opaque `command` bytes MEAN for
//!   [`LAYOUT_VERSION`] 1 (the doer↔checker actuator command, L3): the frozen
//!   little-endian encoding of the checker's input type `ProposedVehicleCommand`,
//!   with a fail-closed [`decode`](VehicleCommandPayload::decode) (wrong length /
//!   non-finite ⇒ reject). The command encoding is part of the Clause 2 contract,
//!   so it too is freeze-asserted and version-bound.
//!
//! ## What's deliberately NOT here
//!
//! No transport, no crypto primitive, no allocator. The release-token **digest**
//! is computed over [`GovernorContractView::canonical_image`] by the verifier's
//! **existing** SHA-256 + Ed25519 machinery at the integration seam (HVCHAN-001
//! §3 steps 5-6: *"no new crypto primitives are introduced"*). This crate only
//! produces the **exact validated bytes** that digest signs.
//!
//! ## The carrier-agnostic seam ([`ContractReader`] / [`ContractWriter`])
//!
//! The two region traits ARE the seam between the **contract** (this crate's
//! frozen layout + trust chain, which never changes) and the **carrier** (how the
//! bytes physically cross the boundary, which can). Three carriers bind the same
//! seam:
//!
//! - **Ideal-B — raw hypervisor shared memory (available today).** The target
//!   binds the traits to a mapped region; the mapping `unsafe` lives in the
//!   ADR-0006 Clause 3 integration shim, *outside* this `forbid(unsafe)` crate.
//! - **Ideal-A — a future minimal-footprint iceoryx2 consumer.** If iceoryx2
//!   offers a static, read-only, no-discovery/lifecycle/pools consumer that fits
//!   the safety-partition TCB, it binds the *same* traits — the contract and the
//!   trust chain are byte-for-byte unchanged; only the carrier differs.
//! - **Host / in-process — [`reference::InProcessRegion`].** An atomics-backed
//!   region for tests and demos (e.g. two threads sharing one `Arc`), with no
//!   `unsafe` and no allocation. The reference the target swaps out.
//!
//! Because the contract is carrier-agnostic, adopting a better carrier later is a
//! drop-in: implement the two traits, change nothing else.
//!
//! ## Boundary clock domain (HVCHAN-001 §5, R-HV-3)
//!
//! All timestamp/deadline fields are defined in the **boundary clock domain**
//! (the hypervisor-provided shared monotonic source). [`validate`] takes a
//! `now_nanos` that the caller MUST read from that same domain — the governor
//! validation path **never** reads wall/PTP time. Mixing domains is a defined
//! fault class (the caller's responsibility; see `AOU-TIMESYNC-001`).

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

mod command;
mod crc;
pub mod reference;
mod seqlock;
mod validate;
mod view;

pub use command::{CommandCodecError, CommandField, VehicleCommandPayload, COMMAND_PAYLOAD_LEN};
pub use crc::crc32_ieee;
pub use seqlock::{publish, read_coherent_snapshot, ContractReader, ContractWriter, SnapshotFault};
pub use validate::{validate, AcceptedWatermark, ContractFault};
pub use view::{
    GovernorContractView, CANONICAL_IMAGE_LEN, LAYOUT_VERSION, MAGIC, MAX_COMMAND_BYTES,
};

/// Bounded seqlock-read retry budget (HVCHAN-001 §3 step 3). Retry-exhaustion —
/// a persistently odd generation or a churning writer — is **fail-closed reject**
/// ([`SnapshotFault::RetryExhausted`]), never a stale-data accept. DESIGN-INTENT
/// bound; the on-target figure is #274 work.
pub const MAX_SNAPSHOT_RETRIES: u32 = 8;
