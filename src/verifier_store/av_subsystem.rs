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
