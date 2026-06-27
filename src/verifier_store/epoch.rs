// src/verifier_store/epoch.rs
// epoch domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    /// In-transaction HA epoch fence (issue #79). Closes the residual TOCTOU in
    /// the request-path gate (`enforce_posture_routing`): that gate compares a
    /// CACHED epoch, but the durable epoch can advance (another instance's
    /// `try_claim_epoch`) in the window between the gate check and the write
    /// commit. By re-reading `ha_state.epoch` on the SAME serialized write
    /// transaction handle and comparing it to this instance's `held_epoch`
    /// BEFORE any mutation, a superseded node cannot land even one stale write:
    /// on any mismatch this returns `Err` and the caller drops the transaction
    /// without committing.
    ///
    /// MUST be called as the FIRST statement inside a top-tier durable
    /// transaction (the callers begin the transaction with
    /// `TransactionBehavior::Immediate`, so the write lock is held before this
    /// read — the durable epoch we observe here cannot change before we commit).
    ///
    /// Fail-closed on every non-match:
    ///   - `held == 0` → never legitimately claimed an epoch → reject.
    ///   - `durable != held` (including `durable < held`) → superseded → reject.
    ///   - SELECT error / row absent → [`FenceError::EpochUnreadable`] → reject.
    pub(crate) fn assert_epoch_held(
        tx: &rusqlite::Transaction,
        held_epoch: u64,
    ) -> std::result::Result<(), FenceError> {
        let durable: u64 = match tx.query_row(
            "SELECT epoch FROM ha_state WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(e) => e as u64,
            // SELECT failed or the singleton row is absent — never write blind.
            Err(_) => return Err(FenceError::EpochUnreadable),
        };
        // `held == 0` is fenced explicitly: it must reject even when the durable
        // epoch is also 0 (genesis, no claim anywhere) — a node that never
        // claimed must not perform a top-tier write.
        if held_epoch == 0 || durable != held_epoch {
            return Err(FenceError::EpochSuperseded { held: held_epoch, durable });
        }
        Ok(())
    }

    /// Current durable HA epoch. Source of truth for "who owns writes."
    pub fn current_epoch(&self) -> Result<u64> {
        let e: i64 = self.conn.query_row(
            "SELECT epoch FROM ha_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(e as u64)
    }

    /// Returns (current_epoch, active_instance_id) for startup arbitration.
    pub fn current_active_holder(&self) -> Result<(u64, Option<String>)> {
        let (e, holder): (i64, Option<String>) = self.conn.query_row(
            "SELECT epoch, active_instance_id FROM ha_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((e as u64, holder))
    }

    /// Conditional claim: bump epoch from `observed` to `observed + 1` and
    /// record this instance as the new holder, IFF the DB epoch still equals
    /// `observed`. Returns the new epoch on a win, or None if another
    /// instance already moved the epoch (claim aborted, fence held).
    ///
    /// `rows_affected == 1` is the durable compare-and-set: two concurrent
    /// callers reading the same `observed` will serialize at the write
    /// transaction boundary and only one will see the row update.
    pub fn try_claim_epoch(
        &mut self,
        observed: u64,
        instance_id: &str,
        now_ms: u64,
    ) -> Result<Option<u64>> {
        // #74 CORRECTNESS FIX: the epoch CAS goes through the FULL (force-synced)
        // connection, so the claim is DURABLE (fsync'd) before this returns —
        // and the caller (standby_monitor) only sets the in-memory held_epoch /
        // acts as Active AFTER this returns. A claimed epoch can no longer
        // regress on power-loss recovery, closing the split-brain window.
        let n = self.durable_ref().execute(
            "UPDATE ha_state SET epoch = epoch + 1, active_instance_id = ?2, updated_at_ms = ?3 \
             WHERE id = 1 AND epoch = ?1",
            params![observed as i64, instance_id, now_ms as i64],
        )?;
        if n == 1 {
            Ok(Some(observed + 1))
        } else {
            Ok(None)
        }
    }
}
