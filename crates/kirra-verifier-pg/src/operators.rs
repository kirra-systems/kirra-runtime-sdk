//! `PgVerifierStore` — OperatorStore seam (de-monolith split of lib.rs).
//!
//! Additional impl block(s); behaviour unchanged. Shared internals (`lock`,
//! `row_to_node`) are `pub(crate)` in the parent module.

use super::*;

impl PgVerifierStore {
    fn row_to_operator(row: &postgres::Row) -> OperatorRecord {
        OperatorRecord {
            operator_id: row.get(0),
            pubkey_pem: row.get(1),
            registered_at_ms: row.get::<_, i64>(2).max(0) as u64,
            revoked_at_ms: row.get::<_, Option<i64>>(3).map(|v| v.max(0) as u64),
        }
    }
}

impl OperatorStore for PgVerifierStore {
    type Error = PgStoreError;

    fn register_operator(
        &mut self,
        operator_id: &str,
        pubkey_pem: &str,
        now_ms: u64,
    ) -> Result<(), PgStoreError> {
        // Fail-closed on an out-of-domain timestamp (see FederationStore).
        let reg_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Register or rotate: overwrite the key and CLEAR any prior revocation (a
        // fresh key reactivates), matching the SQLite upsert.
        self.lock().execute(
            "INSERT INTO operators (operator_id, pubkey_pem, registered_at_ms, revoked_at_ms) \
             VALUES ($1, $2, $3, NULL) \
             ON CONFLICT (operator_id) DO UPDATE SET \
                 pubkey_pem = EXCLUDED.pubkey_pem, \
                 registered_at_ms = EXCLUDED.registered_at_ms, \
                 revoked_at_ms = NULL",
            &[&operator_id, &pubkey_pem, &reg_ms],
        )?;
        Ok(())
    }

    fn revoke_operator(&mut self, operator_id: &str, now_ms: u64) -> Result<bool, PgStoreError> {
        let rev_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Conditional update — `rows == 1` iff an ACTIVE operator transitioned to
        // revoked; `0` if absent or already revoked.
        let n = self.lock().execute(
            "UPDATE operators SET revoked_at_ms = $2 \
             WHERE operator_id = $1 AND revoked_at_ms IS NULL",
            &[&operator_id, &rev_ms],
        )?;
        Ok(n > 0)
    }

    fn load_operator(&self, operator_id: &str) -> Result<Option<OperatorRecord>, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT operator_id, pubkey_pem, registered_at_ms, revoked_at_ms \
             FROM operators WHERE operator_id = $1",
            &[&operator_id],
        )?;
        Ok(row.as_ref().map(Self::row_to_operator))
    }

    fn load_operators(&self) -> Result<Vec<OperatorRecord>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT operator_id, pubkey_pem, registered_at_ms, revoked_at_ms \
             FROM operators ORDER BY operator_id",
            &[],
        )?;
        Ok(rows.iter().map(Self::row_to_operator).collect())
    }
}
