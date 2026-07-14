//! Pure audit/causal hash-chain primitives (ADR-0035 — the `kirra-persistence`
//! enabling work, slice 2).
//!
//! The SHA-256 record-hash computations and the domain-separated canonical
//! signing/anchor-head payload encoders that define the on-disk audit-chain and
//! fabric causal-log formats, plus the content-addressed [`verifying_key_id`].
//! Moved VERBATIM from the root crate's `audit_chain` module so the persistence
//! layer can compute + verify hashes without depending on the root — the stateful
//! `AuditChainLinker` (append-into-a-transaction, rusqlite) stays in root and calls
//! these. No DB, no state.
//!
//! **Byte-exactness is load-bearing.** These encoders ARE the wire/on-disk format:
//! same domain tags (`KIRRA-AUDIT-V2`, `KIRRA-CAUSAL-V1`, `kirra-*:v1`), same
//! length-prefixing, same little-endian field order. Any change re-defines the
//! format and invalidates every stored signature/hash.

use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic per-process discriminator folded into each minted verdict id so two
/// denials of the SAME payload in the SAME millisecond still get distinct ids.
static VERDICT_MINT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Mint a verdict id: 32 lowercase hex chars (the first 16 bytes of a SHA-256
/// over the event time, a monotonic counter, and the payload bytes).
/// Collision-resistant across concurrent denials and restarts for any realistic
/// denial volume; the id is a HANDLE (retrieval key), not a secret. (EP-17 — a
/// content-addressed audit id, so it lives with the audit hash primitives.)
#[must_use]
pub fn mint_verdict_id(now_ms: u64, payload_json: &str) -> String {
    let n = VERDICT_MINT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut h = Sha256::new();
    h.update(now_ms.to_be_bytes());
    h.update(n.to_be_bytes());
    h.update(payload_json.as_bytes());
    let digest = h.finalize();
    hex::encode(&digest[..16])
}

/// Is `s` a well-formed verdict id (exactly 32 lowercase hex chars)? The retrieval
/// handler validates BEFORE the id is interpolated into a SQL LIKE pattern, so
/// `%`/`_` metacharacters can never widen the match.
#[must_use]
pub fn is_valid_verdict_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// V1 canonical signing payload — kept ONLY for verifying pre-migration
/// rows. Format: `{prev_hash}:{entry_hash}:{event_type}:{timestamp_ms}`.
pub fn canonical_signing_payload(
    prev_hash: &str,
    entry_hash: &str,
    event_type: &str,
    timestamp_ms: i64,
) -> String {
    format!(
        "{}:{}:{}:{}",
        prev_hash, entry_hash, event_type, timestamp_ms
    )
}

/// V2 canonical signing payload. Binds `sequence` and explicit version
/// tag so a v2 signature cannot be confused with a v1 signature over the
/// same prev/entry/event_type/ts. Used for all new rows.
pub fn canonical_signing_payload_v2(
    prev_hash: &str,
    entry_hash: &str,
    event_type: &str,
    timestamp_ms: i64,
    sequence: u64,
) -> String {
    format!("v2:{prev_hash}:{entry_hash}:{event_type}:{timestamp_ms}:{sequence}")
}

/// Canonical signing payload for the audit anchor-HEAD high-water mark (#77).
///
/// Binds the highest committed chain position `(sequence, record_hash)`.
/// Domain-separated (`kirra-audit-head:v1` prefix) so a head signature can never
/// be replayed as a row signature — or vice versa — under the same key.
pub fn canonical_anchor_head_payload(sequence: u64, record_hash: &str) -> String {
    format!("kirra-audit-head:v1:{sequence}:{record_hash}")
}

// --- Fabric causal-log forensic chain primitives (issue #87) ---------------
//
// The fabric causal log is a hash-chained, signed, persisted forensic ledger
// that MIRRORS the audit chain machinery, but is domain-separated so a causal
// signature/hash can never be confused with an audit one. The record hash binds
// the CAUSALITY EDGES (`caused_by`, `affects_assets`, `fabric_generation`) plus
// `previous_hash` and `sequence` — tampering an edge changes the record hash.

/// Bundled inputs for [`compute_causal_record_hash`].
#[derive(Debug, Clone)]
pub struct CausalRecordHashInput<'a> {
    pub previous_hash: &'a str,
    pub entry_id: &'a str,
    pub asset_id: &'a str,
    pub event_type: &'a str,
    pub payload: &'a str,
    pub caused_by: &'a [String],
    pub affects_assets: &'a [String],
    pub timestamp_ms: u64,
    pub fabric_generation: u64,
    pub sequence: u64,
}

/// Causal record hash (issue #87). SHA-256 over a domain-separated,
/// length-prefixed encoding that BINDS the causality edges.
///
/// Domain tag `KIRRA-CAUSAL-V1` (distinct from `KIRRA-AUDIT-V2`). Every
/// variable-length scalar field is preceded by its 8-byte LE length; each edge
/// VECTOR is preceded by its element count (8-byte LE) and every element by its
/// own 8-byte LE length. Fixed-width integers are appended as LE bytes.
pub fn compute_causal_record_hash(input: &CausalRecordHashInput<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"KIRRA-CAUSAL-V1");
    for field in [
        input.previous_hash.as_bytes(),
        input.entry_id.as_bytes(),
        input.asset_id.as_bytes(),
        input.event_type.as_bytes(),
        input.payload.as_bytes(),
    ] {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field);
    }
    // Edge vectors: count-prefixed, each element length-prefixed. This is what
    // binds the causality edges into the record hash.
    for vec in [input.caused_by, input.affects_assets] {
        hasher.update((vec.len() as u64).to_le_bytes());
        for elem in vec {
            hasher.update((elem.len() as u64).to_le_bytes());
            hasher.update(elem.as_bytes());
        }
    }
    hasher.update(input.timestamp_ms.to_le_bytes());
    hasher.update(input.fabric_generation.to_le_bytes());
    hasher.update(input.sequence.to_le_bytes());
    hex::encode(hasher.finalize())
}

/// Canonical signing payload for a causal-log row (issue #87).
///
/// The Ed25519 signature is over `record_hash`, which TRANSITIVELY binds the
/// causality edges. `previous_hash` and `sequence` also appear explicitly so the
/// row's chain position is signed directly. Domain prefix `kirra-causal:v1`.
pub fn canonical_causal_signing_payload(
    previous_hash: &str,
    record_hash: &str,
    event_type: &str,
    timestamp_ms: u64,
    sequence: u64,
) -> String {
    format!("kirra-causal:v1:{previous_hash}:{record_hash}:{event_type}:{timestamp_ms}:{sequence}")
}

/// Canonical signing payload for the causal anchor-HEAD high-water mark (#87).
/// Domain-separated (`kirra-causal-head:v1`) from BOTH the audit head and the
/// causal row payload, so no signature can be replayed across the three roles.
pub fn canonical_causal_anchor_head_payload(sequence: u64, record_hash: &str) -> String {
    format!("kirra-causal-head:v1:{sequence}:{record_hash}")
}

/// Content-addressed key id for an audit signing key: hex SHA-256 of the
/// 32-byte Ed25519 verifying-key bytes. No DER/SPKI round-trip — matches how
/// the chain already stores pubkeys (raw 32-byte values), needs no allocator.
/// A row's `key_id` is derivable from the key that signed it, so the verifier
/// can select the correct verifying key PER ROW (issue #76).
#[must_use]
pub fn verifying_key_id(vk: &ed25519_dalek::VerifyingKey) -> String {
    let mut h = Sha256::new();
    h.update(vk.as_bytes());
    hex::encode(h.finalize())
}

/// V1 (legacy) record hash: prev || event_json || created_at_ms.
/// Does NOT bind `event_type` — retained ONLY to verify pre-migration rows.
pub fn compute_record_hash_v1(
    previous_hash: &str,
    canonical_json: &str,
    created_at_ms: i64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(previous_hash.as_bytes());
    hasher.update(canonical_json.as_bytes());
    hasher.update(created_at_ms.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

/// Back-compat alias for [`compute_record_hash_v1`].
pub fn compute_record_hash(
    previous_hash: &str,
    canonical_json: &str,
    created_at_ms: i64,
) -> String {
    compute_record_hash_v1(previous_hash, canonical_json, created_at_ms)
}

/// V2 record hash. Binds `event_type` and `sequence` into the hash so
/// event_type relabeling and row reordering are caught by the cheap hash-only
/// integrity check — without needing signatures.
///
/// Domain-separated (`KIRRA-AUDIT-V2` prefix) and length-prefixed (each
/// variable-length field is preceded by its 8-byte LE length) so field-splicing
/// ambiguities (`"AB"+"C"` vs `"A"+"BC"`) cannot collide.
pub fn compute_record_hash_v2(
    previous_hash: &str,
    event_type: &str,
    event_json: &str,
    created_at_ms: i64,
    sequence: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"KIRRA-AUDIT-V2");
    for field in [
        previous_hash.as_bytes(),
        event_type.as_bytes(),
        event_json.as_bytes(),
    ] {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field);
    }
    hasher.update(created_at_ms.to_le_bytes());
    hasher.update(sequence.to_le_bytes());
    hex::encode(hasher.finalize())
}
