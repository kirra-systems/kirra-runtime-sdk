// src/verifier_store/ota_campaigns.rs
// OTA governor-artifact campaigns (WS-4 · Track 3 · Fleet Plane) — persistence for
// the `crate::ota_campaign` control-plane state machine.
//
// The engine (`kirra_ota_campaign::Campaign`) owns all transition logic and the
// fail-closed halt-on-regression rule; this module only persists a campaign and,
// on every lifecycle mutation, appends an R156-shaped audit-chain entry IN THE
// SAME atomic tx (the `record_grant_outcome` template — a forked chain is
// impossible because the mutation and the audit append share one `Immediate`
// transaction). Campaign state is durable so a `Halted` verdict survives a
// restart (a halted rollout must never silently resume).

use super::*;
use kirra_ota_campaign::{Campaign, CampaignState, HaltReason, NodeArtifactStatus};

impl VerifierStore {
    /// Persist a freshly-authored campaign (`Draft`) and append the
    /// `OtaCampaignCreated` audit entry, atomically. Plain INSERT — a duplicate
    /// `campaign_id` errors (the handler maps it to 409); creation never silently
    /// overwrites an existing campaign.
    pub fn insert_campaign(&mut self, campaign: &Campaign) -> Result<()> {
        let cohorts_json =
            serde_json::to_string(&campaign.cohorts).map_err(|e| json_encode_err("cohorts", e))?;
        let stages_json =
            serde_json::to_string(&campaign.stages).map_err(|e| json_encode_err("stages", e))?;
        let payload = campaign_audit_payload(campaign, "OtaCampaignCreated");

        let tx = Self::audit_tx(&mut self.conn)?; // #685: Immediate — non-forking audit append
        tx.execute(
            "INSERT INTO ota_campaigns
                 (campaign_id, artifact_digest, artifact_version, cohorts_json,
                  stages_json, stage_index, rollout_percent, state, halt_reason,
                  created_at_ms, updated_at_ms, artifact_signature_b64,
                  uptane_metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                campaign.campaign_id,
                campaign.artifact_digest,
                campaign.artifact_version,
                cohorts_json,
                stages_json,
                campaign.stage_index as i64,
                campaign.rollout_percent as i64,
                campaign.state.as_str(),
                campaign.halt_reason.map(|r| r.as_str()),
                campaign.created_at_ms as i64,
                campaign.updated_at_ms as i64,
                campaign.artifact_signature_b64,
                campaign.uptane_metadata_json,
            ],
        )?;
        ChainedAuditAppender {
            signing_key: self.signing_key.as_ref(),
        }
        .append_within(
            &tx,
            "OtaCampaignCreated",
            &payload,
            campaign.updated_at_ms as i64,
        )?;
        tx.commit()
    }

    /// Persist a lifecycle transition (arm / advance / halt / complete) of an
    /// EXISTING campaign and append the given R156-shaped audit `event_type`,
    /// atomically. The mutable fields (`stage_index`, `rollout_percent`, `state`,
    /// `halt_reason`, `updated_at_ms`) are rewritten from `campaign`; the immutable
    /// identity/schedule columns are left untouched. `updated_at_ms` is the audit
    /// timestamp (the engine stamps it on the transition).
    pub fn update_campaign(&mut self, campaign: &Campaign, event_type: &str) -> Result<()> {
        let payload = campaign_audit_payload(campaign, event_type);
        let tx = Self::audit_tx(&mut self.conn)?; // #685: Immediate — non-forking audit append
        let n = tx.execute(
            "UPDATE ota_campaigns
                SET stage_index     = ?2,
                    rollout_percent = ?3,
                    state           = ?4,
                    halt_reason     = ?5,
                    updated_at_ms   = ?6
              WHERE campaign_id = ?1",
            params![
                campaign.campaign_id,
                campaign.stage_index as i64,
                campaign.rollout_percent as i64,
                campaign.state.as_str(),
                campaign.halt_reason.map(|r| r.as_str()),
                campaign.updated_at_ms as i64,
            ],
        )?;
        if n == 0 {
            // No such campaign — do NOT write an audit entry for a phantom mutation.
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        ChainedAuditAppender {
            signing_key: self.signing_key.as_ref(),
        }
        .append_within(&tx, event_type, &payload, campaign.updated_at_ms as i64)?;
        tx.commit()
    }

    /// Load one campaign by id. `None` if absent; a stored row that no longer
    /// parses (unknown state token / malformed JSON) is a fail-closed error, not a
    /// silent `None`.
    pub fn load_campaign(&self, campaign_id: &str) -> Result<Option<Campaign>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT campaign_id, artifact_digest, artifact_version, cohorts_json,
                        stages_json, stage_index, rollout_percent, state, halt_reason,
                        created_at_ms, updated_at_ms, artifact_signature_b64, uptane_metadata_json
                 FROM ota_campaigns WHERE campaign_id = ?1",
                params![campaign_id],
                Self::map_campaign_row,
            )
            .optional()
    }

    /// Read-only listing of every campaign, newest first.
    pub fn load_campaigns(&self) -> Result<Vec<Campaign>> {
        let mut stmt = self.conn.prepare(
            "SELECT campaign_id, artifact_digest, artifact_version, cohorts_json,
                    stages_json, stage_index, rollout_percent, state, halt_reason,
                    created_at_ms, updated_at_ms, artifact_signature_b64, uptane_metadata_json
             FROM ota_campaigns ORDER BY created_at_ms DESC, campaign_id ASC",
        )?;
        let rows = stmt.query_map([], Self::map_campaign_row)?;
        rows.collect()
    }

    /// Load only the *active* campaigns (`Staged` / `Rolling`) — the ones the
    /// background posture-sweep monitor must re-check for halt-on-regression.
    /// Terminal campaigns (`Halted` / `Completed`) and un-armed `Draft`s are
    /// excluded at the query, so the sweep never touches them. Oldest first
    /// (deterministic sweep order).
    pub fn load_active_campaigns(&self) -> Result<Vec<Campaign>> {
        let mut stmt = self.conn.prepare(
            "SELECT campaign_id, artifact_digest, artifact_version, cohorts_json,
                    stages_json, stage_index, rollout_percent, state, halt_reason,
                    created_at_ms, updated_at_ms, artifact_signature_b64, uptane_metadata_json
             FROM ota_campaigns
             WHERE state IN ('staged', 'rolling')
             ORDER BY created_at_ms ASC, campaign_id ASC",
        )?;
        let rows = stmt.query_map([], Self::map_campaign_row)?;
        rows.collect()
    }

    /// Upsert a node's artifact-adoption report (keyed by `node_id` — the latest
    /// report replaces the prior one; a node runs one governor at a time). Pure
    /// observability: a plain upsert, NOT audit-chained (these are high-volume
    /// telemetry, not security mutations, and never gate any actuator decision).
    pub fn upsert_node_artifact_status(&mut self, st: &NodeArtifactStatus) -> Result<()> {
        // MONOTONIC upsert: a report only replaces the stored one when its
        // `reported_at_ms` is NOT older (the `WHERE` on the DO UPDATE). This makes a
        // replayed OLD report (signed or not) a no-op — it can never move a node's
        // recorded adoption backward in time.
        self.conn.execute(
            "INSERT INTO node_artifact_status
                 (node_id, applied_digest, campaign_id, artifact_version, reported_at_ms, attested)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(node_id) DO UPDATE SET
                 applied_digest   = excluded.applied_digest,
                 campaign_id      = excluded.campaign_id,
                 artifact_version = excluded.artifact_version,
                 reported_at_ms   = excluded.reported_at_ms,
                 -- `attested` is MONOTONIC PER DIGEST: a later report for the SAME
                 -- digest cannot CLEAR unforgeable attestation evidence (so a token
                 -- holder can't erase an attested adoption with an unsigned report),
                 -- but a report for a DIFFERENT digest is a fresh claim that must
                 -- re-earn attestation. (RHS references read the pre-update row.)
                 attested         = excluded.attested
                    OR (node_artifact_status.attested
                        AND node_artifact_status.applied_digest = excluded.applied_digest)
               WHERE excluded.reported_at_ms >= node_artifact_status.reported_at_ms",
            params![
                st.node_id,
                st.applied_digest,
                st.campaign_id,
                st.artifact_version,
                st.reported_at_ms as i64,
                st.attested as i64,
            ],
        )?;
        Ok(())
    }

    /// Load every node's latest artifact-adoption report (for the fleet summary
    /// join). Newest report first.
    pub fn load_node_artifact_statuses(&self) -> Result<Vec<NodeArtifactStatus>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, applied_digest, campaign_id, artifact_version, reported_at_ms, attested
             FROM node_artifact_status ORDER BY reported_at_ms DESC, node_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(NodeArtifactStatus {
                node_id: row.get(0)?,
                applied_digest: row.get(1)?,
                campaign_id: row.get(2)?,
                artifact_version: row.get(3)?,
                reported_at_ms: row.get::<_, i64>(4)? as u64,
                attested: row.get::<_, i64>(5)? != 0,
            })
        })?;
        rows.collect()
    }

    fn map_campaign_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Campaign> {
        let cohorts_json: String = row.get(3)?;
        let stages_json: String = row.get(4)?;
        let state_str: String = row.get(7)?;
        let halt_str: Option<String> = row.get(8)?;

        let cohorts: Vec<String> =
            serde_json::from_str(&cohorts_json).map_err(|e| json_decode_err("cohorts", e))?;
        let stages: Vec<u8> =
            serde_json::from_str(&stages_json).map_err(|e| json_decode_err("stages", e))?;
        let state =
            CampaignState::parse(&state_str).ok_or_else(|| corrupt_row_err("state", &state_str))?;
        let halt_reason = match halt_str {
            Some(s) => {
                Some(HaltReason::parse(&s).ok_or_else(|| corrupt_row_err("halt_reason", &s))?)
            }
            None => None,
        };

        // CHECKED numeric conversions — a tampered row must fail closed, never wrap a
        // negative/huge INTEGER into a bogus `usize`/`u8` that the engine could later
        // index out of bounds (`Campaign::advance` -> `stages[stage_index]`). Fail
        // closed here so a corrupt row never becomes a live, panic-prone `Campaign`.
        let stage_index_raw: i64 = row.get(5)?;
        let stage_index = usize::try_from(stage_index_raw)
            .map_err(|_| corrupt_row_err("stage_index", &stage_index_raw.to_string()))?;
        // The current stage must index into the schedule — true for every REACHABLE
        // campaign (Draft/Staged sit at 0 over a non-empty schedule; Rolling/terminal
        // sit at a real stage), so a value out of range is corruption.
        if stage_index >= stages.len() {
            return Err(corrupt_row_err("stage_index", &stage_index_raw.to_string()));
        }
        let rollout_raw: i64 = row.get(6)?;
        let rollout_percent = u8::try_from(rollout_raw)
            .ok()
            .filter(|p| *p <= 100)
            .ok_or_else(|| corrupt_row_err("rollout_percent", &rollout_raw.to_string()))?;

        Ok(Campaign {
            campaign_id: row.get(0)?,
            artifact_digest: row.get(1)?,
            artifact_version: row.get(2)?,
            cohorts,
            stages,
            stage_index,
            rollout_percent,
            state,
            halt_reason,
            created_at_ms: row.get::<_, i64>(9)? as u64,
            updated_at_ms: row.get::<_, i64>(10)? as u64,
            artifact_signature_b64: row.get(11)?,
            uptane_metadata_json: row.get(12)?,
        })
    }
}

/// The R156-shaped audit payload for a campaign lifecycle event: the update
/// identity (artifact digest + version), the target cohorts, and the rollout
/// state reached. Never carries any secret. `action` is the event type.
fn campaign_audit_payload(campaign: &Campaign, action: &str) -> String {
    serde_json::json!({
        "action": action,
        "campaign_id": campaign.campaign_id,
        "artifact_digest": campaign.artifact_digest,
        "artifact_version": campaign.artifact_version,
        "cohorts": campaign.cohorts,
        "rollout_percent": campaign.rollout_percent,
        "state": campaign.state.as_str(),
        "halt_reason": campaign.halt_reason.map(|r| r.as_str()),
    })
    .to_string()
}

fn json_encode_err(field: &str, e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("ota_campaigns.{field} encode: {e}"),
    )))
}

fn json_decode_err(field: &str, e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("ota_campaigns.{field} decode: {e}"),
        )),
    )
}

fn corrupt_row_err(field: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("ota_campaigns.{field} corrupt token: {value:?}"),
        )),
    )
}

// ---------------------------------------------------------------------------
// ADR-0035 Stage 2.5 seam step (family 1) — the OTA-campaign storage trait
//
// The ota_campaigns family, seamed as CRUD exactly like the clean six
// (NodeStore/OperatorStore idiom): the trait shares the inherent method names, so
// inherent methods win resolution — every existing `store.insert_campaign(...)` /
// `store.load_campaign(...)` caller is untouched and the SQLite impl delegates via
// `self.method()` WITHOUT recursion. A second in-memory backend + a shared
// conformance test prove the family's STORAGE contract is genuinely
// backend-portable (SQLite realizes it over the `ota_campaigns` /
// `node_artifact_status` tables).
//
// Scope, matching OperatorStore's discipline: this trait models the pure,
// backend-portable STORAGE surface — campaign persistence + reads and the
// non-audit node-adoption CRUD. `update_campaign` stays INHERENT-ONLY: it carries
// the R156 `event_type` and appends a signed audit entry in the same tx (a
// safety-authority concern already inverted via the `AuditAppender` seam), so it
// belongs to the harder persistence tier, not this storage contract — the same
// reason OperatorStore left `save_clearance_grant_chained` inherent. `insert_campaign`
// IS modelled (its contract is "persist a Draft; a duplicate id conflicts"); the
// SQLite backend additionally audit-chains it, a side effect ORTHOGONAL to — and
// invisible to — the storage contract exercised here.
// ---------------------------------------------------------------------------

/// The OTA-campaign storage contract — persist a campaign + read it back
/// (by id / all / active-only), and upsert/read the per-node artifact-adoption
/// reports. Backend-agnostic; the audit-chaining of lifecycle mutations is a
/// separate (SQLite-backend) concern layered via [`AuditAppender`].
pub trait OtaCampaignStore {
    /// Backend error type (SQLite: `rusqlite::Error`; in-memory: [`InMemOtaError`]).
    type Error;

    /// Persist a new campaign (typically freshly-authored `Draft`, but the storage
    /// layer accepts any state — lifecycle validity is the engine's concern, not
    /// the store's; the conformance suite relies on this to seed a `Staged`
    /// campaign for the active-filter check without the inherent, audit-chained
    /// `update_campaign`). INSERT semantics — a duplicate `campaign_id` is an error,
    /// never a silent overwrite.
    fn insert_campaign(&mut self, campaign: &Campaign) -> std::result::Result<(), Self::Error>;

    /// Load one campaign by id, or `None` if absent. A stored row that no longer
    /// parses is a fail-closed error, never a silent `None`.
    fn load_campaign(
        &self,
        campaign_id: &str,
    ) -> std::result::Result<Option<Campaign>, Self::Error>;

    /// Every campaign, newest first (`created_at_ms` DESC, then `campaign_id` ASC).
    fn load_campaigns(&self) -> std::result::Result<Vec<Campaign>, Self::Error>;

    /// Only the *active* campaigns (`Staged` / `Rolling`), oldest first
    /// (`created_at_ms` ASC, then `campaign_id` ASC) — the sweep-monitor set.
    fn load_active_campaigns(&self) -> std::result::Result<Vec<Campaign>, Self::Error>;

    /// Upsert a node's artifact-adoption report (keyed by `node_id`). MONOTONIC on
    /// `reported_at_ms` (a stale report is a no-op); `attested` is monotonic per
    /// digest (a later unsigned report for the SAME digest cannot clear attestation;
    /// a different digest resets it).
    fn upsert_node_artifact_status(
        &mut self,
        st: &NodeArtifactStatus,
    ) -> std::result::Result<(), Self::Error>;

    /// Every node's latest adoption report, newest first
    /// (`reported_at_ms` DESC, then `node_id` ASC).
    fn load_node_artifact_statuses(
        &self,
    ) -> std::result::Result<Vec<NodeArtifactStatus>, Self::Error>;
}

/// The production SQLite backend: delegates to the inherent `VerifierStore` methods
/// over the `ota_campaigns` / `node_artifact_status` tables. `self.method()`
/// resolves to the INHERENT method (inherent wins over the trait), so this is
/// delegation, not recursion.
impl OtaCampaignStore for VerifierStore {
    type Error = rusqlite::Error;

    fn insert_campaign(&mut self, campaign: &Campaign) -> Result<()> {
        self.insert_campaign(campaign)
    }
    fn load_campaign(&self, campaign_id: &str) -> Result<Option<Campaign>> {
        self.load_campaign(campaign_id)
    }
    fn load_campaigns(&self) -> Result<Vec<Campaign>> {
        self.load_campaigns()
    }
    fn load_active_campaigns(&self) -> Result<Vec<Campaign>> {
        self.load_active_campaigns()
    }
    fn upsert_node_artifact_status(&mut self, st: &NodeArtifactStatus) -> Result<()> {
        self.upsert_node_artifact_status(st)
    }
    fn load_node_artifact_statuses(&self) -> Result<Vec<NodeArtifactStatus>> {
        self.load_node_artifact_statuses()
    }
}

/// In-memory [`OtaCampaignStore`] error: the one failure the storage contract can
/// produce WITHOUT a database — a duplicate `campaign_id` on insert (SQLite raises
/// the same via the `PRIMARY KEY` constraint). A real enum (not `Infallible`)
/// because the contract genuinely has a reject-before-mutate case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InMemOtaError {
    /// `insert_campaign` for an already-present `campaign_id`.
    DuplicateCampaign(String),
}

impl std::fmt::Display for InMemOtaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateCampaign(id) => write!(f, "duplicate campaign_id: {id}"),
        }
    }
}

impl std::error::Error for InMemOtaError {}

/// The in-memory [`OtaCampaignStore`] backend — a portability-proof reference
/// modelling the `ota_campaigns` + `node_artifact_status` tables as maps keyed by
/// `campaign_id` / `node_id`. Realizes the SAME insert-conflict, ordering,
/// active-filter, monotonic-upsert, and attested-per-digest semantics WITHOUT a
/// database, so the family's storage contract is exercised against two backends.
/// It does NOT audit-chain (that is the SQLite backend's `AuditAppender` concern,
/// outside this storage contract). Single-process; `&mut self` writes need no
/// interior mutability (matching the inherent `&mut self` writers).
#[derive(Debug, Default)]
pub struct InMemoryOtaCampaignStore {
    campaigns: std::collections::HashMap<String, Campaign>,
    statuses: std::collections::HashMap<String, NodeArtifactStatus>,
}

impl OtaCampaignStore for InMemoryOtaCampaignStore {
    type Error = InMemOtaError;

    fn insert_campaign(&mut self, campaign: &Campaign) -> std::result::Result<(), InMemOtaError> {
        if self.campaigns.contains_key(&campaign.campaign_id) {
            // Reject BEFORE mutate — mirrors SQLite's plain INSERT conflict.
            return Err(InMemOtaError::DuplicateCampaign(
                campaign.campaign_id.clone(),
            ));
        }
        self.campaigns
            .insert(campaign.campaign_id.clone(), campaign.clone());
        Ok(())
    }

    fn load_campaign(
        &self,
        campaign_id: &str,
    ) -> std::result::Result<Option<Campaign>, InMemOtaError> {
        Ok(self.campaigns.get(campaign_id).cloned())
    }

    fn load_campaigns(&self) -> std::result::Result<Vec<Campaign>, InMemOtaError> {
        let mut all: Vec<Campaign> = self.campaigns.values().cloned().collect();
        // Newest first: created_at_ms DESC, then campaign_id ASC.
        all.sort_by(|a, b| {
            b.created_at_ms
                .cmp(&a.created_at_ms)
                .then_with(|| a.campaign_id.cmp(&b.campaign_id))
        });
        Ok(all)
    }

    fn load_active_campaigns(&self) -> std::result::Result<Vec<Campaign>, InMemOtaError> {
        let mut active: Vec<Campaign> = self
            .campaigns
            .values()
            .filter(|c| matches!(c.state, CampaignState::Staged | CampaignState::Rolling))
            .cloned()
            .collect();
        // Oldest first: created_at_ms ASC, then campaign_id ASC.
        active.sort_by(|a, b| {
            a.created_at_ms
                .cmp(&b.created_at_ms)
                .then_with(|| a.campaign_id.cmp(&b.campaign_id))
        });
        Ok(active)
    }

    fn upsert_node_artifact_status(
        &mut self,
        st: &NodeArtifactStatus,
    ) -> std::result::Result<(), InMemOtaError> {
        match self.statuses.get(&st.node_id) {
            // Fresh node — plain insert.
            None => {
                self.statuses.insert(st.node_id.clone(), st.clone());
            }
            Some(prev) => {
                // MONOTONIC: a report OLDER than the stored one is a no-op; an
                // equal-or-newer timestamp proceeds (the `>=` mirrors the SQLite
                // backend's `WHERE excluded.reported_at_ms >= …` guard exactly).
                if st.reported_at_ms >= prev.reported_at_ms {
                    // `attested` is monotonic PER DIGEST: a later report for the
                    // SAME digest cannot clear unforgeable evidence; a different
                    // digest is a fresh claim that must re-earn attestation.
                    let attested =
                        st.attested || (prev.attested && prev.applied_digest == st.applied_digest);
                    let mut next = st.clone();
                    next.attested = attested;
                    self.statuses.insert(st.node_id.clone(), next);
                }
            }
        }
        Ok(())
    }

    fn load_node_artifact_statuses(
        &self,
    ) -> std::result::Result<Vec<NodeArtifactStatus>, InMemOtaError> {
        let mut all: Vec<NodeArtifactStatus> = self.statuses.values().cloned().collect();
        // Newest first: reported_at_ms DESC, then node_id ASC.
        all.sort_by(|a, b| {
            b.reported_at_ms
                .cmp(&a.reported_at_ms)
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
        Ok(all)
    }
}

/// The OTA-campaign storage contract, driven through [`OtaCampaignStore`] so it
/// runs IDENTICALLY against every backend: insert→load roundtrip, duplicate-id
/// conflict, newest-first listing, the active-only (`Staged`/`Rolling`) filter with
/// oldest-first order, and the node-adoption upsert's monotonic + attested-per-digest
/// invariants.
///
/// `pub` (not `#[cfg(test)]`) by design — the shared backend-conformance suite,
/// mirroring `assert_node_store_contract`. Panics on any violation; call from a test.
///
/// PRECONDITION: `store` must start empty.
pub fn assert_ota_campaign_store_contract<S: OtaCampaignStore>(store: &mut S)
where
    S::Error: core::fmt::Debug,
{
    fn draft(id: &str, created_at_ms: u64) -> Campaign {
        Campaign::new(
            id,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "v1",
            vec!["fleet".to_string()],
            vec![50, 100],
            created_at_ms,
        )
        .expect("valid draft")
    }

    // Empty store.
    assert!(store.load_campaign("camp-1").unwrap().is_none());
    assert!(store.load_campaigns().unwrap().is_empty());
    assert!(store.load_active_campaigns().unwrap().is_empty());
    assert!(store.load_node_artifact_statuses().unwrap().is_empty());

    // Insert → load roundtrip.
    let c1 = draft("camp-1", 1_000);
    store.insert_campaign(&c1).unwrap();
    assert_eq!(store.load_campaign("camp-1").unwrap().as_ref(), Some(&c1));

    // Duplicate id conflicts (never a silent overwrite).
    assert!(
        store.insert_campaign(&draft("camp-1", 9_999)).is_err(),
        "duplicate campaign_id must conflict"
    );

    // A second campaign, armed into `Staged` (active); listing is newest-first.
    let mut c2 = draft("camp-2", 2_000);
    c2.arm(2_100).unwrap();
    store.insert_campaign(&c2).unwrap();
    let all_ids: Vec<String> = store
        .load_campaigns()
        .unwrap()
        .into_iter()
        .map(|c| c.campaign_id)
        .collect();
    assert_eq!(all_ids, vec!["camp-2", "camp-1"], "newest first");

    // Active filter: camp-1 is Draft (excluded), camp-2 is Staged (included).
    let active_ids: Vec<String> = store
        .load_active_campaigns()
        .unwrap()
        .into_iter()
        .map(|c| c.campaign_id)
        .collect();
    assert_eq!(active_ids, vec!["camp-2"], "only Staged/Rolling are active");

    // Node adoption: monotonic upsert + attested-per-digest.
    let dg_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let dg_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let report = |node: &str, digest: &str, at: u64, attested: bool| NodeArtifactStatus {
        node_id: node.to_string(),
        applied_digest: digest.to_string(),
        campaign_id: Some("camp-1".to_string()),
        artifact_version: Some("v1".to_string()),
        reported_at_ms: at,
        attested,
    };
    // Signed report → attested.
    store
        .upsert_node_artifact_status(&report("robot-1", dg_a, 1_000, true))
        .unwrap();
    // A STALE report is a no-op (timestamp older).
    store
        .upsert_node_artifact_status(&report("robot-1", dg_b, 500, false))
        .unwrap();
    {
        let r = store.load_node_artifact_statuses().unwrap();
        let r1 = r.iter().find(|r| r.node_id == "robot-1").unwrap();
        assert_eq!(r1.reported_at_ms, 1_000, "stale report must not overwrite");
        assert_eq!(r1.applied_digest, dg_a, "stale digest must not win");
        assert!(r1.attested);
    }
    // A LATER UNSIGNED report for the SAME digest preserves attestation.
    store
        .upsert_node_artifact_status(&report("robot-1", dg_a, 2_000, false))
        .unwrap();
    assert!(
        store
            .load_node_artifact_statuses()
            .unwrap()
            .iter()
            .find(|r| r.node_id == "robot-1")
            .unwrap()
            .attested,
        "same-digest unsigned report preserves attested"
    );
    // A DIFFERENT digest resets attestation.
    store
        .upsert_node_artifact_status(&report("robot-1", dg_b, 3_000, false))
        .unwrap();
    assert!(
        !store
            .load_node_artifact_statuses()
            .unwrap()
            .iter()
            .find(|r| r.node_id == "robot-1")
            .unwrap()
            .attested,
        "a different digest resets attested"
    );

    // A second node; both are listed, newest first.
    store
        .upsert_node_artifact_status(&report("robot-2", dg_a, 4_000, false))
        .unwrap();
    let node_ids: Vec<String> = store
        .load_node_artifact_statuses()
        .unwrap()
        .into_iter()
        .map(|r| r.node_id)
        .collect();
    assert_eq!(node_ids, vec!["robot-2", "robot-1"], "newest report first");
}

#[cfg(test)]
mod ota_campaign_store_contract_tests {
    use super::*;

    #[test]
    fn sqlite_backend_satisfies_the_ota_campaign_store_contract() {
        let mut store = VerifierStore::new(":memory:").expect("in-memory store");
        assert_ota_campaign_store_contract(&mut store);
    }

    #[test]
    fn in_memory_backend_satisfies_the_ota_campaign_store_contract() {
        assert_ota_campaign_store_contract(&mut InMemoryOtaCampaignStore::default());
    }
}

#[cfg(test)]
mod tests {
    use crate::verifier_store::VerifierStore;
    use kirra_core::FleetPosture;
    use kirra_ota_campaign::{AdvanceOutcome, Campaign, CampaignState, HaltReason};

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn store() -> VerifierStore {
        VerifierStore::new(":memory:").expect("in-memory store")
    }

    fn draft() -> Campaign {
        Campaign::new(
            "camp-1",
            DIGEST,
            "v1.2.3",
            vec!["canary".into(), "fleet".into()],
            vec![10, 50, 100],
            1_000,
        )
        .unwrap()
    }

    #[test]
    fn insert_then_load_roundtrips() {
        let mut s = store();
        let c = draft();
        s.insert_campaign(&c).unwrap();
        let loaded = s.load_campaign("camp-1").unwrap().expect("present");
        assert_eq!(loaded, c);
        assert!(s.load_campaign("absent").unwrap().is_none());
    }

    #[test]
    fn uptane_metadata_json_roundtrips() {
        // EP-13: the carried metadata set survives persistence byte-for-byte
        // (the store is part of the untrusted carrier — it must relay, never
        // rewrite, what the repository signed over).
        let mut s = store();
        let mut c = draft();
        c.uptane_metadata_json = Some(r#"{"timestamp":{"version":3}}"#.to_string());
        s.insert_campaign(&c).unwrap();
        let loaded = s.load_campaign("camp-1").unwrap().expect("present");
        assert_eq!(loaded, c);
        assert_eq!(
            loaded.uptane_metadata_json.as_deref(),
            Some(r#"{"timestamp":{"version":3}}"#)
        );
        // A legacy campaign without metadata loads as None.
        let mut legacy = draft();
        legacy.campaign_id = "camp-legacy".into();
        s.insert_campaign(&legacy).unwrap();
        assert!(s
            .load_campaign("camp-legacy")
            .unwrap()
            .unwrap()
            .uptane_metadata_json
            .is_none());
    }

    /// EP-13 live-upgrade drill: a REAL v1-era database (ota_campaigns without
    /// the `uptane_metadata_json` column, `user_version` stamped 1) opened by
    /// this binary migrates in place — the v2 step adds the column, legacy rows
    /// load as `None`, and a metadata-carrying campaign then persists.
    #[test]
    fn a_v1_database_migrates_in_place_and_gains_the_uptane_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.sqlite");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE ota_campaigns (
                    campaign_id       TEXT    PRIMARY KEY,
                    artifact_digest   TEXT    NOT NULL,
                    artifact_version  TEXT    NOT NULL,
                    cohorts_json      TEXT    NOT NULL,
                    stages_json       TEXT    NOT NULL,
                    stage_index       INTEGER NOT NULL DEFAULT 0,
                    rollout_percent   INTEGER NOT NULL DEFAULT 0,
                    state             TEXT    NOT NULL,
                    halt_reason       TEXT,
                    created_at_ms     INTEGER NOT NULL,
                    updated_at_ms     INTEGER NOT NULL,
                    artifact_signature_b64 TEXT
                );
                INSERT INTO ota_campaigns VALUES
                    ('camp-old', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     'v1', '[\"fleet\"]', '[100]', 0, 0, 'draft', NULL, 1, 1, NULL);
                PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let mut s = VerifierStore::new(path.to_str().unwrap()).expect("v1 DB migrates on open");
        let legacy = s
            .load_campaign("camp-old")
            .unwrap()
            .expect("legacy row survives");
        assert!(
            legacy.uptane_metadata_json.is_none(),
            "pre-migration row reads None"
        );
        let mut c = draft();
        c.uptane_metadata_json = Some("{}".to_string());
        s.insert_campaign(&c).unwrap();
        assert_eq!(
            s.load_campaign("camp-1")
                .unwrap()
                .unwrap()
                .uptane_metadata_json
                .as_deref(),
            Some("{}")
        );
    }

    #[test]
    fn duplicate_id_conflicts() {
        let mut s = store();
        s.insert_campaign(&draft()).unwrap();
        assert!(
            s.insert_campaign(&draft()).is_err(),
            "duplicate campaign_id must conflict"
        );
    }

    #[test]
    fn lifecycle_transitions_persist() {
        let mut s = store();
        let mut c = draft();
        s.insert_campaign(&c).unwrap();

        c.arm(1_100).unwrap();
        s.update_campaign(&c, "OtaCampaignArmed").unwrap();
        assert_eq!(
            s.load_campaign("camp-1").unwrap().unwrap().state,
            CampaignState::Staged
        );

        assert_eq!(
            c.advance(FleetPosture::Nominal, 1_200).unwrap(),
            AdvanceOutcome::Advanced {
                rollout_percent: 10
            }
        );
        s.update_campaign(&c, "OtaCampaignAdvanced").unwrap();
        let loaded = s.load_campaign("camp-1").unwrap().unwrap();
        assert_eq!(loaded.state, CampaignState::Rolling);
        assert_eq!(loaded.rollout_percent, 10);
    }

    #[test]
    fn halt_persists_and_is_durable_terminal() {
        let mut s = store();
        let mut c = draft();
        c.arm(1_100).unwrap();
        s.insert_campaign(&c).unwrap();
        c.advance(FleetPosture::LockedOut, 1_200).unwrap(); // halts
        s.update_campaign(&c, "OtaCampaignHalted").unwrap();
        let loaded = s.load_campaign("camp-1").unwrap().unwrap();
        assert_eq!(loaded.state, CampaignState::Halted);
        assert_eq!(loaded.halt_reason, Some(HaltReason::PostureLockedOut));
    }

    #[test]
    fn corrupt_stage_index_row_fails_closed_on_load() {
        // Simulate a tampered row: a persisted campaign whose `stage_index` is out
        // of range for its `stages`. `load_campaign` must reject it (fail-closed),
        // never hand back a `Campaign` the engine could later index out of bounds.
        let mut s = store();
        let mut bad = draft(); // stages = [10, 50, 100]
        bad.state = CampaignState::Rolling;
        bad.stage_index = 99; // >= stages.len()
        s.insert_campaign(&bad).unwrap();
        let err = s.load_campaign("camp-1");
        assert!(
            err.is_err(),
            "an out-of-range stage_index row must fail closed on load"
        );
    }

    #[test]
    fn update_of_absent_campaign_errors_without_audit() {
        let mut s = store();
        let before = s.count_audit_events_for_test("OtaCampaignAdvanced");
        let mut c = draft();
        c.arm(1_100).unwrap();
        c.advance(FleetPosture::Nominal, 1_200).unwrap();
        // never inserted → update must error and write NO audit entry
        assert!(s.update_campaign(&c, "OtaCampaignAdvanced").is_err());
        assert_eq!(s.count_audit_events_for_test("OtaCampaignAdvanced"), before);
    }

    #[test]
    fn every_transition_writes_one_audit_entry() {
        let mut s = store();
        let mut c = draft();
        s.insert_campaign(&c).unwrap();
        c.arm(1_100).unwrap();
        s.update_campaign(&c, "OtaCampaignArmed").unwrap();
        c.advance(FleetPosture::Nominal, 1_200).unwrap();
        s.update_campaign(&c, "OtaCampaignAdvanced").unwrap();
        assert_eq!(s.count_audit_events_for_test("OtaCampaignCreated"), 1);
        assert_eq!(s.count_audit_events_for_test("OtaCampaignArmed"), 1);
        assert_eq!(s.count_audit_events_for_test("OtaCampaignAdvanced"), 1);
    }

    #[test]
    fn list_returns_all() {
        let mut s = store();
        s.insert_campaign(&draft()).unwrap();
        let mut c2 = Campaign::new(
            "camp-2",
            DIGEST,
            "v2",
            vec!["fleet".into()],
            vec![100],
            2_000,
        )
        .unwrap();
        c2.arm(2_100).unwrap();
        s.insert_campaign(&c2).unwrap();
        let all = s.load_campaigns().unwrap();
        assert_eq!(all.len(), 2);
        // Newest first by created_at_ms.
        assert_eq!(all[0].campaign_id, "camp-2");
    }

    #[test]
    fn node_artifact_status_upserts_latest_wins() {
        use kirra_ota_campaign::NodeArtifactStatus;
        let mut s = store();
        let st = |digest: &str, at: u64| NodeArtifactStatus {
            node_id: "robot-01".into(),
            applied_digest: digest.into(),
            campaign_id: Some("camp-1".into()),
            artifact_version: Some("v2".into()),
            reported_at_ms: at,
            attested: false,
        };
        s.upsert_node_artifact_status(&st(DIGEST, 1_000)).unwrap();
        // A second node on a different digest.
        let other = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        s.upsert_node_artifact_status(&NodeArtifactStatus {
            node_id: "robot-02".into(),
            applied_digest: other.into(),
            campaign_id: None,
            artifact_version: None,
            reported_at_ms: 1_001,
            attested: true,
        })
        .unwrap();
        // robot-01 re-reports a newer digest → the row is REPLACED, not duplicated.
        s.upsert_node_artifact_status(&st(other, 2_000)).unwrap();

        let all = s.load_node_artifact_statuses().unwrap();
        assert_eq!(all.len(), 2, "one row per node (upsert, not append)");
        let r01 = all.iter().find(|r| r.node_id == "robot-01").unwrap();
        assert_eq!(r01.applied_digest, other, "latest report wins");
        assert_eq!(r01.reported_at_ms, 2_000);
        let r02 = all.iter().find(|r| r.node_id == "robot-02").unwrap();
        assert!(r02.attested, "attested flag round-trips");

        // MONOTONIC guard: a STALE report (older timestamp) is a no-op, never
        // overwriting the newer stored one.
        s.upsert_node_artifact_status(&st(DIGEST, 500)).unwrap();
        let r01 = s
            .load_node_artifact_statuses()
            .unwrap()
            .into_iter()
            .find(|r| r.node_id == "robot-01")
            .unwrap();
        assert_eq!(r01.reported_at_ms, 2_000, "stale report must not overwrite");
        assert_eq!(r01.applied_digest, other, "stale digest must not win");
    }

    #[test]
    fn attested_flag_is_monotonic_per_digest() {
        use kirra_ota_campaign::NodeArtifactStatus;
        let other = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let mut s = store();
        let mk = |digest: &str, at: u64, attested: bool| NodeArtifactStatus {
            node_id: "robot-1".into(),
            applied_digest: digest.into(),
            campaign_id: None,
            artifact_version: None,
            reported_at_ms: at,
            attested,
        };
        let attested_of = |s: &VerifierStore| {
            s.load_node_artifact_statuses()
                .unwrap()
                .into_iter()
                .find(|r| r.node_id == "robot-1")
                .unwrap()
                .attested
        };

        // Signed report → attested.
        s.upsert_node_artifact_status(&mk(DIGEST, 1_000, true))
            .unwrap();
        assert!(attested_of(&s));
        // A LATER UNSIGNED report for the SAME digest must NOT clear attestation
        // (a token holder can't erase unforgeable evidence with an unsigned report).
        s.upsert_node_artifact_status(&mk(DIGEST, 2_000, false))
            .unwrap();
        assert!(
            attested_of(&s),
            "same-digest unsigned report preserves attested"
        );
        // A report for a DIFFERENT digest is a fresh claim → attestation must re-earn.
        s.upsert_node_artifact_status(&mk(other, 3_000, false))
            .unwrap();
        assert!(!attested_of(&s), "a different digest resets attested");
    }

    #[test]
    fn load_active_excludes_draft_and_terminal() {
        let mut s = store();
        // Draft (un-armed) — excluded.
        s.insert_campaign(&draft()).unwrap();
        // Staged — included.
        let mut staged = Campaign::new(
            "camp-staged",
            DIGEST,
            "v2",
            vec!["a".into()],
            vec![100],
            2_000,
        )
        .unwrap();
        staged.arm(2_100).unwrap();
        s.insert_campaign(&staged).unwrap();
        // Rolling — included.
        let mut rolling = Campaign::new(
            "camp-rolling",
            DIGEST,
            "v3",
            vec!["a".into()],
            vec![50, 100],
            3_000,
        )
        .unwrap();
        rolling.arm(3_100).unwrap();
        rolling.advance(FleetPosture::Nominal, 3_200).unwrap();
        s.insert_campaign(&rolling).unwrap();
        // Halted (terminal) — excluded.
        let mut halted = Campaign::new(
            "camp-halted",
            DIGEST,
            "v4",
            vec!["a".into()],
            vec![100],
            4_000,
        )
        .unwrap();
        halted.arm(4_100).unwrap();
        halted.halt(HaltReason::OperatorHalt, 4_200).unwrap();
        s.insert_campaign(&halted).unwrap();

        let active = s.load_active_campaigns().unwrap();
        let ids: Vec<_> = active.iter().map(|c| c.campaign_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["camp-staged", "camp-rolling"],
            "only Staged/Rolling, oldest first"
        );
    }
}
