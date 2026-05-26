// src/audit_chain.rs

use rusqlite::{params, Transaction, Result};
use sha2::{Sha256, Digest};
use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};

/// Returns the canonical signing payload string.
/// Format: "{prev_hash}:{entry_hash}:{event_type}:{timestamp_ms}"
pub fn canonical_signing_payload(
    prev_hash: &str,
    entry_hash: &str,
    event_type: &str,
    timestamp_ms: i64,
) -> String {
    format!("{}:{}:{}:{}", prev_hash, entry_hash, event_type, timestamp_ms)
}

pub struct AuditChainLinker;

impl AuditChainLinker {
    pub fn compute_record_hash(
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

    pub fn append_audit_event_tx(
        tx: &Transaction,
        event_type: &str,
        event_json_payload: &str,
        created_at_ms: i64,
        signing_key: Option<&ed25519_dalek::SigningKey>,
    ) -> Result<()> {
        let previous_hash: String = tx
            .query_row(
                "SELECT record_hash_hex FROM audit_log_chain ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| "0".repeat(64));

        let record_hash = Self::compute_record_hash(&previous_hash, event_json_payload, created_at_ms);

        let signature_b64: Option<String> = signing_key.map(|key| {
            use ed25519_dalek::Signer;
            let payload = canonical_signing_payload(&previous_hash, &record_hash, event_type, created_at_ms);
            let sig = key.sign(payload.as_bytes());
            b64e.encode(sig.to_bytes())
        });

        tx.execute(
            "INSERT INTO audit_log_chain
             (event_type, event_json, previous_hash_hex, record_hash_hex, created_at_ms, signature_b64)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![event_type, event_json_payload, previous_hash, record_hash, created_at_ms, signature_b64],
        )?;

        Ok(())
    }
}

#[cfg(test)]
mod audit_signing_tests {
    use super::*;
    use rusqlite::Connection;
    use ed25519_dalek::{SigningKey, VerifyingKey, Signer, Verifier, Signature};

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
                signature_b64 TEXT
            );"
        ).unwrap();
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
                &tx, "TEST_EVENT", r#"{"test": true}"#, 1000, Some(&key),
            ).unwrap();
            tx.commit().unwrap();
        }

        let sig_b64: Option<String> = conn.query_row(
            "SELECT signature_b64 FROM audit_log_chain ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        ).unwrap();

        assert!(sig_b64.is_some(), "signature_b64 should be present when key is configured");
        assert!(!sig_b64.unwrap().is_empty());
    }

    #[test]
    fn test_signing_absent_when_key_not_configured() {
        let conn = setup_db();
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx, "TEST_EVENT", r#"{"test": true}"#, 1000, None,
            ).unwrap();
            tx.commit().unwrap();
        }

        let sig_b64: Option<String> = conn.query_row(
            "SELECT signature_b64 FROM audit_log_chain ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        ).unwrap();

        assert!(sig_b64.is_none(), "signature_b64 should be NULL when no key is configured");
    }

    #[test]
    fn test_signature_verifies_against_canonical_payload() {
        let conn = setup_db();
        let key = test_signing_key();
        let vk = key.verifying_key();

        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx, "TEST_EVENT", r#"{"data": "value"}"#, 2000, Some(&key),
            ).unwrap();
            tx.commit().unwrap();
        }

        let (prev_hash, record_hash, sig_b64, created_at_ms): (String, String, String, i64) = conn.query_row(
            "SELECT previous_hash_hex, record_hash_hex, signature_b64, created_at_ms FROM audit_log_chain LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).unwrap();

        let payload = canonical_signing_payload(&prev_hash, &record_hash, "TEST_EVENT", created_at_ms);
        let sig_bytes = b64e.decode(&sig_b64).unwrap();
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);
        let sig = Signature::from_bytes(&sig_arr);

        assert!(vk.verify(payload.as_bytes(), &sig).is_ok(), "signature should verify against canonical payload");
    }

    #[test]
    fn test_invalid_signature_detected() {
        let conn = setup_db();
        let key = test_signing_key();
        let vk = key.verifying_key();

        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx, "TEST_EVENT", r#"{"tamper": false}"#, 3000, Some(&key),
            ).unwrap();
            tx.commit().unwrap();
        }

        // Tamper: overwrite with a bad signature
        let bad_sig = b64e.encode([0u8; 64]);
        conn.execute(
            "UPDATE audit_log_chain SET signature_b64 = ?1",
            params![bad_sig],
        ).unwrap();

        let (prev_hash, record_hash, sig_b64, created_at_ms): (String, String, String, i64) = conn.query_row(
            "SELECT previous_hash_hex, record_hash_hex, signature_b64, created_at_ms FROM audit_log_chain LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).unwrap();

        let payload = canonical_signing_payload(&prev_hash, &record_hash, "TEST_EVENT", created_at_ms);
        let sig_bytes = b64e.decode(&sig_b64).unwrap();
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);
        let sig = Signature::from_bytes(&sig_arr);

        assert!(vk.verify(payload.as_bytes(), &sig).is_err(), "tampered signature should fail verification");
    }

    #[test]
    fn test_unsigned_entries_coexist_with_signed() {
        let conn = setup_db();
        let key = test_signing_key();

        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx, "SIGNED_EVENT", r#"{"signed": true}"#, 1000, Some(&key),
            ).unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx, "UNSIGNED_EVENT", r#"{"signed": false}"#, 2000, None,
            ).unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx, "SIGNED_EVENT_2", r#"{"signed": true}"#, 3000, Some(&key),
            ).unwrap();
            tx.commit().unwrap();
        }

        let signed_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE signature_b64 IS NOT NULL",
            [],
            |row| row.get(0),
        ).unwrap();
        let unsigned_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE signature_b64 IS NULL",
            [],
            |row| row.get(0),
        ).unwrap();

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
                &tx, "CHAIN_EVENT", &format!(r#"{{"ts": {}}}"#, ts), *ts, Some(&key),
            ).unwrap();
            tx.commit().unwrap();
        }

        // Walk chain manually
        let mut stmt = conn.prepare(
            "SELECT event_json, previous_hash_hex, record_hash_hex, created_at_ms FROM audit_log_chain ORDER BY id ASC"
        ).unwrap();

        let mut expected_prev = "0".repeat(64);
        let mut rows = stmt.query([]).unwrap();

        while let Some(row) = rows.next().unwrap() {
            let event_json: String = row.get(0).unwrap();
            let prev: String = row.get(1).unwrap();
            let record: String = row.get(2).unwrap();
            let ts: i64 = row.get(3).unwrap();

            assert_eq!(prev, expected_prev, "hash chain should be intact");
            let recomputed = AuditChainLinker::compute_record_hash(&prev, &event_json, ts);
            assert_eq!(recomputed, record, "record hash should match");
            expected_prev = record;
        }
    }

    #[test]
    fn test_canonical_payload_format_is_stable() {
        let prev = "a".repeat(64);
        let entry = "b".repeat(64);
        let event_type = "TEST";
        let ts: i64 = 1_700_000_000_000;

        let payload1 = canonical_signing_payload(&prev, &entry, event_type, ts);
        let payload2 = canonical_signing_payload(&prev, &entry, event_type, ts);

        assert_eq!(payload1, payload2, "canonical payload must be deterministic");
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
            ).unwrap();
            tx.commit().unwrap();
        }

        let (event_type, sig_b64): (String, Option<String>) = conn.query_row(
            "SELECT event_type, signature_b64 FROM audit_log_chain LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();

        assert_eq!(event_type, "KEY_ROTATION");
        assert!(sig_b64.is_some(), "KEY_ROTATION entry should be signed");
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
                &tx, "SIGNED_EVT", r#"{"a": 1}"#, 1000, Some(&key),
            ).unwrap();
            tx.commit().unwrap();
        }
        // Write an unsigned entry
        {
            let tx = conn.unchecked_transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(
                &tx, "UNSIGNED_EVT", r#"{"b": 2}"#, 2000, None,
            ).unwrap();
            tx.commit().unwrap();
        }

        let mut stmt = conn.prepare(
            "SELECT event_type, previous_hash_hex, record_hash_hex, created_at_ms, signature_b64 FROM audit_log_chain ORDER BY id ASC"
        ).unwrap();

        let mut rows = stmt.query([]).unwrap();
        let mut statuses: Vec<String> = Vec::new();

        while let Some(row) = rows.next().unwrap() {
            let event_type: String = row.get(0).unwrap();
            let prev_hash: String = row.get(1).unwrap();
            let record_hash: String = row.get(2).unwrap();
            let ts: i64 = row.get(3).unwrap();
            let sig_b64: Option<String> = row.get(4).unwrap();

            let status = match &sig_b64 {
                None => "unsigned".to_string(),
                Some(s) => {
                    let payload = canonical_signing_payload(&prev_hash, &record_hash, &event_type, ts);
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
