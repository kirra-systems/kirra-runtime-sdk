// src/verifier_store/attestation.rs
// attestation domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    pub fn register_attestation_identity(
        &mut self,
        node_id: &str,
        fingerprint_hex: &str,
        source: &str,
        registered_at_ms: u64,
    ) -> Result<()> {
        let tx = Self::audit_tx(&mut self.conn)?; // #685: Immediate — non-forking audit append

        tx.execute(
            "INSERT OR REPLACE INTO attestation_identity_registry
             (node_id, ak_public_fingerprint_hex, registered_at_ms, registration_source)
             VALUES (?1, ?2, ?3, ?4)",
            params![node_id, fingerprint_hex, registered_at_ms as i64, source],
        )?;

        let audit_payload = serde_json::json!({
            "node_id": node_id,
            "ak_public_fingerprint_hex": fingerprint_hex,
            "registration_source": source,
            "registered_at_ms": registered_at_ms,
        });
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "NODE_IDENTITY_REGISTERED",
            &audit_payload.to_string(),
            registered_at_ms as i64,
            self.signing_key.as_ref(),
        )?;

        tx.commit()
    }

    pub fn load_registered_fingerprint(&self, node_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT ak_public_fingerprint_hex FROM attestation_identity_registry
             WHERE node_id = ?1",
        )?;
        match stmt.query_row(params![node_id], |row| row.get::<_, String>(0)) {
            Ok(fp) => Ok(Some(fp)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
