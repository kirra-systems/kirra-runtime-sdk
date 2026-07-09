//! EP-15 — Kani proof harnesses over the checker cores. See Cargo.toml for the
//! full map. This crate root does three things:
//!
//! 1. Provides the TWO tiny constant shims the `#[path]`-included `lease.rs`
//!    expects from its home crate (`crate::posture_cache` /
//!    `crate::standby_monitor`). The shims feed only lease.rs's compile-time
//!    `const` asserts and its own unit tests — the PROOFS quantify over all
//!    `u64` TTLs and never read them.
//! 2. `#[path]`-includes the three checker-core sources VERBATIM from their
//!    shipped locations (no copies — the file on disk under proof IS the file
//!    that ships).
//! 3. Mounts the `#[cfg(kani)]` proof modules and their `#[cfg(test)]`
//!    concrete mirrors.

// The included sources carry items this crate doesn't call (their home crates
// do) and intra-doc links into their home crates.
#![allow(dead_code)]
#![allow(rustdoc::broken_intra_doc_links)]

/// Shim for `lease.rs`'s `use crate::posture_cache::POSTURE_CACHE_TTL_MS`.
/// MUST mirror `src/posture_cache.rs` (the real registry). Feeds only the
/// const asserts at the top of lease.rs — a drift here would skew those
/// asserts, not the proofs (which quantify over every `u64` TTL).
pub mod posture_cache {
    /// Mirror of `kirra_verifier::posture_cache::POSTURE_CACHE_TTL_MS`.
    pub const POSTURE_CACHE_TTL_MS: u64 = 5_000;
}

/// Shim for the legacy-heartbeat constants lease.rs's own tests compare against.
/// MUST mirror `src/standby_monitor.rs`.
pub mod standby_monitor {
    /// Mirror of `kirra_verifier::standby_monitor::PROMOTION_POLL_MS`.
    pub const PROMOTION_POLL_MS: u64 = 1_000;
    /// Mirror of `kirra_verifier::standby_monitor::PROMOTION_TIMEOUT_MS`.
    pub const PROMOTION_TIMEOUT_MS: u64 = 10_000;
}

// ---------------------------------------------------------------------------
// The checker cores under proof — included VERBATIM from their shipped paths.
// ---------------------------------------------------------------------------

/// WP-19/EP-03 HA lease timing model — `src/lease.rs`, the root crate's file.
#[path = "../../../src/lease.rs"]
pub mod lease;

/// The FROZEN kinematics-contract talisman — `kirra-core`'s file (git blob
/// `ed00f4da…`; amended only under explicit review + re-pin, which is WHY its
/// proofs live here instead of inline).
#[path = "../../../crates/kirra-core/src/kinematics_contract.rs"]
#[allow(clippy::doc_overindented_list_items)] // pre-existing in the frozen file
#[rustfmt::skip] // blob-pinned (`ed00f4da…`) — a reformat here breaks the pin
pub mod kinematics_contract;

/// The parko RSS primitives — `parko-core`'s file (its own workspace).
#[path = "../../../parko/crates/parko-core/src/rss.rs"]
pub mod rss;

// ---------------------------------------------------------------------------
// Proof harnesses (compiled ONLY under `cargo kani`) + their concrete mirrors
// (compiled under `cargo test`, so the properties are exercised locally too).
// ---------------------------------------------------------------------------

mod proofs_kinematics;
mod proofs_lease;
mod proofs_rss;
