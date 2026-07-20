// src/verifier_store/nodes.rs
// nodes domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    /// TEST-ONLY: overwrite a node's stored `registered_at_ms` BIGINT with an
    /// arbitrary raw value (e.g. a NEGATIVE one that the type-safe `u64` API can
    /// never produce) to exercise the fail-closed read heal — a corrupt/tampered
    /// negative timestamp must read back as `0`, never a wrapped huge `u64`.
    /// Never compiled into production builds.
    #[cfg(any(test, feature = "test-support"))]
    pub fn force_node_registered_at_ms_for_test(&self, node_id: &str, raw: i64) {
        self.conn
            .execute(
                "UPDATE nodes SET registered_at_ms = ?1 WHERE node_id = ?2",
                params![raw, node_id],
            )
            .expect("test seam: force node registered_at_ms");
    }

    pub fn save_node(&self, node: &RegisteredNode) -> Result<()> {
        let status_json =
            serde_json::to_string(&node.status).map_err(|_| rusqlite::Error::InvalidQuery)?;

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
            let status: NodeTrustState =
                serde_json::from_str(&status_json).unwrap_or(NodeTrustState::Unknown);

            Ok(RegisteredNode {
                node_id: row.get(0)?,
                status,
                registered_at_ms: row.get::<_, i64>(2)?.max(0) as u64,
                last_trust_update_ms: row.get::<_, i64>(3)?.max(0) as u64,
                ak_public_pem: row.get(4)?,
                expected_pcr16_digest_hex: row.get(5)?,
                site: row.get(6)?,
                firmware_version: row.get(7)?,
            })
        })?;

        rows.collect()
    }

    /// Load a single registered node by id, or `None` if unregistered. Additive
    /// single-row loader (mirrors `load_operator` / `load_trusted_federation_controller_key`)
    /// — the targeted lookup `KeyRegistry` (root crate) uses to resolve a
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
                        registered_at_ms: row.get::<_, i64>(2)?.max(0) as u64,
                        last_trust_update_ms: row.get::<_, i64>(3)?.max(0) as u64,
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
    pub fn set_node_attestation_policy(
        &self,
        node_id: &str,
        require_tpm_quote: bool,
    ) -> Result<()> {
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
            let mut stmt = tx
                .prepare("INSERT OR REPLACE INTO dependencies (node_id, dep_id) VALUES (?1, ?2)")?;
            for dep in deps {
                stmt.execute(params![node_id, dep])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// C5 (#1036): epoch-fenced node upsert. Same write as [`save_node`], but the
    /// INSERT rides inside an `Immediate` transaction whose FIRST statement
    /// re-asserts `held_epoch` against the durable `ha_state` row (as
    /// `save_federated_report_chained` / `assert_actuator_epoch_held` do for their
    /// tiers). A just-superseded primary keeps `mode_active == true` (and its stale
    /// cached epoch) for up to one heartbeat; without an in-transaction fence its
    /// node re-registration could overwrite a row the new Active just changed —
    /// trust-registry corruption. `Immediate` takes the WAL write lock at `BEGIN`,
    /// so a concurrent `try_claim_epoch` (on the FULL connection) serializes at the
    /// same file-level lock and cannot interleave between the fence read and this
    /// write. Fail-closed: `held == 0`, epoch mismatch, or an unreadable `ha_state`
    /// rolls the transaction back and writes nothing.
    pub fn save_node_epoch_fenced(
        &mut self,
        node: &RegisteredNode,
        held_epoch: u64,
    ) -> std::result::Result<(), DurableWriteError> {
        let status_json =
            serde_json::to_string(&node.status).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // #79 HA epoch fence — FIRST statement, before the mutation.
        Self::assert_epoch_held(&tx, held_epoch)?;
        tx.execute(
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
        tx.commit()?;
        Ok(())
    }

    /// Sec9 (#1050): register a node AND its attestation policy in ONE
    /// transaction. `register_node` previously wrote the TPM-quote policy and the
    /// node record as two separate durable writes — a correct fail-closed ORDER
    /// (policy first, so a quote-required node is never live without its
    /// requirement) but NOT atomic: a crash between them could leave a policy row
    /// with no node. Folding both INSERT-OR-REPLACEs into one transaction makes
    /// registration all-or-nothing. Plain variant (no epoch fence) for a
    /// never-claimed store (`held == 0`; see `AppState::persist_node_row`).
    pub fn save_node_with_policy(
        &self,
        node: &RegisteredNode,
        require_tpm_quote: bool,
    ) -> Result<()> {
        let status_json =
            serde_json::to_string(&node.status).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let tx = self.conn.unchecked_transaction()?;
        Self::insert_node_and_policy_tx(&tx, node, &status_json, require_tpm_quote)?;
        tx.commit()?;
        Ok(())
    }

    /// Sec9 (#1050): the epoch-fenced sibling of [`save_node_with_policy`] — the
    /// same atomic node+policy write, preceded in the SAME `Immediate` transaction
    /// by the `held_epoch` fence (C5 #1036 semantics; a superseded primary is
    /// rejected before it can write either row). Fail-closed like
    /// [`save_node_epoch_fenced`].
    pub fn save_node_with_policy_epoch_fenced(
        &mut self,
        node: &RegisteredNode,
        require_tpm_quote: bool,
        held_epoch: u64,
    ) -> std::result::Result<(), DurableWriteError> {
        let status_json =
            serde_json::to_string(&node.status).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::assert_epoch_held(&tx, held_epoch)?;
        Self::insert_node_and_policy_tx(&tx, node, &status_json, require_tpm_quote)?;
        tx.commit()?;
        Ok(())
    }

    /// Shared body for the Sec9 combined write: the node INSERT-OR-REPLACE + the
    /// attestation-policy INSERT-OR-REPLACE, both inside the caller's transaction
    /// (so they commit together). Node first, policy second — but atomicity, not
    /// order, is the guarantee here (either both land or neither does).
    fn insert_node_and_policy_tx(
        tx: &rusqlite::Transaction<'_>,
        node: &RegisteredNode,
        status_json: &str,
        require_tpm_quote: bool,
    ) -> Result<()> {
        tx.execute(
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
        tx.execute(
            "INSERT OR REPLACE INTO node_attestation_policy (node_id, require_tpm_quote)
             VALUES (?1, ?2)",
            params![node.node_id, require_tpm_quote as i64],
        )?;
        Ok(())
    }

    /// C5 (#1036): epoch-fenced dependency-set replace. The atomic
    /// DELETE-then-re-INSERT of [`save_dependencies`], preceded in the SAME
    /// `Immediate` transaction by the `held_epoch` fence — a superseded primary can
    /// no longer rewrite the dependency graph out from under the new Active. Same
    /// fail-closed semantics as [`save_node_epoch_fenced`].
    pub fn save_dependencies_epoch_fenced(
        &mut self,
        node_id: &str,
        deps: &[String],
        held_epoch: u64,
    ) -> std::result::Result<(), DurableWriteError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::assert_epoch_held(&tx, held_epoch)?;
        tx.execute(
            "DELETE FROM dependencies WHERE node_id = ?1",
            params![node_id],
        )?;
        {
            let mut stmt = tx
                .prepare("INSERT OR REPLACE INTO dependencies (node_id, dep_id) VALUES (?1, ?2)")?;
            for dep in deps {
                stmt.execute(params![node_id, dep])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_dependencies(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT node_id, dep_id FROM dependencies")?;
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

    /// Number of nodes registered in the durable registry.
    ///
    /// Cheap `COUNT(*)` (no row materialization, unlike `load_nodes`). Used by
    /// the posture engine's M-9 empty-live-set guard to distinguish a genuinely
    /// empty fleet (`0` here too) from a hydration/consistency gap (the in-memory
    /// `app.nodes` is empty while the durable registry still holds nodes) — both
    /// fail closed, but the reason code differs for operators.
    pub fn count_nodes(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
    }
}

// ---------------------------------------------------------------------------
// WP-18 (G-9 store half) — the node-registry storage trait
//
// The SECOND `VerifierStorage`-family trait lifted off `VerifierStore` (after
// `EpochFence`): the node IDENTITY registry CRUD. Same discipline as the epoch
// fence — the trait shares the inherent method names, inherent methods win
// resolution (so every existing `store.save_node(...)` / `load_node(...)` caller
// is untouched and the SQLite impl delegates via `self.method()` WITHOUT
// recursion), and a second in-memory backend + a shared conformance test prove
// the contract is genuinely backend-portable (SQLite realizes it over the `nodes`
// table; EP-10's `crates/kirra-verifier-pg` realizes it over the same schema on
// a LIVE Postgres server, running this same conformance suite in CI).
// ---------------------------------------------------------------------------

/// The node-registry storage contract — persist a registered node and read it
/// back by id / in bulk / by existence / by count. Backend-agnostic identity CRUD.
pub trait NodeStore {
    /// Backend error type (SQLite: `rusqlite::Error`; in-memory: [`std::convert::Infallible`]).
    type Error;

    /// Upsert a node by `node_id` (INSERT-OR-REPLACE semantics — re-saving the
    /// same id overwrites, never duplicates).
    fn save_node(&self, node: &RegisteredNode) -> std::result::Result<(), Self::Error>;

    /// Load one node by id, or `None` if unregistered.
    fn load_node(&self, node_id: &str) -> std::result::Result<Option<RegisteredNode>, Self::Error>;

    /// Load every registered node.
    fn load_nodes(&self) -> std::result::Result<Vec<RegisteredNode>, Self::Error>;

    /// Is `node_id` registered?
    fn node_exists(&self, node_id: &str) -> std::result::Result<bool, Self::Error>;

    /// How many nodes are registered.
    fn count_nodes(&self) -> std::result::Result<i64, Self::Error>;
}

/// The production SQLite backend: delegates to the inherent `VerifierStore`
/// methods over the `nodes` table. `self.method()` resolves to the INHERENT
/// method (inherent wins over the trait), so this is delegation, not recursion.
impl NodeStore for VerifierStore {
    type Error = rusqlite::Error;

    fn save_node(&self, node: &RegisteredNode) -> Result<()> {
        self.save_node(node)
    }
    fn load_node(&self, node_id: &str) -> Result<Option<RegisteredNode>> {
        self.load_node(node_id)
    }
    fn load_nodes(&self) -> Result<Vec<RegisteredNode>> {
        self.load_nodes()
    }
    fn node_exists(&self, node_id: &str) -> Result<bool> {
        self.node_exists(node_id)
    }
    fn count_nodes(&self) -> Result<i64> {
        self.count_nodes()
    }
}

/// The in-memory [`NodeStore`] backend — a portability-proof reference modelling
/// the `nodes` table as a map keyed by `node_id`. Realizes the SAME upsert / load
/// / count semantics WITHOUT a database, so the registry contract is exercised
/// against two backends. Interior mutability (the trait's methods are `&self`,
/// matching the SQLite `Connection`'s `&self` writes). Single-process only.
///
/// `Error = Infallible` is honest, not a shortcut: every method RECOVERS from a
/// poisoned `Mutex` (`lock().unwrap_or_else(PoisonError::into_inner)`) rather than
/// unwrapping, so a panic in another thread while holding the lock can never make
/// a `NodeStore` op panic — the map is a plain `HashMap` with no cross-call
/// invariant a torn write could break, so the recovered data is safe to use.
#[derive(Debug, Default)]
pub struct InMemoryNodeStore {
    nodes: std::sync::Mutex<std::collections::HashMap<String, RegisteredNode>>,
}

impl NodeStore for InMemoryNodeStore {
    type Error = std::convert::Infallible;

    fn save_node(
        &self,
        node: &RegisteredNode,
    ) -> std::result::Result<(), std::convert::Infallible> {
        self.nodes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(node.node_id.clone(), node.clone());
        Ok(())
    }
    fn load_node(
        &self,
        node_id: &str,
    ) -> std::result::Result<Option<RegisteredNode>, std::convert::Infallible> {
        Ok(self
            .nodes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(node_id)
            .cloned())
    }
    fn load_nodes(&self) -> std::result::Result<Vec<RegisteredNode>, std::convert::Infallible> {
        Ok(self
            .nodes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect())
    }
    fn node_exists(&self, node_id: &str) -> std::result::Result<bool, std::convert::Infallible> {
        Ok(self
            .nodes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(node_id))
    }
    fn count_nodes(&self) -> std::result::Result<i64, std::convert::Infallible> {
        Ok(self.nodes.lock().unwrap_or_else(|e| e.into_inner()).len() as i64)
    }
}

/// The node-registry contract, driven through the [`NodeStore`] trait so it runs
/// IDENTICALLY against every backend: empty reads, save→load roundtrip (id +
/// status preserved), existence + count, bulk load, and the UPSERT invariant
/// (re-saving an id overwrites, never duplicates).
///
/// `pub` (not `#[cfg(test)]`) by design: this is the shared backend-conformance
/// suite. The in-crate tests below run it against the SQLite and in-memory
/// backends; an external backend crate (EP-10: `crates/kirra-verifier-pg`
/// against a live Postgres server) runs the SAME function, so every backend is
/// held to the identical contract. Panics on any violation (assert-based) —
/// call it from a test.
///
/// PRECONDITION: `store` must start empty.
pub fn assert_node_store_contract<S: NodeStore>(store: &S)
where
    S::Error: core::fmt::Debug,
{
    fn node(id: &str, status: NodeTrustState) -> RegisteredNode {
        RegisteredNode {
            node_id: id.to_string(),
            status,
            registered_at_ms: 1,
            last_trust_update_ms: 0,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
            site: None,
            firmware_version: None,
        }
    }
    // Empty registry.
    assert_eq!(store.count_nodes().unwrap(), 0);
    assert!(store.load_node("n1").unwrap().is_none());
    assert!(!store.node_exists("n1").unwrap());

    // Save + read back (id + status preserved).
    store
        .save_node(&node("n1", NodeTrustState::Trusted))
        .unwrap();
    assert!(store.node_exists("n1").unwrap());
    assert_eq!(store.count_nodes().unwrap(), 1);
    let loaded = store.load_node("n1").unwrap().expect("n1 present");
    assert_eq!(loaded.node_id, "n1");
    assert_eq!(loaded.status, NodeTrustState::Trusted);

    // A second node; bulk load sees both.
    store
        .save_node(&node("n2", NodeTrustState::Unknown))
        .unwrap();
    assert_eq!(store.count_nodes().unwrap(), 2);
    let ids: Vec<String> = store
        .load_nodes()
        .unwrap()
        .into_iter()
        .map(|n| n.node_id)
        .collect();
    assert!(ids.contains(&"n1".to_string()) && ids.contains(&"n2".to_string()));

    // UPSERT: re-saving n1 with a new status overwrites, count stays 2.
    store
        .save_node(&node("n1", NodeTrustState::Untrusted("fault".to_string())))
        .unwrap();
    assert_eq!(store.count_nodes().unwrap(), 2, "upsert must not duplicate");
    assert_eq!(
        store.load_node("n1").unwrap().unwrap().status,
        NodeTrustState::Untrusted("fault".to_string())
    );

    // An unregistered id stays absent.
    assert!(store.load_node("ghost").unwrap().is_none());
}

#[cfg(test)]
mod node_store_contract_tests {
    use super::*;

    #[test]
    fn sqlite_backend_satisfies_the_node_store_contract() {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        assert_node_store_contract(&store);
    }

    #[test]
    fn in_memory_backend_satisfies_the_node_store_contract() {
        assert_node_store_contract(&InMemoryNodeStore::default());
    }
}

#[cfg(test)]
mod epoch_fenced_write_tests {
    // C5 (#1036): the in-transaction epoch fence on the durable node/dependency
    // writes. A superseded primary (stale `held_epoch < durable`) is rejected
    // before it can corrupt the shared trust registry; the holder writes.
    use super::*;

    fn node(id: &str) -> RegisteredNode {
        RegisteredNode {
            node_id: id.to_string(),
            status: NodeTrustState::Trusted,
            registered_at_ms: 1,
            last_trust_update_ms: 0,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
            site: None,
            firmware_version: None,
        }
    }

    fn superseded(e: &DurableWriteError) -> bool {
        matches!(
            e,
            DurableWriteError::Fenced(FenceError::EpochSuperseded { .. })
        )
    }

    #[test]
    fn superseded_primary_node_write_is_rejected_and_leaves_no_row() {
        let mut store = VerifierStore::new(":memory:").expect("store");
        // Two claims: durable epoch advances 0 -> 1 -> 2. The old primary that
        // claimed epoch 1 is now superseded (durable == 2).
        store.try_claim_epoch(0, "A", 1).unwrap();
        store.try_claim_epoch(1, "B", 2).unwrap();

        // Holder (held == durable == 2) writes.
        store
            .save_node_epoch_fenced(&node("holder-write"), 2)
            .expect("holder write admitted");
        assert!(store.node_exists("holder-write").unwrap());

        // Superseded primary (held == 1 < durable == 2) is rejected, fail-closed:
        // no row lands.
        let err = store
            .save_node_epoch_fenced(&node("stale-write"), 1)
            .expect_err("superseded write rejected");
        assert!(superseded(&err), "expected EpochSuperseded, got {err:?}");
        assert!(
            !store.node_exists("stale-write").unwrap(),
            "a fenced-out write must leave NO row (fail-closed)"
        );

        // A never-claimed process (held == 0) is also rejected by the fenced path.
        assert!(store.save_node_epoch_fenced(&node("zero"), 0).is_err());
        assert!(!store.node_exists("zero").unwrap());
    }

    #[test]
    fn superseded_primary_dependency_write_is_rejected_and_leaves_no_edges() {
        let mut store = VerifierStore::new(":memory:").expect("store");
        store.try_claim_epoch(0, "A", 1).unwrap();
        store.try_claim_epoch(1, "B", 2).unwrap();

        // Holder writes a dependency set.
        store
            .save_dependencies_epoch_fenced("b", &["a".to_string()], 2)
            .expect("holder dep write admitted");
        assert_eq!(
            store.load_dependencies().unwrap().get("b").unwrap().len(),
            1
        );

        // Superseded primary cannot rewrite the graph.
        let err = store
            .save_dependencies_epoch_fenced("c", &["a".to_string()], 1)
            .expect_err("superseded dep write rejected");
        assert!(superseded(&err), "expected EpochSuperseded, got {err:?}");
        assert!(
            store.load_dependencies().unwrap().get("c").is_none(),
            "a fenced-out dependency write must leave NO edges (fail-closed)"
        );
    }

    // Sec9 (#1050): the node record and its attestation policy are written
    // atomically — both land, or (when fenced out) neither does.
    #[test]
    fn node_and_attestation_policy_are_written_and_fenced_together() {
        let mut store = VerifierStore::new(":memory:").expect("store");

        // Plain path (never-claimed store): both the node row AND the quote policy
        // land from one call.
        store
            .save_node_with_policy(&node("n1"), true)
            .expect("combined write");
        assert!(store.node_exists("n1").unwrap());
        assert!(
            store.node_requires_tpm_quote("n1").unwrap(),
            "the attestation policy committed in the same transaction as the node"
        );

        // Fenced path: advance the durable epoch so a stale held-epoch is
        // superseded; the combined write must leave NEITHER row.
        store.try_claim_epoch(0, "A", 1).unwrap();
        store.try_claim_epoch(1, "B", 2).unwrap();
        let err = store
            .save_node_with_policy_epoch_fenced(&node("n2"), true, 1)
            .expect_err("superseded combined write rejected");
        assert!(superseded(&err), "expected EpochSuperseded, got {err:?}");
        assert!(
            !store.node_exists("n2").unwrap(),
            "fenced-out registration must leave no node row"
        );
        assert!(
            !store.node_requires_tpm_quote("n2").unwrap(),
            "fenced-out registration must leave no policy row (defaults false when absent)"
        );

        // Holder (held == durable == 2) commits both.
        store
            .save_node_with_policy_epoch_fenced(&node("n3"), true, 2)
            .expect("holder combined write admitted");
        assert!(store.node_exists("n3").unwrap());
        assert!(store.node_requires_tpm_quote("n3").unwrap());
    }
}
