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
        // ADR-0035 Addendum A, Stage 2.5 step 1: the audit append goes through the
        // injected `AuditAppender` seam (into this tx), not a direct
        // `AuditChainLinker` + signing-key call — the row + append stay atomic on
        // `tx.commit()`. `ChainedAuditAppender` delegates to the same appender, so
        // the audit-chain bytes are byte-identical to the prior call.
        ChainedAuditAppender {
            signing_key: self.signing_key.as_ref(),
        }
        .append_within(
            &tx,
            "NODE_IDENTITY_REGISTERED",
            &audit_payload.to_string(),
            registered_at_ms as i64,
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

#[cfg(test)]
mod attestation_appender_seam_tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    /// ADR-0035 Addendum A: identity registration now appends its
    /// `NODE_IDENTITY_REGISTERED` audit event through the injected `AuditAppender`
    /// seam. Behaviour-preserving: the row persists AND the resulting signed chain
    /// fully verifies (byte-identical to the prior direct `AuditChainLinker` call).
    #[test]
    fn identity_registration_through_the_seam_produces_a_verifiable_chain() {
        let mut store = VerifierStore::new(":memory:").expect("store");
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        store.set_signing_key(sk.clone());

        store
            .register_attestation_identity("node-1", "fp-abc", "self-report", 1_000)
            .unwrap();

        assert_eq!(
            store.load_registered_fingerprint("node-1").unwrap(),
            Some("fp-abc".to_string())
        );
        assert!(
            store
                .verify_audit_chain_full(Some(&sk.verifying_key()))
                .unwrap()
                .verified(),
            "the chain built through the injected appender must fully verify"
        );
    }
}
