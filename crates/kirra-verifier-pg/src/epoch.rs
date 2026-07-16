//! `PgVerifierStore` — EpochFence seam (de-monolith split of lib.rs).
//!
//! Additional impl block(s); behaviour unchanged. Shared internals (`lock`,
//! `row_to_node`) are `pub(crate)` in the parent module.

use super::*;

impl EpochFence for PgVerifierStore {
    type Error = PgStoreError;

    fn current_epoch(&self) -> Result<u64, PgStoreError> {
        // `query_one` errors when the singleton row is absent — the read path
        // is fail-closed exactly like the SQLite backend's `query_row`.
        let row = self
            .lock()
            .query_one("SELECT epoch FROM ha_state WHERE id = 1", &[])?;
        Ok(row.get::<_, i64>(0) as u64)
    }

    fn current_active_holder(&self) -> Result<(u64, Option<String>), PgStoreError> {
        let row = self.lock().query_one(
            "SELECT epoch, active_instance_id FROM ha_state WHERE id = 1",
            &[],
        )?;
        Ok((row.get::<_, i64>(0) as u64, row.get(1)))
    }

    fn try_claim_epoch(
        &mut self,
        observed: u64,
        instance_id: &str,
        now_ms: u64,
    ) -> Result<Option<u64>, PgStoreError> {
        // The SAME rows-affected compare-and-set as the SQLite backend: two
        // racers reading the same `observed` serialize at the row lock and
        // exactly one sees `rows == 1`.
        let n = self.lock().execute(
            "UPDATE ha_state SET epoch = epoch + 1, active_instance_id = $2, updated_at_ms = $3 \
             WHERE id = 1 AND epoch = $1",
            &[&(observed as i64), &instance_id, &(now_ms as i64)],
        )?;
        Ok(if n == 1 { Some(observed + 1) } else { None })
    }

    fn assert_actuator_epoch_held(&mut self, held_epoch: u64) -> Result<(), FenceError> {
        // The cross-backend constraint recorded on the trait, realized the
        // Postgres way: a transaction whose `SELECT … FOR UPDATE` takes the
        // `ha_state` row lock. While this assertion transaction is open a
        // competing `try_claim_epoch` serializes behind it; if a competing
        // claim already landed, the locked read observes the newer epoch and
        // the fence rejects before any actuator response is issued. Fail
        // closed on EVERY failure: transaction/read errors → `EpochUnreadable`;
        // `held == 0` or any mismatch → `EpochSuperseded`. The transaction
        // rolls back on drop, so the reject path never commits anything.
        let mut guard = self.lock();
        let mut tx = guard
            .transaction()
            .map_err(|_| FenceError::EpochUnreadable)?;
        let row = tx
            .query_opt("SELECT epoch FROM ha_state WHERE id = 1 FOR UPDATE", &[])
            .map_err(|_| FenceError::EpochUnreadable)?;
        let durable = match row {
            Some(r) => r.get::<_, i64>(0) as u64,
            // Singleton row absent — never authorize blind.
            None => return Err(FenceError::EpochUnreadable),
        };
        if held_epoch == 0 || durable != held_epoch {
            return Err(FenceError::EpochSuperseded {
                held: held_epoch,
                durable,
            });
        }
        tx.commit().map_err(|_| FenceError::EpochUnreadable)
    }
}
