//! `PgVerifierStore` — FederationStore seam (de-monolith split of lib.rs).
//!
//! Additional impl block(s); behaviour unchanged. Shared internals (`lock`,
//! `row_to_node`) are `pub(crate)` in the parent module.

use super::*;

impl FederationStore for PgVerifierStore {
    type Error = PgStoreError;

    fn save_trusted_federation_controller(
        &self,
        controller_id: &str,
        public_key_b64: &str,
        registered_at_ms: u64,
    ) -> Result<(), PgStoreError> {
        // Fail-closed on an out-of-domain timestamp rather than wrapping a `u64` to a
        // negative `BIGINT` (bounded in practice — overflows i64 only past year 292M).
        let reg_ms = i64::try_from(registered_at_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "registered_at_ms",
            value: registered_at_ms,
        })?;
        // Upsert by controller_id (SQLite: INSERT OR REPLACE) — re-registering a
        // controller overwrites its key.
        self.lock().execute(
            "INSERT INTO trusted_federation_controllers \
                 (controller_id, public_key_b64, registered_at_ms) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (controller_id) DO UPDATE SET \
                 public_key_b64 = EXCLUDED.public_key_b64, \
                 registered_at_ms = EXCLUDED.registered_at_ms",
            &[&controller_id, &public_key_b64, &reg_ms],
        )?;
        Ok(())
    }

    fn load_trusted_federation_controller_key(
        &self,
        controller_id: &str,
    ) -> Result<Option<String>, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT public_key_b64 FROM trusted_federation_controllers WHERE controller_id = $1",
            &[&controller_id],
        )?;
        Ok(row.map(|r| r.get::<_, String>(0)))
    }

    fn has_seen_federation_nonce(&self, nonce_hex: &str) -> Result<bool, PgStoreError> {
        let row = self.lock().query_one(
            "SELECT COUNT(*) FROM federation_report_nonces WHERE nonce_hex = $1",
            &[&nonce_hex],
        )?;
        Ok(row.get::<_, i64>(0) > 0)
    }

    fn burn_federation_nonce(&self, nonce_hex: &str) -> Result<bool, PgStoreError> {
        // Atomic single-use claim: `ON CONFLICT DO NOTHING` is the Postgres
        // `INSERT OR IGNORE` — `rows == 1` iff the nonce was newly recorded (first
        // use → proceed), `0` on a replay (already present → reject). No
        // check-then-act window; the PK conflict decides. `seen_at_ms` is diagnostic
        // only (correctness rests on the PK, never the clock), and the source label
        // mirrors the SQLite burn path.
        let seen_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let n = self.lock().execute(
            "INSERT INTO federation_report_nonces (nonce_hex, source_controller_id, seen_at_ms) \
             VALUES ($1, 'fleet-grant-lane', $2) \
             ON CONFLICT (nonce_hex) DO NOTHING",
            &[&nonce_hex, &seen_at_ms],
        )?;
        Ok(n == 1)
    }

    fn industrial_seq_check_and_advance(
        &self,
        source_id: &str,
        sequence: u64,
        now_ms: u64,
    ) -> Result<bool, PgStoreError> {
        // Fail-closed on an out-of-domain sequence/timestamp: a `u64` wrapped to a
        // negative `BIGINT` would DEFEAT the gate (a wrapped-negative high-water lets a
        // later smaller sequence compare "greater"), so refuse rather than wrap.
        let seq = i64::try_from(sequence).map_err(|_| PgStoreError::OutOfDomain {
            field: "sequence",
            value: sequence,
        })?;
        let now = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Atomic per-source strictly-advancing gate — the SAME conditional compare-
        // and-set as the SQLite backend: a first message from a new source inserts
        // (baseline accept), an existing source advances ONLY on a strictly greater
        // sequence (the `WHERE` gates the DO UPDATE), and a replay/regress no-ops.
        // `rows == 1` on accept, `0` on reject. Race-safe at the row lock.
        let n = self.lock().execute(
            "INSERT INTO industrial_message_seq (source_id, last_sequence, last_seen_ms) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (source_id) DO UPDATE SET \
                 last_sequence = EXCLUDED.last_sequence, \
                 last_seen_ms = EXCLUDED.last_seen_ms \
             WHERE EXCLUDED.last_sequence > industrial_message_seq.last_sequence",
            &[&source_id, &seq, &now],
        )?;
        Ok(n == 1)
    }
}
