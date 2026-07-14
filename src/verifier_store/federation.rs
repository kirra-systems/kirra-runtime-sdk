// src/verifier_store/federation.rs
// federation domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    pub fn save_federated_report_chained(
        &mut self,
        report: &FederatedTrustReport,
        source_generation: Option<u64>,
        received_at_ms: u64,
        held_epoch: u64,
    ) -> std::result::Result<(), DurableWriteError> {
        // #74: route the whole federation commit — report + NONCE BURN + audit —
        // through the FULL (force-synced) connection. A burned nonce must survive
        // power-loss or anti-replay is defeated (the 5 s freshness window only
        // partially bounds replay). Federation reports are rare, so the per-commit
        // fsync is off the hot path and inconsequential to throughput.
        let signing_key = self.signing_key.clone(); // durable_mut() borrows self
                                                    // #79: IMMEDIATE so the durable write lock is held before the epoch
                                                    // re-check below — no concurrent `try_claim_epoch` can interleave
                                                    // between the fence read and this commit.
        let tx = self
            .durable_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // #79 HA epoch fence — FIRST statement, before any mutation. A node
        // fenced after the request-path gate check cannot land a stale report.
        Self::assert_epoch_held(&tx, held_epoch)?;

        // Item 20 — per-(controller, asset) GENERATION HIGH-WATER gate. Runs inside
        // the Immediate write lock (no interleave). Only v2 reports (a supplied
        // `source_generation`) are gated; a v1 report (None) keeps its legacy
        // timestamp-ordered behaviour. Two outcomes that matter here:
        //   * regress/replay: `gen <= high_water` → abort the whole commit
        //     (fail-closed; the stale report never persists and no nonce is burned);
        //   * forward GAP: `gen > high_water + 1` → accept, but record the skipped
        //     generations as an in-chain audit marker (below, after the report row).
        // `gap_from` carries the prior high-water when a gap is detected so the
        // marker can be appended in the SAME transaction as the accepted report.
        let mut gap_from: Option<u64> = None;
        if let Some(gen) = source_generation {
            let high_water: Option<u64> = tx
                .query_row(
                    "SELECT last_generation FROM federation_generation_highwater
                     WHERE source_controller_id = ?1 AND asset_id = ?2",
                    params![report.source_controller_id, report.asset_id],
                    |row| row.get::<_, i64>(0),
                )
                .map(|g| Some(g as u64))
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;

            if let Some(hw) = high_water {
                if gen <= hw {
                    // tx drops here → atomic rollback. Fail-closed.
                    return Err(DurableWriteError::GenerationRegress {
                        found: gen,
                        high_water: hw,
                    });
                }
                if gen > hw + 1 {
                    gap_from = Some(hw);
                }
            }

            // Advance the high-water within the same tx (UPSERT; the gate above
            // guarantees strict advance for an existing row).
            tx.execute(
                "INSERT INTO federation_generation_highwater
                     (source_controller_id, asset_id, last_generation, last_seen_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(source_controller_id, asset_id)
                 DO UPDATE SET last_generation = ?3, last_seen_ms = ?4",
                params![
                    report.source_controller_id,
                    report.asset_id,
                    gen as i64,
                    received_at_ms as i64,
                ],
            )?;
        }

        let posture_json =
            serde_json::to_string(&report.posture).map_err(|_| rusqlite::Error::InvalidQuery)?;

        tx.execute(
            "INSERT INTO federated_trust_reports
             (source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, received_at_ms, source_generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                report.source_controller_id, report.asset_id, posture_json,
                report.issued_at_ms as i64, report.expires_at_ms as i64, received_at_ms as i64,
                source_generation.map(|g| g as i64),
            ],
        )?;

        // H1 — AUTHORITATIVE single-use nonce claim. This MUST stay a plain `INSERT`
        // (never `INSERT OR IGNORE`): the `nonce_hex PRIMARY KEY` UNIQUE violation is
        // what atomically rejects a concurrent replay that raced past the request-path
        // `has_seen_federation_nonce` check. `OR IGNORE` would let the report row above
        // commit while silently no-op-ing the burn → DOUBLE-ACCEPT. The violation is
        // surfaced as the distinct `NonceReplay` (not a generic Db error) so the caller
        // returns a clean replay rejection + audit instead of an opaque 500.
        if let Err(e) = tx.execute(
            "INSERT INTO federation_report_nonces (nonce_hex, source_controller_id, seen_at_ms)
             VALUES (?1, ?2, ?3)",
            params![
                report.nonce_hex,
                report.source_controller_id,
                received_at_ms as i64
            ],
        ) {
            if is_unique_violation(&e) {
                // tx drops here → the report INSERT above is rolled back atomically.
                return Err(DurableWriteError::NonceReplay);
            }
            return Err(DurableWriteError::Db(e));
        }

        let audit = serde_json::json!({
            "source_controller_id": report.source_controller_id,
            "asset_id": report.asset_id,
            "posture": posture_json,
            "issued_at_ms": report.issued_at_ms,
            "expires_at_ms": report.expires_at_ms,
            "nonce_hex": report.nonce_hex,
            "received_at_ms": received_at_ms,
        });
        ChainedAuditAppender {
            signing_key: signing_key.as_ref(),
        }
        .append_within(
            &tx,
            "FEDERATED_TRUST_REPORT_ACCEPTED",
            &audit.to_string(),
            received_at_ms as i64,
        )?;

        // Item 20 — in-chain AUDIT_GAP marker. A forward generation jump means this
        // controller's intermediate reports never reached us (a partition / drop):
        // the chain MUST record that we are missing generations `hw+1 ..= gen-1`, so a
        // later auditor sees an explicit, tamper-evident "coverage gap" instead of an
        // unexplained generation discontinuity. Same tx as the accepted report → the
        // marker is committed iff the report is.
        if let (Some(hw), Some(gen)) = (gap_from, source_generation) {
            let gap = serde_json::json!({
                "source_controller_id": report.source_controller_id,
                "asset_id": report.asset_id,
                "last_accepted_generation": hw,
                "observed_generation": gen,
                "missing_from_generation": hw + 1,
                "missing_through_generation": gen - 1,
                "skipped_generations": gen - hw - 1,
            });
            ChainedAuditAppender {
                signing_key: signing_key.as_ref(),
            }
            .append_within(
                &tx,
                "FEDERATION_GENERATION_GAP",
                &gap.to_string(),
                received_at_ms as i64,
            )?;
        }

        // Bounded retention (review M2): the nonce table is the durable anti-replay
        // set, but it only ever grew (rising disk + fsync cost). A nonce aged past
        // the retention horizon can NEVER reopen a replay slot — a report bearing it
        // carries a FIXED, signed `issued_at_ms`, so a replay fails the freshness
        // gate (`FEDERATION_REPLAY_WINDOW_MS`) regardless of whether the nonce row
        // still exists. The horizon is set FAR above the freshness window to absorb
        // clock skew; we never delete a nonce that could still be inside any
        // plausible replay window. Pruned in the same accept transaction.
        let cutoff = (received_at_ms as i64).saturating_sub(FEDERATION_NONCE_RETENTION_MS);
        tx.execute(
            "DELETE FROM federation_report_nonces WHERE seen_at_ms < ?1",
            params![cutoff],
        )?;

        tx.commit()?;
        Ok(())
    }

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

    /// Per-source monotonic sequence gate for industrial-message replay protection.
    ///
    /// Returns `Ok(true)` when `sequence` is STRICTLY greater than the last-seen
    /// high-water mark for `source_id` — the message is fresh-in-order and the
    /// high-water mark is advanced to it — or `Ok(false)` when `sequence <=` the
    /// stored mark (a replay or out-of-order regress → the caller must reject; the
    /// mark is NOT advanced). The first message from a new source establishes the
    /// baseline (any sequence accepted once).
    ///
    /// The check-and-advance is a single atomic `INSERT … ON CONFLICT … DO UPDATE …
    /// WHERE ? > last_sequence`: under the store mutex, two concurrent ingests of the
    /// same captured sequence cannot both win (the conditional UPDATE makes it a true
    /// compare-and-set, like the federation nonce burn / HA epoch CAS). Durable, so a
    /// replay cannot ride a restart.
    pub fn industrial_seq_check_and_advance(
        &self,
        source_id: &str,
        sequence: u64,
        now_ms: u64,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO industrial_message_seq (source_id, last_sequence, last_seen_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(source_id) DO UPDATE SET last_sequence = ?2, last_seen_ms = ?3
             WHERE ?2 > industrial_message_seq.last_sequence",
            params![source_id, sequence as i64, now_ms as i64],
        )?;
        Ok(changed > 0)
    }

    /// Atomically *burn* a nonce: claim it on first use, reject it on replay.
    ///
    /// Returns `Ok(true)` if the nonce was newly recorded (first time seen →
    /// the caller may proceed) and `Ok(false)` if it was already present (a
    /// replay → the caller must reject). This is the verify-AND-consume primitive
    /// the untrusted fleet carrier needs: a single `INSERT OR IGNORE` against the
    /// `nonce_hex PRIMARY KEY` makes the claim atomic — there is no check-then-act
    /// window for two concurrent ingests of the same captured payload to both win.
    ///
    /// The write rides the `synchronous=FULL` durable connection (same as the
    /// federation report nonce burn), falling back to the main connection for an
    /// in-memory store. The `seen_at_ms` column is diagnostic only — replay
    /// correctness depends solely on the primary-key conflict, never on the clock.
    pub fn burn_federation_nonce(&self, nonce_hex: &str) -> Result<bool> {
        let seen_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let changed = self.durable_ref().execute(
            "INSERT OR IGNORE INTO federation_report_nonces
                 (nonce_hex, source_controller_id, seen_at_ms)
             VALUES (?1, ?2, ?3)",
            params![nonce_hex, "fleet-grant-lane", seen_at_ms],
        )?;
        Ok(changed > 0)
    }

    pub fn load_federated_reports_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, source_generation
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
            let generation: Option<i64> = row.get(5)?;
            Ok(serde_json::json!({
                "source_controller_id": source,
                "asset_id": aid,
                "posture": posture_json,
                "issued_at_ms": issued as u64,
                "expires_at_ms": expires as u64,
                "source_generation": generation.map(|g| g as u64),
            }))
        })?;
        rows.collect()
    }

    /// Typed loader for generation-ordered reconciliation (#329 v2 wiring). Returns
    /// the stored reports for an asset as `FederatedTrustReportV2` so the caller can
    /// run `authoritative_posture`. `nonce_hex` / `signature_b64` are NOT persisted in
    /// this table (they are consumed/verified at ingest), so they are left empty here —
    /// the reconciliation API (`reconcile_reports` / `authoritative_posture`) reads only
    /// `asset_id`, `posture`, `source_generation`, and `issued_at_ms`. A row whose
    /// `posture_json` fails to deserialize is fail-closed-skipped (never silently
    /// treated as Nominal). Ordered newest-first, matching `load_federated_reports_for_asset`.
    pub fn load_federated_report_v2s_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<kirra_fleet_types::federation_reconciliation::FederatedTrustReportV2>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, source_generation
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
            let generation: Option<i64> = row.get(5)?;
            Ok((
                source,
                aid,
                posture_json,
                issued as u64,
                expires as u64,
                generation.map(|g| g as u64),
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (source, aid, posture_json, issued, expires, generation) = row?;
            // Fail-closed: a corrupt posture is skipped, never coerced to Nominal.
            let Ok(posture) = serde_json::from_str::<kirra_core::FleetPosture>(&posture_json)
            else {
                continue;
            };
            out.push(
                kirra_fleet_types::federation_reconciliation::FederatedTrustReportV2 {
                    source_controller_id: source,
                    asset_id: aid,
                    posture,
                    issued_at_ms: issued,
                    expires_at_ms: expires,
                    nonce_hex: String::new(),
                    signature_b64: String::new(),
                    source_generation: generation,
                },
            );
        }
        Ok(out)
    }
}
