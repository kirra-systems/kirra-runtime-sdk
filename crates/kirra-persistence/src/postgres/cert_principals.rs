//! `PgVerifierStore` — CertPrincipalStore seam (de-monolith split of lib.rs).
//!
//! Additional impl block(s); behaviour unchanged. Shared internals (`lock`,
//! `row_to_node`) are `pub(crate)` in the parent module.

use super::*;

impl PgVerifierStore {
    fn row_to_cert_principal(row: &postgres::Row) -> CertPrincipalRecord {
        CertPrincipalRecord {
            principal_id: row.get(0),
            role: row.get(1),
            created_at_ms: row.get::<_, i64>(2).max(0) as u64,
            revoked_at_ms: row.get::<_, Option<i64>>(3).map(|v| v.max(0) as u64),
            // FAIL-CLOSED read (matches the SQLite backend, Copilot #857): a NEGATIVE
            // stored `not_after_ms` — only reachable via corruption, since the write
            // path refuses `> i64::MAX` — maps to `Some(0)` ("expired at epoch"), so a
            // tampered expiry can only make a cert MORE restricted, never a huge
            // never-expiring value. `u64::try_from` fails only for negatives → 0.
            not_after_ms: row
                .get::<_, Option<i64>>(4)
                .map(|v| u64::try_from(v).unwrap_or(0)),
        }
    }
}

impl CertPrincipalStore for PgVerifierStore {
    type Error = PgStoreError;

    fn register_cert_principal(
        &mut self,
        principal_id: &str,
        cert_sha256: &str,
        role: &str,
        not_after_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<(), PgStoreError> {
        let created_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Refuse a `not_after_ms > i64::MAX` (never truncate to a negative that would
        // read back as a huge never-expiring value — a fail-OPEN expiry). Bounded in
        // practice (~292M years past epoch).
        let not_after_i64 = match not_after_ms {
            Some(v) => Some(i64::try_from(v).map_err(|_| PgStoreError::OutOfDomain {
                field: "not_after_ms",
                value: v,
            })?),
            None => None,
        };
        // A `cert_sha256` already pinned to a DIFFERENT principal violates the UNIQUE
        // constraint (surfaces as a driver error — one cert, one principal).
        self.lock().execute(
            "INSERT INTO cert_principals \
                 (principal_id, cert_sha256, role, created_at_ms, revoked_at_ms, not_after_ms) \
             VALUES ($1, $2, $3, $4, NULL, $5) \
             ON CONFLICT (principal_id) DO UPDATE SET \
                 cert_sha256   = EXCLUDED.cert_sha256, \
                 role          = EXCLUDED.role, \
                 created_at_ms = EXCLUDED.created_at_ms, \
                 revoked_at_ms = NULL, \
                 not_after_ms  = EXCLUDED.not_after_ms",
            &[
                &principal_id,
                &cert_sha256,
                &role,
                &created_ms,
                &not_after_i64,
            ],
        )?;
        Ok(())
    }

    fn revoke_cert_principal(
        &mut self,
        principal_id: &str,
        now_ms: u64,
    ) -> Result<bool, PgStoreError> {
        let rev_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        let n = self.lock().execute(
            "UPDATE cert_principals SET revoked_at_ms = $2 \
             WHERE principal_id = $1 AND revoked_at_ms IS NULL",
            &[&principal_id, &rev_ms],
        )?;
        Ok(n > 0)
    }

    fn load_cert_principal_by_fingerprint(
        &self,
        cert_sha256: &str,
    ) -> Result<Option<CertPrincipalRecord>, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms, not_after_ms \
             FROM cert_principals WHERE cert_sha256 = $1",
            &[&cert_sha256],
        )?;
        Ok(row.as_ref().map(Self::row_to_cert_principal))
    }

    fn load_cert_principals(&self) -> Result<Vec<CertPrincipalRecord>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms, not_after_ms \
             FROM cert_principals ORDER BY principal_id",
            &[],
        )?;
        Ok(rows.iter().map(Self::row_to_cert_principal).collect())
    }
}
