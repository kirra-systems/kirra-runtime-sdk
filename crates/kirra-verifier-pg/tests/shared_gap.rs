// crates/kirra-verifier-pg/tests/shared_gap.rs
//
// #1030 stage 2 (ADR-0038) — live-Postgres tests for the shared-tier
// inherent-method gap-fill (`src/shared_ext.rs`): the dependency graph, the
// attestation policy, the epoch-fenced node upserts, the WP-19 HA lease, the
// OTA campaign row UPDATE, the clearance-grant state machine, and the WP-15
// cert-expiry census. Same skip-loudly discipline as `live_pg.rs`: without
// `KIRRA_PG_URL` every test passes vacuously (loudly on stderr); the
// `postgres-conformance` CI lane provides the server.

use std::sync::atomic::{AtomicU32, Ordering};

use kirra_core::{NodeTrustState, RegisteredNode};
use kirra_ota_campaign::{Campaign, CampaignState};
use kirra_persistence::{CertPrincipalStore, EpochFence, FenceError, NodeStore};
use kirra_verifier_pg::{PgDurableWriteError, PgVerifierStore};

static SCHEMA_SEQ: AtomicU32 = AtomicU32::new(0);

fn pg_url() -> Option<String> {
    match std::env::var("KIRRA_PG_URL") {
        Ok(u) if !u.trim().is_empty() => Some(u),
        _ => {
            eprintln!("SKIPPED: KIRRA_PG_URL unset — live-Postgres gap-fill tests need a server");
            None
        }
    }
}

fn raw_client_in_schema(url: &str, schema: &str) -> postgres::Client {
    let mut c = postgres::Client::connect(url, postgres::NoTls).expect("connect to KIRRA_PG_URL");
    c.batch_execute(&format!(
        "CREATE SCHEMA IF NOT EXISTS {schema}; SET search_path TO {schema}"
    ))
    .expect("create + pin test schema");
    c
}

fn isolated_store(test: &str) -> Option<PgVerifierStore> {
    let url = pg_url()?;
    let schema = format!(
        "kirra_gap_{}_{}_{}",
        std::process::id(),
        SCHEMA_SEQ.fetch_add(1, Ordering::Relaxed),
        test
    );
    let mut c = postgres::Client::connect(&url, postgres::NoTls).expect("connect");
    c.batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .expect("drop stale test schema");
    drop(c);
    let client = raw_client_in_schema(&url, &schema);
    Some(PgVerifierStore::from_client(client).expect("initialize schema on live PG"))
}

fn node(id: &str) -> RegisteredNode {
    RegisteredNode {
        node_id: id.to_string(),
        status: NodeTrustState::Trusted,
        registered_at_ms: 10,
        last_trust_update_ms: 20,
        ak_public_pem: None,
        expected_pcr16_digest_hex: None,
        site: None,
        firmware_version: None,
    }
}

// -- Dependency graph ---------------------------------------------------------

#[test]
fn dependencies_replace_atomically_and_load_as_a_map() {
    let Some(store) = isolated_store("deps") else {
        return;
    };
    store
        .save_dependencies("app", &["db".to_string(), "cache".to_string()])
        .unwrap();
    store
        .save_dependencies("db", &["disk".to_string()])
        .unwrap();

    let map = store.load_dependencies().unwrap();
    assert_eq!(map["app"], vec!["cache".to_string(), "db".to_string()]);
    assert_eq!(map["db"], vec!["disk".to_string()]);

    // Replace semantics: a re-save REPLACES the edge set (no stale edges).
    store
        .save_dependencies("app", &["queue".to_string()])
        .unwrap();
    let map = store.load_dependencies().unwrap();
    assert_eq!(map["app"], vec!["queue".to_string()]);

    // Emptying removes the node's edges entirely.
    store.save_dependencies("app", &[]).unwrap();
    assert!(!store.load_dependencies().unwrap().contains_key("app"));
}

// -- Attestation policy ---------------------------------------------------------

#[test]
fn attestation_policy_upserts_flips_and_defaults_false() {
    let Some(store) = isolated_store("policy") else {
        return;
    };
    // Absent row → false (never opted in).
    assert!(!store.node_requires_tpm_quote("n1").unwrap());
    store.set_node_attestation_policy("n1", true).unwrap();
    assert!(store.node_requires_tpm_quote("n1").unwrap());
    // Operator intent can flip it back OFF (the INSERT-OR-REPLACE parity).
    store.set_node_attestation_policy("n1", false).unwrap();
    assert!(!store.node_requires_tpm_quote("n1").unwrap());
}

// -- Epoch-fenced node upserts ----------------------------------------------------

#[test]
fn fenced_node_save_commits_with_the_right_epoch_and_refuses_otherwise() {
    let Some(mut store) = isolated_store("fenced_save") else {
        return;
    };
    let epoch = store.try_claim_epoch(0, "inst-A", 1).unwrap().unwrap();

    // Right epoch → the write lands.
    store.save_node_epoch_fenced(&node("n1"), epoch).unwrap();
    assert!(store.load_node("n1").unwrap().is_some());

    // held == 0 is never authorized (fail-closed, C5 parity).
    match store.save_node_epoch_fenced(&node("n2"), 0) {
        Err(PgDurableWriteError::Fenced(FenceError::EpochSuperseded { .. })) => {}
        other => panic!("held=0 must be EpochSuperseded, got {other:?}"),
    }
    assert!(store.load_node("n2").unwrap().is_none(), "nothing written");

    // A superseded epoch (another instance claimed) → refused, nothing written.
    let newer = store.try_claim_epoch(epoch, "inst-B", 2).unwrap().unwrap();
    match store.save_node_epoch_fenced(&node("n3"), epoch) {
        Err(PgDurableWriteError::Fenced(FenceError::EpochSuperseded { held, durable })) => {
            assert_eq!(held, epoch);
            assert_eq!(durable, newer);
        }
        other => panic!("stale epoch must be EpochSuperseded, got {other:?}"),
    }
    assert!(store.load_node("n3").unwrap().is_none(), "nothing written");
}

#[test]
fn fenced_node_save_with_policy_is_atomic() {
    let Some(mut store) = isolated_store("fenced_policy") else {
        return;
    };
    let epoch = store.try_claim_epoch(0, "inst-A", 1).unwrap().unwrap();
    store
        .save_node_with_policy_epoch_fenced(&node("q1"), true, epoch)
        .unwrap();
    assert!(store.load_node("q1").unwrap().is_some());
    assert!(store.node_requires_tpm_quote("q1").unwrap());

    // A fenced refusal writes NEITHER half.
    let _ = store.try_claim_epoch(epoch, "inst-B", 2).unwrap().unwrap();
    assert!(store
        .save_node_with_policy_epoch_fenced(&node("q2"), true, epoch)
        .is_err());
    assert!(store.load_node("q2").unwrap().is_none());
    assert!(!store.node_requires_tpm_quote("q2").unwrap());
}

// -- WP-19 HA lease ----------------------------------------------------------------

#[test]
fn lease_renews_for_the_holder_and_fails_for_a_superseded_instance() {
    let Some(mut store) = isolated_store("lease") else {
        return;
    };
    let epoch = store.try_claim_epoch(0, "inst-A", 100).unwrap().unwrap();

    assert!(store.renew_lease("inst-A", epoch, 250).unwrap());
    let lease = store.read_ha_lease().unwrap();
    assert_eq!(lease.epoch, epoch);
    assert_eq!(lease.holder.as_deref(), Some("inst-A"));
    assert_eq!(lease.last_renew_ms, 250);

    // A challenger claims → the old holder's renewal FAILS (its self-demote
    // signal), and the lease reflects the new holder.
    let newer = store
        .try_claim_epoch(epoch, "inst-B", 300)
        .unwrap()
        .unwrap();
    assert!(!store.renew_lease("inst-A", epoch, 400).unwrap());
    let lease = store.read_ha_lease().unwrap();
    assert_eq!(lease.epoch, newer);
    assert_eq!(lease.holder.as_deref(), Some("inst-B"));
}

// -- OTA campaign row UPDATE ---------------------------------------------------------

fn draft_campaign(id: &str) -> Campaign {
    Campaign {
        campaign_id: id.to_string(),
        artifact_digest: "d".repeat(64),
        artifact_version: "1.0.0".to_string(),
        artifact_signature_b64: None,
        uptane_metadata_json: None,
        cohorts: vec!["all".to_string()],
        stages: vec![10, 50, 100],
        stage_index: 0,
        rollout_percent: 0,
        state: CampaignState::Draft,
        halt_reason: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

#[test]
fn campaign_row_update_applies_and_reports_phantoms() {
    let Some(mut store) = isolated_store("campaign") else {
        return;
    };
    use kirra_persistence::OtaCampaignStore;
    let mut c = draft_campaign("c-1");
    store.insert_campaign(&c).unwrap();

    c.state = CampaignState::Staged;
    c.updated_at_ms = 2;
    assert!(store.update_campaign_row(&c).unwrap(), "row updated");
    let got = store.load_campaign("c-1").unwrap().unwrap();
    assert_eq!(got.state, CampaignState::Staged);
    assert_eq!(got.updated_at_ms, 2);

    // Phantom: no such campaign → Ok(false), the caller must not ledger it.
    let ghost = draft_campaign("c-none");
    assert!(!store.update_campaign_row(&ghost).unwrap());

    // Fenced variant: right epoch applies; a stale epoch refuses.
    let epoch = store.try_claim_epoch(0, "inst-A", 3).unwrap().unwrap();
    c.stage_index = 1;
    c.rollout_percent = 10;
    c.state = CampaignState::Rolling;
    c.updated_at_ms = 4;
    assert!(store.update_campaign_row_epoch_fenced(&c, epoch).unwrap());
    let _ = store.try_claim_epoch(epoch, "inst-B", 5).unwrap().unwrap();
    c.updated_at_ms = 6;
    assert!(matches!(
        store.update_campaign_row_epoch_fenced(&c, epoch),
        Err(PgDurableWriteError::Fenced(_))
    ));
    // held == 0 takes the plain (unfenced) path — SQLite parity for
    // never-claimed stores.
    assert!(store.update_campaign_row_epoch_fenced(&c, 0).unwrap());
}

// -- Clearance-grant state machine -----------------------------------------------------

#[test]
fn clearance_grant_lifecycle_is_exactly_once() {
    let Some(store) = isolated_store("grants") else {
        return;
    };
    // No grants yet.
    assert!(store.latest_clearance_grant("nodeX").unwrap().is_none());
    assert!(store
        .take_pending_clearance_grant("nodeX", 10)
        .unwrap()
        .is_none());

    let id1 = store
        .insert_clearance_grant_row("nodeX", "op-1", 100, 100)
        .unwrap();
    let id2 = store
        .insert_clearance_grant_row("nodeX", "op-2", 200, 200)
        .unwrap();
    assert!(id2 > id1);

    // Oldest-first, exactly-once consume.
    let g1 = store
        .take_pending_clearance_grant("nodeX", 300)
        .unwrap()
        .expect("first grant");
    assert_eq!(g1.rowid, id1);
    assert_eq!(g1.operator_id, "op-1");
    let g2 = store
        .take_pending_clearance_grant("nodeX", 301)
        .unwrap()
        .expect("second grant");
    assert_eq!(g2.rowid, id2);
    assert!(store
        .take_pending_clearance_grant("nodeX", 302)
        .unwrap()
        .is_none());

    // Outcome row update + console read surface.
    assert!(store
        .record_grant_outcome_row(id2, "Cleared", None)
        .unwrap());
    assert!(!store
        .record_grant_outcome_row(999_999, "Cleared", None)
        .unwrap());
    let latest = store
        .latest_clearance_grant("nodeX")
        .unwrap()
        .expect("latest grant");
    assert_eq!(latest.granted_at_ms, 200);
    assert_eq!(latest.consumed_at_ms, Some(301));
    assert_eq!(latest.outcome.as_deref(), Some("Cleared"));
}

// -- WP-15 cert-expiry census ------------------------------------------------------------

#[test]
fn cert_expiry_summary_classifies_like_sqlite() {
    let Some(mut store) = isolated_store("certsum") else {
        return;
    };
    let now = 1_000_000u64;
    let warn = 10_000u64;
    // active, no expiry
    store
        .register_cert_principal("p-noexp", "aa".repeat(32).as_str(), "auditor", None, 1)
        .unwrap();
    // active, expiring soon (inside the warn window)
    store
        .register_cert_principal(
            "p-soon",
            "bb".repeat(32).as_str(),
            "auditor",
            Some(now + warn / 2),
            1,
        )
        .unwrap();
    // active, comfortably far expiry
    store
        .register_cert_principal(
            "p-far",
            "cc".repeat(32).as_str(),
            "auditor",
            Some(now + warn * 100),
            1,
        )
        .unwrap();
    // expired
    store
        .register_cert_principal(
            "p-old",
            "dd".repeat(32).as_str(),
            "auditor",
            Some(now - 1),
            1,
        )
        .unwrap();
    // revoked (revocation classifies FIRST, even with a live expiry)
    store
        .register_cert_principal(
            "p-rev",
            "ee".repeat(32).as_str(),
            "auditor",
            Some(now + warn * 100),
            1,
        )
        .unwrap();
    store.revoke_cert_principal("p-rev", 2).unwrap();

    let s = store.cert_expiry_summary(now, warn).unwrap();
    assert_eq!(s.total, 5);
    assert_eq!(s.revoked, 1);
    assert_eq!(s.expired, 1);
    assert_eq!(s.active, 3);
    assert_eq!(s.no_expiry, 1);
    assert_eq!(s.expiring_soon, 1);
}
