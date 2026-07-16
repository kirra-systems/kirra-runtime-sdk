//! `PgVerifierStore` — PrincipalStore seam (de-monolith split of lib.rs).
//!
//! Additional impl block(s); behaviour unchanged. Shared internals (`lock`,
//! `row_to_node`) are `pub(crate)` in the parent module.

use super::*;

impl PgVerifierStore {
    fn row_to_api_principal(row: &postgres::Row) -> ApiPrincipalRecord {
        ApiPrincipalRecord {
            principal_id: row.get(0),
            role: row.get(1),
            created_at_ms: row.get::<_, i64>(2).max(0) as u64,
            revoked_at_ms: row.get::<_, Option<i64>>(3).map(|v| v.max(0) as u64),
        }
    }
}

impl PrincipalStore for PgVerifierStore {
    type Error = PgStoreError;

    fn register_api_principal(
        &mut self,
        principal_id: &str,
        token_sha256: &str,
        role: &str,
        now_ms: u64,
    ) -> Result<(), PgStoreError> {
        let created_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Register / rotate: overwrite token hash + role and CLEAR revocation. A
        // `token_sha256` already held by a DIFFERENT principal violates the UNIQUE
        // constraint (only the principal_id conflict is handled by ON CONFLICT), so
        // it surfaces as a driver error — the fail-closed "one token, one principal"
        // guarantee, exactly like the SQLite backend.
        self.lock().execute(
            "INSERT INTO api_principals \
                 (principal_id, token_sha256, role, created_at_ms, revoked_at_ms) \
             VALUES ($1, $2, $3, $4, NULL) \
             ON CONFLICT (principal_id) DO UPDATE SET \
                 token_sha256  = EXCLUDED.token_sha256, \
                 role          = EXCLUDED.role, \
                 created_at_ms = EXCLUDED.created_at_ms, \
                 revoked_at_ms = NULL",
            &[&principal_id, &token_sha256, &role, &created_ms],
        )?;
        Ok(())
    }

    fn revoke_api_principal(
        &mut self,
        principal_id: &str,
        now_ms: u64,
    ) -> Result<bool, PgStoreError> {
        let rev_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        let n = self.lock().execute(
            "UPDATE api_principals SET revoked_at_ms = $2 \
             WHERE principal_id = $1 AND revoked_at_ms IS NULL",
            &[&principal_id, &rev_ms],
        )?;
        Ok(n > 0)
    }

    fn load_api_principal_by_token_hash(
        &self,
        token_sha256: &str,
    ) -> Result<Option<ApiPrincipalRecord>, PgStoreError> {
        // Lookup by hash only; the record never carries the token back.
        let row = self.lock().query_opt(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms \
             FROM api_principals WHERE token_sha256 = $1",
            &[&token_sha256],
        )?;
        Ok(row.as_ref().map(Self::row_to_api_principal))
    }

    fn load_api_principals(&self) -> Result<Vec<ApiPrincipalRecord>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms \
             FROM api_principals ORDER BY principal_id",
            &[],
        )?;
        Ok(rows.iter().map(Self::row_to_api_principal).collect())
    }
}
