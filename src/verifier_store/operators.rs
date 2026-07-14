// src/verifier_store/operators.rs
// operators domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    /// Persist a supervisor clearance grant — **RECORD-ONLY** (operator-console
    /// Phase A). One transaction writes the `clearance_grants` row (Phase-B
    /// pickup, `delivery = PENDING-NODE-TRANSPORT`) AND appends a signed,
    /// hash-chained `OperatorClearanceGrantIssued` audit event. It records and
    /// signs the grant; it does **NOT** release the node — delivery to the node's
    /// `ClearanceLoop` is Phase B. Mirrors `save_posture_event_chained`'s
    /// table+chain transaction. Does not touch nodes / posture. Returns the row id.
    pub fn save_clearance_grant_chained(
        &mut self,
        node_id: &str,
        operator_id: &str,
        granted_at_ms: u64,
    ) -> Result<i64> {
        // Backward-compatible: existing callers (Phase-B tests, the fleet relay,
        // the demo seed) record with auth_method = "unspecified". The auth-aware
        // path below is what the upgraded console route uses.
        self.save_clearance_grant_chained_with_auth(
            node_id,
            operator_id,
            granted_at_ms,
            "unspecified",
            None,
        )
    }

    /// Record a clearance grant with its **authorization provenance** (#314 Phase
    /// 1) — `auth_method` (`operator-signed` / `supervisor-break-glass` /
    /// `unspecified`) and the signing operator's key `fingerprint`. Both are
    /// written to the (additive) grant columns AND embedded in the
    /// `OperatorClearanceGrantIssued` chain event — the non-repudiation payoff: the
    /// signed ledger records WHO authorized the release and with WHICH key. The
    /// `PENDING-NODE-TRANSPORT` row + the columns Phase-B reads are unchanged.
    pub fn save_clearance_grant_chained_with_auth(
        &mut self,
        node_id: &str,
        operator_id: &str,
        granted_at_ms: u64,
        auth_method: &str,
        operator_key_fingerprint: Option<&str>,
    ) -> Result<i64> {
        let payload = serde_json::json!({
            "node_id": node_id,
            "operator_id": operator_id,
            "granted_at_ms": granted_at_ms,
            "delivery": "PENDING-NODE-TRANSPORT",
            "auth_method": auth_method,
            "operator_key_fingerprint": operator_key_fingerprint,
        })
        .to_string();
        let tx = Self::audit_tx(&mut self.conn)?; // #685: Immediate — non-forking audit append
        tx.execute(
            "INSERT INTO clearance_grants
             (node_id, operator_id, granted_at_ms, delivery, created_at_ms,
              auth_method, operator_key_fingerprint)
             VALUES (?1, ?2, ?3, 'PENDING-NODE-TRANSPORT', ?4, ?5, ?6)",
            params![
                node_id,
                operator_id,
                granted_at_ms as i64,
                granted_at_ms as i64,
                auth_method,
                operator_key_fingerprint,
            ],
        )?;
        let id = tx.last_insert_rowid();
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "OperatorClearanceGrantIssued",
            &payload,
            granted_at_ms as i64,
            self.signing_key.as_ref(),
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Register (or re-register / rotate) an operator's Ed25519 PUBLIC key.
    /// Re-registration overwrites the key and CLEARS any prior revocation (a fresh
    /// key for an operator is an active operator). Admin-gated at the route layer.
    pub fn register_operator(
        &mut self,
        operator_id: &str,
        pubkey_pem: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO operators (operator_id, pubkey_pem, registered_at_ms, revoked_at_ms)
             VALUES (?1, ?2, ?3, NULL)
             ON CONFLICT(operator_id) DO UPDATE SET
                 pubkey_pem = excluded.pubkey_pem,
                 registered_at_ms = excluded.registered_at_ms,
                 revoked_at_ms = NULL",
            params![operator_id, pubkey_pem, now_ms as i64],
        )?;
        Ok(())
    }

    /// Revoke an operator (sets `revoked_at_ms`). Returns `true` if an ACTIVE
    /// operator was revoked, `false` if absent or already revoked.
    pub fn revoke_operator(&mut self, operator_id: &str, now_ms: u64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE operators SET revoked_at_ms = ?2
             WHERE operator_id = ?1 AND revoked_at_ms IS NULL",
            params![operator_id, now_ms as i64],
        )?;
        Ok(n > 0)
    }

    /// Load an operator record (active or revoked), or `None` if unregistered.
    pub fn load_operator(&self, operator_id: &str) -> Result<Option<OperatorRecord>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT operator_id, pubkey_pem, registered_at_ms, revoked_at_ms
                 FROM operators WHERE operator_id = ?1",
                params![operator_id],
                |row| {
                    Ok(OperatorRecord {
                        operator_id: row.get(0)?,
                        pubkey_pem: row.get(1)?,
                        registered_at_ms: row.get::<_, i64>(2)? as u64,
                        revoked_at_ms: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    })
                },
            )
            .optional()
    }

    /// Read-only listing of every registered operator. `pubkey_pem` is the
    /// PUBLIC key; the handler exposes only its fingerprint.
    pub fn load_operators(&self) -> Result<Vec<OperatorRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT operator_id, pubkey_pem, registered_at_ms, revoked_at_ms
             FROM operators ORDER BY operator_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(OperatorRecord {
                operator_id: row.get(0)?,
                pubkey_pem: row.get(1)?,
                registered_at_ms: row.get::<_, i64>(2)? as u64,
                revoked_at_ms: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
            })
        })?;
        rows.collect()
    }

    /// Append a signed, hash-chained console audit event with **no** other table
    /// write — used for REJECTED clearance attempts (unauthorized / malformed).
    /// The `payload_json` must NEVER contain the supervisor key bytes (outcome +
    /// reason + attempted ids only).
    pub fn append_clearance_audit_event(
        &mut self,
        event_type: &str,
        payload_json: &str,
        created_at_ms: u64,
    ) -> Result<()> {
        let tx = Self::audit_tx(&mut self.conn)?; // #685: Immediate — non-forking audit append
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            event_type,
            payload_json,
            created_at_ms as i64,
            self.signing_key.as_ref(),
        )?;
        tx.commit()
    }

    /// **THE ONE-SHOT CONSUME** (operator-console Phase B). In a SINGLE atomic
    /// `UPDATE … RETURNING`, marks the OLDEST unconsumed grant for `node_id`
    /// consumed (`consumed_at_ms = now_ms`) and returns it. A grant can be taken
    /// **exactly once, ever** — double-pickup is impossible by the
    /// `consumed_at_ms IS NULL` guard combined with the atomic single-statement
    /// update (SQLite serializes writers), NOT by convention. This is the
    /// store-level verify-then-consume pattern — the same single-use discipline
    /// as the attestation nonce (`AppState::consume_challenge`, `src/verifier.rs`:
    /// atomically removes the pending entry so a replay finds nothing). Returns
    /// `None` when no pending grant exists (idempotent-empty pickup).
    pub fn take_pending_clearance_grant(
        &self,
        node_id: &str,
        now_ms: u64,
    ) -> Result<Option<PendingClearanceGrant>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "UPDATE clearance_grants
                    SET consumed_at_ms = ?2
                  WHERE id = (
                    SELECT id FROM clearance_grants
                     WHERE node_id = ?1 AND consumed_at_ms IS NULL
                     ORDER BY id ASC LIMIT 1)
                  RETURNING id, node_id, operator_id, granted_at_ms",
                params![node_id, now_ms as i64],
                |row| {
                    Ok(PendingClearanceGrant {
                        rowid: row.get(0)?,
                        node_id: row.get(1)?,
                        operator_id: row.get(2)?,
                        granted_at_ms: row.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .optional()
    }

    /// Record a delivered grant's OUTCOME (operator-console Phase B): the
    /// `ClearanceLoop` verdict at delivery, plus the chained audit event — ONE
    /// signed transaction, same shape as Phase A's writes. `outcome == "Cleared"`
    /// → a `ClearanceDelivered` event; any other `outcome` (a rejection reason
    /// code) → a `ClearanceDeliveryRejected` event. `now_ms` is supplied (no
    /// `SystemTime::now()` in the store).
    pub fn record_grant_outcome(
        &mut self,
        grant_rowid: i64,
        outcome: &str,
        detail: Option<&str>,
        now_ms: u64,
    ) -> Result<()> {
        let event_type = if outcome == "Cleared" {
            "ClearanceDelivered"
        } else {
            "ClearanceDeliveryRejected"
        };
        let payload = serde_json::json!({
            "grant_rowid": grant_rowid,
            "outcome": outcome,
            "detail": detail,
        })
        .to_string();
        let tx = Self::audit_tx(&mut self.conn)?; // #685: Immediate — non-forking audit append
        tx.execute(
            "UPDATE clearance_grants SET outcome = ?2, outcome_detail = ?3 WHERE id = ?1",
            params![grant_rowid, outcome, detail],
        )?;
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            event_type,
            &payload,
            now_ms as i64,
            self.signing_key.as_ref(),
        )?;
        tx.commit()
    }

    /// The most recent clearance grant's delivery state for `node_id` (console
    /// read surface, Phase B). `None` if the node has no grants.
    pub fn latest_clearance_grant(&self, node_id: &str) -> Result<Option<ClearanceGrantState>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT granted_at_ms, consumed_at_ms, outcome, outcome_detail
                   FROM clearance_grants WHERE node_id = ?1 ORDER BY id DESC LIMIT 1",
                params![node_id],
                |row| {
                    Ok(ClearanceGrantState {
                        granted_at_ms: row.get::<_, i64>(0)? as u64,
                        consumed_at_ms: row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                        outcome: row.get(2)?,
                        outcome_detail: row.get(3)?,
                    })
                },
            )
            .optional()
    }
}

// ---------------------------------------------------------------------------
// ADR-0035 Stage 2 (trait-seam inversion) — the operator-registry storage trait
//
// The operator IDENTITY registry CRUD (register/rotate the Ed25519 PUBLIC key,
// revoke, load, list), lifted off `VerifierStore` as a `VerifierStorage`-family
// seam like `NodeStore` / `PrincipalStore`. Scope is deliberately JUST the
// operator registry — the clearance-GRANT methods above stay inherent-only
// because they are audit-chained (they append signed `AuditChainLinker` events),
// a coupling that belongs to the harder persistence tier, not a portable CRUD
// seam. Inherent methods win resolution, so the SQLite impl delegates via
// `self.method()` without recursion and every existing caller is untouched.
// ---------------------------------------------------------------------------

/// The operator-registry storage contract — register/rotate an operator's Ed25519
/// PUBLIC key, revoke it, load one by id / list all. Backend-agnostic identity CRUD
/// (only the PUBLIC key is ever stored). Mirrors [`super::NodeStore`].
pub trait OperatorStore {
    /// Backend error type (SQLite: `rusqlite::Error`; in-memory: [`std::convert::Infallible`]).
    type Error;

    /// Register or rotate an operator by `operator_id`: overwrite the public key and
    /// CLEAR any prior revocation (a fresh key reactivates an operator).
    fn register_operator(
        &mut self,
        operator_id: &str,
        pubkey_pem: &str,
        now_ms: u64,
    ) -> std::result::Result<(), Self::Error>;

    /// Revoke an operator. Returns `true` iff an ACTIVE operator transitioned to
    /// revoked; `false` if absent or already revoked.
    fn revoke_operator(
        &mut self,
        operator_id: &str,
        now_ms: u64,
    ) -> std::result::Result<bool, Self::Error>;

    /// Load one operator by id (active or revoked), or `None` if unregistered.
    fn load_operator(
        &self,
        operator_id: &str,
    ) -> std::result::Result<Option<OperatorRecord>, Self::Error>;

    /// List every registered operator, ordered by `operator_id`.
    fn load_operators(&self) -> std::result::Result<Vec<OperatorRecord>, Self::Error>;
}

/// The production SQLite backend: delegates to the inherent `VerifierStore` methods
/// over the `operators` table. `self.method()` resolves to the INHERENT method
/// (inherent wins), so this is delegation, not recursion.
impl OperatorStore for VerifierStore {
    type Error = rusqlite::Error;

    fn register_operator(
        &mut self,
        operator_id: &str,
        pubkey_pem: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.register_operator(operator_id, pubkey_pem, now_ms)
    }
    fn revoke_operator(&mut self, operator_id: &str, now_ms: u64) -> Result<bool> {
        self.revoke_operator(operator_id, now_ms)
    }
    fn load_operator(&self, operator_id: &str) -> Result<Option<OperatorRecord>> {
        self.load_operator(operator_id)
    }
    fn load_operators(&self) -> Result<Vec<OperatorRecord>> {
        self.load_operators()
    }
}

/// The in-memory [`OperatorStore`] backend — a portability-proof reference modelling
/// the `operators` table as a map keyed by `operator_id`. Realizes the SAME
/// register/rotate/revoke/load semantics WITHOUT a database. Single-process.
#[derive(Debug, Default)]
pub struct InMemoryOperatorStore {
    rows: std::collections::HashMap<String, InMemoryOperatorRow>,
}

#[derive(Debug, Clone)]
struct InMemoryOperatorRow {
    pubkey_pem: String,
    registered_at_ms: u64,
    revoked_at_ms: Option<u64>,
}

impl InMemoryOperatorRow {
    fn to_record(&self, operator_id: &str) -> OperatorRecord {
        OperatorRecord {
            operator_id: operator_id.to_string(),
            pubkey_pem: self.pubkey_pem.clone(),
            registered_at_ms: self.registered_at_ms,
            revoked_at_ms: self.revoked_at_ms,
        }
    }
}

impl OperatorStore for InMemoryOperatorStore {
    type Error = std::convert::Infallible;

    fn register_operator(
        &mut self,
        operator_id: &str,
        pubkey_pem: &str,
        now_ms: u64,
    ) -> std::result::Result<(), std::convert::Infallible> {
        // Upsert by id; rotation overwrites the key and CLEARS revocation —
        // matching the SQLite `ON CONFLICT … SET … revoked_at_ms = NULL`.
        self.rows.insert(
            operator_id.to_string(),
            InMemoryOperatorRow {
                pubkey_pem: pubkey_pem.to_string(),
                registered_at_ms: now_ms,
                revoked_at_ms: None,
            },
        );
        Ok(())
    }

    fn revoke_operator(
        &mut self,
        operator_id: &str,
        now_ms: u64,
    ) -> std::result::Result<bool, std::convert::Infallible> {
        match self.rows.get_mut(operator_id) {
            Some(row) if row.revoked_at_ms.is_none() => {
                row.revoked_at_ms = Some(now_ms);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn load_operator(
        &self,
        operator_id: &str,
    ) -> std::result::Result<Option<OperatorRecord>, std::convert::Infallible> {
        Ok(self
            .rows
            .get(operator_id)
            .map(|row| row.to_record(operator_id)))
    }

    fn load_operators(&self) -> std::result::Result<Vec<OperatorRecord>, std::convert::Infallible> {
        let mut out: Vec<OperatorRecord> = self
            .rows
            .iter()
            .map(|(id, row)| row.to_record(id))
            .collect();
        out.sort_by(|a, b| a.operator_id.cmp(&b.operator_id));
        Ok(out)
    }
}

/// The operator-registry contract, driven through the [`OperatorStore`] trait so it
/// runs IDENTICALLY against every backend: empty read, register→load roundtrip (id +
/// key + active), rotation (key overwrites, revocation clears), revoke idempotence +
/// reports the transition + loads-while-revoked, and the ordered listing.
///
/// `pub` (not `#[cfg(test)]`) by design — the shared backend-conformance suite, run
/// below against the SQLite and in-memory backends. Panics on violation; call from a
/// test. PRECONDITION: `store` must start empty.
pub fn assert_operator_store_contract<S: OperatorStore>(store: &mut S)
where
    S::Error: core::fmt::Debug,
{
    // Empty registry.
    assert!(store.load_operator("op-a").unwrap().is_none());
    assert!(store.load_operators().unwrap().is_empty());

    // Register + load (id + key preserved, active).
    store.register_operator("op-a", "PEM-A", 1_000).unwrap();
    let rec = store.load_operator("op-a").unwrap().expect("op-a present");
    assert_eq!(rec.operator_id, "op-a");
    assert_eq!(rec.pubkey_pem, "PEM-A");
    assert!(rec.is_active());

    // Revoke transitions once, then no-op; the record still loads while revoked.
    assert!(
        store.revoke_operator("op-a", 2_000).unwrap(),
        "first revoke transitions"
    );
    assert!(
        !store.revoke_operator("op-a", 3_000).unwrap(),
        "second revoke is a no-op"
    );
    assert!(
        !store.revoke_operator("absent", 3_000).unwrap(),
        "absent operator → false"
    );
    assert!(store
        .load_operator("op-a")
        .unwrap()
        .expect("loads while revoked")
        .revoked_at_ms
        .is_some());

    // Rotation overwrites the key and reactivates.
    store.register_operator("op-a", "PEM-A2", 4_000).unwrap();
    let rotated = store.load_operator("op-a").unwrap().unwrap();
    assert_eq!(rotated.pubkey_pem, "PEM-A2");
    assert!(rotated.is_active(), "rotation clears revocation");

    // A second operator; listing is ordered by id.
    store.register_operator("op-b", "PEM-B", 5_000).unwrap();
    let ids: Vec<String> = store
        .load_operators()
        .unwrap()
        .into_iter()
        .map(|o| o.operator_id)
        .collect();
    assert_eq!(ids, ["op-a", "op-b"], "listing ordered by operator_id");
}

#[cfg(test)]
mod operator_store_contract_tests {
    use super::*;

    #[test]
    fn sqlite_backend_satisfies_the_operator_store_contract() {
        let mut store = VerifierStore::new(":memory:").expect("in-memory store");
        assert_operator_store_contract(&mut store);
    }

    #[test]
    fn in_memory_backend_satisfies_the_operator_store_contract() {
        assert_operator_store_contract(&mut InMemoryOperatorStore::default());
    }
}
