// src/verifier_store/audit_appender.rs
//
// ADR-0035 Addendum A — the injected audit-append seam + the audit-chain WRITE
// mechanics (persistence enabling slice 2b).
//
// The hard-tier `verifier_store` families couple to the audit chain by appending a
// signed, hash-chained event *inside the same transaction* as their table write
// (coupling "C1"). Slice 1 inverted that behind the `AuditAppender` trait so callers
// no longer name the append function directly. Slice 2b removes the LAST
// `crate::audit_chain` reference from persistence: the append MECHANICS —
// `append_audit_event_tx` — write the `audit_log_chain` / `audit_anchor_head` tables
// that persistence OWNS, using only the pure `kirra_audit_hash` primitives + Ed25519,
// so they belong in this layer, not the root `audit_chain` module. The root
// `AuditChainLinker::append_audit_event_tx` now DELEGATES down to the function here
// (for its own typed wrappers + callers), so the byte-for-byte chain format is
// unchanged (power-loss drill + tamper tests re-verify it).

use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
use rusqlite::{params, Transaction};

/// Appends an audit event INTO a caller-owned transaction. The implementation
/// supplies whatever signing material the chain needs (the production impl below
/// borrows the store's key; the trait imposes no ownership); the persistence layer
/// depends only on this trait, so the append happens atomically with the caller's
/// table write while the store stays ignorant of the key.
///
/// `append_within` MUST NOT commit or roll back `tx` — it only stages its rows so
/// the CALLER's `tx.commit()` makes the table write and the audit append all-or-
/// nothing (and any error here propagates to abort the caller's transaction).
pub trait AuditAppender {
    fn append_within(
        &self,
        tx: &rusqlite::Transaction<'_>,
        event_type: &str,
        payload: &str,
        created_at_ms: i64,
    ) -> rusqlite::Result<()>;
}

/// The production appender: the hash-chained, Ed25519-signed audit append. Holds a
/// borrow of the store's signing key and delegates to [`append_audit_event_tx`] (the
/// persistence-local write mechanics), so this type carries NO dependency on the
/// root `audit_chain` module — the seam a future `kirra-persistence` crate needs.
pub struct ChainedAuditAppender<'k> {
    pub signing_key: Option<&'k ed25519_dalek::SigningKey>,
}

impl AuditAppender for ChainedAuditAppender<'_> {
    fn append_within(
        &self,
        tx: &rusqlite::Transaction<'_>,
        event_type: &str,
        payload: &str,
        created_at_ms: i64,
    ) -> rusqlite::Result<()> {
        append_audit_event_tx(tx, event_type, payload, created_at_ms, self.signing_key)
    }
}

/// Append one event to the hash-chained, signed audit ledger, into a caller-owned
/// transaction (the row + the anchor-head advance commit atomically with the
/// caller's write — the #77 / power-loss invariant).
///
/// Relocated from the root `audit_chain` module (ADR-0035 slice 2b): the write
/// touches only the persistence-owned `audit_log_chain` / `audit_anchor_head`
/// tables and the pure `kirra_audit_hash` hash/canonical primitives, so it belongs
/// in the persistence layer. Byte-for-byte identical to the prior implementation —
/// same v2 record hash, same canonical signing payloads, same key-id derivation.
pub fn append_audit_event_tx(
    tx: &Transaction,
    event_type: &str,
    event_json_payload: &str,
    created_at_ms: i64,
    signing_key: Option<&ed25519_dalek::SigningKey>,
) -> rusqlite::Result<()> {
    // Read previous (record_hash, sequence). Distinguish empty-table
    // (legitimate genesis) from real read errors — a real error must NOT
    // silently fork to genesis (which would hide a corrupted store behind a
    // brand-new chain). Real errors propagate; only an empty table is genesis.
    let prev = tx.query_row(
        "SELECT record_hash_hex, sequence FROM audit_log_chain \
         ORDER BY id DESC LIMIT 1",
        [],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)),
    );
    let (previous_hash, prev_seq) = match prev {
        Ok((h, seq)) => (h, seq.unwrap_or(-1)),
        Err(rusqlite::Error::QueryReturnedNoRows) => ("0".repeat(64), -1),
        Err(e) => return Err(e), // FAIL CLOSED — never fork-to-genesis on read error
    };
    // Genesis -> 0; first v2 row after a v1 tail (prev_seq NULL -> -1) -> 0.
    let sequence: u64 = (prev_seq + 1) as u64;

    let record_hash = kirra_audit_hash::compute_record_hash_v2(
        &previous_hash,
        event_type,
        event_json_payload,
        created_at_ms,
        sequence,
    );

    let signature_b64: Option<String> = signing_key.map(|key| {
        use ed25519_dalek::Signer;
        let payload = kirra_audit_hash::canonical_signing_payload_v2(
            &previous_hash,
            &record_hash,
            event_type,
            created_at_ms,
            sequence,
        );
        let sig = key.sign(payload.as_bytes());
        b64e.encode(sig.to_bytes())
    });

    // Record the content-addressed id of the SIGNING key (#76). The verifier
    // selects the verifying key per row by this id, so rows signed under a prior
    // key still verify after rotation. `key_id` is unsigned metadata: tampering it
    // makes the row verify under the WRONG key and fail (no need to bind it into
    // the existing signed payload, which keeps v1/v2 signatures unchanged).
    let key_id: Option<String> =
        signing_key.map(|key| kirra_audit_hash::verifying_key_id(&key.verifying_key()));

    tx.execute(
        "INSERT INTO audit_log_chain
         (event_type, event_json, previous_hash_hex, record_hash_hex,
          created_at_ms, signature_b64, hash_version, sequence, key_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 2, ?7, ?8)",
        params![
            event_type,
            event_json_payload,
            previous_hash,
            record_hash,
            created_at_ms,
            signature_b64,
            sequence as i64,
            key_id,
        ],
    )?;

    // #77: advance the signed anchor-HEAD high-water mark to this new tail, IN THE
    // SAME TRANSACTION as the row above. The head and the row it points to commit
    // atomically on the same connection, so the head can never be more (or less)
    // durable than the chain tail. #74 INTERACTION: the chain is synchronous=NORMAL
    // and its last rows may be lost on an ungraceful power cut — but because the
    // head update rides the SAME commit as each row, a lost tail row takes its head
    // update with it, leaving head == the recovered tail. No false truncation alarm.
    let head_sig: Option<String> = signing_key.map(|key| {
        use ed25519_dalek::Signer;
        let payload = kirra_audit_hash::canonical_anchor_head_payload(sequence, &record_hash);
        b64e.encode(key.sign(payload.as_bytes()).to_bytes())
    });
    tx.execute(
        "INSERT INTO audit_anchor_head (id, sequence, record_hash_hex, signature_b64, key_id)
         VALUES (1, ?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
             sequence        = excluded.sequence,
             record_hash_hex = excluded.record_hash_hex,
             signature_b64   = excluded.signature_b64,
             key_id          = excluded.key_id",
        params![sequence as i64, record_hash, head_sig, key_id],
    )?;

    Ok(())
}
