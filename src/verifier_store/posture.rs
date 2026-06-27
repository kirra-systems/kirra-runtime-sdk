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
            params![node_id, event_type, posture_json, reason, created_at_ms as i64],
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

            let posture: serde_json::Value = serde_json::from_str(&posture_json)
                .unwrap_or(serde_json::Value::Null);

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
    pub fn load_posture_events_since(
        &self,
        since_ms: u64,
    ) -> Result<Vec<(u64, String)>> {
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
    pub fn count_posture_events_by_node_since(
        &self,
        since_ms: u64,
    ) -> Result<Vec<(String, u64)>> {
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

            let posture: serde_json::Value = serde_json::from_str(&posture_json)
                .unwrap_or(serde_json::Value::Null);

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

    /// Audit-chained posture-event insert. **All production posture-event
    /// writes MUST go through this function**; the non-chained inserter is
    /// `#[cfg(test)]`-only so events cannot bypass the audit chain. Writes
    /// the posture row and the corresponding `AuditChainLinker` entry in
    /// the same SQLite transaction so the chain is never partially updated.
    pub fn save_posture_event_chained(
        &mut self,
        node_id: &str,
        event_type: &str,
        posture_json: &str,
        reason: Option<&str>,
        created_at_ms: u64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO posture_events
             (node_id, event_type, posture_json, reason, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![node_id, event_type, posture_json, reason, created_at_ms as i64],
        )?;
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx, event_type, posture_json, created_at_ms as i64, self.signing_key.as_ref(),
        )?;
        tx.commit()
    }

    pub fn load_last_generation(&self) -> Result<u64> {
        match self.conn.query_row(
            "SELECT value FROM posture_engine_state WHERE key = 'last_generation'",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(s)  => Ok(s.parse::<u64>().unwrap_or(0)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e),
        }
    }

    pub fn save_last_generation(&self, generation: u64) -> Result<()> {
        // Monotonic max-write: never persist a generation lower than the one
        // already stored. Concurrent recalculations each claim a generation
        // then race to persist; a blind INSERT OR REPLACE let a slower thread
        // overwrite a higher value with its lower one, regressing the
        // cross-restart monotonicity that federation peers depend on.
        self.conn.execute(
            "INSERT INTO posture_engine_state (key, value)
             VALUES ('last_generation', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1
             WHERE CAST(?1 AS INTEGER) > CAST(value AS INTEGER)",
            params![generation.to_string()],
        )?;
        Ok(())
    }

    /// Reads an arbitrary key from the posture_engine_state key-value store.
    /// Returns None if the key doesn't exist.
    pub fn load_engine_state(&self, key: &str) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT value FROM posture_engine_state WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(v)  => Ok(Some(v)),
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
