// src/verifier_store/av_subsystem.rs
// av_subsystem domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    pub fn register_av_subsystem_meta(
        &self,
        node_id: &str,
        subsystem_type: &str,
        hardware_id: &str,
        confidence_floor: f64,
        initial_telemetry_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO av_subsystem_meta
             (node_id, subsystem_type, hardware_id, confidence_floor, last_telemetry_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                node_id,
                subsystem_type,
                hardware_id,
                confidence_floor,
                initial_telemetry_ms as i64
            ],
        )?;
        Ok(())
    }

    pub fn load_av_confidence_floor(&self, node_id: &str) -> Result<Option<f64>> {
        match self.conn.query_row(
            "SELECT confidence_floor FROM av_subsystem_meta WHERE node_id = ?1",
            params![node_id],
            |row| row.get::<_, f64>(0),
        ) {
            Ok(f) => Ok(Some(f)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn touch_av_telemetry_timestamp(&self, node_id: &str, now_ms: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE av_subsystem_meta SET last_telemetry_ms = ?1 WHERE node_id = ?2",
            params![now_ms as i64, node_id],
        )?;
        Ok(())
    }

    pub fn get_last_telemetry_timestamp(&self, node_id: &str) -> Result<u64> {
        match self.conn.query_row(
            "SELECT last_telemetry_ms FROM av_subsystem_meta WHERE node_id = ?1",
            params![node_id],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(ts) => Ok(ts as u64),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e),
        }
    }

    pub fn load_all_registered_av_node_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT node_id FROM av_subsystem_meta")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// Read-only listing of every registered AV subsystem's diagnostic meta
    /// (confidence floor, recovery streak, last telemetry). No secrets.
    pub fn load_av_subsystems(&self) -> Result<Vec<AvSubsystemRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, subsystem_type, hardware_id, confidence_floor,
                    last_telemetry_ms, recovery_streak_count, recovery_streak_start_ms
             FROM av_subsystem_meta ORDER BY node_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AvSubsystemRecord {
                node_id: row.get(0)?,
                subsystem_type: row.get(1)?,
                hardware_id: row.get(2)?,
                confidence_floor: row.get(3)?,
                last_telemetry_ms: row.get::<_, i64>(4)? as u64,
                recovery_streak_count: row.get::<_, i64>(5)? as u32,
                recovery_streak_start_ms: row.get::<_, i64>(6)? as u64,
            })
        })?;
        rows.collect()
    }

    pub fn load_recovery_streak(&self, node_id: &str) -> Result<(u32, u64)> {
        match self.conn.query_row(
            "SELECT recovery_streak_count, recovery_streak_start_ms
             FROM av_subsystem_meta WHERE node_id = ?1",
            params![node_id],
            |row| Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)? as u64)),
        ) {
            Ok(data) => Ok(data),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((0, 0)),
            Err(e) => Err(e),
        }
    }

    pub fn reset_recovery_streak(&self, node_id: &str, now_ms: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE av_subsystem_meta
             SET recovery_streak_count = 0, recovery_streak_start_ms = 0,
                 last_telemetry_ms = ?1
             WHERE node_id = ?2",
            params![now_ms as i64, node_id],
        )?;
        Ok(())
    }

    /// Reset the recovery streak WITHOUT touching `last_telemetry_ms` (Q4).
    ///
    /// The sibling `reset_recovery_streak` stamps `last_telemetry_ms = now`
    /// because its callers reset on the RECEIPT of a (fault) report — the report
    /// genuinely arrived "now". The telemetry watchdog is the opposite case: a
    /// node went SILENT past the timeout, so NO report arrived. Resetting the
    /// streak there must NOT fabricate a fresh "last seen" (which would reset the
    /// watchdog's own silence detection). This variant clears only the streak
    /// counters so a timed-out node must earn a full `AV_RECOVERY_STREAK_THRESHOLD`
    /// window again before it can be re-trusted.
    pub fn reset_recovery_streak_preserving_telemetry(&self, node_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE av_subsystem_meta
             SET recovery_streak_count = 0, recovery_streak_start_ms = 0
             WHERE node_id = ?1",
            params![node_id],
        )?;
        Ok(())
    }

    pub fn increment_recovery_streak(&self, node_id: &str, now_ms: u64) -> Result<u32> {
        self.conn.execute(
            "UPDATE av_subsystem_meta
             SET recovery_streak_count = recovery_streak_count + 1,
                 recovery_streak_start_ms = CASE
                     WHEN recovery_streak_count = 0 THEN ?1
                     ELSE recovery_streak_start_ms
                 END,
                 last_telemetry_ms = ?1
             WHERE node_id = ?2",
            params![now_ms as i64, node_id],
        )?;
        self.conn.query_row(
            "SELECT recovery_streak_count FROM av_subsystem_meta WHERE node_id = ?1",
            params![node_id],
            |row| row.get::<_, i64>(0).map(|v| v as u32),
        )
    }
}

// ---------------------------------------------------------------------------
// ADR-0035 Stage 2 (trait-seam inversion) — the AV-subsystem-meta storage trait
//
// The AV diagnostic-meta store (per-node confidence floor + last-telemetry stamp
// + the recovery-streak counters that drive the AV recovery hysteresis and the
// telemetry watchdog), lifted off `VerifierStore` as another `VerifierStorage`-
// family seam. All methods are `&self` (matching the inherent signatures and
// `NodeStore`), so the in-memory backend uses interior mutability and the
// conformance driver takes `&S`. Inherent methods win resolution → the SQLite
// impl delegates via `self.method()` without recursion; existing callers
// (`recovery_hysteresis`, `telemetry_watchdog`, the fleet handler) are untouched.
//
// One faithful failure mode: `increment_recovery_streak` on an UNREGISTERED node
// errors on SQLite (the trailing `SELECT` finds no row → `QueryReturnedNoRows`),
// so the in-memory backend's `Error` is a real enum ([`InMemAvError`]) returning
// `NodeNotRegistered` there — the portable contract holds both backends to it.
// ---------------------------------------------------------------------------

/// The AV-subsystem diagnostic-meta storage contract — upsert a subsystem's meta,
/// read/update its confidence floor + last-telemetry stamp, and drive its
/// recovery-streak counters. Backend-agnostic; no secrets.
pub trait AvSubsystemStore {
    /// Backend error type (SQLite: `rusqlite::Error`; in-memory: [`InMemAvError`]).
    type Error;

    /// Upsert an AV subsystem's meta by `node_id` (INSERT-OR-REPLACE — re-registering
    /// overwrites the row and RESETS the recovery-streak counters to 0, since the
    /// streak columns are not part of the write and fall back to their DEFAULT 0).
    fn register_av_subsystem_meta(
        &self,
        node_id: &str,
        subsystem_type: &str,
        hardware_id: &str,
        confidence_floor: f64,
        initial_telemetry_ms: u64,
    ) -> std::result::Result<(), Self::Error>;

    /// The node's confidence floor, or `None` if unregistered.
    fn load_av_confidence_floor(
        &self,
        node_id: &str,
    ) -> std::result::Result<Option<f64>, Self::Error>;

    /// Stamp `last_telemetry_ms = now_ms` (no-op if the node is unregistered).
    fn touch_av_telemetry_timestamp(
        &self,
        node_id: &str,
        now_ms: u64,
    ) -> std::result::Result<(), Self::Error>;

    /// The node's last-telemetry stamp, or `0` if unregistered.
    fn get_last_telemetry_timestamp(&self, node_id: &str) -> std::result::Result<u64, Self::Error>;

    /// Every registered AV node id.
    fn load_all_registered_av_node_ids(&self) -> std::result::Result<Vec<String>, Self::Error>;

    /// Every registered AV subsystem's diagnostic-meta record, ordered by `node_id`.
    fn load_av_subsystems(&self) -> std::result::Result<Vec<AvSubsystemRecord>, Self::Error>;

    /// The node's `(recovery_streak_count, recovery_streak_start_ms)`, or `(0, 0)`
    /// if unregistered.
    fn load_recovery_streak(&self, node_id: &str) -> std::result::Result<(u32, u64), Self::Error>;

    /// Reset the streak counters AND stamp `last_telemetry_ms = now_ms` (used on
    /// receipt of a fault report — the report genuinely arrived "now"). No-op if
    /// unregistered.
    fn reset_recovery_streak(
        &self,
        node_id: &str,
        now_ms: u64,
    ) -> std::result::Result<(), Self::Error>;

    /// Reset the streak counters WITHOUT touching `last_telemetry_ms` (used by the
    /// watchdog on a SILENT node — no report arrived, so the silence clock must not
    /// be fabricated forward). No-op if unregistered.
    fn reset_recovery_streak_preserving_telemetry(
        &self,
        node_id: &str,
    ) -> std::result::Result<(), Self::Error>;

    /// Increment the streak (setting `start_ms = now_ms` only on the 0→1 edge) and
    /// stamp `last_telemetry_ms = now_ms`; returns the NEW count. Errors if the node
    /// is unregistered (there is no row to increment).
    fn increment_recovery_streak(
        &self,
        node_id: &str,
        now_ms: u64,
    ) -> std::result::Result<u32, Self::Error>;
}

/// The production SQLite backend: delegates to the inherent `VerifierStore` methods
/// over the `av_subsystem_meta` table. `self.method()` resolves to the INHERENT
/// method (inherent wins), so this is delegation, not recursion.
impl AvSubsystemStore for VerifierStore {
    type Error = rusqlite::Error;

    fn register_av_subsystem_meta(
        &self,
        node_id: &str,
        subsystem_type: &str,
        hardware_id: &str,
        confidence_floor: f64,
        initial_telemetry_ms: u64,
    ) -> Result<()> {
        self.register_av_subsystem_meta(
            node_id,
            subsystem_type,
            hardware_id,
            confidence_floor,
            initial_telemetry_ms,
        )
    }
    fn load_av_confidence_floor(&self, node_id: &str) -> Result<Option<f64>> {
        self.load_av_confidence_floor(node_id)
    }
    fn touch_av_telemetry_timestamp(&self, node_id: &str, now_ms: u64) -> Result<()> {
        self.touch_av_telemetry_timestamp(node_id, now_ms)
    }
    fn get_last_telemetry_timestamp(&self, node_id: &str) -> Result<u64> {
        self.get_last_telemetry_timestamp(node_id)
    }
    fn load_all_registered_av_node_ids(&self) -> Result<Vec<String>> {
        self.load_all_registered_av_node_ids()
    }
    fn load_av_subsystems(&self) -> Result<Vec<AvSubsystemRecord>> {
        self.load_av_subsystems()
    }
    fn load_recovery_streak(&self, node_id: &str) -> Result<(u32, u64)> {
        self.load_recovery_streak(node_id)
    }
    fn reset_recovery_streak(&self, node_id: &str, now_ms: u64) -> Result<()> {
        self.reset_recovery_streak(node_id, now_ms)
    }
    fn reset_recovery_streak_preserving_telemetry(&self, node_id: &str) -> Result<()> {
        self.reset_recovery_streak_preserving_telemetry(node_id)
    }
    fn increment_recovery_streak(&self, node_id: &str, now_ms: u64) -> Result<u32> {
        self.increment_recovery_streak(node_id, now_ms)
    }
}

/// The failure mode of the in-memory [`AvSubsystemStore`] backend — the one the
/// portable contract preserves across every backend (the SQLite backend surfaces
/// the same condition as a `rusqlite::Error`).
#[derive(Debug, PartialEq, Eq)]
pub enum InMemAvError {
    /// `increment_recovery_streak` was called on a node with no meta row (nothing
    /// to increment) — SQLite's trailing `SELECT` returns `QueryReturnedNoRows`.
    NodeNotRegistered,
}

#[derive(Debug, Clone)]
struct InMemoryAvRow {
    subsystem_type: String,
    hardware_id: String,
    confidence_floor: f64,
    last_telemetry_ms: u64,
    recovery_streak_count: u32,
    recovery_streak_start_ms: u64,
}

/// The in-memory [`AvSubsystemStore`] backend — a portability-proof reference
/// modelling the `av_subsystem_meta` table as a map keyed by `node_id`. Realizes
/// the SAME upsert / floor / telemetry-stamp / recovery-streak semantics WITHOUT a
/// database. `&self` throughout (interior `Mutex`, poison-recovered like
/// `InMemoryNodeStore`), so the conformance driver takes `&S`. Single-process.
#[derive(Debug, Default)]
pub struct InMemoryAvSubsystemStore {
    rows: std::sync::Mutex<std::collections::HashMap<String, InMemoryAvRow>>,
}

impl AvSubsystemStore for InMemoryAvSubsystemStore {
    type Error = InMemAvError;

    fn register_av_subsystem_meta(
        &self,
        node_id: &str,
        subsystem_type: &str,
        hardware_id: &str,
        confidence_floor: f64,
        initial_telemetry_ms: u64,
    ) -> std::result::Result<(), InMemAvError> {
        // INSERT OR REPLACE: a re-register overwrites the whole row, so the streak
        // counters fall back to their DEFAULT 0 (they are not in the write).
        self.rows.lock().unwrap_or_else(|e| e.into_inner()).insert(
            node_id.to_string(),
            InMemoryAvRow {
                subsystem_type: subsystem_type.to_string(),
                hardware_id: hardware_id.to_string(),
                confidence_floor,
                last_telemetry_ms: initial_telemetry_ms,
                recovery_streak_count: 0,
                recovery_streak_start_ms: 0,
            },
        );
        Ok(())
    }

    fn load_av_confidence_floor(
        &self,
        node_id: &str,
    ) -> std::result::Result<Option<f64>, InMemAvError> {
        Ok(self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(node_id)
            .map(|r| r.confidence_floor))
    }

    fn touch_av_telemetry_timestamp(
        &self,
        node_id: &str,
        now_ms: u64,
    ) -> std::result::Result<(), InMemAvError> {
        if let Some(row) = self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(node_id)
        {
            row.last_telemetry_ms = now_ms;
        }
        Ok(())
    }

    fn get_last_telemetry_timestamp(
        &self,
        node_id: &str,
    ) -> std::result::Result<u64, InMemAvError> {
        Ok(self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(node_id)
            .map(|r| r.last_telemetry_ms)
            .unwrap_or(0))
    }

    fn load_all_registered_av_node_ids(&self) -> std::result::Result<Vec<String>, InMemAvError> {
        Ok(self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect())
    }

    fn load_av_subsystems(&self) -> std::result::Result<Vec<AvSubsystemRecord>, InMemAvError> {
        let mut out: Vec<AvSubsystemRecord> = self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, r)| AvSubsystemRecord {
                node_id: id.clone(),
                subsystem_type: r.subsystem_type.clone(),
                hardware_id: r.hardware_id.clone(),
                confidence_floor: r.confidence_floor,
                last_telemetry_ms: r.last_telemetry_ms,
                recovery_streak_count: r.recovery_streak_count,
                recovery_streak_start_ms: r.recovery_streak_start_ms,
            })
            .collect();
        out.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        Ok(out)
    }

    fn load_recovery_streak(&self, node_id: &str) -> std::result::Result<(u32, u64), InMemAvError> {
        Ok(self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(node_id)
            .map(|r| (r.recovery_streak_count, r.recovery_streak_start_ms))
            .unwrap_or((0, 0)))
    }

    fn reset_recovery_streak(
        &self,
        node_id: &str,
        now_ms: u64,
    ) -> std::result::Result<(), InMemAvError> {
        if let Some(row) = self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(node_id)
        {
            row.recovery_streak_count = 0;
            row.recovery_streak_start_ms = 0;
            row.last_telemetry_ms = now_ms;
        }
        Ok(())
    }

    fn reset_recovery_streak_preserving_telemetry(
        &self,
        node_id: &str,
    ) -> std::result::Result<(), InMemAvError> {
        if let Some(row) = self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(node_id)
        {
            row.recovery_streak_count = 0;
            row.recovery_streak_start_ms = 0;
        }
        Ok(())
    }

    fn increment_recovery_streak(
        &self,
        node_id: &str,
        now_ms: u64,
    ) -> std::result::Result<u32, InMemAvError> {
        let mut guard = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        match guard.get_mut(node_id) {
            Some(row) => {
                // `start_ms` is set only on the 0→1 edge (matches the SQL CASE).
                if row.recovery_streak_count == 0 {
                    row.recovery_streak_start_ms = now_ms;
                }
                row.recovery_streak_count += 1;
                row.last_telemetry_ms = now_ms;
                Ok(row.recovery_streak_count)
            }
            None => Err(InMemAvError::NodeNotRegistered),
        }
    }
}

/// The AV-subsystem-meta contract, driven through the [`AvSubsystemStore`] trait so
/// it runs IDENTICALLY against every backend: empty defaults (absent → `None`/`0`/
/// `(0,0)`), register→read floor + telemetry, telemetry touch, the recovery-streak
/// lifecycle (increment sets `start` only on the 0→1 edge and advances telemetry;
/// both resets; the preserving reset keeps telemetry), increment-on-absent errors,
/// re-register resets the streak, and the ordered listing.
///
/// `pub` (not `#[cfg(test)]`) — the shared backend-conformance suite, run below
/// against the SQLite and in-memory backends. Panics on violation; call from a test.
/// PRECONDITION: `store` must start empty.
pub fn assert_av_subsystem_store_contract<S: AvSubsystemStore>(store: &S)
where
    S::Error: core::fmt::Debug,
{
    // Empty defaults.
    assert!(store.load_av_confidence_floor("n1").unwrap().is_none());
    assert_eq!(store.get_last_telemetry_timestamp("n1").unwrap(), 0);
    assert_eq!(store.load_recovery_streak("n1").unwrap(), (0, 0));
    assert!(store.load_all_registered_av_node_ids().unwrap().is_empty());
    assert!(store.load_av_subsystems().unwrap().is_empty());

    // Register + read back floor + telemetry; a fresh node starts with no streak.
    store
        .register_av_subsystem_meta("n1", "lidar", "hw-1", 0.8, 100)
        .unwrap();
    assert_eq!(store.load_av_confidence_floor("n1").unwrap(), Some(0.8));
    assert_eq!(store.get_last_telemetry_timestamp("n1").unwrap(), 100);
    assert_eq!(store.load_recovery_streak("n1").unwrap(), (0, 0));

    // Telemetry touch advances the stamp.
    store.touch_av_telemetry_timestamp("n1", 200).unwrap();
    assert_eq!(store.get_last_telemetry_timestamp("n1").unwrap(), 200);

    // Increment: start stamps only on the 0→1 edge; telemetry advances each time.
    assert_eq!(store.increment_recovery_streak("n1", 300).unwrap(), 1);
    assert_eq!(store.load_recovery_streak("n1").unwrap(), (1, 300));
    assert_eq!(store.increment_recovery_streak("n1", 400).unwrap(), 2);
    assert_eq!(
        store.load_recovery_streak("n1").unwrap(),
        (2, 300),
        "start unchanged after the 0→1 edge"
    );
    assert_eq!(store.get_last_telemetry_timestamp("n1").unwrap(), 400);

    // Preserving reset clears the streak but keeps telemetry.
    store
        .reset_recovery_streak_preserving_telemetry("n1")
        .unwrap();
    assert_eq!(store.load_recovery_streak("n1").unwrap(), (0, 0));
    assert_eq!(
        store.get_last_telemetry_timestamp("n1").unwrap(),
        400,
        "preserving reset keeps last_telemetry"
    );

    // Full reset clears the streak AND stamps telemetry = now.
    assert_eq!(store.increment_recovery_streak("n1", 500).unwrap(), 1);
    assert_eq!(store.load_recovery_streak("n1").unwrap(), (1, 500));
    store.reset_recovery_streak("n1", 600).unwrap();
    assert_eq!(store.load_recovery_streak("n1").unwrap(), (0, 0));
    assert_eq!(
        store.get_last_telemetry_timestamp("n1").unwrap(),
        600,
        "full reset stamps telemetry"
    );

    // Increment on an UNREGISTERED node errors (nothing to increment).
    assert!(
        store.increment_recovery_streak("ghost", 700).is_err(),
        "increment on an absent node must error"
    );
    assert!(
        store.load_av_confidence_floor("ghost").unwrap().is_none(),
        "the failed increment created no row"
    );

    // A second node; the id listing + record listing are complete and ordered.
    store
        .register_av_subsystem_meta("n2", "radar", "hw-2", 0.6, 50)
        .unwrap();
    let mut ids = store.load_all_registered_av_node_ids().unwrap();
    ids.sort();
    assert_eq!(ids, ["n1", "n2"]);
    let recs = store.load_av_subsystems().unwrap();
    assert_eq!(
        recs.iter().map(|r| r.node_id.as_str()).collect::<Vec<_>>(),
        ["n1", "n2"],
        "records ordered by node_id"
    );
    assert_eq!(recs[1].subsystem_type, "radar");
    assert_eq!(recs[1].confidence_floor, 0.6);

    // Re-register n1 (after building a streak) RESETS its streak to (0,0).
    store.increment_recovery_streak("n1", 800).unwrap();
    assert_eq!(store.load_recovery_streak("n1").unwrap().0, 1);
    store
        .register_av_subsystem_meta("n1", "lidar", "hw-1b", 0.9, 900)
        .unwrap();
    assert_eq!(
        store.load_recovery_streak("n1").unwrap(),
        (0, 0),
        "re-register resets the streak (INSERT OR REPLACE)"
    );
    assert_eq!(store.load_av_confidence_floor("n1").unwrap(), Some(0.9));
}

#[cfg(test)]
mod av_subsystem_store_contract_tests {
    use super::*;

    #[test]
    fn sqlite_backend_satisfies_the_av_subsystem_store_contract() {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        assert_av_subsystem_store_contract(&store);
    }

    #[test]
    fn in_memory_backend_satisfies_the_av_subsystem_store_contract() {
        assert_av_subsystem_store_contract(&InMemoryAvSubsystemStore::default());
    }
}
