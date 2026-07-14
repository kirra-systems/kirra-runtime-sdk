//! Lean fabric-plane domain types (ADR-0035 Stage 2.5 C2 slice 2).
//!
//! The PURE data the persistence layer marshals — [`asset::FabricAsset`] and the
//! [`CausalLogEntry`] forensic-ledger record — lifted out of the heavy
//! `kirra-verifier` root crate so `verifier_store::fabric` can name them without
//! the fabric service tree (`fabric::router` = axum, `fabric::governor`, and the
//! store-backed `FabricCausalLog` facade, all of which stay in the verifier crate).
//!
//! The hash-chain fields on [`CausalLogEntry`] are plain data here; computing and
//! signing them (the `FabricCausalLog` facade over the shared `VerifierStore`)
//! remains in the verifier crate — this crate has no DB or audit dependency.

use serde::{Deserialize, Serialize};

pub mod asset;

/// Max number of entries returned by a single causal-log export page (#87).
/// Bounds the response so a forensic export can never load an unbounded ledger.
pub const CAUSAL_EXPORT_MAX_PAGE: u32 = 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalLogEntry {
    pub entry_id: String,
    /// #87: monotone chain position (genesis = 0).
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub asset_id: String,
    pub event_type: String,
    pub payload: String,
    pub caused_by: Vec<String>,
    pub affects_assets: Vec<String>,
    pub fabric_generation: u64,
    /// #87: hash of the predecessor record (genesis = 64 zeros).
    pub previous_hash: String,
    /// #87: hash of THIS record, binding the causality edges + prev + sequence.
    pub record_hash: String,
    pub signature_b64: Option<String>,
    /// #87: content-addressed id of the signing key (None when unsigned).
    pub key_id: Option<String>,
}
