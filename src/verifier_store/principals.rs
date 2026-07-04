// src/verifier_store/principals.rs
// api_principals domain (WS-1 · G7) — per-principal scoped bearer tokens.
//
// Only the SHA-256 hex of a token is ever stored; resolution is by hash, never
// plaintext. Mirrors the `operators` registry shape.

use super::*;

impl VerifierStore {
    /// Register (or rotate) an API principal. Re-registration overwrites the token
    /// hash + role and CLEARS any prior revocation (a fresh token for a principal
    /// is an active principal). `token_sha256` is the SHA-256 hex of the bearer
    /// token — the plaintext never reaches the store.
    pub fn register_api_principal(
        &mut self,
        principal_id: &str,
        token_sha256: &str,
        role: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO api_principals
                 (principal_id, token_sha256, role, created_at_ms, revoked_at_ms)
             VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(principal_id) DO UPDATE SET
                 token_sha256  = excluded.token_sha256,
                 role          = excluded.role,
                 created_at_ms = excluded.created_at_ms,
                 revoked_at_ms = NULL",
            params![principal_id, token_sha256, role, now_ms as i64],
        )?;
        Ok(())
    }

    /// Revoke an API principal (sets `revoked_at_ms`). Returns `true` if an ACTIVE
    /// principal was revoked, `false` if absent or already revoked.
    pub fn revoke_api_principal(&mut self, principal_id: &str, now_ms: u64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE api_principals SET revoked_at_ms = ?2
             WHERE principal_id = ?1 AND revoked_at_ms IS NULL",
            params![principal_id, now_ms as i64],
        )?;
        Ok(n > 0)
    }

    /// Resolve an API principal by the SHA-256 hex of its presented token. Returns
    /// the record (active OR revoked — the caller fail-closes on revoked), or
    /// `None` if no principal holds that token hash. Lookup is by hash only.
    pub fn load_api_principal_by_token_hash(
        &self,
        token_sha256: &str,
    ) -> Result<Option<ApiPrincipalRecord>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT principal_id, role, created_at_ms, revoked_at_ms
                 FROM api_principals WHERE token_sha256 = ?1",
                params![token_sha256],
                Self::map_api_principal_row,
            )
            .optional()
    }

    /// Read-only listing of every registered API principal. Never returns the
    /// token hash — the handler exposes only id / role / status.
    pub fn load_api_principals(&self) -> Result<Vec<ApiPrincipalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms
             FROM api_principals ORDER BY principal_id",
        )?;
        let rows = stmt.query_map([], Self::map_api_principal_row)?;
        rows.collect()
    }

    fn map_api_principal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiPrincipalRecord> {
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
    fn register_then_resolve_by_hash() {
        let mut s = store();
        s.register_api_principal("svc-a", "hash-a", "integrator", 1_000).unwrap();
        let rec = s.load_api_principal_by_token_hash("hash-a").unwrap().expect("present");
        assert_eq!(rec.principal_id, "svc-a");
        assert_eq!(rec.role, "integrator");
        assert!(rec.is_active());
        assert!(s.load_api_principal_by_token_hash("nope").unwrap().is_none());
    }

    #[test]
    fn rotation_overwrites_hash_and_clears_revocation() {
        let mut s = store();
        s.register_api_principal("svc-a", "hash-old", "integrator", 1_000).unwrap();
        assert!(s.revoke_api_principal("svc-a", 2_000).unwrap());
        // The old hash still resolves the (now revoked) record.
        assert!(s.load_api_principal_by_token_hash("hash-old").unwrap().unwrap().revoked_at_ms.is_some());
        // Re-register rotates the token and reactivates.
        s.register_api_principal("svc-a", "hash-new", "auditor", 3_000).unwrap();
        assert!(s.load_api_principal_by_token_hash("hash-old").unwrap().is_none(),
            "the rotated-out hash no longer resolves");
        let rec = s.load_api_principal_by_token_hash("hash-new").unwrap().unwrap();
        assert_eq!(rec.role, "auditor");
        assert!(rec.is_active());
    }

    #[test]
    fn revoke_is_idempotent_and_reports_transition() {
        let mut s = store();
        s.register_api_principal("svc-a", "h", "operator", 1_000).unwrap();
        assert!(s.revoke_api_principal("svc-a", 2_000).unwrap(), "first revoke transitions");
        assert!(!s.revoke_api_principal("svc-a", 3_000).unwrap(), "second revoke is a no-op");
        assert!(!s.revoke_api_principal("absent", 3_000).unwrap(), "absent principal → false");
    }

    #[test]
    fn list_orders_by_id_and_hides_no_secret() {
        let mut s = store();
        s.register_api_principal("svc-b", "hb", "auditor", 1_000).unwrap();
        s.register_api_principal("svc-a", "ha", "integrator", 1_000).unwrap();
        let all = s.load_api_principals().unwrap();
        assert_eq!(all.iter().map(|p| p.principal_id.as_str()).collect::<Vec<_>>(), ["svc-a", "svc-b"]);
    }
}
