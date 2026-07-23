//! #1030 stage 2 (ADR-0038) — the shared-tier INHERENT-method gap-fill.
//!
//! The ten trait seams cover the portable CRUD/fence contracts, but the
//! verifier service also calls a set of shared-STATE methods that were
//! inherent-only on the SQLite `VerifierStore` (the stage-1 call-surface
//! audit): the dependency graph, the epoch-fenced node upserts, the
//! per-node attestation policy, the WP-19 HA lease, the OTA campaign
//! lifecycle UPDATE, the clearance-grant state machine, and the WP-15 cert
//! expiry census. The hybrid design routes ALL shared tiers to one backend
//! atomically (a split would be a data-integrity hazard), so every one of
//! them needs a Postgres realization before `KIRRA_DB_URL` can serve
//! traffic. This module is those realizations.
//!
//! **What is deliberately NOT here — the audit chaining.** On SQLite,
//! `update_campaign` / `record_grant_outcome` / the grant-create path fuse
//! the row mutation with a hash-chained `AuditChainLinker` append in one
//! transaction. Under ADR-0038 the ledger stays on per-instance LOCAL
//! SQLite, so the PG methods here are the UNCHAINED row primitives (named
//! `*_row` where the SQLite namesake is chained, so a reader can't mistake
//! one for the other); the caller composes them with the LOCAL chained
//! append, ledger-write-first (INVARIANT #12 extended — the ledger is the
//! pessimistic record).
//!
//! Concurrency notes mirror the SQLite originals:
//! - the fenced writes take the `ha_state` row lock (`SELECT … FOR UPDATE`)
//!   as their FIRST statement, exactly like `assert_actuator_epoch_held` —
//!   a concurrent `try_claim_epoch` serializes behind it, so the fence
//!   check and the mutation cannot interleave;
//! - `take_pending_clearance_grant` keeps the exactly-once consume under
//!   PG's concurrency model: the picking subquery locks the candidate row
//!   (`FOR UPDATE SKIP LOCKED`) and the outer UPDATE re-guards on
//!   `consumed_at_ms IS NULL`, so two racers can never both consume one
//!   grant (a loser sees no row this poll — safe, it retries next poll).

use std::collections::HashMap;

use super::*;
use crate::{CertExpirySummary, ClearanceGrantState, FenceError, HaLease, PendingClearanceGrant};

/// A fenced PG shared-state write failed. The Postgres analogue of the SQLite
/// backend's `DurableWriteError`: `Fenced` = the epoch assertion rejected
/// (superseded / unreadable — nothing was written); `Db` = an ordinary store
/// error. Callers treat BOTH as fail-closed.
#[derive(Debug)]
pub enum PgDurableWriteError {
    Fenced(FenceError),
    Db(PgStoreError),
    /// H1 parity: the federation nonce was already burned (the plain INSERT's
    /// UNIQUE violation aborted the transaction — never `ON CONFLICT DO
    /// NOTHING`, which would double-accept).
    NonceReplay,
    /// Item 20 parity: same-epoch generation regress/replay. Transaction
    /// aborted; nothing persisted, nonce not burned, high-water unchanged.
    GenerationRegress {
        found: u64,
        high_water: u64,
    },
    /// #791 I1 parity: effective-epoch regress (or omission-downgrade,
    /// `found: 0`). Transaction aborted, as above.
    EpochRegress {
        found: u64,
        high_water: u64,
    },
}

impl From<PgStoreError> for PgDurableWriteError {
    fn from(e: PgStoreError) -> Self {
        PgDurableWriteError::Db(e)
    }
}

impl From<postgres::Error> for PgDurableWriteError {
    fn from(e: postgres::Error) -> Self {
        PgDurableWriteError::Db(PgStoreError::from(e))
    }
}

impl std::fmt::Display for PgDurableWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PgDurableWriteError::Fenced(e) => write!(f, "epoch fence rejected: {e:?}"),
            PgDurableWriteError::Db(e) => write!(f, "store error: {e}"),
            PgDurableWriteError::NonceReplay => f.write_str("federation nonce replay"),
            PgDurableWriteError::GenerationRegress { found, high_water } => write!(
                f,
                "federation generation regress: found {found}, high-water {high_water}"
            ),
            PgDurableWriteError::EpochRegress { found, high_water } => write!(
                f,
                "federation epoch regress: found {found}, high-water {high_water}"
            ),
        }
    }
}

impl std::error::Error for PgDurableWriteError {}

/// The in-transaction epoch fence: read the `ha_state` singleton under the
/// row lock and reject unless it still equals `held_epoch`. FIRST statement
/// of every fenced write below (the `BEGIN IMMEDIATE` analogue).
fn assert_epoch_held_tx(
    tx: &mut postgres::Transaction<'_>,
    held_epoch: u64,
) -> Result<(), PgDurableWriteError> {
    let row = tx
        .query_opt("SELECT epoch FROM ha_state WHERE id = 1 FOR UPDATE", &[])
        .map_err(|_| PgDurableWriteError::Fenced(FenceError::EpochUnreadable))?;
    let durable = match row {
        Some(r) => r.get::<_, i64>(0) as u64,
        None => return Err(PgDurableWriteError::Fenced(FenceError::EpochUnreadable)),
    };
    if held_epoch == 0 || durable != held_epoch {
        return Err(PgDurableWriteError::Fenced(FenceError::EpochSuperseded {
            held: held_epoch,
            durable,
        }));
    }
    Ok(())
}

/// The node upsert, inside the caller's transaction (shared by the two fenced
/// saves — same column set as the `NodeStore::save_node` upsert).
fn upsert_node_tx(
    tx: &mut postgres::Transaction<'_>,
    node: &RegisteredNode,
    status_json: &str,
) -> Result<(), PgDurableWriteError> {
    tx.execute(
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

impl PgVerifierStore {
    // -- Dependency graph ---------------------------------------------------

    /// Atomic replace of one node's outbound dependency edges (the SQLite
    /// original's DELETE + re-INSERT, inside one transaction — a mid-loop
    /// failure must never leave a torn DAG).
    pub fn save_dependencies(&self, node_id: &str, deps: &[String]) -> Result<(), PgStoreError> {
        let mut guard = self.lock();
        let mut tx = guard.transaction()?;
        tx.execute("DELETE FROM dependencies WHERE node_id = $1", &[&node_id])?;
        for dep in deps {
            tx.execute(
                "INSERT INTO dependencies (node_id, dep_id) VALUES ($1, $2) \
                 ON CONFLICT DO NOTHING",
                &[&node_id, &dep],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The full dependency graph, keyed by node id.
    pub fn load_dependencies(&self) -> Result<HashMap<String, Vec<String>>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT node_id, dep_id FROM dependencies ORDER BY node_id, dep_id",
            &[],
        )?;
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            map.entry(row.get(0)).or_default().push(row.get(1));
        }
        Ok(map)
    }

    // -- Attestation policy ---------------------------------------------------

    /// Upsert the node's TPM-quote requirement (operator intent, including
    /// flipping it back off — the SQLite `INSERT OR REPLACE` parity).
    pub fn set_node_attestation_policy(
        &self,
        node_id: &str,
        require_tpm_quote: bool,
    ) -> Result<(), PgStoreError> {
        self.lock().execute(
            "INSERT INTO node_attestation_policy (node_id, require_tpm_quote) \
             VALUES ($1, $2) \
             ON CONFLICT (node_id) DO UPDATE SET require_tpm_quote = EXCLUDED.require_tpm_quote",
            &[&node_id, &require_tpm_quote],
        )?;
        Ok(())
    }

    /// Whether the node must present a hardware TPM quote. Absent row →
    /// `false` (never opted in); the CALL SITE fail-closes on `Err`.
    pub fn node_requires_tpm_quote(&self, node_id: &str) -> Result<bool, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT require_tpm_quote FROM node_attestation_policy WHERE node_id = $1",
            &[&node_id],
        )?;
        Ok(row.map(|r| r.get::<_, bool>(0)).unwrap_or(false))
    }

    // -- Epoch-fenced node upserts (C5 #1036 parity) --------------------------

    /// [`NodeStore::save_node`], fenced on the caller's held HA epoch: the
    /// upsert rides a transaction whose FIRST statement takes the `ha_state`
    /// row lock and re-asserts `held_epoch`. Fail-closed: a superseded /
    /// unreadable epoch rolls back and writes nothing.
    pub fn save_node_epoch_fenced(
        &self,
        node: &RegisteredNode,
        held_epoch: u64,
    ) -> Result<(), PgDurableWriteError> {
        let status_json = serde_json::to_string(&node.status)
            .map_err(|e| PgDurableWriteError::Db(PgStoreError::Encode(e)))?;
        let mut guard = self.lock();
        let mut tx = guard.transaction()?;
        assert_epoch_held_tx(&mut tx, held_epoch)?;
        upsert_node_tx(&mut tx, node, &status_json)?;
        tx.commit()?;
        Ok(())
    }

    /// The Sec9 combined write: node upsert + attestation-policy upsert, both
    /// inside ONE fenced transaction (either both land or neither does).
    pub fn save_node_with_policy_epoch_fenced(
        &self,
        node: &RegisteredNode,
        require_tpm_quote: bool,
        held_epoch: u64,
    ) -> Result<(), PgDurableWriteError> {
        let status_json = serde_json::to_string(&node.status)
            .map_err(|e| PgDurableWriteError::Db(PgStoreError::Encode(e)))?;
        let mut guard = self.lock();
        let mut tx = guard.transaction()?;
        assert_epoch_held_tx(&mut tx, held_epoch)?;
        upsert_node_tx(&mut tx, node, &status_json)?;
        tx.execute(
            "INSERT INTO node_attestation_policy (node_id, require_tpm_quote) \
             VALUES ($1, $2) \
             ON CONFLICT (node_id) DO UPDATE SET require_tpm_quote = EXCLUDED.require_tpm_quote",
            &[&node.node_id, &require_tpm_quote],
        )?;
        tx.commit()?;
        Ok(())
    }

    // -- WP-19 HA lease -------------------------------------------------------

    /// Renew the HA lease iff this instance still holds the epoch (`Ok(false)`
    /// = superseded → the caller self-demotes). Same rows-affected guard as
    /// the SQLite original; PG's WAL durability stands in for the forced-sync
    /// connection.
    pub fn renew_lease(
        &self,
        instance_id: &str,
        held_epoch: u64,
        now_ms: u64,
    ) -> Result<bool, PgStoreError> {
        let n = self.lock().execute(
            "UPDATE ha_state SET updated_at_ms = $3 \
             WHERE id = 1 AND epoch = $1 AND active_instance_id = $2",
            &[&(held_epoch as i64), &instance_id, &(now_ms as i64)],
        )?;
        Ok(n == 1)
    }

    /// Read the durable lease (epoch, holder, last renew) a standby measures
    /// freshness against. Fail-closed: an absent singleton row is an error.
    pub fn read_ha_lease(&self) -> Result<HaLease, PgStoreError> {
        let row = self.lock().query_one(
            "SELECT epoch, active_instance_id, updated_at_ms FROM ha_state WHERE id = 1",
            &[],
        )?;
        Ok(HaLease {
            epoch: row.get::<_, i64>(0).max(0) as u64,
            holder: row.get(1),
            last_renew_ms: row.get::<_, i64>(2).max(0) as u64,
        })
    }

    // -- OTA campaign lifecycle UPDATE (unchained row primitive) --------------

    /// The `update_campaign` ROW mutation only — the audit-chained append the
    /// SQLite namesake fuses in stays with the caller's LOCAL ledger
    /// (ADR-0038). `Ok(false)` = no such campaign (the caller must NOT ledger
    /// a phantom transition — the SQLite `QueryReturnedNoRows` parity).
    pub fn update_campaign_row(&self, campaign: &Campaign) -> Result<bool, PgStoreError> {
        let n = self.lock().execute(
            "UPDATE ota_campaigns \
                SET stage_index     = $2, \
                    rollout_percent = $3, \
                    state           = $4, \
                    halt_reason     = $5, \
                    updated_at_ms   = $6 \
              WHERE campaign_id = $1",
            &[
                &campaign.campaign_id,
                &(campaign.stage_index as i64),
                &(campaign.rollout_percent as i64),
                &campaign.state.as_str(),
                &campaign.halt_reason.map(|r| r.as_str()),
                &(campaign.updated_at_ms as i64),
            ],
        )?;
        Ok(n == 1)
    }

    /// [`Self::update_campaign_row`], fenced on the caller's held HA epoch
    /// (#1093 parity). `held_epoch == 0` (never-claimed store) takes the
    /// plain path, byte-identical to the SQLite original's semantics.
    pub fn update_campaign_row_epoch_fenced(
        &self,
        campaign: &Campaign,
        held_epoch: u64,
    ) -> Result<bool, PgDurableWriteError> {
        if held_epoch == 0 {
            return self.update_campaign_row(campaign).map_err(Into::into);
        }
        let mut guard = self.lock();
        let mut tx = guard.transaction()?;
        assert_epoch_held_tx(&mut tx, held_epoch)?;
        let n = tx.execute(
            "UPDATE ota_campaigns \
                SET stage_index     = $2, \
                    rollout_percent = $3, \
                    state           = $4, \
                    halt_reason     = $5, \
                    updated_at_ms   = $6 \
              WHERE campaign_id = $1",
            &[
                &campaign.campaign_id,
                &(campaign.stage_index as i64),
                &(campaign.rollout_percent as i64),
                &campaign.state.as_str(),
                &campaign.halt_reason.map(|r| r.as_str()),
                &(campaign.updated_at_ms as i64),
            ],
        )?;
        tx.commit()?;
        Ok(n == 1)
    }

    // -- Clearance-grant state (unchained row primitives) ----------------------

    /// Insert a clearance grant row and return its id. The SQLite grant-create
    /// path (`save_clearance_grant_chained_with_auth`) fuses this with the
    /// chained audit append; here the caller composes with the LOCAL ledger.
    pub fn insert_clearance_grant_row(
        &self,
        node_id: &str,
        operator_id: &str,
        granted_at_ms: u64,
        created_at_ms: u64,
    ) -> Result<i64, PgStoreError> {
        let row = self.lock().query_one(
            "INSERT INTO clearance_grants \
                 (node_id, operator_id, granted_at_ms, created_at_ms) \
             VALUES ($1, $2, $3, $4) RETURNING id",
            &[
                &node_id,
                &operator_id,
                &(granted_at_ms as i64),
                &(created_at_ms as i64),
            ],
        )?;
        Ok(row.get(0))
    }

    /// THE ONE-SHOT CONSUME, PG-race-safe: the picking subquery locks the
    /// oldest pending row (`FOR UPDATE SKIP LOCKED`) and the outer UPDATE
    /// re-guards `consumed_at_ms IS NULL`, so a grant is taken exactly once
    /// ever, under any interleaving. `None` = no pending grant this poll.
    pub fn take_pending_clearance_grant(
        &self,
        node_id: &str,
        now_ms: u64,
    ) -> Result<Option<PendingClearanceGrant>, PgStoreError> {
        let row = self.lock().query_opt(
            "UPDATE clearance_grants \
                SET consumed_at_ms = $2 \
              WHERE id = (SELECT id FROM clearance_grants \
                           WHERE node_id = $1 AND consumed_at_ms IS NULL \
                           ORDER BY id ASC LIMIT 1 \
                           FOR UPDATE SKIP LOCKED) \
                AND consumed_at_ms IS NULL \
              RETURNING id, node_id, operator_id, granted_at_ms",
            &[&node_id, &(now_ms as i64)],
        )?;
        Ok(row.map(|r| PendingClearanceGrant {
            rowid: r.get(0),
            node_id: r.get(1),
            operator_id: r.get(2),
            granted_at_ms: r.get::<_, i64>(3).max(0) as u64,
        }))
    }

    /// Record a delivered grant's outcome — the ROW half only (the chained
    /// `ClearanceDelivered` / `ClearanceDeliveryRejected` event stays with the
    /// caller's local ledger). `Ok(false)` = no such grant row.
    pub fn record_grant_outcome_row(
        &self,
        grant_rowid: i64,
        outcome: &str,
        detail: Option<&str>,
    ) -> Result<bool, PgStoreError> {
        let n = self.lock().execute(
            "UPDATE clearance_grants SET outcome = $2, outcome_detail = $3 WHERE id = $1",
            &[&grant_rowid, &outcome, &detail],
        )?;
        Ok(n == 1)
    }

    /// The most recent grant's delivery state for `node_id` (console read
    /// surface). `None` if the node has no grants.
    pub fn latest_clearance_grant(
        &self,
        node_id: &str,
    ) -> Result<Option<ClearanceGrantState>, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT granted_at_ms, consumed_at_ms, outcome, outcome_detail \
               FROM clearance_grants WHERE node_id = $1 ORDER BY id DESC LIMIT 1",
            &[&node_id],
        )?;
        Ok(row.map(|r| ClearanceGrantState {
            granted_at_ms: r.get::<_, i64>(0).max(0) as u64,
            consumed_at_ms: r.get::<_, Option<i64>>(1).map(|v| v.max(0) as u64),
            outcome: r.get(2),
            outcome_detail: r.get(3),
        }))
    }

    // -- WP-15 cert expiry census ----------------------------------------------

    /// The cert-principal lifecycle census (metrics + warning sweep). Pure
    /// derivation over the CertPrincipalStore list — same classification as
    /// the SQLite original (revocation first, then expiry, then the warn
    /// window), so the gauges agree across backends.
    pub fn cert_expiry_summary(
        &self,
        now_ms: u64,
        warn_window_ms: u64,
    ) -> Result<CertExpirySummary, PgStoreError> {
        use crate::CertPrincipalStore;
        let mut s = CertExpirySummary::default();
        for rec in self.load_cert_principals()? {
            s.total += 1;
            if !rec.is_active() {
                s.revoked += 1;
            } else if rec.is_expired(now_ms) {
                s.expired += 1;
            } else {
                s.active += 1;
                match rec.not_after_ms {
                    None => s.no_expiry += 1,
                    Some(exp) if exp.saturating_sub(now_ms) <= warn_window_ms => {
                        s.expiring_soon += 1
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(s)
    }

    // -- &self fence/CAS mirrors (#1030 stage 2) --------------------------------
    //
    // The `EpochFence` trait methods take `&mut self`; the PG store's interior
    // connection lock makes `&self` equivalents sound. The root `SharedOps`
    // facade holds the store in an `Arc`, so these are the forms it calls.

    /// [`EpochFence::try_claim_epoch`] via interior mutability — the identical
    /// rows-affected compare-and-set.
    pub fn try_claim_epoch_shared(
        &self,
        observed: u64,
        instance_id: &str,
        now_ms: u64,
    ) -> Result<Option<u64>, PgStoreError> {
        let n = self.lock().execute(
            "UPDATE ha_state SET epoch = epoch + 1, active_instance_id = $2, updated_at_ms = $3 \
             WHERE id = 1 AND epoch = $1",
            &[&(observed as i64), &instance_id, &(now_ms as i64)],
        )?;
        Ok(if n == 1 { Some(observed + 1) } else { None })
    }

    /// [`EpochFence::assert_actuator_epoch_held`] via interior mutability —
    /// the identical `SELECT … FOR UPDATE` fence, fail-closed on every error.
    pub fn assert_actuator_epoch_held_shared(&self, held_epoch: u64) -> Result<(), FenceError> {
        let mut guard = self.lock();
        let mut tx = guard
            .transaction()
            .map_err(|_| FenceError::EpochUnreadable)?;
        let row = tx
            .query_opt("SELECT epoch FROM ha_state WHERE id = 1 FOR UPDATE", &[])
            .map_err(|_| FenceError::EpochUnreadable)?;
        let durable = match row {
            Some(r) => r.get::<_, i64>(0) as u64,
            None => return Err(FenceError::EpochUnreadable),
        };
        if held_epoch == 0 || durable != held_epoch {
            return Err(FenceError::EpochSuperseded {
                held: held_epoch,
                durable,
            });
        }
        tx.commit().map_err(|_| FenceError::EpochUnreadable)
    }

    // -- Federated-report tier (#1030 stage 2) ---------------------------------
    //
    // The PG half of `save_federated_report_chained`: the SAME lexicographic
    // `(epoch, generation)` tuple gates, high-water advance, report INSERT,
    // and authoritative nonce burn — one PG transaction, with the high-water
    // row taken `FOR UPDATE` (PG's analogue of SQLite's Immediate write lock:
    // two concurrent reports from one controller serialize at the row lock,
    // so the gate check and the advance cannot interleave). The chained audit
    // events the SQLite fused save appends (ACCEPTED / GENERATION_GAP /
    // EPOCH_ADVANCE) stay with the caller's LOCAL ledger — the returned
    // outcome carries what the caller must ledger.

    /// Save a federated report under the full gate set. On `Ok`, the report
    /// row is committed, the nonce burned, and the high-water advanced; the
    /// outcome names the local audit markers the caller must append
    /// (`gap_from` / `epoch_advance_from`, same semantics as the SQLite fused
    /// save). All rejects roll back atomically.
    #[allow(clippy::too_many_arguments)]
    pub fn save_federated_report_row_gated(
        &self,
        source_controller_id: &str,
        asset_id: &str,
        posture_json: &str,
        issued_at_ms: u64,
        expires_at_ms: u64,
        nonce_hex: &str,
        source_generation: Option<u64>,
        source_epoch: Option<u64>,
        received_at_ms: u64,
        held_epoch: u64,
    ) -> Result<PgFederationOutcome, PgDurableWriteError> {
        // #791 I1 defensive normalization (mirrors the SQLite save).
        let source_epoch = if source_generation.is_some() {
            source_epoch
        } else {
            None
        };
        let mut guard = self.lock();
        let mut tx = guard.transaction()?;
        // #79 fence — FIRST statement. held == 0 means "never claimed" here
        // exactly as the SQLite save's assert_epoch_held treats it: reports
        // can only be accepted by an Active holder.
        assert_epoch_held_tx(&mut tx, held_epoch)?;

        let mut gap_from: Option<u64> = None;
        let mut epoch_advance_from: Option<u64> = None;
        let eff_epoch = source_epoch.unwrap_or(0);

        let hw_row = tx.query_opt(
            "SELECT last_epoch, last_generation FROM federation_generation_highwater \
             WHERE source_controller_id = $1 AND asset_id = $2 FOR UPDATE",
            &[&source_controller_id, &asset_id],
        )?;
        if let Some(r) = hw_row {
            let hw_epoch = r.get::<_, i64>(0).max(0) as u64;
            let hw_gen = r.get::<_, i64>(1).max(0) as u64;
            if hw_epoch >= 1 && source_epoch.is_none() {
                return Err(PgDurableWriteError::EpochRegress {
                    found: 0,
                    high_water: hw_epoch,
                });
            }
            if let Some(gen) = source_generation {
                if eff_epoch < hw_epoch {
                    return Err(PgDurableWriteError::EpochRegress {
                        found: eff_epoch,
                        high_water: hw_epoch,
                    });
                }
                if eff_epoch == hw_epoch {
                    if gen <= hw_gen {
                        return Err(PgDurableWriteError::GenerationRegress {
                            found: gen,
                            high_water: hw_gen,
                        });
                    }
                    if gen > hw_gen + 1 {
                        gap_from = Some(hw_gen);
                    }
                } else {
                    epoch_advance_from = Some(hw_epoch);
                }
            }
        }

        if let Some(gen) = source_generation {
            tx.execute(
                "INSERT INTO federation_generation_highwater \
                     (source_controller_id, asset_id, last_epoch, last_generation, last_seen_ms) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (source_controller_id, asset_id) \
                 DO UPDATE SET last_epoch = $3, last_generation = $4, last_seen_ms = $5",
                &[
                    &source_controller_id,
                    &asset_id,
                    &(eff_epoch as i64),
                    &(gen as i64),
                    &(received_at_ms as i64),
                ],
            )?;
        }

        tx.execute(
            "INSERT INTO federated_trust_reports \
                 (source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, \
                  received_at_ms, source_generation, source_epoch) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &source_controller_id,
                &asset_id,
                &posture_json,
                &(issued_at_ms as i64),
                &(expires_at_ms as i64),
                &(received_at_ms as i64),
                &source_generation.map(|g| g as i64),
                &source_epoch.map(|e| e as i64),
            ],
        )?;

        // H1 — the authoritative single-use claim: a PLAIN insert whose UNIQUE
        // violation aborts the whole transaction (report row included).
        match tx.execute(
            "INSERT INTO federation_report_nonces (nonce_hex, source_controller_id, seen_at_ms) \
             VALUES ($1, $2, $3)",
            &[&nonce_hex, &source_controller_id, &(received_at_ms as i64)],
        ) {
            Ok(_) => {}
            Err(e) => {
                return if e.code() == Some(&postgres::error::SqlState::UNIQUE_VIOLATION) {
                    Err(PgDurableWriteError::NonceReplay)
                } else {
                    Err(PgDurableWriteError::Db(PgStoreError::from(e)))
                };
            }
        }

        tx.commit()?;
        Ok(PgFederationOutcome {
            eff_epoch,
            gap_from,
            epoch_advance_from,
        })
    }

    /// The stored reports for an asset, newest-first (the posture-exempt GET's
    /// shape — mirrors the SQLite loader's JSON rows).
    pub fn load_federated_reports_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<serde_json::Value>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, \
                    source_generation, source_epoch \
             FROM federated_trust_reports WHERE asset_id = $1 ORDER BY received_at_ms DESC",
            &[&asset_id],
        )?;
        Ok(rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "source_controller_id": row.get::<_, String>(0),
                    "asset_id": row.get::<_, String>(1),
                    "posture": row.get::<_, String>(2),
                    "issued_at_ms": row.get::<_, i64>(3).max(0) as u64,
                    "expires_at_ms": row.get::<_, i64>(4).max(0) as u64,
                    "source_generation": row.get::<_, Option<i64>>(5).map(|g| g.max(0) as u64),
                    "source_epoch": row.get::<_, Option<i64>>(6).map(|e| e.max(0) as u64),
                })
            })
            .collect())
    }

    /// Typed v2 loader for reconciliation — same fail-closed skip of a corrupt
    /// posture row as the SQLite loader (never coerced to Nominal).
    pub fn load_federated_report_v2s_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<
        Vec<kirra_fleet_types::federation_reconciliation::FederatedTrustReportV2>,
        PgStoreError,
    > {
        let rows = self.lock().query(
            "SELECT source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, \
                    source_generation, source_epoch \
             FROM federated_trust_reports WHERE asset_id = $1 ORDER BY received_at_ms DESC",
            &[&asset_id],
        )?;
        let mut out = Vec::new();
        for row in rows {
            let posture_json: String = row.get(2);
            let Ok(posture) = serde_json::from_str::<kirra_core::FleetPosture>(&posture_json)
            else {
                continue;
            };
            out.push(
                kirra_fleet_types::federation_reconciliation::FederatedTrustReportV2 {
                    source_controller_id: row.get(0),
                    asset_id: row.get(1),
                    posture,
                    issued_at_ms: row.get::<_, i64>(3).max(0) as u64,
                    expires_at_ms: row.get::<_, i64>(4).max(0) as u64,
                    nonce_hex: String::new(),
                    signature_b64: String::new(),
                    source_generation: row.get::<_, Option<i64>>(5).map(|g| g.max(0) as u64),
                    source_epoch: row.get::<_, Option<i64>>(6).map(|e| e.max(0) as u64),
                },
            );
        }
        Ok(out)
    }
}

/// The accepted-federated-report outcome: what the caller must ledger locally
/// (the SQLite fused save appends these markers in-transaction; the PG path
/// returns them for the local chained append).
#[derive(Debug, Clone, Copy)]
pub struct PgFederationOutcome {
    /// The effective epoch the gate evaluated (0 = epoch-less legacy report).
    pub eff_epoch: u64,
    /// `Some(hw)` = forward generation GAP accepted — ledger the
    /// FEDERATION_GENERATION_GAP marker over `hw+1 ..= gen-1`.
    pub gap_from: Option<u64>,
    /// `Some(prev)` = epoch ADVANCE accepted — ledger the
    /// FEDERATION_EPOCH_ADVANCE marker.
    pub epoch_advance_from: Option<u64>,
}
