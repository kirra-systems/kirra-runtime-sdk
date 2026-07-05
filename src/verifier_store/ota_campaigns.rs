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
use crate::ota_campaign::{Campaign, CampaignState, HaltReason};

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
                  created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                        created_at_ms, updated_at_ms
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
                    created_at_ms, updated_at_ms
             FROM ota_campaigns ORDER BY created_at_ms DESC, campaign_id ASC",
        )?;
        let rows = stmt.query_map([], Self::map_campaign_row)?;
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
}
