// src/verifier_store.rs

use std::collections::HashMap;
use rusqlite::{params, Connection, Result};
use crate::verifier::{NodeTrustState, RegisteredNode};
use crate::federation::FederatedTrustReport;

pub struct VerifierStore {
    conn: Connection,
}

impl VerifierStore {
    pub fn new(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS nodes (
                node_id                    TEXT PRIMARY KEY,
                status_json                TEXT NOT NULL,
                registered_at_ms           INTEGER NOT NULL DEFAULT 0,
                last_trust_update_ms       INTEGER NOT NULL DEFAULT 0,
                ak_public_pem              TEXT,
                expected_pcr16_digest_hex  TEXT
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS dependencies (
                node_id  TEXT NOT NULL,
                dep_id   TEXT NOT NULL,
                PRIMARY KEY (node_id, dep_id)
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS posture_events (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id        TEXT    NOT NULL,
                event_type     TEXT    NOT NULL,
                posture_json   TEXT    NOT NULL,
                reason         TEXT,
                created_at_ms  INTEGER NOT NULL
            )",
            [],
        )?;

        // AV subsystem metadata: confidence floors, telemetry timestamps, recovery streak.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS av_subsystem_meta (
                node_id                  TEXT    PRIMARY KEY,
                subsystem_type           TEXT    NOT NULL,
                hardware_id              TEXT    NOT NULL,
                confidence_floor         REAL    NOT NULL DEFAULT 0.70,
                last_telemetry_ms        INTEGER NOT NULL DEFAULT 0,
                recovery_streak_count    INTEGER NOT NULL DEFAULT 0,
                recovery_streak_start_ms INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;

        // Posture engine persistent state (generation counter, heartbeat, etc.).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS posture_engine_state (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )?;

        Self::init_audit_chain_schema(&conn)?;

        Ok(Self { conn })
    }

    pub fn save_node(&self, node: &RegisteredNode) -> Result<()> {
        let status_json = serde_json::to_string(&node.status)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;

        self.conn.execute(
            "INSERT OR REPLACE INTO nodes
             (node_id, status_json, registered_at_ms, last_trust_update_ms,
              ak_public_pem, expected_pcr16_digest_hex)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                node.node_id,
                status_json,
                node.registered_at_ms as i64,
                node.last_trust_update_ms as i64,
                node.ak_public_pem,
                node.expected_pcr16_digest_hex,
            ],
        )?;

        Ok(())
    }

    pub fn load_nodes(&self) -> Result<Vec<RegisteredNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, status_json, registered_at_ms, last_trust_update_ms,
                    ak_public_pem, expected_pcr16_digest_hex
             FROM nodes",
        )?;

        let rows = stmt.query_map([], |row| {
            let status_json: String = row.get(1)?;
            let status: NodeTrustState = serde_json::from_str(&status_json)
                .unwrap_or(NodeTrustState::Unknown);

            Ok(RegisteredNode {
                node_id: row.get(0)?,
                status,
                registered_at_ms: row.get::<_, i64>(2)? as u64,
                last_trust_update_ms: row.get::<_, i64>(3)? as u64,
                ak_public_pem: row.get(4)?,
                expected_pcr16_digest_hex: row.get(5)?,
            })
        })?;

        rows.collect()
    }

    pub fn save_dependencies(&self, node_id: &str, deps: &[String]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM dependencies WHERE node_id = ?1",
            params![node_id],
        )?;

        for dep in deps {
            self.conn.execute(
                "INSERT OR REPLACE INTO dependencies (node_id, dep_id) VALUES (?1, ?2)",
                params![node_id, dep],
            )?;
        }

        Ok(())
    }

    pub fn load_dependencies(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut stmt = self.conn.prepare("SELECT node_id, dep_id FROM dependencies")?;
        let mut map: HashMap<String, Vec<String>> = HashMap::new();

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        for row in rows {
            let (node_id, dep_id) = row?;
            map.entry(node_id).or_default().push(dep_id);
        }

        Ok(map)
    }

    // --- v0.9.7 posture event log -------------------------------------------

    pub fn save_posture_event(
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

    // --- v0.9.8 HA probes & backup export ---

    pub fn health_check(&self) -> Result<()> {
        self.conn.query_row("SELECT 1", [], |_| Ok(()))
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

    // --- v1.1 tamper-evident audit chain ------------------------------------

    fn init_audit_chain_schema(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS audit_log_chain (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type        TEXT NOT NULL,
                event_json        TEXT NOT NULL,
                previous_hash_hex TEXT NOT NULL,
                record_hash_hex   TEXT NOT NULL,
                created_at_ms     INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS federated_trust_reports (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                source_controller_id TEXT NOT NULL,
                asset_id             TEXT NOT NULL,
                posture_json         TEXT NOT NULL,
                issued_at_ms         INTEGER NOT NULL,
                expires_at_ms        INTEGER NOT NULL,
                received_at_ms       INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS trusted_federation_controllers (
                controller_id    TEXT PRIMARY KEY,
                public_key_b64   TEXT NOT NULL,
                registered_at_ms INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS federation_report_nonces (
                nonce_hex            TEXT PRIMARY KEY,
                source_controller_id TEXT NOT NULL,
                seen_at_ms           INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS attestation_identity_registry (
                node_id                    TEXT PRIMARY KEY,
                ak_public_fingerprint_hex  TEXT NOT NULL,
                registered_at_ms           INTEGER NOT NULL,
                registration_source        TEXT NOT NULL
            )",
            [],
        )?;
        Ok(())
    }

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
            &tx, event_type, posture_json, created_at_ms as i64,
        )?;
        tx.commit()
    }

    pub fn save_federated_report_chained(
        &mut self,
        report: &FederatedTrustReport,
        received_at_ms: u64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;

        let posture_json = serde_json::to_string(&report.posture)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;

        tx.execute(
            "INSERT INTO federated_trust_reports
             (source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, received_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                report.source_controller_id, report.asset_id, posture_json,
                report.issued_at_ms as i64, report.expires_at_ms as i64, received_at_ms as i64,
            ],
        )?;

        tx.execute(
            "INSERT INTO federation_report_nonces (nonce_hex, source_controller_id, seen_at_ms)
             VALUES (?1, ?2, ?3)",
            params![report.nonce_hex, report.source_controller_id, received_at_ms as i64],
        )?;

        let audit = serde_json::json!({
            "source_controller_id": report.source_controller_id,
            "asset_id": report.asset_id,
            "posture": posture_json,
            "issued_at_ms": report.issued_at_ms,
            "expires_at_ms": report.expires_at_ms,
            "nonce_hex": report.nonce_hex,
            "received_at_ms": received_at_ms,
        });
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "FEDERATED_TRUST_REPORT_ACCEPTED",
            &audit.to_string(),
            received_at_ms as i64,
        )?;

        tx.commit()
    }

    // --- v1.1 trusted federation controller registry ------------------------

    pub fn save_trusted_federation_controller(
        &self,
        controller_id: &str,
        public_key_b64: &str,
        registered_at_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO trusted_federation_controllers
             (controller_id, public_key_b64, registered_at_ms)
             VALUES (?1, ?2, ?3)",
            params![controller_id, public_key_b64, registered_at_ms as i64],
        )?;
        Ok(())
    }

    pub fn load_trusted_federation_controller_key(
        &self,
        controller_id: &str,
    ) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT public_key_b64 FROM trusted_federation_controllers
             WHERE controller_id = ?1",
        )?;
        match stmt.query_row(params![controller_id], |row| row.get::<_, String>(0)) {
            Ok(key) => Ok(Some(key)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn has_seen_federation_nonce(&self, nonce_hex: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM federation_report_nonces WHERE nonce_hex = ?1",
            params![nonce_hex],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn load_federated_reports_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms
             FROM federated_trust_reports
             WHERE asset_id = ?1
             ORDER BY received_at_ms DESC",
        )?;
        let rows = stmt.query_map(params![asset_id], |row| {
            let source: String = row.get(0)?;
            let aid: String = row.get(1)?;
            let posture_json: String = row.get(2)?;
            let issued: i64 = row.get(3)?;
            let expires: i64 = row.get(4)?;
            Ok(serde_json::json!({
                "source_controller_id": source,
                "asset_id": aid,
                "posture": posture_json,
                "issued_at_ms": issued as u64,
                "expires_at_ms": expires as u64,
            }))
        })?;
        rows.collect()
    }

    pub fn verify_audit_chain_integrity(&self) -> Result<bool> {
        let mut stmt = self.conn.prepare(
            "SELECT event_json, previous_hash_hex, record_hash_hex, created_at_ms
             FROM audit_log_chain
             ORDER BY id ASC",
        )?;

        let mut expected_previous_hash = "0".repeat(64);
        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let event_json: String = row.get(0)?;
            let previous_hash_hex: String = row.get(1)?;
            let record_hash_hex: String = row.get(2)?;
            let created_at_ms: i64 = row.get(3)?;

            if previous_hash_hex != expected_previous_hash {
                return Ok(false);
            }
            let recalc = crate::audit_chain::AuditChainLinker::compute_record_hash(
                &previous_hash_hex,
                &event_json,
                created_at_ms,
            );
            if recalc != record_hash_hex {
                return Ok(false);
            }
            expected_previous_hash = record_hash_hex;
        }

        Ok(true)
    }

    // --- Patch 1: attestation identity registry ----------------------------

    pub fn register_attestation_identity(
        &mut self,
        node_id: &str,
        fingerprint_hex: &str,
        source: &str,
        registered_at_ms: u64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;

        tx.execute(
            "INSERT OR REPLACE INTO attestation_identity_registry
             (node_id, ak_public_fingerprint_hex, registered_at_ms, registration_source)
             VALUES (?1, ?2, ?3, ?4)",
            params![node_id, fingerprint_hex, registered_at_ms as i64, source],
        )?;

        let audit_payload = serde_json::json!({
            "node_id": node_id,
            "ak_public_fingerprint_hex": fingerprint_hex,
            "registration_source": source,
            "registered_at_ms": registered_at_ms,
        });
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "NODE_IDENTITY_REGISTERED",
            &audit_payload.to_string(),
            registered_at_ms as i64,
        )?;

        tx.commit()
    }

    pub fn load_registered_fingerprint(&self, node_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT ak_public_fingerprint_hex FROM attestation_identity_registry
             WHERE node_id = ?1",
        )?;
        match stmt.query_row(params![node_id], |row| row.get::<_, String>(0)) {
            Ok(fp) => Ok(Some(fp)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    // --- AV subsystem metadata ---------------------------------------------

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
            params![node_id, subsystem_type, hardware_id, confidence_floor, initial_telemetry_ms as i64],
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

    // --- Posture engine persistent state -----------------------------------

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
        self.conn.execute(
            "INSERT OR REPLACE INTO posture_engine_state (key, value)
             VALUES ('last_generation', ?1)",
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

#[cfg(test)]
mod attestation_registry_tests {
    use super::*;

    fn in_memory() -> VerifierStore {
        VerifierStore::new(":memory:").unwrap()
    }

    #[test]
    fn test_register_and_load_fingerprint() {
        let mut store = in_memory();
        let fp = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(store.register_attestation_identity("node-01", fp, "admin", 1_000).is_ok());
        assert_eq!(store.load_registered_fingerprint("node-01").unwrap(), Some(fp.to_string()));
    }

    #[test]
    fn test_load_fingerprint_missing_node_returns_none() {
        let store = in_memory();
        assert_eq!(store.load_registered_fingerprint("ghost-node").unwrap(), None);
    }

    #[test]
    fn test_identity_registration_chains_audit_entry() {
        let mut store = in_memory();
        let fp = "abc123def456";
        store.register_attestation_identity("node-02", fp, "admin", 2_000).unwrap();
        assert!(store.verify_audit_chain_integrity().unwrap());
    }

    #[test]
    fn test_identity_registration_is_idempotent_on_rotate() {
        let mut store = in_memory();
        let fp1 = "aaaa";
        let fp2 = "bbbb";
        store.register_attestation_identity("node-03", fp1, "admin", 1_000).unwrap();
        store.register_attestation_identity("node-03", fp2, "admin", 2_000).unwrap();
        assert_eq!(store.load_registered_fingerprint("node-03").unwrap(), Some(fp2.to_string()));
        assert!(store.verify_audit_chain_integrity().unwrap());
    }

    #[test]
    fn test_av_subsystem_meta_round_trip() {
        let store = in_memory();
        store.register_av_subsystem_meta("lidar_front", "Perception", "LIDAR-001", 0.70, 0).unwrap();
        let floor = store.load_av_confidence_floor("lidar_front").unwrap();
        assert_eq!(floor, Some(0.70));
    }

    #[test]
    fn test_recovery_streak_increments_and_resets() {
        let store = in_memory();
        store.register_av_subsystem_meta("cam", "Perception", "CAM-001", 0.70, 0).unwrap();
        let n1 = store.increment_recovery_streak("cam", 1000).unwrap();
        let n2 = store.increment_recovery_streak("cam", 1100).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 2);
        store.reset_recovery_streak("cam", 1200).unwrap();
        let (count, start) = store.load_recovery_streak("cam").unwrap();
        assert_eq!(count, 0);
        assert_eq!(start, 0);
    }

    #[test]
    fn test_generation_persistence() {
        let store = in_memory();
        assert_eq!(store.load_last_generation().unwrap(), 0);
        store.save_last_generation(42).unwrap();
        assert_eq!(store.load_last_generation().unwrap(), 42);
    }
}

#[cfg(test)]
mod standby_store_tests {
    use super::*;

    fn in_memory() -> VerifierStore {
        VerifierStore::new(":memory:").unwrap()
    }

    #[test]
    fn test_load_engine_state_absent_key_returns_none() {
        let store = in_memory();
        assert_eq!(store.load_engine_state("nonexistent_key").unwrap(), None);
    }

    #[test]
    fn test_save_and_load_engine_state_round_trip() {
        let store = in_memory();
        store.save_engine_state("primary_heartbeat_ms", "12345").unwrap();
        let val = store.load_engine_state("primary_heartbeat_ms").unwrap();
        assert_eq!(val, Some("12345".to_string()));
    }

    #[test]
    fn test_save_engine_state_is_idempotent_upsert() {
        let store = in_memory();
        store.save_engine_state("key", "first").unwrap();
        store.save_engine_state("key", "second").unwrap();
        assert_eq!(store.load_engine_state("key").unwrap(), Some("second".to_string()));
    }

    #[test]
    fn test_multiple_keys_are_independent() {
        let store = in_memory();
        store.save_engine_state("key_a", "value_a").unwrap();
        store.save_engine_state("key_b", "value_b").unwrap();
        assert_eq!(store.load_engine_state("key_a").unwrap(), Some("value_a".to_string()));
        assert_eq!(store.load_engine_state("key_b").unwrap(), Some("value_b".to_string()));
    }

    #[test]
    fn test_heartbeat_age_parse_from_stored_string() {
        let store = in_memory();
        let ts: u64 = 1_700_000_000_000;
        store.save_engine_state("primary_heartbeat_ms", &ts.to_string()).unwrap();
        let loaded = store.load_engine_state("primary_heartbeat_ms").unwrap().unwrap();
        let parsed: u64 = loaded.parse().expect("must parse as u64");
        assert_eq!(parsed, ts);
    }
}
