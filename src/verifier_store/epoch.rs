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

    /// Layer-3 HA actuator fence.
    ///
    /// Unlike federation/key-rotation writes, the actuator command path does not
    /// naturally own a durable SQLite mutation whose transaction can carry
    /// `Self::assert_epoch_held`. This helper creates a bounded
    /// `BEGIN IMMEDIATE` assertion transaction solely for the actuator authority
    /// check: while it is open, a competing epoch claim serializes behind it; if
    /// a competing claim already landed, the assertion observes the newer epoch
    /// and rejects before the actuator response is issued.
    ///
    /// SAFETY: SG-009 / HA-L3 / REQ-HA-ACTUATOR-EPOCH-FENCE.
    /// Fail closed on `held == 0`, epoch mismatch, unreadable `ha_state`, or
    /// transaction failure. The check is O(1), uses no heap allocation, and has
    /// no unbounded retry loop; SQLite's configured busy timeout is the bound.
    pub fn assert_actuator_epoch_held(
        &mut self,
        held_epoch: u64,
    ) -> std::result::Result<(), FenceError> {
        let tx = self
            .durable_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| FenceError::EpochUnreadable)?;
        Self::assert_epoch_held(&tx, held_epoch)?;
        tx.commit().map_err(|_| FenceError::EpochUnreadable)
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

    /// TEST-ONLY: remove the singleton HA row to simulate an epoch-read/disk
    /// wedge at the fence boundary. Production code never deletes this row.
    #[cfg(test)]
    pub fn delete_ha_state_for_test(&mut self) {
        self.durable_mut()
            .execute("DELETE FROM ha_state WHERE id = 1", [])
            .expect("delete ha_state singleton row for test");
    }
}

// ---------------------------------------------------------------------------
// WP-18 3/3 (G-9 store half) — the backend-portable HA epoch-fence contract
//
// This is the first `VerifierStorage`-family trait lifted off `VerifierStore`:
// the "who owns writes" epoch fence, the piece WP-19's lease builds on so
// promotion is backend-portable, and the one with the explicit cross-backend
// constraint (SQLite realizes it with a `ha_state` singleton + a serialized
// `BEGIN IMMEDIATE` transaction; a Postgres backend realizes the SAME CAS +
// fence semantics with `SELECT … FOR UPDATE`). The trait is defined with the
// SAME method names as the existing inherent `VerifierStore` methods; inherent
// methods win method resolution, so (a) every existing `store.current_epoch()`
// caller is untouched and (b) the trait impl below delegates via `self.method()`
// WITHOUT recursion. A second (in-memory) backend + a shared conformance test
// prove the contract is genuinely portable, in miniature of the plan's
// "run the store suite against both backends" evidence.
// ---------------------------------------------------------------------------

/// The HA epoch-fence contract — the durable "who owns writes" authority —
/// abstracted over the storage backend. A single durable epoch counter plus a
/// compare-and-set claim gives at-most-one-writer across a fleet; the actuator
/// fence rejects a superseded holder before it can emit a command.
pub trait EpochFence {
    /// Backend read/claim error (SQLite: `rusqlite::Error`; in-memory: [`InMemFenceError`]).
    type Error;

    /// The current durable epoch — the source of truth for write ownership.
    fn current_epoch(&self) -> std::result::Result<u64, Self::Error>;

    /// `(epoch, active holder id)` for startup arbitration.
    fn current_active_holder(&self) -> std::result::Result<(u64, Option<String>), Self::Error>;

    /// Durable compare-and-set: bump `observed → observed + 1` and record
    /// `instance_id` as the holder IFF the durable epoch still equals `observed`.
    /// `Some(new)` on a win; `None` if another instance already moved the epoch
    /// (claim aborted, fence held). Exactly one of two racers reading the same
    /// `observed` wins.
    fn try_claim_epoch(
        &mut self,
        observed: u64,
        instance_id: &str,
        now_ms: u64,
    ) -> std::result::Result<Option<u64>, Self::Error>;

    /// Fail-closed actuator fence: reject unless the durable epoch equals
    /// `held_epoch` (and `held_epoch != 0`). Every mismatch / unreadable state is
    /// a [`FenceError`] denial — a superseded or never-claimed instance can never
    /// emit even one command. Shared (backend-agnostic) error type by design.
    fn assert_actuator_epoch_held(
        &mut self,
        held_epoch: u64,
    ) -> std::result::Result<(), FenceError>;
}

/// The production SQLite backend: delegates to the inherent `VerifierStore`
/// methods (which own the `ha_state` singleton + `BEGIN IMMEDIATE` fence). The
/// `self.method()` calls resolve to the INHERENT methods (inherent wins over the
/// trait in method resolution), so this is delegation, not recursion.
impl EpochFence for VerifierStore {
    type Error = rusqlite::Error;

    fn current_epoch(&self) -> Result<u64> {
        self.current_epoch()
    }

    fn current_active_holder(&self) -> Result<(u64, Option<String>)> {
        self.current_active_holder()
    }

    fn try_claim_epoch(
        &mut self,
        observed: u64,
        instance_id: &str,
        now_ms: u64,
    ) -> Result<Option<u64>> {
        self.try_claim_epoch(observed, instance_id, now_ms)
    }

    fn assert_actuator_epoch_held(&mut self, held_epoch: u64) -> std::result::Result<(), FenceError> {
        self.assert_actuator_epoch_held(held_epoch)
    }
}

/// The in-memory [`EpochFence`] backend — a portability-proof second
/// implementation modelling the `ha_state` singleton (epoch + holder + a
/// present/absent flag). It realizes the SAME CAS + fail-closed fence semantics
/// as the SQLite store WITHOUT a database, so the fence contract can be exercised
/// deterministically (and a future backend has a reference to conform to). Not a
/// distributed store — single-process only; the durability/serialization the
/// SQLite/Postgres backends provide is out of its scope.
#[derive(Debug, Clone)]
pub struct InMemoryEpochFence {
    epoch: u64,
    holder: Option<String>,
    updated_at_ms: u64,
    /// Models the `ha_state` singleton row's presence (a wedged/absent row → the
    /// fail-closed `EpochUnreadable` path).
    row_present: bool,
}

/// Why an [`InMemoryEpochFence`] READ (`current_epoch` / `current_active_holder`)
/// could not proceed. Note this is a read-only error: `try_claim_epoch` never
/// returns it — a claim against an absent/wedged row returns `Ok(None)` (the CAS
/// simply doesn't win), mirroring SQLite's `UPDATE … WHERE …` affecting 0 rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InMemFenceError {
    /// The modelled `ha_state` row is absent (see [`InMemoryEpochFence::wedge`]).
    RowAbsent,
}

impl core::fmt::Display for InMemFenceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            InMemFenceError::RowAbsent => write!(f, "in-memory ha_state row absent"),
        }
    }
}
impl std::error::Error for InMemFenceError {}

impl InMemoryEpochFence {
    /// A genesis fence: epoch 0, no holder, row present (mirrors a fresh
    /// `VerifierStore`'s seeded `ha_state`).
    #[must_use]
    pub fn genesis() -> Self {
        Self { epoch: 0, holder: None, updated_at_ms: 0, row_present: true }
    }

    /// Simulate an epoch-read/disk wedge (the row becomes unreadable) — the
    /// in-memory analogue of `VerifierStore::delete_ha_state_for_test`.
    pub fn wedge(&mut self) {
        self.row_present = false;
    }
}

impl EpochFence for InMemoryEpochFence {
    type Error = InMemFenceError;

    fn current_epoch(&self) -> std::result::Result<u64, InMemFenceError> {
        if self.row_present {
            Ok(self.epoch)
        } else {
            Err(InMemFenceError::RowAbsent)
        }
    }

    fn current_active_holder(&self) -> std::result::Result<(u64, Option<String>), InMemFenceError> {
        if self.row_present {
            Ok((self.epoch, self.holder.clone()))
        } else {
            Err(InMemFenceError::RowAbsent)
        }
    }

    fn try_claim_epoch(
        &mut self,
        observed: u64,
        instance_id: &str,
        now_ms: u64,
    ) -> std::result::Result<Option<u64>, InMemFenceError> {
        // The CAS: win iff the row is present AND the durable epoch still equals
        // `observed` (mirrors the SQLite `UPDATE … WHERE id = 1 AND epoch = ?`).
        if self.row_present && self.epoch == observed {
            self.epoch = observed + 1;
            self.holder = Some(instance_id.to_string());
            self.updated_at_ms = now_ms;
            Ok(Some(self.epoch))
        } else {
            Ok(None)
        }
    }

    fn assert_actuator_epoch_held(&mut self, held_epoch: u64) -> std::result::Result<(), FenceError> {
        if !self.row_present {
            return Err(FenceError::EpochUnreadable);
        }
        // Identical fail-closed predicate to `VerifierStore::assert_epoch_held`.
        if held_epoch == 0 || self.epoch != held_epoch {
            return Err(FenceError::EpochSuperseded { held: held_epoch, durable: self.epoch });
        }
        Ok(())
    }
}

#[cfg(test)]
mod fence_contract_tests {
    use super::*;

    /// The full fence contract, driven through the [`EpochFence`] trait so it runs
    /// IDENTICALLY against every backend. Proves the one load-bearing property —
    /// **at most one writer** — end to end: a lost claim never advances the epoch,
    /// the winner holds the fence, a stale/never-claimed/future holder is fenced,
    /// and a second instance's claim supersedes the first.
    fn assert_fence_contract<F: EpochFence>(f: &mut F)
    where
        F::Error: core::fmt::Debug,
    {
        // Genesis: epoch 0, no holder.
        assert_eq!(f.current_epoch().unwrap(), 0);
        assert_eq!(f.current_active_holder().unwrap(), (0, None));

        // A claim on a WRONG observed epoch loses and never advances the counter.
        assert_eq!(f.try_claim_epoch(7, "A", 1).unwrap(), None);
        assert_eq!(f.current_epoch().unwrap(), 0, "a lost claim must not advance the epoch");

        // The correct claim wins and records the holder.
        assert_eq!(f.try_claim_epoch(0, "A", 10).unwrap(), Some(1));
        assert_eq!(f.current_active_holder().unwrap(), (1, Some("A".to_string())));

        // The winner (held == durable == 1) holds the fence.
        assert_eq!(f.assert_actuator_epoch_held(1), Ok(()));
        // A stale holder (old epoch), a never-claimed holder (held == 0), and a
        // future epoch are all fenced.
        assert_eq!(
            f.assert_actuator_epoch_held(0),
            Err(FenceError::EpochSuperseded { held: 0, durable: 1 })
        );
        assert_eq!(
            f.assert_actuator_epoch_held(2),
            Err(FenceError::EpochSuperseded { held: 2, durable: 1 })
        );

        // A second instance observing epoch 1 claims → exactly one writer moves on,
        // and instance A is now fenced.
        assert_eq!(f.try_claim_epoch(1, "B", 20).unwrap(), Some(2));
        assert_eq!(
            f.assert_actuator_epoch_held(1),
            Err(FenceError::EpochSuperseded { held: 1, durable: 2 }),
            "A is superseded by B's claim"
        );
        assert_eq!(f.assert_actuator_epoch_held(2), Ok(()), "B now holds the fence");

        // A double-claim on the already-consumed observed epoch loses (idempotent CAS).
        assert_eq!(f.try_claim_epoch(1, "C", 30).unwrap(), None);
    }

    #[test]
    fn sqlite_backend_satisfies_the_epoch_fence_contract() {
        let mut store = VerifierStore::new(":memory:").expect("in-memory store");
        assert_fence_contract(&mut store);
    }

    #[test]
    fn in_memory_backend_satisfies_the_epoch_fence_contract() {
        let mut fence = InMemoryEpochFence::genesis();
        assert_fence_contract(&mut fence);
    }

    /// The fail-closed `EpochUnreadable` path holds identically on both backends:
    /// a wedged / absent `ha_state` row denies the actuator fence.
    #[test]
    fn an_unreadable_row_fences_both_backends() {
        let mut store = VerifierStore::new(":memory:").expect("in-memory store");
        store.try_claim_epoch(0, "A", 1).unwrap();
        store.delete_ha_state_for_test();
        assert_eq!(
            EpochFence::assert_actuator_epoch_held(&mut store, 1),
            Err(FenceError::EpochUnreadable)
        );

        let mut fence = InMemoryEpochFence::genesis();
        fence.try_claim_epoch(0, "A", 1).unwrap();
        fence.wedge();
        assert_eq!(fence.assert_actuator_epoch_held(1), Err(FenceError::EpochUnreadable));
    }
}
