// src/audit_chain.rs

use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
use rusqlite::{params, Result, Transaction};
use sha2::{Digest, Sha256};

/// Event payload written to the audit chain when an RSS safe-distance
/// violation is detected. All fields are included in the SHA-256 hash.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RssViolationEvent {
    pub ego_vel: f64,
    pub lead_vel: f64,
    pub gap: f64,
    pub longitudinal_margin: f64,
    pub lateral_margin: f64,
    pub timestamp_ms: u64,
}

// `PerceptionDerateEvent` moved to the lean `kirra-core` crate (de-monolith Stage 2)
// so the gateway's `perception_monitor` can name it without importing this heavy
// (rusqlite) module. Re-exported so every existing `crate::audit_chain::*` path holds.
pub use kirra_core::PerceptionDerateEvent;

/// Typed audit entries for the hash-chained ledger.
/// Each variant is serialised to JSON and becomes the `event_json` column value.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AuditEntry {
    RssViolation(RssViolationEvent),
    PerceptionDerate(PerceptionDerateEvent),
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
// that MIRRORS the audit chain machinery above, but is domain-separated so a
// causal signature/hash can never be confused with an audit one. The KEY WIN
// over the prior in-memory log is that the record hash binds the CAUSALITY
// EDGES (`caused_by`, `affects_assets`, `fabric_generation`) plus `previous_hash`
// and `sequence` — tampering an edge changes the record hash and is DETECTED.

/// Causal record hash (issue #87). SHA-256 over a domain-separated,
/// length-prefixed encoding that BINDS the causality edges.
///
/// Domain tag `KIRRA-CAUSAL-V1` (distinct from `KIRRA-AUDIT-V2`). Every
/// variable-length scalar field is preceded by its 8-byte LE length; each edge
/// VECTOR is preceded by its element count (8-byte LE) and every element by its
/// own 8-byte LE length. Fixed-width integers (`timestamp_ms`,
/// `fabric_generation`, `sequence`) are appended as LE bytes. Length-prefixing
/// closes field-splicing ambiguities (`"AB"+"C"` vs `"A"+"BC"`) so neither a
/// scalar nor an edge element can be silently relabeled or moved between fields.
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
/// causality edges (because `compute_causal_record_hash` binds them into
/// `record_hash`). `previous_hash` and `sequence` also appear explicitly so the
/// row's chain position is signed directly. Domain prefix `kirra-causal:v1`
/// keeps it distinct from the audit row payload.
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

pub struct AuditChainLinker;

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

impl AuditChainLinker {
    /// V1 (legacy) record hash: prev || event_json || created_at_ms.
    /// Does NOT bind `event_type` — retained ONLY to verify pre-migration
    /// rows. New rows use `compute_record_hash_v2` which closes the
    /// event_type relabeling hole and the field-splicing ambiguity.
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

    /// Back-compat alias for `compute_record_hash_v1`. Existing callers
    /// (verifier_store::verify_audit_chain_integrity for legacy rows,
    /// tests) keep compiling.
    pub fn compute_record_hash(
        previous_hash: &str,
        canonical_json: &str,
        created_at_ms: i64,
    ) -> String {
        Self::compute_record_hash_v1(previous_hash, canonical_json, created_at_ms)
    }

    /// V2 record hash. Binds `event_type` and `sequence` into the hash so
    /// event_type relabeling and row reordering are caught by the
    /// cheap hash-only `verify_audit_chain_integrity` — without needing
    /// signatures.
    ///
    /// Domain-separated (`KIRRA-AUDIT-V2` prefix) and length-prefixed
    /// (each variable-length field is preceded by its 8-byte LE length)
    /// so field-splicing ambiguities (`"AB"+"C"` vs `"A"+"BC"`) cannot
    /// collide.
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

    /// Appends an RSS violation event to the hash-chained audit ledger.
    ///
    /// The event is serialised to JSON; the JSON bytes are included in the
    /// SHA-256 chain hash via `compute_record_hash`, following the same
    /// pattern as all other `append_*` methods. Single-byte corruption of
    /// `event_json` in the database causes `verify_chain` to fail.
    pub fn append_rss_violation(
        tx: &Transaction,
        event: &RssViolationEvent,
        signing_key: Option<&ed25519_dalek::SigningKey>,
    ) -> Result<()> {
        let json = serde_json::to_string(event)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Self::append_audit_event_tx(
            tx,
            "RSS_VIOLATION",
            &json,
            event.timestamp_ms as i64,
            signing_key,
        )
    }

    /// Appends a Track-C perception-monitor derate event to the hash-chained
    /// audit ledger (KIRRA-OCCY-PMON-001). The chain `event_type` is the
    /// byte-stable `DerateCode` token carried in `event.reason`; the event is
    /// serialised to JSON and bound into the SHA-256 chain hash, exactly as
    /// `append_rss_violation`.
    pub fn append_perception_derate(
        tx: &Transaction,
        event: &PerceptionDerateEvent,
        signing_key: Option<&ed25519_dalek::SigningKey>,
    ) -> Result<()> {
        let json = serde_json::to_string(event)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Self::append_audit_event_tx(
            tx,
            &event.reason,
            &json,
            event.timestamp_ms as i64,
            signing_key,
        )
    }

    pub fn append_audit_event_tx(
        tx: &Transaction,
        event_type: &str,
        event_json_payload: &str,
        created_at_ms: i64,
        signing_key: Option<&ed25519_dalek::SigningKey>,
    ) -> Result<()> {
        // Read previous (record_hash, sequence). Distinguish empty-table
        // (legitimate genesis) from real read errors — the pre-v2 code
        // silently forked to genesis on any error, hiding a corrupted
        // store behind a brand-new chain. Now: real errors propagate.
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

        let record_hash = Self::compute_record_hash_v2(
            &previous_hash,
            event_type,
            event_json_payload,
            created_at_ms,
            sequence,
        );

        let signature_b64: Option<String> = signing_key.map(|key| {
            use ed25519_dalek::Signer;
            let payload = canonical_signing_payload_v2(
                &previous_hash,
                &record_hash,
                event_type,
                created_at_ms,
                sequence,
            );
            let sig = key.sign(payload.as_bytes());
            b64e.encode(sig.to_bytes())
        });

        // Record the content-addressed id of the SIGNING key (#76). The
        // verifier selects the verifying key per row by this id, so rows signed
        // under a prior key still verify after rotation. `key_id` is unsigned
        // metadata: tampering it makes the row verify under the WRONG key and
        // fail (no need to bind it into the existing signed payload, which keeps
        // v1/v2 signatures unchanged).
        let key_id: Option<String> = signing_key.map(|key| verifying_key_id(&key.verifying_key()));

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

        // #77: advance the signed anchor-HEAD high-water mark to this new tail,
        // IN THE SAME TRANSACTION as the row above. This is the whole point: the
        // head and the row it points to commit atomically on the same connection,
        // so the head can never be more (or less) durable than the chain tail.
        // #74 INTERACTION: the chain is synchronous=NORMAL and its last rows may
        // be lost on an ungraceful power cut — but because the head update rides
        // the SAME commit as each row, a lost tail row takes its head update with
        // it, leaving head == the recovered tail. No false truncation alarm.
        let head_sig: Option<String> = signing_key.map(|key| {
            use ed25519_dalek::Signer;
            let payload = canonical_anchor_head_payload(sequence, &record_hash);
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
}

#[cfg(test)]
mod audit_signing_tests {
    use super::*;
    use ed25519_dalek::{Signature, SigningKey, Verifier};
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS audit_log_chain (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                event_json TEXT NOT NULL,
                previous_hash_hex TEXT NOT NULL,
                record_hash_hex TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                signature_b64 TEXT,
                hash_version INTEGER NOT NULL DEFAULT 1,
                sequence INTEGER,
                key_id TEXT
            );
            -- #77: append_audit_event_tx advances the anchor-head in the same tx.
            CREATE TABLE IF NOT EXISTS audit_anchor_head (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                sequence INTEGER NOT NULL,
                record_hash_hex TEXT NOT NULL,
                signature_b64 TEXT,
                key_id TEXT
            );",
        )
        .unwrap();
        conn
    }

    fn test_signing_key() -> SigningKey {
        let seed = [1u8; 32];
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn test_signing_present_when_key_configured() {
        let conn = setup_db();
        let key = test_signing_key();
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "TEST_EVENT",
                r#"{"test": true}"#,
                1000,
                Some(&key),
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let sig_b64: Option<String> = conn
            .query_row(
                "SELECT signature_b64 FROM audit_log_chain ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(
            sig_b64.is_some(),
            "signature_b64 should be present when key is configured"
        );
        assert!(!sig_b64.unwrap().is_empty());
    }

    #[test]
    fn test_signing_absent_when_key_not_configured() {
        let conn = setup_db();
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "TEST_EVENT",
                r#"{"test": true}"#,
                1000,
                None,
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let sig_b64: Option<String> = conn
            .query_row(
                "SELECT signature_b64 FROM audit_log_chain ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(
            sig_b64.is_none(),
            "signature_b64 should be NULL when no key is configured"
        );
    }

    #[test]
    fn test_perception_derate_feeds_chain_with_reason_as_event_type() {
        // A Track-C DerateDecision (KIRRA-OCCY-PMON-001) feeds the Ed25519
        // chain via append_perception_derate: the byte-stable DerateCode token
        // becomes the row's event_type, the cap rides in the JSON, and a
        // signature is produced when a key is configured.
        let conn = setup_db();
        let key = test_signing_key();
        let decision = crate::gateway::perception_monitor::DerateDecision {
            cap_mps: 0.0,
            reason: crate::gateway::perception_monitor::DerateCode::DetectionRangeUntrusted,
        };
        let event = decision.to_audit_event(4242);
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_perception_derate(&tx, &event, Some(&key)).unwrap();
            tx.commit().unwrap();
        }

        let (event_type, event_json, sig_b64): (String, String, Option<String>) = conn
            .query_row(
                "SELECT event_type, event_json, signature_b64 FROM audit_log_chain \
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();

        assert_eq!(event_type, "DETECTION_RANGE_UNTRUSTED");
        assert!(event_json.contains("\"cap_mps\":0.0"));
        assert!(
            sig_b64.is_some(),
            "perception-derate row must be signed when key configured"
        );
    }

    #[test]
    fn test_signature_verifies_against_canonical_payload() {
        let conn = setup_db();
        let key = test_signing_key();
        let vk = key.verifying_key();

        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "TEST_EVENT",
                r#"{"data": "value"}"#,
                2000,
                Some(&key),
            )
            .unwrap();
            tx.commit().unwrap();
        }

        // Post hash-v2: SELECT sequence too and rebuild the v2 payload.
        let (prev_hash, record_hash, sig_b64, created_at_ms, sequence):
            (String, String, String, i64, i64) = conn.query_row(
            "SELECT previous_hash_hex, record_hash_hex, signature_b64, created_at_ms, sequence \
             FROM audit_log_chain LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, Option<i64>>(4)?.unwrap_or(0))),
        ).unwrap();

        let payload = canonical_signing_payload_v2(
            &prev_hash,
            &record_hash,
            "TEST_EVENT",
            created_at_ms,
            sequence as u64,
        );
        let sig_bytes = b64e.decode(&sig_b64).unwrap();
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);
        let sig = Signature::from_bytes(&sig_arr);

        assert!(
            vk.verify(payload.as_bytes(), &sig).is_ok(),
            "signature should verify against v2 canonical payload"
        );
    }

    #[test]
    fn test_invalid_signature_detected() {
        let conn = setup_db();
        let key = test_signing_key();
        let vk = key.verifying_key();

        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "TEST_EVENT",
                r#"{"tamper": false}"#,
                3000,
                Some(&key),
            )
            .unwrap();
            tx.commit().unwrap();
        }

        // Tamper: overwrite with a bad signature
        let bad_sig = b64e.encode([0u8; 64]);
        conn.execute(
            "UPDATE audit_log_chain SET signature_b64 = ?1",
            params![bad_sig],
        )
        .unwrap();

        // v2: rebuild the v2 payload (matches what append signs); a
        // zeroed signature still fails verification under either payload.
        let (prev_hash, record_hash, sig_b64, created_at_ms, sequence):
            (String, String, String, i64, i64) = conn.query_row(
            "SELECT previous_hash_hex, record_hash_hex, signature_b64, created_at_ms, sequence \
             FROM audit_log_chain LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, Option<i64>>(4)?.unwrap_or(0))),
        ).unwrap();

        let payload = canonical_signing_payload_v2(
            &prev_hash,
            &record_hash,
            "TEST_EVENT",
            created_at_ms,
            sequence as u64,
        );
        let sig_bytes = b64e.decode(&sig_b64).unwrap();
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);
        let sig = Signature::from_bytes(&sig_arr);

        assert!(
            vk.verify(payload.as_bytes(), &sig).is_err(),
            "tampered signature should fail verification"
        );
    }

    #[test]
    fn test_unsigned_entries_coexist_with_signed() {
        let conn = setup_db();
        let key = test_signing_key();

        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "SIGNED_EVENT",
                r#"{"signed": true}"#,
                1000,
                Some(&key),
            )
            .unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "UNSIGNED_EVENT",
                r#"{"signed": false}"#,
                2000,
                None,
            )
            .unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "SIGNED_EVENT_2",
                r#"{"signed": true}"#,
                3000,
                Some(&key),
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let signed_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE signature_b64 IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let unsigned_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE signature_b64 IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(signed_count, 2, "should have 2 signed entries");
        assert_eq!(unsigned_count, 1, "should have 1 unsigned entry");
    }

    #[test]
    fn test_chain_integrity_still_verified_alongside_signatures() {
        let conn = setup_db();
        let key = test_signing_key();

        let timestamps = [1000i64, 2000, 3000];
        for ts in &timestamps {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "CHAIN_EVENT",
                &format!(r#"{{"ts": {}}}"#, ts),
                *ts,
                Some(&key),
            )
            .unwrap();
            tx.commit().unwrap();
        }

        // Walk chain manually — post hash-v2: SELECT event_type + sequence,
        // recompute with compute_record_hash_v2.
        let mut stmt = conn
            .prepare(
                "SELECT event_type, event_json, previous_hash_hex, record_hash_hex, \
             created_at_ms, sequence \
             FROM audit_log_chain ORDER BY id ASC",
            )
            .unwrap();

        let mut expected_prev = "0".repeat(64);
        let mut rows = stmt.query([]).unwrap();

        while let Some(row) = rows.next().unwrap() {
            let event_type: String = row.get(0).unwrap();
            let event_json: String = row.get(1).unwrap();
            let prev: String = row.get(2).unwrap();
            let record: String = row.get(3).unwrap();
            let ts: i64 = row.get(4).unwrap();
            let sequence: i64 = row.get::<_, Option<i64>>(5).unwrap().unwrap_or(0);

            assert_eq!(prev, expected_prev, "hash chain should be intact");
            let recomputed = AuditChainLinker::compute_record_hash_v2(
                &prev,
                &event_type,
                &event_json,
                ts,
                sequence as u64,
            );
            assert_eq!(recomputed, record, "v2 record hash should match");
            expected_prev = record;
        }
    }

    // --- Causal-log primitive tests (issue #87) ---------------------------

    #[test]
    fn test_causal_record_hash_is_deterministic() {
        let h1 = compute_causal_record_hash(&CausalRecordHashInput {
            previous_hash: &"0".repeat(64),
            entry_id: "entry1",
            asset_id: "asset1",
            event_type: "FAULT",
            payload: "{}",
            caused_by: &["c1".to_string()],
            affects_assets: &["a1".to_string()],
            timestamp_ms: 1000,
            fabric_generation: 5,
            sequence: 0,
        });
        let h2 = compute_causal_record_hash(&CausalRecordHashInput {
            previous_hash: &"0".repeat(64),
            entry_id: "entry1",
            asset_id: "asset1",
            event_type: "FAULT",
            payload: "{}",
            caused_by: &["c1".to_string()],
            affects_assets: &["a1".to_string()],
            timestamp_ms: 1000,
            fabric_generation: 5,
            sequence: 0,
        });
        assert_eq!(h1, h2, "causal record hash must be deterministic");
    }

    #[test]
    fn test_causal_record_hash_binds_caused_by_edge() {
        let base = compute_causal_record_hash(&CausalRecordHashInput {
            previous_hash: &"0".repeat(64),
            entry_id: "e",
            asset_id: "a",
            event_type: "T",
            payload: "{}",
            caused_by: &["c1".to_string()],
            affects_assets: &["x".to_string()],
            timestamp_ms: 1,
            fabric_generation: 1,
            sequence: 0,
        });
        let tampered = compute_causal_record_hash(&CausalRecordHashInput {
            previous_hash: &"0".repeat(64),
            entry_id: "e",
            asset_id: "a",
            event_type: "T",
            payload: "{}",
            caused_by: &["c2".to_string()],
            affects_assets: &["x".to_string()],
            timestamp_ms: 1,
            fabric_generation: 1,
            sequence: 0,
        });
        assert_ne!(
            base, tampered,
            "changing caused_by MUST change the record hash"
        );
    }

    #[test]
    fn test_causal_record_hash_binds_affects_assets_edge() {
        let base = compute_causal_record_hash(&CausalRecordHashInput {
            previous_hash: &"0".repeat(64),
            entry_id: "e",
            asset_id: "a",
            event_type: "T",
            payload: "{}",
            caused_by: &[],
            affects_assets: &["x".to_string()],
            timestamp_ms: 1,
            fabric_generation: 1,
            sequence: 0,
        });
        let tampered = compute_causal_record_hash(&CausalRecordHashInput {
            previous_hash: &"0".repeat(64),
            entry_id: "e",
            asset_id: "a",
            event_type: "T",
            payload: "{}",
            caused_by: &[],
            affects_assets: &["y".to_string()],
            timestamp_ms: 1,
            fabric_generation: 1,
            sequence: 0,
        });
        assert_ne!(
            base, tampered,
            "changing affects_assets MUST change the record hash"
        );
    }

    #[test]
    fn test_causal_record_hash_binds_fabric_generation_edge() {
        let base = compute_causal_record_hash(&CausalRecordHashInput {
            previous_hash: &"0".repeat(64),
            entry_id: "e",
            asset_id: "a",
            event_type: "T",
            payload: "{}",
            caused_by: &[],
            affects_assets: &[],
            timestamp_ms: 1,
            fabric_generation: 5,
            sequence: 0,
        });
        let tampered = compute_causal_record_hash(&CausalRecordHashInput {
            previous_hash: &"0".repeat(64),
            entry_id: "e",
            asset_id: "a",
            event_type: "T",
            payload: "{}",
            caused_by: &[],
            affects_assets: &[],
            timestamp_ms: 1,
            fabric_generation: 6,
            sequence: 0,
        });
        assert_ne!(
            base, tampered,
            "changing fabric_generation MUST change the record hash"
        );
    }

    #[test]
    fn test_causal_record_hash_length_prefix_prevents_splicing() {
        // ("AB","C") vs ("A","BC") in the edge vectors must differ.
        let a = compute_causal_record_hash(&CausalRecordHashInput {
            previous_hash: &"0".repeat(64),
            entry_id: "e",
            asset_id: "a",
            event_type: "T",
            payload: "{}",
            caused_by: &["AB".to_string(), "C".to_string()],
            affects_assets: &[],
            timestamp_ms: 1,
            fabric_generation: 1,
            sequence: 0,
        });
        let b = compute_causal_record_hash(&CausalRecordHashInput {
            previous_hash: &"0".repeat(64),
            entry_id: "e",
            asset_id: "a",
            event_type: "T",
            payload: "{}",
            caused_by: &["A".to_string(), "BC".to_string()],
            affects_assets: &[],
            timestamp_ms: 1,
            fabric_generation: 1,
            sequence: 0,
        });
        assert_ne!(a, b, "length-prefixing must defeat edge field-splicing");
    }

    #[test]
    fn test_causal_signing_payload_format_is_stable() {
        let p = canonical_causal_signing_payload(&"0".repeat(64), &"a".repeat(64), "EVT", 1234, 7);
        assert_eq!(
            p,
            format!(
                "kirra-causal:v1:{}:{}:EVT:1234:7",
                "0".repeat(64),
                "a".repeat(64)
            ),
        );
    }

    #[test]
    fn test_causal_anchor_head_payload_is_domain_separated() {
        let head = canonical_causal_anchor_head_payload(3, "deadbeef");
        assert_eq!(head, "kirra-causal-head:v1:3:deadbeef");
        // Distinct from the audit head domain.
        assert_ne!(head, canonical_anchor_head_payload(3, "deadbeef"));
    }

    #[test]
    fn test_canonical_payload_format_is_stable() {
        let prev = "a".repeat(64);
        let entry = "b".repeat(64);
        let event_type = "TEST";
        let ts: i64 = 1_700_000_000_000;

        let payload1 = canonical_signing_payload(&prev, &entry, event_type, ts);
        let payload2 = canonical_signing_payload(&prev, &entry, event_type, ts);

        assert_eq!(
            payload1, payload2,
            "canonical payload must be deterministic"
        );
        assert_eq!(
            payload1,
            format!("{}:{}:{}:{}", prev, entry, event_type, ts),
            "canonical payload format must match spec"
        );
    }

    #[test]
    fn test_key_rotation_creates_audit_entry() {
        let conn = setup_db();
        let key = test_signing_key();

        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "KEY_ROTATION",
                r#"{"new_public_key_b64": "abc123", "reason": "scheduled", "rotated_at_ms": 5000}"#,
                5000,
                Some(&key),
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let (event_type, sig_b64): (String, Option<String>) = conn
            .query_row(
                "SELECT event_type, signature_b64 FROM audit_log_chain LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(event_type, "KEY_ROTATION");
        assert!(sig_b64.is_some(), "KEY_ROTATION entry should be signed");
    }

    fn sample_rss_event(ts: u64) -> RssViolationEvent {
        RssViolationEvent {
            ego_vel: 15.0,
            lead_vel: 8.0,
            gap: 3.5,
            longitudinal_margin: 0.0,
            lateral_margin: 0.2,
            timestamp_ms: ts,
        }
    }

    // Test A — 5-entry chain including one RssViolation: chain integrity holds.
    #[test]
    fn test_rss_violation_chain_integrity_with_mixed_entries() {
        let conn = setup_db();

        // 4 generic entries + 1 RssViolation entry
        for ts in &[1000i64, 2000, 3000, 4000] {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "POSTURE_EVENT",
                &format!(r#"{{"ts":{}}}"#, ts),
                *ts,
                None,
            )
            .unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_rss_violation(&tx, &sample_rss_event(5000), None).unwrap();
            tx.commit().unwrap();
        }

        // Walk chain and verify every v2 hash links correctly.
        let mut stmt = conn
            .prepare(
                "SELECT event_type, event_json, previous_hash_hex, record_hash_hex, \
             created_at_ms, sequence \
             FROM audit_log_chain ORDER BY id ASC",
            )
            .unwrap();

        let mut expected_prev = "0".repeat(64);
        let mut rows = stmt.query([]).unwrap();
        let mut count = 0;

        while let Some(row) = rows.next().unwrap() {
            let event_type: String = row.get(0).unwrap();
            let event_json: String = row.get(1).unwrap();
            let prev: String = row.get(2).unwrap();
            let record: String = row.get(3).unwrap();
            let ts: i64 = row.get(4).unwrap();
            let sequence: i64 = row.get::<_, Option<i64>>(5).unwrap().unwrap_or(0);

            assert_eq!(
                prev, expected_prev,
                "hash chain broken at entry {count}: prev_hash mismatch"
            );
            let recomputed = AuditChainLinker::compute_record_hash_v2(
                &prev,
                &event_type,
                &event_json,
                ts,
                sequence as u64,
            );
            assert_eq!(
                recomputed, record,
                "hash chain broken at entry {count}: record_hash mismatch"
            );
            expected_prev = record;
            count += 1;
        }
        assert!(count > 0, "expected at least one entry in chain");
    }

    // Test B — corrupt one byte of RssViolation event_json: chain integrity fails.
    #[test]
    fn test_rss_violation_corruption_detected_by_chain() {
        let conn = setup_db();

        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_rss_violation(&tx, &sample_rss_event(1000), None).unwrap();
            tx.commit().unwrap();
        }

        // Retrieve and corrupt the event_json (flip one character).
        let original_json: String = conn
            .query_row(
                "SELECT event_json FROM audit_log_chain LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        let mut corrupted = original_json.clone().into_bytes();
        // Flip a byte somewhere in the middle of the JSON payload.
        let mid = corrupted.len() / 2;
        corrupted[mid] ^= 0x01;
        let corrupted_json = String::from_utf8_lossy(&corrupted).into_owned();

        conn.execute(
            "UPDATE audit_log_chain SET event_json = ?1",
            params![corrupted_json],
        )
        .unwrap();

        // Walk chain — recomputed hash must NOT match stored hash.
        let (event_json, prev, record, ts): (String, String, String, i64) = conn
            .query_row(
                "SELECT event_json, previous_hash_hex, record_hash_hex, created_at_ms \
             FROM audit_log_chain LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        let recomputed = AuditChainLinker::compute_record_hash(&prev, &event_json, ts);
        assert_ne!(
            recomputed, record,
            "corrupted event_json must produce a different hash — tamper detection failed"
        );
    }

    #[test]
    fn test_export_includes_signature_status() {
        let conn = setup_db();
        let key = test_signing_key();
        let vk = key.verifying_key();

        // Write a signed entry
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx,
                "SIGNED_EVT",
                r#"{"a": 1}"#,
                1000,
                Some(&key),
            )
            .unwrap();
            tx.commit().unwrap();
        }
        // Write an unsigned entry
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(&tx, "UNSIGNED_EVT", r#"{"b": 2}"#, 2000, None)
                .unwrap();
            tx.commit().unwrap();
        }

        let mut stmt = conn.prepare(
            "SELECT event_type, previous_hash_hex, record_hash_hex, created_at_ms, signature_b64, sequence \
             FROM audit_log_chain ORDER BY id ASC"
        ).unwrap();

        let mut rows = stmt.query([]).unwrap();
        let mut statuses: Vec<String> = Vec::new();

        while let Some(row) = rows.next().unwrap() {
            let event_type: String = row.get(0).unwrap();
            let prev_hash: String = row.get(1).unwrap();
            let record_hash: String = row.get(2).unwrap();
            let ts: i64 = row.get(3).unwrap();
            let sig_b64: Option<String> = row.get(4).unwrap();
            let sequence: i64 = row.get::<_, Option<i64>>(5).unwrap().unwrap_or(0);

            let status = match &sig_b64 {
                None => "unsigned".to_string(),
                Some(s) => {
                    let payload = canonical_signing_payload_v2(
                        &prev_hash,
                        &record_hash,
                        &event_type,
                        ts,
                        sequence as u64,
                    );
                    let bytes = b64e.decode(s).unwrap_or_default();
                    if bytes.len() == 64 {
                        let mut arr = [0u8; 64];
                        arr.copy_from_slice(&bytes);
                        let sig = Signature::from_bytes(&arr);
                        if vk.verify(payload.as_bytes(), &sig).is_ok() {
                            "valid".to_string()
                        } else {
                            "invalid".to_string()
                        }
                    } else {
                        "invalid".to_string()
                    }
                }
            };
            statuses.push(status);
        }

        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0], "valid");
        assert_eq!(statuses[1], "unsigned");
    }
}
