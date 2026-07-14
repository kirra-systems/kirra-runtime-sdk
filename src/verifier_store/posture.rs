// src/verifier_store/posture.rs
// posture domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    /// Plain (non-chained) posture-event insert. **TEST-ONLY** — gated
    /// `#[cfg(test)]` after the audit-chain-bypass fix so production code
    /// cannot reintroduce a write that misses the SHA-256 hash chain.
    /// Production writes go through `save_posture_event_chained` exclusively.
    #[cfg(test)]
    pub(crate) fn save_posture_event(
        &self,
        node_id: &str,
        event_type: &str,
        posture_json: &str,
        reason: Option<&str>,
        created_at_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO posture_events
             (node_id, event_type, posture_json, reason, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                node_id,
                event_type,
                posture_json,
                reason,
                created_at_ms as i64
            ],
        )?;
        Ok(())
    }

    pub fn load_node_history(&self, node_id: &str) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT event_type, posture_json, reason, created_at_ms
             FROM posture_events
             WHERE node_id = ?1
             ORDER BY created_at_ms DESC",
        )?;

        let rows = stmt.query_map(params![node_id], |row| {
            let event_type: String = row.get(0)?;
            let posture_json: String = row.get(1)?;
            let reason: Option<String> = row.get(2)?;
            let created_at_ms: i64 = row.get(3)?;

            let posture: serde_json::Value =
                serde_json::from_str(&posture_json).unwrap_or(serde_json::Value::Null);

            Ok(serde_json::json!({
                "event_type": event_type,
                "posture": posture,
                "reason": reason,
                "created_at_ms": created_at_ms as u64,
            }))
        })?;

        rows.collect()
    }

    pub fn count_recent_posture_events(&self, node_id: &str, since_ms: u64) -> Result<u64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM posture_events
             WHERE node_id = ?1 AND created_at_ms >= ?2",
            params![node_id, since_ms as i64],
            |row| row.get(0),
        )
    }

    /// #396 console analytics — posture-transition rows since `since_ms`, each
    /// carrying its `posture_json` (the resulting `FleetPosture`) and the row
    /// timestamp. The handler buckets these client-side over the window. Pure
    /// read; no new data class. Returns `(created_at_ms, posture_json)` ASC.
    pub fn load_posture_events_since(&self, since_ms: u64) -> Result<Vec<(u64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT created_at_ms, posture_json FROM posture_events
             WHERE created_at_ms >= ?1
             ORDER BY created_at_ms ASC",
        )?;
        let rows = stmt.query_map(params![since_ms as i64], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
        })?;
        rows.collect()
    }

    /// #396 console analytics — per-node posture-event counts since `since_ms`,
    /// the "flapping" leaderboard input. `(node_id, count)` DESC by count.
    pub fn count_posture_events_by_node_since(&self, since_ms: u64) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, COUNT(*) AS c FROM posture_events
             WHERE created_at_ms >= ?1
             GROUP BY node_id
             ORDER BY c DESC",
        )?;
        let rows = stmt.query_map(params![since_ms as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        rows.collect()
    }

    pub fn load_all_posture_events(&self) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, event_type, posture_json, reason, created_at_ms
             FROM posture_events
             ORDER BY created_at_ms ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let node_id: String = row.get(0)?;
            let event_type: String = row.get(1)?;
            let posture_json: String = row.get(2)?;
            let reason: Option<String> = row.get(3)?;
            let created_at_ms: i64 = row.get(4)?;

            let posture: serde_json::Value =
                serde_json::from_str(&posture_json).unwrap_or(serde_json::Value::Null);

            Ok(serde_json::json!({
                "node_id": node_id,
                "event_type": event_type,
                "posture": posture,
                "reason": reason,
                "created_at_ms": created_at_ms as u64,
            }))
        })?;

        rows.collect()
    }

    /// Shared body of every chained posture-event write. Runs the posture-row
    /// INSERT, the `AuditChainLinker` append, and (when `generation` is `Some`)
    /// the monotonic high-water UPSERT in ONE `Immediate` transaction on the
    /// GIVEN connection. Callers pass either the NORMAL `conn` (checkpoint-bounded
    /// durability, INV-12 throughput path) or the FULL `durable_conn` (per-commit
    /// fsync — hard-power-loss durable at write time, #772 F2). Threading the
    /// connection + signing key as params keeps the durable and normal variants a
    /// single source of truth so they cannot drift.
    ///
    /// Returns whether the high-water ADVANCED (`false` when `generation` is
    /// `None`, or when the monotonic guard rejected a stale/equal generation).
    #[allow(clippy::too_many_arguments)] // faithful pass-through of the write's columns
    fn write_posture_event_chained_tx(
        conn: &mut Connection,
        // ADR-0035 Addendum A, Stage 2.5 step 1: the audit append now goes through
        // an INJECTED `AuditAppender` instead of a direct `AuditChainLinker` +
        // signing-key call. The store still owns the transaction (`tx` below), so
        // the posture row and the audit append stay all-or-nothing — the appender
        // only stages its rows into `tx`; this method's `tx.commit()` is what makes
        // them atomic. The production caller injects `ChainedAuditAppender`, so the
        // audit-chain bytes are byte-identical to the prior direct call.
        appender: &dyn AuditAppender,
        node_id: &str,
        event_type: &str,
        posture_json: &str,
        reason: Option<&str>,
        created_at_ms: u64,
        generation: Option<u64>,
    ) -> Result<bool> {
        let tx = Self::audit_tx(conn)?; // #685: Immediate — non-forking audit append
        tx.execute(
            "INSERT INTO posture_events
             (node_id, event_type, posture_json, reason, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                node_id,
                event_type,
                posture_json,
                reason,
                created_at_ms as i64
            ],
        )?;
        appender.append_within(&tx, event_type, posture_json, created_at_ms as i64)?;
        let advanced = match generation {
            Some(g) => {
                // Monotonic max-write — identical guard to `save_last_generation`,
                // but in THIS transaction so the stamp and its high-water commit
                // together (#771 F2).
                let changed = tx.execute(
                    "INSERT INTO posture_engine_state (key, value)
                     VALUES ('last_generation', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = ?1
                     WHERE CAST(?1 AS INTEGER) > CAST(value AS INTEGER)",
                    params![g.to_string()],
                )?;
                changed > 0
            }
            None => false,
        };
        tx.commit()?;
        Ok(advanced)
    }

    /// Audit-chained posture-event insert. **All production posture-event
    /// writes MUST go through this (or a `_chained*` sibling)**; the non-chained
    /// inserter is `#[cfg(test)]`-only so events cannot bypass the audit chain.
    /// Writes the posture row and its `AuditChainLinker` entry in one transaction
    /// on the NORMAL connection (checkpoint-bounded durability).
    pub fn save_posture_event_chained(
        &mut self,
        node_id: &str,
        event_type: &str,
        posture_json: &str,
        reason: Option<&str>,
        created_at_ms: u64,
    ) -> Result<()> {
        // Disjoint field borrows: the tx borrows `self.conn`, the append reads
        // `self.signing_key` — different fields, so both live at once.
        Self::write_posture_event_chained_tx(
            &mut self.conn,
            &ChainedAuditAppender {
                signing_key: self.signing_key.as_ref(),
            },
            node_id,
            event_type,
            posture_json,
            reason,
            created_at_ms,
            None,
        )?;
        Ok(())
    }

    /// Like [`Self::save_posture_event_chained`], but ALSO advances the
    /// generation high-water in the SAME transaction (#771 F2). Folding the
    /// stamp and its high-water into one commit closes the cross-restart crash
    /// window the separate two-write path left open: a hard kill between the
    /// audit-event commit and the high-water UPSERT re-seeded from a STALE
    /// high-water on restart, so the durable audit chain could hold two events
    /// stamped with the same generation, or an already-broadcast generation
    /// could be re-issued. With one transaction the event and its high-water
    /// are all-or-nothing: a failed commit lands neither, and the caller (which
    /// gates cache/broadcast on success) fails closed.
    ///
    /// Returns whether the high-water ADVANCED (`true`) or the monotonic guard
    /// rejected `generation` as stale/equal (`false`, a benign concurrent-recalc
    /// race); either way the posture event + audit append committed.
    ///
    /// FAIL-CLOSED (#771 F4): a `generation` outside the storable INTEGER domain
    /// (`>= i64::MAX`) is rejected BEFORE the transaction — SQLite's
    /// `CAST(value AS INTEGER)` saturates at `i64::MAX`, so storing such a value
    /// would silently corrupt the monotonic guard itself. The whole write errors.
    pub fn save_posture_event_chained_with_generation(
        &mut self,
        node_id: &str,
        event_type: &str,
        posture_json: &str,
        reason: Option<&str>,
        created_at_ms: u64,
        generation: u64,
    ) -> Result<bool> {
        if generation >= i64::MAX as u64 {
            return Err(rusqlite::Error::IntegralValueOutOfRange(0, i64::MAX));
        }
        Self::write_posture_event_chained_tx(
            &mut self.conn,
            &ChainedAuditAppender {
                signing_key: self.signing_key.as_ref(),
            },
            node_id,
            event_type,
            posture_json,
            reason,
            created_at_ms,
            Some(generation),
        )
    }

    /// INCIDENT-CLASS durable variant of [`Self::save_posture_event_chained`]
    /// (#772 F2). Writes the row + audit link on the `synchronous=FULL`
    /// connection, so the COMMIT ITSELF fsyncs the WAL: the forensic row is
    /// hard-power-loss durable at write time, atomically, with no separate marker
    /// and no cross-connection piggyback inference. Use for post-incident forensic
    /// rows — never for the 20 Hz `POSTURE_CACHE_REFRESHED` traffic (INV-12
    /// throughput). On an in-memory store `durable_conn` is absent and this rides
    /// the main connection (semantics preserved for tests).
    pub fn save_posture_event_chained_durable(
        &mut self,
        node_id: &str,
        event_type: &str,
        posture_json: &str,
        reason: Option<&str>,
        created_at_ms: u64,
    ) -> Result<()> {
        #[cfg(test)]
        {
            self.durable_posture_writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self
                .fail_durable_posture_writes
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(Self::injected_durable_fault());
            }
        }
        // Disjoint field borrows: `durable_conn`/`conn` for the tx, `signing_key`
        // for the append.
        let conn = self.durable_conn.as_mut().unwrap_or(&mut self.conn);
        Self::write_posture_event_chained_tx(
            conn,
            &ChainedAuditAppender {
                signing_key: self.signing_key.as_ref(),
            },
            node_id,
            event_type,
            posture_json,
            reason,
            created_at_ms,
            None,
        )?;
        Ok(())
    }

    /// INCIDENT-CLASS durable variant of
    /// [`Self::save_posture_event_chained_with_generation`] (#772 F2). Folds the
    /// posture row, audit link, AND generation high-water into one transaction on
    /// the `synchronous=FULL` connection — the atomic, evidence-grade replacement
    /// for the prior "NORMAL-commit then separate `fsync_wal_durable` marker"
    /// two-step (which left a crash window and rested on emergent whole-inode
    /// fsync semantics rather than a documented per-write guarantee). Use for a
    /// posture TRANSITION. Same `>= i64::MAX` fail-closed guard as the normal
    /// variant.
    pub fn save_posture_event_chained_with_generation_durable(
        &mut self,
        node_id: &str,
        event_type: &str,
        posture_json: &str,
        reason: Option<&str>,
        created_at_ms: u64,
        generation: u64,
    ) -> Result<bool> {
        if generation >= i64::MAX as u64 {
            return Err(rusqlite::Error::IntegralValueOutOfRange(0, i64::MAX));
        }
        #[cfg(test)]
        {
            self.durable_posture_writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self
                .fail_durable_posture_writes
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(Self::injected_durable_fault());
            }
        }
        let conn = self.durable_conn.as_mut().unwrap_or(&mut self.conn);
        Self::write_posture_event_chained_tx(
            conn,
            &ChainedAuditAppender {
                signing_key: self.signing_key.as_ref(),
            },
            node_id,
            event_type,
            posture_json,
            reason,
            created_at_ms,
            Some(generation),
        )
    }

    /// TEST-ONLY (#772 F6): how many incident-class durable posture-event writes
    /// this store has performed. The gating test asserts a TRANSITION bumps this
    /// and a `POSTURE_CACHE_REFRESHED` does not.
    #[cfg(test)]
    pub fn durable_posture_write_count(&self) -> u64 {
        self.durable_posture_writes
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// TEST-ONLY (#772 F3): force the durable posture-event writes to fail at
    /// entry (no DB touch), so the recalc's fall-back-to-NORMAL-write path can be
    /// exercised without a real fsync/BUSY fault.
    #[cfg(test)]
    pub fn set_fail_durable_posture_writes(&self, fail: bool) {
        self.fail_durable_posture_writes
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    fn injected_durable_fault() -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            Some("injected durable-write fault (#772 F3 test)".into()),
        )
    }

    pub fn load_last_generation(&self) -> Result<u64> {
        match self.conn.query_row(
            "SELECT value FROM posture_engine_state WHERE key = 'last_generation'",
            [],
            |row| row.get::<_, String>(0),
        ) {
            // FAIL-CLOSED parse (#771 review): a present-but-non-numeric value is
            // CORRUPTION, not a fresh store. The prior `unwrap_or(0)` read it as
            // "no history", silently reintroducing the restart time-reversal the
            // boot init exists to prevent. Only a genuinely absent row is 0.
            Ok(s) => {
                let parsed = s.parse::<u64>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                // #771 F4: a numeric-but-out-of-domain value (`>= i64::MAX`) is
                // ALSO corruption, not a valid high-water. It parses cleanly but
                // (a) `init_generation_from_store` would compute `last + 1` — a
                // release-build wraparound to 0 with overflow-checks off, silently
                // failing OPEN to the very restart time-reversal this guard exists
                // to prevent; and (b) it breaks the store's own `CAST(value AS
                // INTEGER)` monotonic guard (SQLite saturates at i64::MAX). Reject
                // it fail-closed, exactly like the non-numeric case.
                if parsed >= i64::MAX as u64 {
                    return Err(rusqlite::Error::IntegralValueOutOfRange(0, i64::MAX));
                }
                Ok(parsed)
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e),
        }
    }

    /// Persist the posture generation high-water mark, monotonically.
    ///
    /// Returns `Ok(true)` if `generation` was PERSISTED (it was strictly greater
    /// than the stored value, or the row was created), `Ok(false)` if it was
    /// REJECTED as stale/regressing (a lower-or-equal generation). #695: the
    /// monotonic UPSERT silently no-ops on a stale write, so returning the
    /// rows-affected lets the caller distinguish a benign concurrent-recalc race
    /// from a genuine generation regression/time-reversal it may want to log.
    pub fn save_last_generation(&self, generation: u64) -> Result<bool> {
        // Monotonic max-write: never persist a generation lower than the one
        // already stored. Concurrent recalculations each claim a generation
        // then race to persist; a blind INSERT OR REPLACE let a slower thread
        // overwrite a higher value with its lower one, regressing the
        // cross-restart monotonicity that federation peers depend on.
        let changed = self.conn.execute(
            "INSERT INTO posture_engine_state (key, value)
             VALUES ('last_generation', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1
             WHERE CAST(?1 AS INTEGER) > CAST(value AS INTEGER)",
            params![generation.to_string()],
        )?;
        // `changed` is 1 when the row was inserted (first write) or updated
        // (strictly-greater generation), 0 when the `WHERE` rejected a stale write.
        Ok(changed > 0)
    }

    /// Reads an arbitrary key from the posture_engine_state key-value store.
    /// Returns None if the key doesn't exist.
    pub fn load_engine_state(&self, key: &str) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT value FROM posture_engine_state WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Writes an arbitrary key to the posture_engine_state key-value store (upsert).
    pub fn save_engine_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO posture_engine_state (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ADR-0035 Addendum A, Stage 2.5 step 1 (prototype) — proof-of-mechanics that the
// injected `AuditAppender` seam (a) still produces a byte-identical, verifiable
// signed chain, (b) participates ATOMICALLY in the store's transaction (a failing
// appender rolls the posture row back too), and (c) receives the store's event.
// These drive the private `write_posture_event_chained_tx` with CUSTOM appenders,
// which is only possible because the append is now injected rather than hardwired.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod audit_appender_seam_tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn count_posture_events(store: &VerifierStore) -> i64 {
        store
            .conn
            .query_row("SELECT COUNT(*) FROM posture_events", [], |r| r.get(0))
            .unwrap()
    }

    /// An appender that always fails — to prove the caller-owned transaction rolls
    /// back the posture row when the injected append errors (atomicity).
    struct FailingAppender;
    impl AuditAppender for FailingAppender {
        fn append_within(
            &self,
            _tx: &rusqlite::Transaction<'_>,
            _event_type: &str,
            _payload: &str,
            _created_at_ms: i64,
        ) -> rusqlite::Result<()> {
            Err(rusqlite::Error::QueryReturnedNoRows)
        }
    }

    /// An appender that records what it was asked to append (and stages no audit
    /// row) — to prove the store routes its event through the seam verbatim.
    struct RecordingAppender {
        seen: std::cell::RefCell<Vec<(String, String, i64)>>,
    }
    impl AuditAppender for RecordingAppender {
        fn append_within(
            &self,
            _tx: &rusqlite::Transaction<'_>,
            event_type: &str,
            payload: &str,
            created_at_ms: i64,
        ) -> rusqlite::Result<()> {
            self.seen.borrow_mut().push((
                event_type.to_string(),
                payload.to_string(),
                created_at_ms,
            ));
            Ok(())
        }
    }

    /// (a) The production path (which now injects `ChainedAuditAppender`) produces a
    /// signed chain that fully verifies — byte-identical to the prior direct call.
    #[test]
    fn chained_appender_seam_produces_a_verifiable_chain() {
        let mut store = VerifierStore::new(":memory:").expect("store");
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        store.set_signing_key(sk.clone());

        store
            .save_posture_event_chained(
                "node-1",
                "POSTURE_TRANSITION",
                "{\"p\":\"Nominal\"}",
                None,
                1_000,
            )
            .unwrap();

        let result = store
            .verify_audit_chain_full(Some(&sk.verifying_key()))
            .unwrap();
        assert!(
            result.verified(),
            "the chain built through the injected appender must fully verify"
        );
        assert_eq!(count_posture_events(&store), 1);
    }

    /// (b) THE CRUX: the injected append participates in the store's transaction.
    /// A failing appender aborts the whole write — the posture row is rolled back,
    /// so atomicity (the power-loss invariant) survives the dependency inversion.
    #[test]
    fn injected_appender_failure_rolls_back_the_posture_row_atomically() {
        let mut store = VerifierStore::new(":memory:").expect("store");
        assert_eq!(count_posture_events(&store), 0);

        let r = VerifierStore::write_posture_event_chained_tx(
            &mut store.conn,
            &FailingAppender,
            "node-1",
            "POSTURE_TRANSITION",
            "{}",
            None,
            1_000,
            None,
        );
        assert!(r.is_err(), "a failing appender must abort the write");
        assert_eq!(
            count_posture_events(&store),
            0,
            "the posture row must roll back with the failed audit append (atomic)"
        );
    }

    /// (c) The store routes its event through the seam verbatim (type + payload +
    /// timestamp), so any injected appender sees exactly what the table row carries.
    #[test]
    fn store_delegates_the_event_to_the_injected_appender() {
        let mut store = VerifierStore::new(":memory:").expect("store");
        let recorder = RecordingAppender {
            seen: std::cell::RefCell::new(Vec::new()),
        };
        VerifierStore::write_posture_event_chained_tx(
            &mut store.conn,
            &recorder,
            "node-1",
            "POSTURE_CACHE_REFRESHED",
            "{\"p\":\"Degraded\"}",
            None,
            4_242,
            None,
        )
        .unwrap();
        let seen = recorder.seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0],
            (
                "POSTURE_CACHE_REFRESHED".to_string(),
                "{\"p\":\"Degraded\"}".to_string(),
                4_242
            ),
            "the injected appender receives the row's event verbatim"
        );
    }
}
