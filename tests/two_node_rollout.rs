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
        artifact_signature_b64: assignment.artifact_signature_b64.clone(),
        uptane_metadata: None,
        artifact_version: assignment.artifact_version.clone(),
        campaign_id: assignment.campaign_id.clone(),
    };
    match decide_pull(&view, Some(current_digest), false) {
        PullAction::UpToDate => current_digest.to_string(),
        PullAction::Stage { digest, .. } => {
            let mut inst =
                Installer::new(InMemoryBootController::new(Slot::A), 1).expect("installer");
            inst.stage(artifact, &digest)
                .expect("stage verified artifact");
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
    let mut campaign = Campaign::new(
        CID,
        &digest,
        "v2",
        vec!["fleet".into()],
        vec![50, 90, 100],
        1,
    )
    .unwrap();
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
    assert_eq!(
        node_runs_cycle(&a_early, OLD_DIGEST, &artifact, true),
        digest
    );
    assert_eq!(
        node_runs_cycle(&a_mid, OLD_DIGEST, &artifact, true),
        OLD_DIGEST
    );

    // --- Stage 2: advance to Rolling @ 90% — mid now rolls ---
    campaign.advance(FleetPosture::Nominal, 4).unwrap();
    store
        .update_campaign(&campaign, "OtaCampaignAdvanced")
        .unwrap();
    let active = store.load_active_campaigns().unwrap();
    let a_mid2 = resolve_node_assignment(&mid, &cohort, &active);
    assert!(
        a_mid2.rolled,
        "mid node rolls once the campaign reaches 90%"
    );
    assert_eq!(
        node_runs_cycle(&a_mid2, OLD_DIGEST, &artifact, true),
        digest
    );

    // Both installed the new digest → they report adoption; the fleet summary counts 2.
    store
        .upsert_node_artifact_status(&adoption(&early, &digest, CID, 10))
        .unwrap();
    store
        .upsert_node_artifact_status(&adoption(&mid, &digest, CID, 10))
        .unwrap();
    let summary = summarize_campaigns(
        &store.load_campaigns().unwrap(),
        &store.load_node_artifact_statuses().unwrap(),
    );
    let prog = summary
        .active
        .iter()
        .find(|c| c.campaign_id == CID)
        .expect("campaign in summary");
    assert_eq!(prog.rollout_percent, 90);
    assert_eq!(
        prog.applied_nodes, 2,
        "both rolled nodes adopted the new digest"
    );

    // --- Stage 3: final advance completes the rollout (terminal, no new assignments) ---
    campaign.advance(FleetPosture::Nominal, 5).unwrap(); // → Completed @ 100%
    store
        .update_campaign(&campaign, "OtaCampaignCompleted")
        .unwrap();
    assert!(
        store.load_active_campaigns().unwrap().is_empty(),
        "completed campaign is no longer active"
    );
    // A brand-new node querying now gets nothing — the rollout is done.
    let a_fresh = resolve_node_assignment(
        "node-newcomer",
        &cohort,
        &store.load_active_campaigns().unwrap(),
    );
    assert!(
        !a_fresh.rolled,
        "a completed campaign assigns nothing further"
    );
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
    assert!(
        a_good.rolled && a_bad.rolled,
        "both nodes are in the 50% rollout"
    );

    // good commits the new digest; bad's trial is UNHEALTHY → rolls back to baseline.
    assert_eq!(
        node_runs_cycle(&a_good, OLD_DIGEST, &artifact, true),
        digest
    );
    assert_eq!(
        node_runs_cycle(&a_bad, OLD_DIGEST, &artifact, false),
        OLD_DIGEST,
        "an unhealthy trial rolls back — the node keeps its baseline artifact"
    );

    // Each reports what it is ACTUALLY running: good on the new digest, bad on baseline.
    store
        .upsert_node_artifact_status(&adoption(&good, &digest, CID, 10))
        .unwrap();
    store
        .upsert_node_artifact_status(&adoption(&bad, OLD_DIGEST, CID, 10))
        .unwrap();

    let summary = summarize_campaigns(
        &store.load_campaigns().unwrap(),
        &store.load_node_artifact_statuses().unwrap(),
    );
    let prog = summary
        .active
        .iter()
        .find(|c| c.campaign_id == CID)
        .expect("campaign in summary");
    assert_eq!(
        prog.applied_nodes, 1,
        "only the healthy node adopted; the rolled-back node is not counted"
    );
}

/// WP-12 — the release signature flows verifier → assignment → node, and the
/// node's provisioned installer ENFORCES it: a correctly-signed campaign
/// stages and commits; a FORGED signature and an UNSIGNED (legacy) campaign
/// both leave a key-provisioned node on its baseline with the slot never armed.
#[test]
fn signed_campaign_flows_the_release_signature_to_a_verifying_node() {
    use kirra_ota_installer::InstallError;
    use kirra_release_token::artifact_release::sign_artifact_release;

    let dir = tempfile::tempdir().expect("tempdir");
    let artifact = dir.path().join("governor-v2.bin");
    std::fs::write(&artifact, b"signed governor v2").expect("write artifact");
    let digest = artifact_sha256_hex(&artifact).expect("digest");

    let release_key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
    let sig = sign_artifact_release(&digest, &release_key).expect("sign");

    // Verifier side: a SIGNED campaign, armed and advanced to 100%.
    let mut store = VerifierStore::new(":memory:").expect("store");
    let mut c = Campaign::new(
        "camp-signed",
        &digest,
        "v2",
        vec!["fleet".into()],
        vec![50, 100],
        1,
    )
    .unwrap()
    .with_artifact_signature(&sig);
    store.insert_campaign(&c).expect("insert");
    c.arm(2).unwrap();
    store.update_campaign(&c, "OtaCampaignArmed").expect("arm");
    // One advance → Rolling@50% (a single [100] stage would jump to Completed,
    // which resolve_node_assignment excludes).
    c.advance(FleetPosture::Nominal, 3).unwrap();
    store
        .update_campaign(&c, "OtaCampaignAdvanced")
        .expect("advance");

    // A node deterministically inside the 50% bucket fetches the assignment,
    // which carries the signature.
    let node = (0..500)
        .map(|i| format!("node-{i}"))
        .find(|n| is_node_rolled("camp-signed", n, 50))
        .expect("a node in the 50% bucket");
    let active = store.load_active_campaigns().expect("active");
    let assignment = resolve_node_assignment(&node, &["fleet".into()], &active);
    assert!(assignment.rolled);
    assert_eq!(
        assignment.artifact_signature_b64.as_deref(),
        Some(sig.as_str())
    );

    // Node side: a key-provisioned installer accepts the signed stage...
    let mut inst = Installer::new(InMemoryBootController::new(Slot::A), 1)
        .expect("installer")
        .with_release_key(release_key.verifying_key());
    let sig_from_assignment = assignment.artifact_signature_b64.as_deref().unwrap();
    inst.stage_verified_signed(&artifact, &digest, sig_from_assignment)
        .expect("a correctly-signed artifact stages");
    inst.begin_trial().expect("trial");
    match inst.report_health(true).expect("health") {
        HealthOutcome::Committed { active } => assert_eq!(active, Slot::B),
        other => panic!("healthy signed rollout must commit, got {other:?}"),
    }

    // ...and refuses a FORGED signature (attacker key), slot never armed.
    let attacker = ed25519_dalek::SigningKey::from_bytes(&[13u8; 32]);
    let forged = sign_artifact_release(&digest, &attacker).expect("sign");
    let mut inst2 = Installer::new(InMemoryBootController::new(Slot::A), 1)
        .expect("installer")
        .with_release_key(release_key.verifying_key());
    assert!(matches!(
        inst2.stage_verified_signed(&artifact, &digest, &forged),
        Err(InstallError::ArtifactSignatureInvalid)
    ));

    // ...and refuses the UNSIGNED legacy path outright.
    assert!(matches!(
        inst2.stage(&artifact, &digest),
        Err(InstallError::SignatureRequired)
    ));
}

/// EP-13 (MGA G-7 remainder) — the FULL Uptane flow, end-to-end over the real
/// wire shapes: the repository authors a signed metadata set → the campaign
/// carries it → `resolve_node_assignment` relays it (untrusted carrier) → the
/// node's `AssignmentView` deserializes the assignment JSON → the anchored
/// node verifies the whole set against its DURABLE trust state, pulls only the
/// authorized digest, installs, and persists the advanced rollback floor.
/// Then the two attacks the DoD names:
///   1. a ROLLBACK ATTACK — the carrier re-serves an OLDER (still correctly
///      signed) metadata set; the node's persisted floor refuses it;
///   2. downgrade-by-omission — a campaign carrying NO metadata set is refused
///      outright by an anchored node (never a silent legacy fallback).
#[test]
fn uptane_anchored_rollout_verifies_and_rejects_a_rollback_attack() {
    use ed25519_dalek::SigningKey;
    use kirra_ota_installer::{uptane_pull_gate, UptaneGateError, UptaneTrustStore};
    use kirra_release_token::uptane::{
        author_initial_root, sign_snapshot, sign_targets, sign_timestamp, Role, RootMetadata,
        SnapshotMetadata, TargetEntry, TargetsMetadata, TimestampMetadata, UptaneError,
        UptaneMetadataSet,
    };

    const EXP: u64 = 100_000;
    const NOW: u64 = 1_000;

    let dir = tempfile::tempdir().expect("tempdir");
    let artifact = dir.path().join("governor-v3.bin");
    std::fs::write(&artifact, b"uptane governor v3").expect("write artifact");
    let digest = artifact_sha256_hex(&artifact).expect("digest");

    // --- The repository: role keys + a signed metadata set at version 3 ----
    let root_sk = SigningKey::from_bytes(&[1u8; 32]);
    let targets_sk = SigningKey::from_bytes(&[2u8; 32]);
    let snapshot_sk = SigningKey::from_bytes(&[3u8; 32]);
    let timestamp_sk = SigningKey::from_bytes(&[4u8; 32]);
    let root_meta = RootMetadata {
        version: 1,
        expires_at_ms: EXP,
        root_key: root_sk.verifying_key().to_bytes(),
        targets_key: targets_sk.verifying_key().to_bytes(),
        snapshot_key: snapshot_sk.verifying_key().to_bytes(),
        timestamp_key: timestamp_sk.verifying_key().to_bytes(),
    };
    let anchor = author_initial_root(root_meta, &root_sk);
    let set_at = |v: u64| -> UptaneMetadataSet {
        let targets = TargetsMetadata {
            version: v,
            expires_at_ms: EXP,
            targets: vec![TargetEntry {
                digest_hex: digest.clone(),
                length_bytes: 18,
                version: "v3".into(),
            }],
        };
        let snapshot = SnapshotMetadata {
            version: v,
            expires_at_ms: EXP,
            targets_version: v,
        };
        let timestamp = TimestampMetadata {
            version: v,
            expires_at_ms: EXP,
            snapshot_version: v,
        };
        UptaneMetadataSet {
            timestamp_sig_b64: sign_timestamp(&timestamp, &timestamp_sk),
            timestamp,
            snapshot_sig_b64: sign_snapshot(&snapshot, &snapshot_sk),
            snapshot,
            targets_sig_b64: sign_targets(&targets, &targets_sk),
            targets,
        }
    };
    let set_v3 = set_at(3);

    // --- Verifier side: the campaign CARRIES the set (untrusted relay) -----
    let mut store = VerifierStore::new(":memory:").expect("store");
    const CID: &str = "camp-uptane";
    let mut c = Campaign::new(CID, &digest, "v3", vec!["fleet".into()], vec![50, 100], 1)
        .unwrap()
        .with_uptane_metadata(&serde_json::to_string(&set_v3).unwrap())
        .expect("metadata authorizes the campaign digest");
    store.insert_campaign(&c).expect("insert");
    c.arm(2).unwrap();
    store.update_campaign(&c, "OtaCampaignArmed").expect("arm");
    c.advance(FleetPosture::Nominal, 3).unwrap(); // → Rolling @ 50%
    store
        .update_campaign(&c, "OtaCampaignAdvanced")
        .expect("advance");

    let node = (0..500)
        .map(|i| format!("node-{i}"))
        .find(|n| is_node_rolled(CID, n, 50))
        .expect("a node in the 50% bucket");
    let active = store.load_active_campaigns().expect("active");
    let assignment = resolve_node_assignment(&node, &["fleet".into()], &active);
    assert!(assignment.rolled);

    // --- The WIRE boundary: assignment JSON → the installer's view --------
    let wire = serde_json::to_string(&assignment).expect("assignment JSON");
    let view: AssignmentView = serde_json::from_str(&wire).expect("installer view");
    let carried = view
        .uptane_metadata
        .as_ref()
        .expect("assignment carries the metadata set");
    assert_eq!(carried, &set_v3, "the set survives the relay bit-for-bit");

    // --- Node side: anchored trust store, gate BEFORE the pull ------------
    let trust_path = dir.path().join("uptane-trust.json");
    let trust = UptaneTrustStore::new(&trust_path);
    trust.provision(&anchor).expect("anchor the node once");

    let state = trust.load().expect("load trust state");
    let floor = uptane_pull_gate(Some(&state), view.uptane_metadata.as_ref(), &digest, NOW)
        .expect("a correctly-signed, floor-advancing set verifies")
        .expect("anchored path yields a floor");

    // Install through the REAL slot state machine, then persist the floor
    // (floor AFTER a successful stage — a failed stage never burns it).
    let mut inst = Installer::new(InMemoryBootController::new(Slot::A), 1).expect("installer");
    inst.stage(&artifact, &digest)
        .expect("stage the authorized artifact");
    inst.begin_trial().expect("trial");
    match inst.report_health(true).expect("health") {
        HealthOutcome::Committed { active } => assert_eq!(active, Slot::B),
        other => panic!("healthy uptane rollout must commit, got {other:?}"),
    }
    trust
        .record_versions(floor)
        .expect("persist the advanced floor");

    // --- Attack 1: ROLLBACK — the carrier re-serves an OLDER signed set ---
    // (correctly signed by the real role keys, so only the persisted floor
    // stands between the node and the downgrade).
    let stale = set_at(2);
    let mut rollback_campaign = Campaign::new(
        "camp-rollback",
        &digest,
        "v3",
        vec!["fleet".into()],
        vec![100],
        4,
    )
    .unwrap()
    .with_uptane_metadata(&serde_json::to_string(&stale).unwrap())
    .expect("the verifier is a carrier — it cannot know the node's floor");
    rollback_campaign.arm(5).unwrap();
    store
        .insert_campaign(&rollback_campaign)
        .expect("insert rollback campaign");
    // (arm only — the node-side gate is what must refuse, not campaign state)
    let stale_view: AssignmentView = serde_json::from_str(
        &serde_json::to_string(&NodeAssignment {
            node_id: node.clone(),
            rolled: true,
            campaign_id: Some("camp-rollback".into()),
            artifact_digest: Some(digest.clone()),
            artifact_signature_b64: None,
            uptane_metadata: Some(serde_json::to_value(&stale).unwrap()),
            artifact_version: Some("v3".into()),
        })
        .unwrap(),
    )
    .expect("view");
    let reloaded = trust.load().expect("reload trust state (restart boundary)");
    assert!(
        matches!(
            uptane_pull_gate(
                Some(&reloaded),
                stale_view.uptane_metadata.as_ref(),
                &digest,
                NOW
            ),
            // Timestamp is the first role the version floor screens.
            Err(UptaneGateError::Verify(UptaneError::RollbackAttempt(
                Role::Timestamp
            )))
        ),
        "an older re-served metadata set must be refused by the persisted floor"
    );

    // --- Attack 2: downgrade-by-omission — no metadata set at all ---------
    assert_eq!(
        uptane_pull_gate(Some(&reloaded), None, &digest, NOW),
        Err(UptaneGateError::MetadataMissing),
        "an anchored node never falls back to an unattested pull"
    );
}
