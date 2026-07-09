//! WP-17 (MGA G-17) — the effective boot-config digest is committed to the
//! tamper-evident audit chain, and the chain still verifies with it in place.
//!
//! The startup path (in the verifier binary) computes `EffectiveConfig::from_env()`
//! and appends an `EffectiveConfigDigest` event via the SAME store append the rest
//! of the console audit trail uses. This exercises that composition at the library
//! layer (no env / no process — INVARIANT #13), proving the digest reaches the
//! chain, that the chain integrity holds with the config event in it, and that a
//! config change yields a different committed digest.

use kirra_verifier::env_config::{EffectiveConfig, RawConfig};
use kirra_verifier::verifier_store::VerifierStore;

fn config_event_payload(cfg: &EffectiveConfig) -> String {
    // Plain string (no serde_json dep in the integration-test crate); shape mirrors
    // the startup handler's payload closely enough to prove the append.
    format!(
        "{{\"config_version\":{},\"digest\":\"{}\",\"mode\":\"{}\"}}",
        cfg.config_version,
        cfg.effective_digest(),
        cfg.mode
    )
}

#[test]
fn effective_config_digest_commits_to_the_audit_chain_and_verifies() {
    let mut store = VerifierStore::new(":memory:").expect("in-memory store");

    let cfg = EffectiveConfig::from_values(RawConfig {
        verifier_addr: Some("0.0.0.0:8090"),
        db_path: Some("/data/kirra.sqlite"),
        mode: Some("active"),
        vehicle_class: Some("robotaxi"),
        ..RawConfig::default()
    })
    .expect("valid config");
    let len_before = store.audit_chain_len().expect("len");
    store
        .append_clearance_audit_event("EffectiveConfigDigest", &config_event_payload(&cfg), 1_000)
        .expect("append config digest");

    assert_eq!(
        store.audit_chain_len().expect("len"),
        len_before + 1,
        "the boot-config digest event was appended to the audit chain"
    );
    // The hash-chained ledger still verifies intact with the config event committed.
    assert!(
        store.verify_audit_chain_integrity().expect("verify"),
        "the audit chain remains intact with the EffectiveConfigDigest event"
    );

    // A DIFFERENT effective config commits a DIFFERENT digest — drift is detectable
    // in the durable trail (here: standby vs active mode).
    let standby = EffectiveConfig::from_values(RawConfig {
        verifier_addr: Some("0.0.0.0:8090"),
        db_path: Some("/data/kirra.sqlite"),
        mode: Some("passive_standby"),
        vehicle_class: Some("robotaxi"),
        ..RawConfig::default()
    })
    .expect("valid standby config");
    assert_ne!(cfg.effective_digest(), standby.effective_digest());
    store
        .append_clearance_audit_event(
            "EffectiveConfigDigest",
            &config_event_payload(&standby),
            2_000,
        )
        .expect("append second digest");
    assert_eq!(store.audit_chain_len().expect("len"), len_before + 2);
    assert!(store.verify_audit_chain_integrity().expect("verify again"));
}
