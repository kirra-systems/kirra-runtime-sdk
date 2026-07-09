// src/verifier_store/ota_campaigns.rs
// OTA governor-artifact campaigns (WS-4 · Track 3 · Fleet Plane) — persistence for
// the `crate::ota_campaign` control-plane state machine.
//
// The engine (`crate::ota_campaign::Campaign`) owns all transition logic and the
// fail-closed halt-on-regression rule; this module only persists a campaign and,
// on every lifecycle mutation, appends an R156-shaped audit-chain entry IN THE
// SAME atomic tx (the `record_grant_outcome` template — a forked chain is
// impossible because the mutation and the audit append share one `Immediate`
// transaction). Campaign state is durable so a `Halted` verdict survives a
// restart (a halted rollout must never silently resume).

use super::*;
use crate::ota_campaign::{Campaign, CampaignState, HaltReason, NodeArtifactStatus};

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
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "OtaCampaignCreated",
            &payload,
            campaign.updated_at_ms as i64,
            self.signing_key.as_ref(),
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
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            event_type,
            &payload,
            campaign.updated_at_ms as i64,
            self.signing_key.as_ref(),
        )?;
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

#[cfg(test)]
mod tests {
    use crate::ota_campaign::{AdvanceOutcome, Campaign, CampaignState, HaltReason};
    use crate::verifier::FleetPosture;
    use crate::verifier_store::VerifierStore;

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
        use crate::ota_campaign::NodeArtifactStatus;
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
        use crate::ota_campaign::NodeArtifactStatus;
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
