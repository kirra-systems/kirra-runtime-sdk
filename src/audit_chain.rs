// src/audit_chain.rs

use rusqlite::{Result, Transaction};

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

// The PURE hash/canonical-payload primitives + `verifying_key_id` +
// `CausalRecordHashInput` were extracted to the lean `kirra-audit-hash` crate
// (ADR-0035 — kirra-persistence enabling slice 2) so the persistence layer can
// compute/verify hashes WITHOUT this rusqlite-backed module. Re-exported here so
// every existing `crate::audit_chain::<fn>` path (service bin, store, tests)
// resolves unchanged. Byte-identical: the encoders moved verbatim (they ARE the
// on-disk audit/causal-chain format — same domain tags, length-prefixing, LE order).
pub use kirra_audit_hash::{
    canonical_anchor_head_payload, canonical_causal_anchor_head_payload,
    canonical_causal_signing_payload, canonical_signing_payload, canonical_signing_payload_v2,
    compute_causal_record_hash, verifying_key_id, CausalRecordHashInput,
};

/// The audit-chain append machinery — the STATEFUL half: it writes rows and
/// advances the signed anchor-head into a caller-owned rusqlite transaction. The
/// PURE hash computations it uses now live in `kirra-audit-hash`; the
/// `compute_record_hash*` associated functions below DELEGATE to them so existing
/// `AuditChainLinker::compute_record_hash_v2` callers are unchanged.
pub struct AuditChainLinker;

impl AuditChainLinker {
    /// V1 (legacy) record hash — delegates to [`kirra_audit_hash::compute_record_hash_v1`].
    /// Retained ONLY to verify pre-migration rows.
    pub fn compute_record_hash_v1(
        previous_hash: &str,
        canonical_json: &str,
        created_at_ms: i64,
    ) -> String {
        kirra_audit_hash::compute_record_hash_v1(previous_hash, canonical_json, created_at_ms)
    }

    /// Back-compat alias for `compute_record_hash_v1`.
    pub fn compute_record_hash(
        previous_hash: &str,
        canonical_json: &str,
        created_at_ms: i64,
    ) -> String {
        kirra_audit_hash::compute_record_hash(previous_hash, canonical_json, created_at_ms)
    }

    /// V2 record hash — delegates to [`kirra_audit_hash::compute_record_hash_v2`]
    /// (binds `event_type` + `sequence`, domain-separated, length-prefixed).
    pub fn compute_record_hash_v2(
        previous_hash: &str,
        event_type: &str,
        event_json: &str,
        created_at_ms: i64,
        sequence: u64,
    ) -> String {
        kirra_audit_hash::compute_record_hash_v2(
            previous_hash,
            event_type,
            event_json,
            created_at_ms,
            sequence,
        )
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

    /// Append one event to the hash-chained, signed audit ledger, into a
    /// caller-owned transaction.
    ///
    /// ADR-0035 slice 2b: the write mechanics were relocated to the persistence
    /// layer ([`crate::verifier_store::append_audit_event_tx`]) — the write touches
    /// only the persistence-owned `audit_log_chain` / `audit_anchor_head` tables and
    /// the pure `kirra_audit_hash` primitives. This associated function DELEGATES to
    /// it so `AuditChainLinker::append_audit_event_tx` callers (the typed wrappers
    /// above, tests, external callers) are unchanged and the chain bytes identical.
    pub fn append_audit_event_tx(
        tx: &Transaction,
        event_type: &str,
        event_json_payload: &str,
        created_at_ms: i64,
        signing_key: Option<&ed25519_dalek::SigningKey>,
    ) -> Result<()> {
        crate::verifier_store::append_audit_event_tx(
            tx,
            event_type,
            event_json_payload,
            created_at_ms,
            signing_key,
        )
    }
}

#[cfg(test)]
mod audit_signing_tests {
    use super::*;
    // `params!` + base64 encoding are used only by these tests now that the write
    // mechanics moved to `verifier_store` (slice 2b); import them locally so the
    // production module carries no unused import.
    use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
    use ed25519_dalek::{Signature, SigningKey, Verifier};
    use rusqlite::{params, Connection};

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
