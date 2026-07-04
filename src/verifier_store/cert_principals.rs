// src/verifier_store/cert_principals.rs
// cert_principals domain (WS-1 · G7 · Track 1.2) — mTLS client-certificate principals.
//
// A client cert (already CA-verified by rustls) is pinned to a principal by the
// SHA-256 hex of its leaf DER. Resolution is by fingerprint. Mirrors the
// `api_principals` (token) module — a cert is just another least-privilege
// sub-credential on top of the KIRRA_ADMIN_TOKEN root.

use super::*;

impl VerifierStore {
    /// Register (or rotate) a cert principal. Re-registration overwrites the
    /// fingerprint + role and CLEARS any prior revocation. `cert_sha256` is the
    /// SHA-256 hex of the client cert's leaf DER.
    pub fn register_cert_principal(
        &mut self,
        principal_id: &str,
        cert_sha256: &str,
        role: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cert_principals
                 (principal_id, cert_sha256, role, created_at_ms, revoked_at_ms)
             VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(principal_id) DO UPDATE SET
                 cert_sha256   = excluded.cert_sha256,
                 role          = excluded.role,
                 created_at_ms = excluded.created_at_ms,
                 revoked_at_ms = NULL",
            params![principal_id, cert_sha256, role, now_ms as i64],
        )?;
        Ok(())
    }

    /// Revoke a cert principal. Returns `true` if an ACTIVE principal was revoked,
    /// `false` if absent or already revoked.
    pub fn revoke_cert_principal(&mut self, principal_id: &str, now_ms: u64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE cert_principals SET revoked_at_ms = ?2
             WHERE principal_id = ?1 AND revoked_at_ms IS NULL",
            params![principal_id, now_ms as i64],
        )?;
        Ok(n > 0)
    }

    /// Resolve a cert principal by the SHA-256 hex of the presented leaf cert.
    /// Returns the record (active OR revoked — the caller fail-closes on revoked),
    /// or `None` if no principal holds that fingerprint. Lookup is by fingerprint only.
    pub fn load_cert_principal_by_fingerprint(
        &self,
        cert_sha256: &str,
    ) -> Result<Option<ApiPrincipalRecord>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT principal_id, role, created_at_ms, revoked_at_ms
                 FROM cert_principals WHERE cert_sha256 = ?1",
                params![cert_sha256],
                Self::map_api_principal_row_cert,
            )
            .optional()
    }

    /// Read-only listing of every registered cert principal. Never returns the
    /// fingerprint — the handler exposes only id / role / status.
    pub fn load_cert_principals(&self) -> Result<Vec<ApiPrincipalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms
             FROM cert_principals ORDER BY principal_id",
        )?;
        let rows = stmt.query_map([], Self::map_api_principal_row_cert)?;
        rows.collect()
    }

    fn map_api_principal_row_cert(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiPrincipalRecord> {
        Ok(ApiPrincipalRecord {
            principal_id: row.get(0)?,
            role: row.get(1)?,
            created_at_ms: row.get::<_, i64>(2)? as u64,
            revoked_at_ms: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::verifier_store::VerifierStore;

    fn store() -> VerifierStore {
        VerifierStore::new(":memory:").expect("in-memory store")
    }

    #[test]
    fn register_then_resolve_by_fingerprint() {
        let mut s = store();
        s.register_cert_principal("svc-a", "fp-a", "integrator", 1_000).unwrap();
        let rec = s.load_cert_principal_by_fingerprint("fp-a").unwrap().expect("present");
        assert_eq!(rec.principal_id, "svc-a");
        assert_eq!(rec.role, "integrator");
        assert!(rec.is_active());
        assert!(s.load_cert_principal_by_fingerprint("nope").unwrap().is_none());
    }

    #[test]
    fn rotation_overwrites_fingerprint_and_clears_revocation() {
        let mut s = store();
        s.register_cert_principal("svc-a", "fp-old", "integrator", 1_000).unwrap();
        assert!(s.revoke_cert_principal("svc-a", 2_000).unwrap());
        assert!(s
            .load_cert_principal_by_fingerprint("fp-old")
            .unwrap()
            .unwrap()
            .revoked_at_ms
            .is_some());
        // Re-register rotates the pinned cert and reactivates.
        s.register_cert_principal("svc-a", "fp-new", "auditor", 3_000).unwrap();
        assert!(
            s.load_cert_principal_by_fingerprint("fp-old").unwrap().is_none(),
            "the rotated-out fingerprint no longer resolves"
        );
        let rec = s.load_cert_principal_by_fingerprint("fp-new").unwrap().unwrap();
        assert_eq!(rec.role, "auditor");
        assert!(rec.is_active());
    }

    #[test]
    fn revoke_is_idempotent_and_reports_transition() {
        let mut s = store();
        s.register_cert_principal("svc-a", "fp", "operator", 1_000).unwrap();
        assert!(s.revoke_cert_principal("svc-a", 2_000).unwrap(), "first revoke transitions");
        assert!(!s.revoke_cert_principal("svc-a", 3_000).unwrap(), "second revoke is a no-op");
        assert!(!s.revoke_cert_principal("absent", 3_000).unwrap(), "absent principal → false");
    }

    #[test]
    fn same_fingerprint_on_a_new_principal_is_a_unique_conflict() {
        // The UNIQUE(cert_sha256) column means one cert pins to at most one
        // principal — pinning the same fingerprint under a DIFFERENT id errors
        // (the handler maps this to 409). `ON CONFLICT(principal_id)` only rotates
        // the SAME id, so it does not absorb this case.
        let mut s = store();
        s.register_cert_principal("svc-a", "shared-fp", "operator", 1_000).unwrap();
        let err = s.register_cert_principal("svc-b", "shared-fp", "operator", 1_000);
        assert!(err.is_err(), "a second principal on the same fingerprint must conflict");
        // Re-pinning the SAME principal with the same fp is fine (idempotent rotate).
        assert!(s.register_cert_principal("svc-a", "shared-fp", "auditor", 2_000).is_ok());
    }

    #[test]
    fn distinct_from_token_principals() {
        // A cert principal and a token principal are separate credentials, even with
        // the same principal_id string — different tables, resolved by different keys.
        let mut s = store();
        s.register_api_principal("svc-a", "tokhash", "admin", 1_000).unwrap();
        s.register_cert_principal("svc-a", "certfp", "auditor", 1_000).unwrap();
        assert_eq!(s.load_api_principal_by_token_hash("tokhash").unwrap().unwrap().role, "admin");
        assert_eq!(s.load_cert_principal_by_fingerprint("certfp").unwrap().unwrap().role, "auditor");
        // Cross-lookups miss.
        assert!(s.load_cert_principal_by_fingerprint("tokhash").unwrap().is_none());
        assert!(s.load_api_principal_by_token_hash("certfp").unwrap().is_none());
    }
}
