//! `PgVerifierStore` — OtaCampaignStore seam (de-monolith split of lib.rs).
//!
//! Additional impl block(s); behaviour unchanged. Shared internals (`lock`,
//! `row_to_node`) are `pub(crate)` in the parent module.

use super::*;

/// The ordered column list for an `ota_campaigns` SELECT, shared by the three read
/// paths so [`PgVerifierStore::row_to_campaign`]'s positional `row.get(..)` indices
/// stay in lock-step with the projection.
const CAMPAIGN_COLUMNS: &str = "campaign_id, artifact_digest, artifact_version, cohorts_json, \
                                stages_json, stage_index, rollout_percent, state, halt_reason, \
                                created_at_ms, updated_at_ms, artifact_signature_b64, \
                                uptane_metadata_json";

impl PgVerifierStore {
    /// Decode one `ota_campaigns` row into a [`Campaign`], FAIL-CLOSED exactly as
    /// the SQLite backend's `map_campaign_row`: a malformed `cohorts`/`stages` blob,
    /// an unknown `state`/`halt_reason` token, or a `stage_index` out of range for
    /// the schedule is an error, never a silently-defaulted `Campaign` the engine
    /// could later index out of bounds. Timestamps read fail-closed (a negative
    /// stored BIGINT → 0, never a wrap to a huge `u64`), matching the FabricAsset
    /// read path and the #936/#938 lesson.
    fn row_to_campaign(row: &postgres::Row) -> Result<Campaign, PgStoreError> {
        let cohorts_json: String = row.get(3);
        let stages_json: String = row.get(4);
        let state_s: String = row.get(7);
        let halt_s: Option<String> = row.get(8);

        // A malformed stored blob is stored-row CORRUPTION (the read-path inverse of
        // an encode failure), classified like SQLite's `json_decode_err` — NOT
        // `Encode`, which is strictly the write-path direction.
        let cohorts: Vec<String> = serde_json::from_str(&cohorts_json)
            .map_err(|e| PgStoreError::CorruptCampaignRow(format!("cohorts_json decode: {e}")))?;
        let stages: Vec<u8> = serde_json::from_str(&stages_json)
            .map_err(|e| PgStoreError::CorruptCampaignRow(format!("stages_json decode: {e}")))?;
        let state = CampaignState::parse(&state_s)
            .ok_or_else(|| PgStoreError::CorruptCampaignRow(format!("state token {state_s:?}")))?;
        let halt_reason = match halt_s {
            Some(s) => Some(HaltReason::parse(&s).ok_or_else(|| {
                PgStoreError::CorruptCampaignRow(format!("halt_reason token {s:?}"))
            })?),
            None => None,
        };

        // CHECKED numeric conversions — a tampered row must fail closed, never wrap a
        // negative/huge BIGINT into a bogus `usize`/`u8` the engine could index out
        // of bounds.
        let stage_index_raw: i64 = row.get(5);
        let stage_index = usize::try_from(stage_index_raw).map_err(|_| {
            PgStoreError::CorruptCampaignRow(format!("stage_index {stage_index_raw}"))
        })?;
        // The current stage must index into the schedule (true for every REACHABLE
        // campaign) — out of range is corruption.
        if stage_index >= stages.len() {
            return Err(PgStoreError::CorruptCampaignRow(format!(
                "stage_index {stage_index_raw} out of range for {} stage(s)",
                stages.len()
            )));
        }
        let rollout_raw: i64 = row.get(6);
        let rollout_percent = u8::try_from(rollout_raw)
            .ok()
            .filter(|p| *p <= 100)
            .ok_or_else(|| {
                PgStoreError::CorruptCampaignRow(format!("rollout_percent {rollout_raw}"))
            })?;

        Ok(Campaign {
            campaign_id: row.get(0),
            artifact_digest: row.get(1),
            artifact_version: row.get(2),
            cohorts,
            stages,
            stage_index,
            rollout_percent,
            state,
            halt_reason,
            // Fail-closed timestamp reads (negative → 0), matching the save-path
            // OutOfDomain guard and the FabricAsset read path.
            created_at_ms: u64::try_from(row.get::<_, i64>(9)).unwrap_or(0),
            updated_at_ms: u64::try_from(row.get::<_, i64>(10)).unwrap_or(0),
            artifact_signature_b64: row.get(11),
            uptane_metadata_json: row.get(12),
        })
    }

    fn row_to_node_artifact_status(row: &postgres::Row) -> NodeArtifactStatus {
        NodeArtifactStatus {
            node_id: row.get(0),
            applied_digest: row.get(1),
            campaign_id: row.get(2),
            artifact_version: row.get(3),
            // Fail-closed read (negative → 0), as everywhere else in this backend.
            reported_at_ms: u64::try_from(row.get::<_, i64>(4)).unwrap_or(0),
            attested: row.get(5),
        }
    }
}

impl OtaCampaignStore for PgVerifierStore {
    type Error = PgStoreError;

    fn insert_campaign(&mut self, campaign: &Campaign) -> Result<(), PgStoreError> {
        // Fail-closed on encode (a bad cohorts/stages blob is load-bearing — stages
        // bounds stage_index) and on out-of-domain timestamps (the #936 lesson).
        let cohorts_json =
            serde_json::to_string(&campaign.cohorts).map_err(PgStoreError::Encode)?;
        let stages_json = serde_json::to_string(&campaign.stages).map_err(PgStoreError::Encode)?;
        let stage_index =
            i64::try_from(campaign.stage_index).map_err(|_| PgStoreError::OutOfDomain {
                field: "stage_index",
                value: campaign.stage_index as u64,
            })?;
        let created_ms =
            i64::try_from(campaign.created_at_ms).map_err(|_| PgStoreError::OutOfDomain {
                field: "created_at_ms",
                value: campaign.created_at_ms,
            })?;
        let updated_ms =
            i64::try_from(campaign.updated_at_ms).map_err(|_| PgStoreError::OutOfDomain {
                field: "updated_at_ms",
                value: campaign.updated_at_ms,
            })?;
        let rollout = i64::from(campaign.rollout_percent);
        let state = campaign.state.as_str();
        let halt_reason = campaign.halt_reason.map(|r| r.as_str());
        // PLAIN INSERT — a duplicate `campaign_id` violates the PRIMARY KEY and
        // surfaces as a driver error (the storage contract's "duplicate id
        // conflicts, never a silent overwrite"), exactly as the SQLite backend's
        // plain INSERT. The audit-chaining SQLite additionally performs on create is
        // an inherent concern OUTSIDE this storage contract. Autocommit: the failed
        // INSERT is its own implicit tx, leaving the connection usable.
        self.lock().execute(
            "INSERT INTO ota_campaigns \
                 (campaign_id, artifact_digest, artifact_version, cohorts_json, stages_json, \
                  stage_index, rollout_percent, state, halt_reason, created_at_ms, updated_at_ms, \
                  artifact_signature_b64, uptane_metadata_json) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            &[
                &campaign.campaign_id,
                &campaign.artifact_digest,
                &campaign.artifact_version,
                &cohorts_json,
                &stages_json,
                &stage_index,
                &rollout,
                &state,
                &halt_reason,
                &created_ms,
                &updated_ms,
                &campaign.artifact_signature_b64,
                &campaign.uptane_metadata_json,
            ],
        )?;
        Ok(())
    }

    fn load_campaign(&self, campaign_id: &str) -> Result<Option<Campaign>, PgStoreError> {
        let sql = format!("SELECT {CAMPAIGN_COLUMNS} FROM ota_campaigns WHERE campaign_id = $1");
        let rows = self.lock().query(&sql, &[&campaign_id])?;
        match rows.first() {
            Some(row) => Ok(Some(Self::row_to_campaign(row)?)),
            None => Ok(None),
        }
    }

    fn load_campaigns(&self) -> Result<Vec<Campaign>, PgStoreError> {
        // Newest first: created_at_ms DESC, then campaign_id ASC (deterministic ties).
        let sql = format!(
            "SELECT {CAMPAIGN_COLUMNS} FROM ota_campaigns \
             ORDER BY created_at_ms DESC, campaign_id ASC"
        );
        let rows = self.lock().query(&sql, &[])?;
        rows.iter().map(Self::row_to_campaign).collect()
    }

    fn load_active_campaigns(&self) -> Result<Vec<Campaign>, PgStoreError> {
        // Only Staged/Rolling (the sweep-monitor set), oldest first: created_at_ms
        // ASC, then campaign_id ASC. The state tokens match `CampaignState::as_str`.
        let sql = format!(
            "SELECT {CAMPAIGN_COLUMNS} FROM ota_campaigns \
             WHERE state IN ('staged', 'rolling') \
             ORDER BY created_at_ms ASC, campaign_id ASC"
        );
        let rows = self.lock().query(&sql, &[])?;
        rows.iter().map(Self::row_to_campaign).collect()
    }

    fn upsert_node_artifact_status(&mut self, st: &NodeArtifactStatus) -> Result<(), PgStoreError> {
        let reported_ms =
            i64::try_from(st.reported_at_ms).map_err(|_| PgStoreError::OutOfDomain {
                field: "reported_at_ms",
                value: st.reported_at_ms,
            })?;
        // MONOTONIC upsert with attested-per-digest carry, byte-for-byte the SQLite
        // backend's semantics: the `WHERE excluded.reported_at_ms >= …` makes a
        // stale report a no-op; `attested` can only be SET or preserved-for-the-
        // same-digest, never cleared by a later unsigned same-digest report — but a
        // DIFFERENT digest resets it (the RHS reads the pre-update row).
        self.lock().execute(
            "INSERT INTO node_artifact_status \
                 (node_id, applied_digest, campaign_id, artifact_version, reported_at_ms, attested) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (node_id) DO UPDATE SET \
                 applied_digest   = EXCLUDED.applied_digest, \
                 campaign_id      = EXCLUDED.campaign_id, \
                 artifact_version = EXCLUDED.artifact_version, \
                 reported_at_ms   = EXCLUDED.reported_at_ms, \
                 attested         = EXCLUDED.attested \
                    OR (node_artifact_status.attested \
                        AND node_artifact_status.applied_digest = EXCLUDED.applied_digest) \
               WHERE EXCLUDED.reported_at_ms >= node_artifact_status.reported_at_ms",
            &[
                &st.node_id,
                &st.applied_digest,
                &st.campaign_id,
                &st.artifact_version,
                &reported_ms,
                &st.attested,
            ],
        )?;
        Ok(())
    }

    fn load_node_artifact_statuses(&self) -> Result<Vec<NodeArtifactStatus>, PgStoreError> {
        // Newest report first: reported_at_ms DESC, then node_id ASC.
        let rows = self.lock().query(
            "SELECT node_id, applied_digest, campaign_id, artifact_version, reported_at_ms, attested \
             FROM node_artifact_status ORDER BY reported_at_ms DESC, node_id ASC",
            &[],
        )?;
        Ok(rows.iter().map(Self::row_to_node_artifact_status).collect())
    }
}
