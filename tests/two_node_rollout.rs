//! Gate C criterion #1 — "two-site fleet with OTA campaign, staged rollout +
//! rollback demonstrated end-to-end."
//!
//! This harness drives BOTH sides of the fleet plane against each other, using the
//! REAL modules — no mocks of the logic under test:
//!   - the verifier: `Campaign` state machine + `resolve_node_assignment` (staged-%
//!     + cohort membership) + `VerifierStore` persistence + `summarize_campaigns`;
//!   - the node: `decide_pull` + the `Installer` slot state machine (stage → trial →
//!     health-gated commit/rollback) over an `InMemoryBootController`.
//!
//! Two distinct node identities go through the whole loop. It proves: (1) a staged
//! rollout admits nodes gradually as the percentage advances; (2) an out-of-cohort
//! node is never assigned; (3) a HEALTHY node installs + commits + is counted as
//! adopted; (4) an UNHEALTHY node rolls back, stays on its baseline artifact, and is
//! NOT counted as adopted. A true two-SITE claim additionally wants two physical
//! boxes; this is the two-NODE logical end-to-end that the hardware drill instantiates.

use std::path::Path;

use kirra_ota_installer::{
    artifact_sha256_hex, decide_pull, AssignmentView, HealthOutcome, InMemoryBootController,
    Installer, PullAction, Slot,
};
use kirra_verifier::ota_campaign::{
    is_node_rolled, resolve_node_assignment, summarize_campaigns, Campaign, NodeArtifactStatus,
    NodeAssignment,
};
use kirra_verifier::verifier::FleetPosture;
use kirra_verifier::verifier_store::VerifierStore;

/// The artifact digest every node is running BEFORE the campaign (its baseline).
const OLD_DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// One node executes the OTA cycle for its `assignment`, driving the REAL installer
/// state machine, and returns the digest it ends up running. Not rolled / already
/// current → stays put; rolled → pull, stage, trial, then health-gated commit or
/// rollback (single trial attempt, so an unhealthy report rolls back immediately).
fn node_runs_cycle(
    assignment: &NodeAssignment,
    current_digest: &str,
    artifact: &Path,
    healthy: bool,
) -> String {
    let view = AssignmentView {
        rolled: assignment.rolled,
        artifact_digest: assignment.artifact_digest.clone(),
        artifact_version: assignment.artifact_version.clone(),
        campaign_id: assignment.campaign_id.clone(),
    };
    match decide_pull(&view, Some(current_digest), false) {
        PullAction::UpToDate => current_digest.to_string(),
        PullAction::Stage { digest, .. } => {
            let mut inst = Installer::new(InMemoryBootController::new(Slot::A), 1).expect("installer");
            inst.stage(artifact, &digest).expect("stage verified artifact");
            inst.begin_trial().expect("arm trial");
            match inst.report_health(healthy).expect("health report") {
                HealthOutcome::Committed { .. } => digest, // now running the new artifact
                HealthOutcome::RolledBack { .. } => current_digest.to_string(), // stayed on baseline
                HealthOutcome::Retrying { attempts } => {
                    panic!("single-attempt installer never retries (got attempts={attempts})")
                }
            }
        }
    }
}

fn adoption(node: &str, digest: &str, campaign: &str, at: u64) -> NodeArtifactStatus {
    NodeArtifactStatus {
        node_id: node.to_string(),
        applied_digest: digest.to_string(),
        campaign_id: Some(campaign.to_string()),
        artifact_version: Some("v2".to_string()),
        reported_at_ms: at,
        attested: false,
    }
}

#[test]
fn two_node_staged_rollout_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    let artifact = dir.path().join("kirra-governor");
    std::fs::write(&artifact, b"governor v2 artifact bytes").expect("write artifact");
    let digest = artifact_sha256_hex(&artifact).expect("hash artifact");

    let db = dir.path().join("verifier.sqlite");
    let mut store = VerifierStore::new(db.to_str().unwrap()).expect("open store");

    // Author a campaign to cohort "fleet", staged 50% → 90% → 100%.
    const CID: &str = "gov-2025.11";
    let mut campaign =
        Campaign::new(CID, &digest, "v2", vec!["fleet".into()], vec![50, 90, 100], 1).unwrap();
    campaign.arm(2).unwrap();
    campaign.advance(FleetPosture::Nominal, 3).unwrap(); // Staged → Rolling @ 50%
    store.insert_campaign(&campaign).expect("persist campaign");

    // Two cohort nodes that STRADDLE the 50% bucket (deterministic SHA-256 bucketing):
    // `early` rolls at 50%, `mid` only rolls once we reach 90%.
    let early = (0..4000)
        .map(|i| format!("node-{i}"))
        .find(|n| is_node_rolled(CID, n, 50))
        .expect("a node in the 50% bucket");
    let mid = (0..4000)
        .map(|i| format!("node-{i}"))
        .find(|n| !is_node_rolled(CID, n, 50) && is_node_rolled(CID, n, 90))
        .expect("a node in the (50,90] band");
    assert_ne!(early, mid);

    let cohort = vec!["fleet".to_string()];

    // --- Stage 1: Rolling @ 50% ---
    let active = store.load_active_campaigns().unwrap();
    let a_early = resolve_node_assignment(&early, &cohort, &active);
    let a_mid = resolve_node_assignment(&mid, &cohort, &active);
    assert!(a_early.rolled, "early node is inside the 50% rollout");
    assert!(!a_mid.rolled, "mid node is not yet rolled at 50%");
    // An out-of-cohort node is never assigned, even mid-rollout.
    let a_out = resolve_node_assignment("robot-x", &["other".into()], &active);
    assert!(!a_out.rolled, "out-of-cohort node is never assigned");

    // early installs (healthy) → new digest; mid isn't rolled → stays on baseline.
    assert_eq!(node_runs_cycle(&a_early, OLD_DIGEST, &artifact, true), digest);
    assert_eq!(node_runs_cycle(&a_mid, OLD_DIGEST, &artifact, true), OLD_DIGEST);

    // --- Stage 2: advance to Rolling @ 90% — mid now rolls ---
    campaign.advance(FleetPosture::Nominal, 4).unwrap();
    store.update_campaign(&campaign, "OtaCampaignAdvanced").unwrap();
    let active = store.load_active_campaigns().unwrap();
    let a_mid2 = resolve_node_assignment(&mid, &cohort, &active);
    assert!(a_mid2.rolled, "mid node rolls once the campaign reaches 90%");
    assert_eq!(node_runs_cycle(&a_mid2, OLD_DIGEST, &artifact, true), digest);

    // Both installed the new digest → they report adoption; the fleet summary counts 2.
    store.upsert_node_artifact_status(&adoption(&early, &digest, CID, 10)).unwrap();
    store.upsert_node_artifact_status(&adoption(&mid, &digest, CID, 10)).unwrap();
    let summary = summarize_campaigns(
        &store.load_campaigns().unwrap(),
        &store.load_node_artifact_statuses().unwrap(),
    );
    let prog = summary.active.iter().find(|c| c.campaign_id == CID).expect("campaign in summary");
    assert_eq!(prog.rollout_percent, 90);
    assert_eq!(prog.applied_nodes, 2, "both rolled nodes adopted the new digest");

    // --- Stage 3: final advance completes the rollout (terminal, no new assignments) ---
    campaign.advance(FleetPosture::Nominal, 5).unwrap(); // → Completed @ 100%
    store.update_campaign(&campaign, "OtaCampaignCompleted").unwrap();
    assert!(store.load_active_campaigns().unwrap().is_empty(), "completed campaign is no longer active");
    // A brand-new node querying now gets nothing — the rollout is done.
    let a_fresh = resolve_node_assignment("node-newcomer", &cohort, &store.load_active_campaigns().unwrap());
    assert!(!a_fresh.rolled, "a completed campaign assigns nothing further");
}

#[test]
fn unhealthy_node_rolls_back_and_is_not_counted_adopted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let artifact = dir.path().join("kirra-governor");
    std::fs::write(&artifact, b"governor v2 artifact bytes").expect("write artifact");
    let digest = artifact_sha256_hex(&artifact).expect("hash artifact");

    let db = dir.path().join("verifier.sqlite");
    let mut store = VerifierStore::new(db.to_str().unwrap()).expect("open store");

    const CID: &str = "gov-hotfix";
    let mut campaign =
        Campaign::new(CID, &digest, "v2", vec!["fleet".into()], vec![50, 100], 1).unwrap();
    campaign.arm(2).unwrap();
    campaign.advance(FleetPosture::Nominal, 3).unwrap(); // Rolling @ 50%
    store.insert_campaign(&campaign).expect("persist");

    // Two cohort nodes BOTH rolled at 50%: one trials healthy, one trials unhealthy.
    let good = (0..4000)
        .map(|i| format!("node-{i}"))
        .find(|n| is_node_rolled(CID, n, 50))
        .expect("first rolled node");
    let bad = (0..4000)
        .map(|i| format!("node-{i}"))
        .find(|n| is_node_rolled(CID, n, 50) && *n != good)
        .expect("second rolled node");

    let cohort = vec!["fleet".to_string()];
    let active = store.load_active_campaigns().unwrap();
    let a_good = resolve_node_assignment(&good, &cohort, &active);
    let a_bad = resolve_node_assignment(&bad, &cohort, &active);
    assert!(a_good.rolled && a_bad.rolled, "both nodes are in the 50% rollout");

    // good commits the new digest; bad's trial is UNHEALTHY → rolls back to baseline.
    assert_eq!(node_runs_cycle(&a_good, OLD_DIGEST, &artifact, true), digest);
    assert_eq!(
        node_runs_cycle(&a_bad, OLD_DIGEST, &artifact, false),
        OLD_DIGEST,
        "an unhealthy trial rolls back — the node keeps its baseline artifact"
    );

    // Each reports what it is ACTUALLY running: good on the new digest, bad on baseline.
    store.upsert_node_artifact_status(&adoption(&good, &digest, CID, 10)).unwrap();
    store.upsert_node_artifact_status(&adoption(&bad, OLD_DIGEST, CID, 10)).unwrap();

    let summary = summarize_campaigns(
        &store.load_campaigns().unwrap(),
        &store.load_node_artifact_statuses().unwrap(),
    );
    let prog = summary.active.iter().find(|c| c.campaign_id == CID).expect("campaign in summary");
    assert_eq!(
        prog.applied_nodes, 1,
        "only the healthy node adopted; the rolled-back node is not counted"
    );
}
