//! `PgVerifierStore` — NodeStore seam (de-monolith split of lib.rs).
//!
//! Additional impl block(s); behaviour unchanged. Shared internals (`lock`,
//! `row_to_node`) are `pub(crate)` in the parent module.

use super::*;

const NODE_COLUMNS: &str = "node_id, status_json, registered_at_ms, last_trust_update_ms, \
                            ak_public_pem, expected_pcr16_digest_hex, site, firmware_version";

impl NodeStore for PgVerifierStore {
    type Error = PgStoreError;

    fn save_node(&self, node: &RegisteredNode) -> Result<(), PgStoreError> {
        let status_json = serde_json::to_string(&node.status).map_err(PgStoreError::Encode)?;
        self.lock().execute(
            "INSERT INTO nodes (node_id, status_json, registered_at_ms, last_trust_update_ms, \
                                ak_public_pem, expected_pcr16_digest_hex, site, firmware_version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (node_id) DO UPDATE SET \
                 status_json = EXCLUDED.status_json, \
                 registered_at_ms = EXCLUDED.registered_at_ms, \
                 last_trust_update_ms = EXCLUDED.last_trust_update_ms, \
                 ak_public_pem = EXCLUDED.ak_public_pem, \
                 expected_pcr16_digest_hex = EXCLUDED.expected_pcr16_digest_hex, \
                 site = EXCLUDED.site, \
                 firmware_version = EXCLUDED.firmware_version",
            &[
                &node.node_id,
                &status_json,
                &(node.registered_at_ms as i64),
                &(node.last_trust_update_ms as i64),
                &node.ak_public_pem,
                &node.expected_pcr16_digest_hex,
                &node.site,
                &node.firmware_version,
            ],
        )?;
        Ok(())
    }

    fn load_node(&self, node_id: &str) -> Result<Option<RegisteredNode>, PgStoreError> {
        let row = self.lock().query_opt(
            &format!("SELECT {NODE_COLUMNS} FROM nodes WHERE node_id = $1"),
            &[&node_id],
        )?;
        Ok(row.as_ref().map(Self::row_to_node))
    }

    fn load_nodes(&self) -> Result<Vec<RegisteredNode>, PgStoreError> {
        let rows = self
            .lock()
            .query(&format!("SELECT {NODE_COLUMNS} FROM nodes"), &[])?;
        Ok(rows.iter().map(Self::row_to_node).collect())
    }

    fn node_exists(&self, node_id: &str) -> Result<bool, PgStoreError> {
        let row = self
            .lock()
            .query_one("SELECT COUNT(*) FROM nodes WHERE node_id = $1", &[&node_id])?;
        Ok(row.get::<_, i64>(0) > 0)
    }

    fn count_nodes(&self) -> Result<i64, PgStoreError> {
        let row = self.lock().query_one("SELECT COUNT(*) FROM nodes", &[])?;
        Ok(row.get::<_, i64>(0))
    }
}
