// src/verifier_store.rs

use std::collections::HashMap;
use rusqlite::{params, Connection, Result};
use crate::verifier::{NodeTrustState, RegisteredNode};

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
                // Nonces are purposely ephemeral — not persisted, reset to None on boot.
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
}
