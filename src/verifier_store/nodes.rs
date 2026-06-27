// src/verifier_store/nodes.rs
// nodes domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    pub fn save_node(&self, node: &RegisteredNode) -> Result<()> {
        let status_json = serde_json::to_string(&node.status)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;

        self.conn.execute(
            "INSERT OR REPLACE INTO nodes
             (node_id, status_json, registered_at_ms, last_trust_update_ms,
              ak_public_pem, expected_pcr16_digest_hex, site, firmware_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                node.node_id,
                status_json,
                node.registered_at_ms as i64,
                node.last_trust_update_ms as i64,
                node.ak_public_pem,
                node.expected_pcr16_digest_hex,
                node.site,
                node.firmware_version,
            ],
        )?;

        Ok(())
    }

    pub fn load_nodes(&self) -> Result<Vec<RegisteredNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, status_json, registered_at_ms, last_trust_update_ms,
                    ak_public_pem, expected_pcr16_digest_hex, site, firmware_version
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
                site: row.get(6)?,
                firmware_version: row.get(7)?,
            })
        })?;

        rows.collect()
    }

    /// Load a single registered node by id, or `None` if unregistered. Additive
    /// single-row loader (mirrors [`load_operator`] / `load_trusted_federation_controller_key`)
    /// — the targeted lookup [`crate::key_registry::KeyRegistry`] uses to resolve a
    /// node's `ak_public_pem` without scanning the whole registry.
    pub fn load_node(&self, node_id: &str) -> Result<Option<RegisteredNode>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT node_id, status_json, registered_at_ms, last_trust_update_ms,
                        ak_public_pem, expected_pcr16_digest_hex, site, firmware_version
                 FROM nodes WHERE node_id = ?1",
                params![node_id],
                |row| {
                    let status_json: String = row.get(1)?;
                    let status: NodeTrustState =
                        serde_json::from_str(&status_json).unwrap_or(NodeTrustState::Unknown);
                    Ok(RegisteredNode {
                        node_id: row.get(0)?,
                        status,
                        registered_at_ms: row.get::<_, i64>(2)? as u64,
                        last_trust_update_ms: row.get::<_, i64>(3)? as u64,
                        ak_public_pem: row.get(4)?,
                        expected_pcr16_digest_hex: row.get(5)?,
                        site: row.get(6)?,
                        firmware_version: row.get(7)?,
                    })
                },
            )
            .optional()
    }

    /// Persist a node's attestation policy (TPM-quote follow-up to #572).
    /// `INSERT OR REPLACE` so re-registration reflects the operator's current
    /// intent (including flipping the requirement back off). Isolated from the
    /// `nodes` identity record by design — see the table comment.
    pub fn set_node_attestation_policy(&self, node_id: &str, require_tpm_quote: bool) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO node_attestation_policy (node_id, require_tpm_quote)
             VALUES (?1, ?2)",
            params![node_id, require_tpm_quote as i64],
        )?;
        Ok(())
    }

    /// Whether the node requires a hardware TPM quote on `/attestation/verify`.
    /// An absent row → `false` (a node that never opted in / back-compat). The
    /// CALL SITE must treat a store error as fail-closed (cannot prove → reject).
    pub fn node_requires_tpm_quote(&self, node_id: &str) -> Result<bool> {
        use rusqlite::OptionalExtension;
        let v: Option<i64> = self
            .conn
            .query_row(
                "SELECT require_tpm_quote FROM node_attestation_policy WHERE node_id = ?1",
                params![node_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(v.unwrap_or(0) != 0)
    }

    pub fn save_dependencies(&self, node_id: &str, deps: &[String]) -> Result<()> {
        // Atomic replace: the DELETE and the re-INSERTs must commit together, or
        // a mid-loop failure leaves a torn dependency set (some edges dropped,
        // not all re-added) — a corrupt DAG for the next posture calculation.
        // unchecked_transaction works on &self (matches the other writers here).
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM dependencies WHERE node_id = ?1",
            params![node_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO dependencies (node_id, dep_id) VALUES (?1, ?2)",
            )?;
            for dep in deps {
                stmt.execute(params![node_id, dep])?;
            }
        }
        tx.commit()?;
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

    /// True iff `node_id` is a registered node — clearance-grant well-formedness
    /// (operator-console Phase A; mirrors `OperatorClearanceGrant::is_well_formed`'s
    /// "the node must exist" half).
    pub fn node_exists(&self, node_id: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE node_id = ?1",
            params![node_id],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }
}
