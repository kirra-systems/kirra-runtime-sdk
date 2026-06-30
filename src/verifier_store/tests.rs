// src/verifier_store/tests.rs
// Test modules — moved verbatim from verifier_store.rs (super::* -> crate path).

#[cfg(test)]
mod attestation_registry_tests {
    use crate::verifier_store::*;

    fn in_memory() -> VerifierStore {
        VerifierStore::new(":memory:").unwrap()
    }

    #[test]
    fn test_load_av_subsystems_lists_registered_rows() {
        let store = in_memory();
        store.register_av_subsystem_meta("lidar-1", "Perception", "LIDAR-001", 0.65, 1_000).unwrap();
        store.register_av_subsystem_meta("radar-1", "Perception", "RADAR-002", 0.70, 2_000).unwrap();
        store.increment_recovery_streak("lidar-1", 1_500).unwrap();
        let rows = store.load_av_subsystems().unwrap();
        assert_eq!(rows.len(), 2);
        let lidar = rows.iter().find(|r| r.node_id == "lidar-1").unwrap();
        assert_eq!(lidar.subsystem_type, "Perception");
        assert_eq!(lidar.hardware_id, "LIDAR-001");
        assert!((lidar.confidence_floor - 0.65).abs() < 1e-9);
        assert_eq!(lidar.recovery_streak_count, 1);
    }

    #[test]
    fn test_load_operators_lists_registered() {
        let mut store = in_memory();
        store.register_operator("op-2", "pem-b", 2_000).unwrap();
        store.register_operator("op-1", "pem-a", 1_000).unwrap();
        let ops = store.load_operators().unwrap();
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|o| o.revoked_at_ms.is_none() && o.is_active()));
        assert_eq!(ops[0].operator_id, "op-1", "ordered by operator_id");
    }

    #[test]
    fn test_register_and_load_fingerprint() {
        let mut store = in_memory();
        let fp = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(store.register_attestation_identity("node-01", fp, "admin", 1_000).is_ok());
        assert_eq!(store.load_registered_fingerprint("node-01").unwrap(), Some(fp.to_string()));
    }

    #[test]
    fn test_load_fingerprint_missing_node_returns_none() {
        let store = in_memory();
        assert_eq!(store.load_registered_fingerprint("ghost-node").unwrap(), None);
    }

    #[test]
    fn test_identity_registration_chains_audit_entry() {
        let mut store = in_memory();
        let fp = "abc123def456";
        store.register_attestation_identity("node-02", fp, "admin", 2_000).unwrap();
        assert!(store.verify_audit_chain_integrity().unwrap());
    }

    #[test]
    fn test_identity_registration_is_idempotent_on_rotate() {
        let mut store = in_memory();
        let fp1 = "aaaa";
        let fp2 = "bbbb";
        store.register_attestation_identity("node-03", fp1, "admin", 1_000).unwrap();
        store.register_attestation_identity("node-03", fp2, "admin", 2_000).unwrap();
        assert_eq!(store.load_registered_fingerprint("node-03").unwrap(), Some(fp2.to_string()));
        assert!(store.verify_audit_chain_integrity().unwrap());
    }

    #[test]
    fn test_av_subsystem_meta_round_trip() {
        let store = in_memory();
        store.register_av_subsystem_meta("lidar_front", "Perception", "LIDAR-001", 0.70, 0).unwrap();
        let floor = store.load_av_confidence_floor("lidar_front").unwrap();
        assert_eq!(floor, Some(0.70));
    }

    #[test]
    fn test_recovery_streak_increments_and_resets() {
        let store = in_memory();
        store.register_av_subsystem_meta("cam", "Perception", "CAM-001", 0.70, 0).unwrap();
        let n1 = store.increment_recovery_streak("cam", 1000).unwrap();
        let n2 = store.increment_recovery_streak("cam", 1100).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 2);
        store.reset_recovery_streak("cam", 1200).unwrap();
        let (count, start) = store.load_recovery_streak("cam").unwrap();
        assert_eq!(count, 0);
        assert_eq!(start, 0);
    }

    // Q4: the watchdog timeout path resets the streak WITHOUT stamping a fresh
    // last_telemetry_ms (no report arrived). Streak clears; telemetry timestamp
    // is preserved (unlike `reset_recovery_streak`, which sets it to `now`).
    #[test]
    fn test_reset_recovery_streak_preserving_telemetry() {
        let store = in_memory();
        store.register_av_subsystem_meta("lidar", "Perception", "LDR-1", 0.70, 5_000).unwrap();
        store.increment_recovery_streak("lidar", 5_000).unwrap();
        store.increment_recovery_streak("lidar", 5_000).unwrap();
        assert_eq!(store.load_recovery_streak("lidar").unwrap().0, 2);
        assert_eq!(store.get_last_telemetry_timestamp("lidar").unwrap(), 5_000);

        store.reset_recovery_streak_preserving_telemetry("lidar").unwrap();

        let (count, start) = store.load_recovery_streak("lidar").unwrap();
        assert_eq!(count, 0, "streak must be cleared");
        assert_eq!(start, 0, "streak start must be cleared");
        assert_eq!(
            store.get_last_telemetry_timestamp("lidar").unwrap(),
            5_000,
            "last_telemetry_ms must be PRESERVED — the timeout must not fabricate a fresh last-seen"
        );
    }

    #[test]
    fn test_generation_persistence() {
        let store = in_memory();
        assert_eq!(store.load_last_generation().unwrap(), 0);
        store.save_last_generation(42).unwrap();
        assert_eq!(store.load_last_generation().unwrap(), 42);
    }

    /// #695: save_last_generation reports whether the write was ACCEPTED, so a
    /// caller can distinguish a persisted generation from one rejected as stale.
    #[test]
    fn test_save_last_generation_reports_acceptance() {
        let store = in_memory();
        // First write creates the row → accepted.
        assert!(store.save_last_generation(10).unwrap(), "first write must be accepted");
        // Strictly greater → accepted.
        assert!(store.save_last_generation(11).unwrap(), "a higher generation is accepted");
        assert_eq!(store.load_last_generation().unwrap(), 11);
        // Lower → REJECTED (returns false), high-water unchanged.
        assert!(!store.save_last_generation(5).unwrap(), "a lower generation is rejected");
        assert_eq!(store.load_last_generation().unwrap(), 11, "stale write must not regress the high-water");
        // Equal → REJECTED (strict > required).
        assert!(!store.save_last_generation(11).unwrap(), "an equal generation is rejected (strict >)");
        assert_eq!(store.load_last_generation().unwrap(), 11);
    }
}

#[cfg(test)]
mod standby_store_tests {
    use crate::verifier_store::*;

    fn in_memory() -> VerifierStore {
        VerifierStore::new(":memory:").unwrap()
    }

    #[test]
    fn test_load_engine_state_absent_key_returns_none() {
        let store = in_memory();
        assert_eq!(store.load_engine_state("nonexistent_key").unwrap(), None);
    }

    #[test]
    fn test_save_and_load_engine_state_round_trip() {
        let store = in_memory();
        store.save_engine_state("primary_heartbeat_ms", "12345").unwrap();
        let val = store.load_engine_state("primary_heartbeat_ms").unwrap();
        assert_eq!(val, Some("12345".to_string()));
    }

    #[test]
    fn test_save_engine_state_is_idempotent_upsert() {
        let store = in_memory();
        store.save_engine_state("key", "first").unwrap();
        store.save_engine_state("key", "second").unwrap();
        assert_eq!(store.load_engine_state("key").unwrap(), Some("second".to_string()));
    }

    #[test]
    fn node_attestation_policy_defaults_absent_to_false_and_round_trips() {
        // TPM-quote follow-up: an unknown node requires no quote (fail-closed
        // opt-in default); set persists; re-set can flip it back off.
        let store = in_memory();
        assert!(!store.node_requires_tpm_quote("unknown").unwrap(), "absent → false");
        store.set_node_attestation_policy("n1", true).unwrap();
        assert!(store.node_requires_tpm_quote("n1").unwrap(), "set true persists");
        store.set_node_attestation_policy("n1", false).unwrap();
        assert!(!store.node_requires_tpm_quote("n1").unwrap(), "re-set clears the requirement");
    }

    // --- #394 console rollups -----------------------------------------------

    #[test]
    fn test_audit_chain_len_empty_is_zero() {
        let store = in_memory();
        assert_eq!(store.audit_chain_len().unwrap(), 0);
    }

    #[test]
    fn test_node_site_and_firmware_round_trip() {
        // #397/#398: the additive nullable columns persist and reload.
        let store = in_memory();
        store
            .save_node(&RegisteredNode {
                node_id: "n1".into(),
                status: NodeTrustState::Trusted,
                registered_at_ms: 1,
                last_trust_update_ms: 1,
                ak_public_pem: None,
                expected_pcr16_digest_hex: None,
                site: Some("dock-7".into()),
                firmware_version: Some("v2.3".into()),
            })
            .unwrap();
        let loaded = store.load_node("n1").unwrap().expect("node present");
        assert_eq!(loaded.site.as_deref(), Some("dock-7"));
        assert_eq!(loaded.firmware_version.as_deref(), Some("v2.3"));
    }

    #[test]
    fn test_posture_event_analytics_queries() {
        // #396: window/group queries return the expected rows.
        let store = in_memory();
        let nominal = serde_json::to_string(&crate::verifier::FleetPosture::Nominal).unwrap();
        let degraded = serde_json::to_string(&crate::verifier::FleetPosture::Degraded).unwrap();
        store.save_posture_event("a", "E", &nominal, None, 1_000).unwrap();
        store.save_posture_event("a", "E", &degraded, None, 2_000).unwrap();
        store.save_posture_event("b", "E", &nominal, None, 3_000).unwrap();
        // An old event outside the window is excluded.
        store.save_posture_event("c", "E", &nominal, None, 10).unwrap();

        let since = 500;
        let events = store.load_posture_events_since(since).unwrap();
        assert_eq!(events.len(), 3, "the pre-window event is excluded");
        assert!(events.iter().all(|(ts, _)| *ts >= since));

        let by_node = store.count_posture_events_by_node_since(since).unwrap();
        let a = by_node.iter().find(|(n, _)| n == "a").unwrap();
        assert_eq!(a.1, 2, "node a has two in-window events");
        // DESC by count → node "a" leads.
        assert_eq!(by_node[0].0, "a");
    }

    #[test]
    fn test_multiple_keys_are_independent() {
        let store = in_memory();
        store.save_engine_state("key_a", "value_a").unwrap();
        store.save_engine_state("key_b", "value_b").unwrap();
        assert_eq!(store.load_engine_state("key_a").unwrap(), Some("value_a".to_string()));
        assert_eq!(store.load_engine_state("key_b").unwrap(), Some("value_b".to_string()));
    }

    #[test]
    fn test_heartbeat_age_parse_from_stored_string() {
        let store = in_memory();
        let ts: u64 = 1_700_000_000_000;
        store.save_engine_state("primary_heartbeat_ms", &ts.to_string()).unwrap();
        let loaded = store.load_engine_state("primary_heartbeat_ms").unwrap().unwrap();
        let parsed: u64 = loaded.parse().expect("must parse as u64");
        assert_eq!(parsed, ts);
    }
}


/// Regression suite for the audit-chain bypass fix.
///
/// Before this fix, `save_posture_event` (plain INSERT) was the writer at
/// six production call sites, so events like `ATTESTATION_TRUSTED` and
/// `MOTION_COMMAND_ADMITTED` were written to `posture_events` but NOT
/// appended to the SHA-256 hash chain — meaning `verify_audit_chain_*`
/// could not detect tampering of those events. This test proves the
/// chained writer covers a posture event and the chain remains verifiable.
#[cfg(test)]
mod audit_chain_bypass_tests {
    use crate::verifier_store::*;

    fn in_memory() -> VerifierStore {
        VerifierStore::new(":memory:").unwrap()
    }

    #[test]
    fn test_posture_event_is_covered_by_audit_chain() {
        let mut store = in_memory();

        // Write an ATTESTATION_TRUSTED event through the chained writer.
        store
            .save_posture_event_chained(
                "node-x",
                "ATTESTATION_TRUSTED",
                r#"{"trusted":true}"#,
                None,
                1_000,
            )
            .expect("chained write succeeds");

        // Chain verifies clean — the event landed as a chain link.
        assert!(
            store
                .verify_audit_chain_integrity()
                .expect("verify_audit_chain_integrity should succeed on a healthy chain"),
            "Chained posture write must produce a verifiable chain link"
        );

        // Stronger integration assertion: load_audit_chain_page reports
        // at least one entry, proving the event really IS in the chain
        // (not just that the chain happens to verify with zero entries).
        let page = store
            .load_audit_chain_page(10, 0, None)
            .expect("load_audit_chain_page should succeed");
        assert!(
            page.total >= 1,
            "Audit chain page must contain the just-written event; total={}",
            page.total
        );
        assert!(
            page.chain_intact,
            "Audit chain page must self-report intact after a chained write"
        );

        // Add a second event of a different type — chain must still verify
        // (covers the multi-link case, not just the first-write case).
        store
            .save_posture_event_chained(
                "node-x",
                "DEPENDENCY_UPDATED",
                r#"{"parent":"node-y"}"#,
                Some("test reason"),
                2_000,
            )
            .expect("second chained write succeeds");

        assert!(
            store
                .verify_audit_chain_integrity()
                .expect("verify_audit_chain_integrity should succeed"),
            "Multi-link chain must still verify after a second chained posture write"
        );

        // TODO: negative test — mutate the persisted posture_events row
        // directly and assert `verify_audit_chain_integrity` returns false.
        // Skipped here because VerifierStore does not expose raw
        // `Connection` access for tests (intentional encapsulation); the
        // chain-tamper-detection property is covered separately by the
        // SG-010 fault-injection-suite stub in
        // `tests/cert_003_rtm_gap_stubs.rs`.
    }
}

/// Tests for the v2 hash + sequence binding. The CORE WIN is that the
/// cheap hash-only `verify_audit_chain_integrity` now catches event_type
/// relabeling and v2 sequence reorder/gaps on v2 rows — without needing
/// signatures. Pre-v2 these were undetected by the hash-only check.
#[cfg(test)]
mod audit_hash_v2_tests {
    use crate::verifier_store::*;
    use crate::audit_chain::AuditChainLinker;

    fn in_memory() -> VerifierStore {
        VerifierStore::new(":memory:").unwrap()
    }

    /// CORE WIN: relabeling a v2 row's event_type is now caught by
    /// `verify_audit_chain_integrity`. Pre-v2 this was undetected — the
    /// row's event_type wasn't bound into the hash, so the cheap check
    /// returned true after relabeling.
    #[test]
    fn test_v2_event_type_relabel_detected_by_hash_only_check() {
        let mut store = in_memory();
        store
            .save_posture_event_chained("node", "ATTESTATION_TRUSTED", "{}", None, 1_000)
            .unwrap();
        // Sanity: chain verifies clean.
        assert!(store.verify_audit_chain_integrity().unwrap());

        // Tamper: relabel the just-written event_type via direct UPDATE.
        // (Both the row's `event_type` and any other tampering of the
        // row's content must now make the hash mismatch under v2.)
        store
            .conn
            .execute(
                "UPDATE audit_log_chain SET event_type = 'FEDERATION_ACCEPTED' \
                 WHERE id = (SELECT MAX(id) FROM audit_log_chain)",
                [],
            )
            .unwrap();

        // Cheap hash-only verifier must now reject — event_type is bound
        // into compute_record_hash_v2.
        assert!(
            !store.verify_audit_chain_integrity().unwrap(),
            "v2 hash must catch event_type relabeling; this is the relabeling-hole fix"
        );
    }

    /// V2 sequence tampering (gap / reorder) is caught.
    #[test]
    fn test_v2_sequence_tamper_detected() {
        let mut store = in_memory();
        for i in 0..3 {
            store
                .save_posture_event_chained("n", "EVT", "{}", None, 1_000 + i)
                .unwrap();
        }
        assert!(store.verify_audit_chain_integrity().unwrap());

        // Tamper: bump the middle row's sequence so it skips a value.
        store
            .conn
            .execute(
                "UPDATE audit_log_chain SET sequence = 99 \
                 WHERE id = (SELECT MIN(id) + 1 FROM audit_log_chain)",
                [],
            )
            .unwrap();

        assert!(
            !store.verify_audit_chain_integrity().unwrap(),
            "v2 verifier must reject when sequence is non-monotonic"
        );
    }

    /// V2 hash has no field-splicing ambiguity: ("AB","C") and ("A","BC")
    /// must produce different hashes. Length-prefixing every variable
    /// field prevents the boundary from sliding.
    #[test]
    fn test_v2_hash_no_field_splicing() {
        let prev = "0".repeat(64);
        let ts = 1_000;
        let seq = 0;
        let h_ab_c = AuditChainLinker::compute_record_hash_v2(&prev, "AB", "C", ts, seq);
        let h_a_bc = AuditChainLinker::compute_record_hash_v2(&prev, "A", "BC", ts, seq);
        assert_ne!(
            h_ab_c, h_a_bc,
            "v2 must not collide on field-boundary slides — length-prefixing prevents this"
        );
    }

    /// Mixed v1+v2 chain still verifies. We can't create a v1 row through
    /// the current append (which always writes v2) without raw insert, so
    /// this test uses raw INSERT to simulate a pre-migration v1 row, then
    /// chains a v2 row after it.
    #[test]
    fn test_mixed_v1_v2_chain_verifies() {
        let mut store = in_memory();

        // Manually insert a v1-shape row at the start of the chain (the
        // way upgraded databases will look).
        let prev_v1 = "0".repeat(64);
        let v1_ts: i64 = 1_000;
        let v1_payload = "{\"legacy\":true}";
        let v1_hash =
            AuditChainLinker::compute_record_hash_v1(&prev_v1, v1_payload, v1_ts);
        store
            .conn
            .execute(
                "INSERT INTO audit_log_chain
                 (event_type, event_json, previous_hash_hex, record_hash_hex,
                  created_at_ms, signature_b64, hash_version, sequence)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, 1, NULL)",
                rusqlite::params!["LEGACY_V1", v1_payload, prev_v1, v1_hash, v1_ts],
            )
            .unwrap();

        // Now append a v2 event via the chained writer. It must chain to
        // the v1 head and start at sequence 0.
        store
            .save_posture_event_chained("n", "NEW_V2", "{}", None, 2_000)
            .unwrap();

        assert!(
            store.verify_audit_chain_integrity().unwrap(),
            "mixed v1+v2 chain must verify under the version-dispatching verifier"
        );
    }

    /// V2 payload tamper (event_json changed) is still detected — the
    /// existing pre-v2 guarantee survives the migration.
    #[test]
    fn test_v2_payload_tamper_still_detected() {
        let mut store = in_memory();
        store
            .save_posture_event_chained("n", "EVT", r#"{"x":1}"#, None, 1_000)
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE audit_log_chain SET event_json = '{\"x\":2}' \
                 WHERE id = (SELECT MAX(id) FROM audit_log_chain)",
                [],
            )
            .unwrap();
        assert!(
            !store.verify_audit_chain_integrity().unwrap(),
            "v2 must still detect event_json tampering"
        );
    }

    /// Migration anchor is idempotent and is a no-op on a brand-new chain.
    #[test]
    fn test_migration_anchor_idempotent_and_noop_on_empty_chain() {
        let mut store = in_memory();
        // No v1 rows present → anchor is a no-op.
        store.ensure_hash_v2_migration_anchor(5_000).unwrap();
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE event_type='HASH_V2_MIGRATION'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "no v1 rows → no migration marker needed");

        // Simulate an upgraded DB: one v1 row, then run the anchor.
        let h = AuditChainLinker::compute_record_hash_v1(&"0".repeat(64), "{}", 100);
        store
            .conn
            .execute(
                "INSERT INTO audit_log_chain
                 (event_type, event_json, previous_hash_hex, record_hash_hex,
                  created_at_ms, signature_b64, hash_version, sequence)
                 VALUES ('LEGACY', '{}', ?1, ?2, 100, NULL, 1, NULL)",
                rusqlite::params![&"0".repeat(64), &h],
            )
            .unwrap();
        store.ensure_hash_v2_migration_anchor(5_000).unwrap();
        let count_after: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE event_type='HASH_V2_MIGRATION'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count_after, 1, "exactly one anchor written");

        // Second call must NOT write a second anchor.
        store.ensure_hash_v2_migration_anchor(6_000).unwrap();
        let count_idem: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE event_type='HASH_V2_MIGRATION'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count_idem, 1, "anchor is idempotent — second call no-ops");
    }
}

/// Issue #76 — audit key-rotation: cross-rotation verify, the sign-side swap
/// proof, the on-vs-off negative control, tamper-evidence, migration, and the
/// fail-closed unknown-key-id case.
#[cfg(test)]
mod audit_key_rotation_tests {
    use crate::verifier_store::*;
    use ed25519_dalek::SigningKey;
    use crate::audit_chain::{AuditChainLinker, verifying_key_id};

    fn store_with_key(seed: u8) -> (VerifierStore, SigningKey) {
        let mut s = VerifierStore::new(":memory:").expect("store");
        let sk = SigningKey::from_bytes(&[seed; 32]);
        s.set_signing_key(sk.clone());
        (s, sk)
    }

    /// Claim the first HA epoch on a fresh store and return the held fencing
    /// token an Active node holds — the only legitimate context for the fenced
    /// top-tier writes (`record_key_rotation`, `save_federated_report_chained`).
    /// (#79: those methods re-check this token inside their write transaction.)
    fn claim_epoch(s: &mut VerifierStore) -> u64 {
        s.try_claim_epoch(0, "test-node", 0)
            .unwrap()
            .expect("first epoch claim must win on a fresh store")
    }

    fn append(s: &mut VerifierStore, event_type: &str, ts: i64) {
        let sk = s.signing_key.clone();
        let tx = s.conn.transaction().unwrap();
        AuditChainLinker::append_audit_event_tx(&tx, event_type, "{}", ts, sk.as_ref()).unwrap();
        tx.commit().unwrap();
    }

    fn max_id(s: &VerifierStore) -> i64 {
        s.conn.query_row("SELECT MAX(id) FROM audit_log_chain", [], |r| r.get(0)).unwrap()
    }

    /// (payload, signature_b64, key_id) for a row, for direct sig checks.
    fn row_payload_sig(s: &VerifierStore, id: i64) -> (String, String, String) {
        s.conn.query_row(
            "SELECT event_type, previous_hash_hex, record_hash_hex, created_at_ms, \
             signature_b64, hash_version, sequence, key_id \
             FROM audit_log_chain WHERE id = ?1",
            [id],
            |r| {
                let et: String = r.get(0)?; let prev: String = r.get(1)?; let rec: String = r.get(2)?;
                let ts: i64 = r.get(3)?; let sig: String = r.get(4)?; let hv: i64 = r.get(5)?;
                let seq: Option<i64> = r.get(6)?; let kid: String = r.get(7)?;
                Ok((audit_signing_payload(hv, &prev, &rec, &et, ts, seq), sig, kid))
            },
        ).unwrap()
    }

    /// CROSS-ROTATION VERIFY: sign under A → rotate to B → append under B →
    /// verify_audit_chain_full asserts ALL rows (A and B) verify.
    #[test]
    fn cross_rotation_all_rows_verify() {
        let (mut s, a) = store_with_key(1);
        let held = claim_epoch(&mut s);
        append(&mut s, "E1", 100);
        append(&mut s, "E2", 200);
        let b = SigningKey::from_bytes(&[2; 32]);
        s.record_key_rotation(b.clone(), "scheduled", 300, held).unwrap();
        append(&mut s, "E3", 400);
        append(&mut s, "E4", 500);

        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(r.chain_intact, "hash chain intact across rotation");
        assert!(r.signature_valid, "all A-rows AND B-rows must verify");
        assert_eq!(r.first_invalid_signature_index, None);
        assert!(r.signed_entries >= 5);
    }

    /// SIGN-SIDE PROOF: the signing key actually swapped — a post-rotation row
    /// is signed by B (verifies under B, FAILS under A), and the store's
    /// in-memory key is now B. Directly kills the old cosmetic-rotation bug.
    #[test]
    fn rotation_actually_swaps_signing_key() {
        let (mut s, a) = store_with_key(1);
        let held = claim_epoch(&mut s);
        append(&mut s, "E1", 100);
        let b = SigningKey::from_bytes(&[2; 32]);
        s.record_key_rotation(b.clone(), "swap", 200, held).unwrap();
        // In-memory key swapped to B.
        assert_eq!(
            s.signing_key.as_ref().unwrap().verifying_key(),
            b.verifying_key(),
            "record_key_rotation must swap self.signing_key (not cosmetic)"
        );
        append(&mut s, "E2", 300);
        let id = max_id(&s);
        let (payload, sig, kid) = row_payload_sig(&s, id);
        assert_eq!(kid, verifying_key_id(&b.verifying_key()), "post-rotation row's key_id is B");
        assert!(audit_verify_sig(&b.verifying_key(), &payload, &sig), "verifies under B");
        assert!(!audit_verify_sig(&a.verifying_key(), &payload, &sig), "FAILS under A — signing swapped");
    }

    /// NEGATIVE CONTROL: the OLD single-key verify (one vk = A for every row)
    /// WOULD fail the post-rotation B-row; the new per-row keyring verify passes.
    /// The delta is the evidence the fix changed the outcome.
    #[test]
    fn negative_control_old_single_key_would_fail() {
        let (mut s, a) = store_with_key(1);
        let held = claim_epoch(&mut s);
        append(&mut s, "E1", 100);
        let b = SigningKey::from_bytes(&[2; 32]);
        s.record_key_rotation(b.clone(), "rot", 200, held).unwrap();
        append(&mut s, "E2", 300);
        let id = max_id(&s);
        let (payload, sig, _kid) = row_payload_sig(&s, id);

        // OLD behavior: verify the B-row under the single key A → fails.
        assert!(!audit_verify_sig(&a.verifying_key(), &payload, &sig),
            "old single-key(A) verify WOULD have failed the B-row (false tamper alarm)");
        // NEW behavior: full per-row keyring verify passes.
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(r.signature_valid, "new keyring verify passes for the same chain");
    }

    /// TAMPER: mutating a row's payload is detected (tamper-evidence intact).
    #[test]
    fn tamper_is_detected() {
        let (mut s, a) = store_with_key(1);
        append(&mut s, "E1", 100);
        append(&mut s, "E2", 200);
        s.conn.execute("UPDATE audit_log_chain SET event_json = '{\"x\":1}' WHERE id = 1", []).unwrap();
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(!r.chain_intact || !r.signature_valid, "tamper must be detected");
    }

    /// MIGRATION: existing rows with NULL key_id are backfilled with the genesis
    /// key's id by ensure_key_id_backfill_migration, and still verify.
    #[test]
    fn migration_backfills_genesis_key_id() {
        let (mut s, a) = store_with_key(1);
        append(&mut s, "E1", 100);
        append(&mut s, "E2", 200);
        // Simulate a pre-upgrade chain: drop the key_id the new append recorded.
        s.conn.execute("UPDATE audit_log_chain SET key_id = NULL", []).unwrap();

        s.ensure_key_id_backfill_migration(999).unwrap();
        let gid = verifying_key_id(&a.verifying_key());
        let nulls: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE key_id IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(nulls, 0, "all rows backfilled");
        let backfilled: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE key_id = ?1", [&gid], |r| r.get(0)).unwrap();
        assert!(backfilled >= 2, "rows carry the genesis key_id");
        // A signed KEY_ID_BACKFILL anchor exists, and the chain still verifies.
        let anchors: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'KEY_ID_BACKFILL'", [], |r| r.get(0)).unwrap();
        assert_eq!(anchors, 1, "migration anchored by a signed event");
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(r.chain_intact && r.signature_valid, "backfilled rows still verify under genesis");
        // Idempotent.
        s.ensure_key_id_backfill_migration(1000).unwrap();
        let anchors2: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'KEY_ID_BACKFILL'", [], |r| r.get(0)).unwrap();
        assert_eq!(anchors2, 1, "migration is idempotent");
    }

    /// UNKNOWN KEY-ID: a row whose key_id isn't in the keyring fails closed
    /// (not skipped).
    #[test]
    fn unknown_key_id_fails_closed() {
        let (mut s, a) = store_with_key(1);
        append(&mut s, "E1", 100);
        s.conn.execute(
            "UPDATE audit_log_chain SET key_id = 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef' WHERE id = 1",
            [],
        ).unwrap();
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(!r.signature_valid, "unknown key_id must fail closed, not skip");
        assert_eq!(r.first_invalid_signature_index, Some(0));
    }
}

/// Issue #74 — SQLite durability at power-loss: durable (FULL) connection
/// routing, epoch non-regression (the fence-correctness proof), nonce
/// durability, the in-memory fallback, and the shutdown checkpoint.
#[cfg(test)]
mod durability_tests {
    use crate::verifier_store::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CTR: AtomicU64 = AtomicU64::new(0);

    /// Temp DB file (+ -wal/-shm) cleaned up on drop.
    struct TmpDb(String);
    impl TmpDb {
        fn new(tag: &str) -> Self {
            let n = CTR.fetch_add(1, Ordering::SeqCst);
            let p = std::env::temp_dir()
                .join(format!("kirra74_{tag}_{}_{n}.db", std::process::id()));
            TmpDb(p.to_string_lossy().into_owned())
        }
        fn path(&self) -> &str { &self.0 }
    }
    impl Drop for TmpDb {
        fn drop(&mut self) {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{}", self.0, ext));
            }
        }
    }

    fn pragma_synchronous(c: &Connection) -> i64 {
        c.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap()
    }

    fn report(nonce: &str) -> crate::federation::FederatedTrustReport {
        crate::federation::FederatedTrustReport {
            source_controller_id: "ctrl-A".to_string(),
            asset_id: "asset-1".to_string(),
            posture: crate::verifier::FleetPosture::Nominal,
            issued_at_ms: 1_000,
            expires_at_ms: 9_000,
            nonce_hex: nonce.to_string(),
            signature_b64: "sig".to_string(),
        }
    }

    /// DURABLE ROUTING + config: a file-backed store has a FULL durable
    /// connection distinct from the NORMAL main connection.
    #[test]
    fn durable_connection_is_full_main_is_normal() {
        let db = TmpDb::new("routing");
        let s = VerifierStore::new(db.path()).unwrap();
        assert_eq!(pragma_synchronous(&s.conn), 1, "main conn is NORMAL (1)");
        let dc = s.durable_conn.as_ref().expect("file store must have a durable connection");
        assert_eq!(pragma_synchronous(dc), 2, "durable conn is FULL (2)");
    }

    /// IN-MEMORY FALLBACK: no separate durable conn (a 2nd :memory: open would be
    /// a distinct db), and epoch/nonce still work via the main connection.
    #[test]
    fn memory_store_has_no_durable_conn_but_works() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        assert!(s.durable_conn.is_none(), ":memory: must fall back to the main conn");
        assert_eq!(s.try_claim_epoch(0, "A", 1).unwrap(), Some(1));
        // #79: held == durable epoch (1) → the fence admits the legitimate write.
        s.save_federated_report_chained(&report("aa"), None, 2_000, 1).unwrap();
        assert!(s.has_seen_federation_nonce("aa").unwrap());
    }

    /// H1: a second commit bearing the SAME nonce maps the PRIMARY KEY violation to
    /// `NonceReplay` (a clean replay rejection), not an opaque Db error, and rolls
    /// back atomically so no second report row is persisted. This is the durable
    /// single-use claim that closes the check-then-act race in the submit handler.
    #[test]
    fn duplicate_nonce_chain_returns_nonce_replay_and_rolls_back() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        assert_eq!(s.try_claim_epoch(0, "A", 1).unwrap(), Some(1));
        s.save_federated_report_chained(&report("dup"), None, 2_000, 1).unwrap();

        let second = s.save_federated_report_chained(&report("dup"), None, 2_001, 1);
        assert!(
            matches!(second, Err(DurableWriteError::NonceReplay)),
            "a duplicate nonce must surface as NonceReplay, got {second:?}"
        );
        let count: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM federated_trust_reports WHERE asset_id = 'asset-1'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 1, "the replayed report must NOT have been persisted (atomic rollback)");
    }

    /// Item 20 — the per-(controller, asset) generation high-water gate ACCEPTS a
    /// strictly-ascending generation and advances the mark each time.
    #[test]
    fn generation_highwater_accepts_ascending() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        assert_eq!(s.try_claim_epoch(0, "A", 1).unwrap(), Some(1));
        s.save_federated_report_chained(&report("g1"), Some(10), 2_000, 1).unwrap();
        s.save_federated_report_chained(&report("g2"), Some(11), 2_001, 1).unwrap();
        s.save_federated_report_chained(&report("g3"), Some(50), 2_002, 1).unwrap();
        let hw: i64 = s.conn.query_row(
            "SELECT last_generation FROM federation_generation_highwater
             WHERE source_controller_id = 'ctrl-A' AND asset_id = 'asset-1'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(hw, 50, "the high-water mark advances to the latest accepted generation");
    }

    /// Item 20 — a report whose generation is <= the high-water (a regress, or an
    /// equal-generation replay of a distinct signed report) is REJECTED fail-closed
    /// and the whole commit rolls back: no report row, no burned nonce.
    #[test]
    fn generation_highwater_rejects_regress_and_rolls_back() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        assert_eq!(s.try_claim_epoch(0, "A", 1).unwrap(), Some(1));
        s.save_federated_report_chained(&report("hi"), Some(20), 2_000, 1).unwrap();

        for (nonce, gen) in [("lo", 19u64), ("eq", 20u64)] {
            let res = s.save_federated_report_chained(&report(nonce), Some(gen), 2_010, 1);
            assert!(
                matches!(res, Err(DurableWriteError::GenerationRegress { found, high_water })
                    if found == gen && high_water == 20),
                "generation {gen} <= high-water 20 must surface as GenerationRegress, got {res:?}"
            );
            assert!(!s.has_seen_federation_nonce(nonce).unwrap(),
                "a rejected report must NOT have burned its nonce (atomic rollback)");
        }

        let count: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM federated_trust_reports WHERE asset_id = 'asset-1'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 1, "only the first (gen 20) report persists; the regresses rolled back");
    }

    /// Item 20 — a v1 report (no generation) is NOT gated and does not seed the
    /// high-water table, preserving backward-compatible timestamp ordering.
    #[test]
    fn generation_highwater_skips_v1_reports() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        assert_eq!(s.try_claim_epoch(0, "A", 1).unwrap(), Some(1));
        s.save_federated_report_chained(&report("v1a"), None, 2_000, 1).unwrap();
        s.save_federated_report_chained(&report("v1b"), None, 2_001, 1).unwrap();
        let rows: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM federation_generation_highwater", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(rows, 0, "a v1 (None-generation) report must not touch the high-water table");
    }

    /// Item 20 — a forward generation JUMP (gen > high-water + 1) is accepted but
    /// records an in-chain FEDERATION_GENERATION_GAP marker naming the skipped
    /// generations; a contiguous +1 step does NOT.
    #[test]
    fn generation_gap_emits_in_chain_audit_marker() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        assert_eq!(s.try_claim_epoch(0, "A", 1).unwrap(), Some(1));
        s.save_federated_report_chained(&report("base"), Some(5), 2_000, 1).unwrap();
        // Contiguous step: no gap marker.
        s.save_federated_report_chained(&report("step"), Some(6), 2_001, 1).unwrap();
        // Jump 6 -> 9: missing 7,8 -> one gap marker.
        s.save_federated_report_chained(&report("jump"), Some(9), 2_002, 1).unwrap();

        let markers: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'FEDERATION_GENERATION_GAP'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(markers, 1, "exactly one gap marker for the 6->9 jump (the +1 step emits none)");

        let payload: String = s.conn.query_row(
            "SELECT event_json FROM audit_log_chain
             WHERE event_type = 'FEDERATION_GENERATION_GAP'",
            [], |r| r.get(0),
        ).unwrap();
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["last_accepted_generation"], 6);
        assert_eq!(v["observed_generation"], 9);
        assert_eq!(v["missing_from_generation"], 7);
        assert_eq!(v["missing_through_generation"], 8);
        assert_eq!(v["skipped_generations"], 2);
    }

    /// M2: accepting a report prunes nonces older than the retention horizon, so the
    /// anti-replay table stays bounded. Pruning an aged nonce is safe — a replay of
    /// its report fails the freshness gate on its fixed signed issued_at_ms — this
    /// test only asserts the bound is enforced.
    #[test]
    fn aged_nonces_are_pruned_on_accept() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        assert_eq!(s.try_claim_epoch(0, "A", 1).unwrap(), Some(1));
        s.save_federated_report_chained(&report("old"), None, 2_000, 1).unwrap();
        assert!(s.has_seen_federation_nonce("old").unwrap());

        // A later accept whose received_at is past the retention horizon over "old".
        s.save_federated_report_chained(&report("new"), None, 2_000 + FEDERATION_NONCE_RETENTION_MS as u64 + 1, 1).unwrap();
        assert!(!s.has_seen_federation_nonce("old").unwrap(),
            "an aged nonce must be pruned once an accept lands past the retention horizon");
        assert!(s.has_seen_federation_nonce("new").unwrap(), "the fresh nonce stays");
    }

    /// EPOCH NON-REGRESSION (the fence-correctness core of #74): a claim
    /// committed via the FULL path survives a store reopen ("recovery") and does
    /// NOT regress — a stale-observed re-claim then fails (no double-claim).
    #[test]
    fn epoch_claim_durable_across_reopen_and_fence_holds() {
        let db = TmpDb::new("epoch");
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            assert_eq!(s.try_claim_epoch(0, "primary", 100).unwrap(), Some(1),
                "primary claims epoch 1 (FULL-synced)");
        } // drop → simulate process loss; the claim was fsync'd on its FULL commit.

        // Recover: reopen the SAME file.
        let mut s2 = VerifierStore::new(db.path()).unwrap();
        assert_eq!(s2.try_claim_epoch(0, "ghost", 200).unwrap(), None,
            "a stale-observed (epoch 0) re-claim MUST fail — the epoch did not regress to 0");
        assert_eq!(s2.try_claim_epoch(1, "standby", 300).unwrap(), Some(2),
            "the durable epoch is 1; the legitimate next claim advances to 2 (fence intact)");
    }

    /// NONCE DURABILITY: a burned federation nonce survives reopen → no replay.
    #[test]
    fn nonce_burn_durable_across_reopen() {
        let db = TmpDb::new("nonce");
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            let held = s.try_claim_epoch(0, "test-node", 0).unwrap().unwrap();
            s.save_federated_report_chained(&report("deadbeef"), None, 2_000, held).unwrap();
            assert!(s.has_seen_federation_nonce("deadbeef").unwrap(), "burned before reopen");
        } // drop → simulate process loss.
        let s2 = VerifierStore::new(db.path()).unwrap();
        assert!(s2.has_seen_federation_nonce("deadbeef").unwrap(),
            "burned nonce must survive recovery — no replay window");
    }

    /// AUDIT-CHAIN INTEGRITY + shutdown checkpoint: appends stay sequenced and
    /// hash-linked under the dual-connection setup; durable_checkpoint() flushes
    /// without breaking verification.
    #[test]
    fn audit_chain_intact_after_checkpoint() {
        use ed25519_dalek::SigningKey;
        let db = TmpDb::new("audit");
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.set_signing_key(key.clone());
        // Append a few chained rows via a real store write path.
        for i in 0..3 {
            s.save_posture_event_chained("n", "EVT", "{}", None, 100 + i).unwrap();
        }
        // Force the shutdown-style durable checkpoint.
        s.durable_checkpoint().unwrap();
        let r = s.verify_audit_chain_full(Some(&key.verifying_key())).unwrap();
        assert!(r.chain_intact, "hash chain intact across the dual-conn + checkpoint");
        assert!(r.signature_valid, "signatures verify");
        // Reopen and re-verify — checkpointed rows are durable.
        drop(s);
        let s2 = VerifierStore::new(db.path()).unwrap();
        let r2 = s2.verify_audit_chain_full(Some(&key.verifying_key())).unwrap();
        assert!(r2.chain_intact && r2.signed_entries >= 3, "rows durable + intact after reopen");
    }
}

#[cfg(test)]
mod key_durability_165_tests {
    use crate::verifier_store::*;
    use ed25519_dalek::SigningKey;
    use crate::audit_chain::{verifying_key_id, AuditChainLinker};
    use std::sync::atomic::{AtomicU64, Ordering};

    static CTR: AtomicU64 = AtomicU64::new(0);

    /// Temp DB file (+ -wal/-shm) cleaned up on drop — a file store so the
    /// FULL durable_conn exists and survives reopen.
    struct TmpDb(String);
    impl TmpDb {
        fn new(tag: &str) -> Self {
            let n = CTR.fetch_add(1, Ordering::SeqCst);
            let p = std::env::temp_dir()
                .join(format!("kirra165_{tag}_{}_{n}.db", std::process::id()));
            TmpDb(p.to_string_lossy().into_owned())
        }
        fn path(&self) -> &str { &self.0 }
    }
    impl Drop for TmpDb {
        fn drop(&mut self) {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{}", self.0, ext));
            }
        }
    }

    fn key(seed: u8) -> SigningKey { SigningKey::from_bytes(&[seed; 32]) }
    fn kid(k: &SigningKey) -> String { verifying_key_id(&k.verifying_key()) }

    // --- Test 1: DURABLE ROTATION (gap-1 proof) -----------------------------
    #[test]
    fn durable_rotation_then_reverted_env_is_fail_closed() {
        let db = TmpDb::new("g1");
        let (a, b) = (key(1), key(2));
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            assert_eq!(
                s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(),
                KeyAdmission::BackfilledGenesis
            );
            assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&a).as_str()));
            // #79: an Active node holds the epoch it claimed; the rotation fence
            // re-checks it inside the write transaction.
            let held = s.try_claim_epoch(0, "test-node", 0).unwrap().unwrap();
            s.record_key_rotation(b.clone(), "scheduled", 2_000, held).unwrap();
            assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&b).as_str()));
        }
        // Reopen with env reverted to A (the retired key) → FAIL CLOSED.
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&b).as_str()),
                "active=B is durable across reopen");
            assert_eq!(
                s.admit_signing_key(a.clone(), false, None, 3_000).unwrap(),
                KeyAdmission::RetiredKeyRejected
            );
            assert!(s.signing_key.is_none(), "must NOT adopt a retired key for signing");
        }
        // Reopen with the correct active key B → resume.
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            assert_eq!(
                s.admit_signing_key(b.clone(), false, None, 4_000).unwrap(),
                KeyAdmission::Resumed
            );
            assert!(s.signing_key.is_some());
        }
    }

    // --- Test 2: ENV-ROTATION (adopt vs fail-closed, gap-2) -----------------
    #[test]
    fn env_rotation_new_key_requires_explicit_adopt() {
        let db = TmpDb::new("g2");
        let (a, c) = (key(1), key(3));
        { let mut s = VerifierStore::new(db.path()).unwrap();
          s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(); }
        // New env key, NO adopt → fail closed.
        { let mut s = VerifierStore::new(db.path()).unwrap();
          assert_eq!(
              s.admit_signing_key(c.clone(), false, None, 2_000).unwrap(),
              KeyAdmission::UnadoptedNewKeyRejected);
          assert!(s.signing_key.is_none()); }
        // New env key, WITH adopt → records reanchor, adopts C.
        { let mut s = VerifierStore::new(db.path()).unwrap();
          assert_eq!(
              s.admit_signing_key(c.clone(), true, None, 3_000).unwrap(),
              KeyAdmission::AdoptedReanchor);
          assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&c).as_str()));
          assert!(s.signing_key.is_some()); }
        // Subsequent boot with C (now active) resumes without adopt.
        { let mut s = VerifierStore::new(db.path()).unwrap();
          assert_eq!(
              s.admit_signing_key(c.clone(), false, None, 4_000).unwrap(),
              KeyAdmission::Resumed); }
    }

    // --- Test 3: GENESIS ANCHOR (gap-2) — mutated env can't re-root ---------
    #[test]
    fn genesis_comes_from_durable_anchor_not_env() {
        let db = TmpDb::new("g3");
        let (a, mutated) = (key(1), key(9));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(); // anchor genesis = A
        // Append a normal signed row under A.
        {
            let sk = s.signing_key.clone();
            let tx = s.conn.transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(&tx, "TEST", "{}", 1_500, sk.as_ref()).unwrap();
            tx.commit().unwrap();
        }
        // Verify while passing a MUTATED key: genesis must resolve from the
        // durable anchor (A), so the prior rows still verify and the mutated
        // key cannot re-root the keyring.
        let r = s.verify_audit_chain_full(Some(&mutated.verifying_key())).unwrap();
        assert!(r.chain_intact, "chain intact");
        assert!(r.signature_valid, "prior rows verify under the durable genesis anchor, not the mutated env key");
    }

    // --- Test 4: FIRST-BOOT BACKFILL + idempotency --------------------------
    #[test]
    fn first_boot_backfill_writes_anchor_and_is_idempotent() {
        let db = TmpDb::new("g4");
        let a = key(1);
        let mut s = VerifierStore::new(db.path()).unwrap();
        assert_eq!(
            s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(),
            KeyAdmission::BackfilledGenesis);
        assert_eq!(s.audit_trust_anchor_genesis_id().unwrap().as_deref(), Some(kid(&a).as_str()));
        let genesis_rows = s.audit_key_ledger_rows().unwrap()
            .into_iter().filter(|r| r.role == "genesis").count();
        assert_eq!(genesis_rows, 1, "exactly one genesis ledger row");
        // Re-run admission with the same key → resume, no second backfill.
        assert_eq!(
            s.admit_signing_key(a.clone(), false, None, 2_000).unwrap(),
            KeyAdmission::Resumed);
        let genesis_rows2 = s.audit_key_ledger_rows().unwrap()
            .into_iter().filter(|r| r.role == "genesis").count();
        assert_eq!(genesis_rows2, 1, "backfill is idempotent — still exactly one genesis row");
    }

    /// Inject a pre-#165 in-chain `KEY_ROTATION` (old→new) with NO ledger row,
    /// simulating an in-process rotation done before #165.
    fn inject_chain_rotation(s: &mut VerifierStore, old: &SigningKey, new: &SigningKey, ts: i64) {
        let payload = serde_json::json!({
            "new_public_key_b64": b64e.encode(new.verifying_key().as_bytes()),
            "new_key_id": kid(new),
            "reason": "preexisting",
            "rotated_at_ms": ts,
        }).to_string();
        let tx = s.conn.transaction().unwrap();
        AuditChainLinker::append_audit_event_tx(&tx, "KEY_ROTATION", &payload, ts, Some(old)).unwrap();
        tx.commit().unwrap();
    }

    // --- Test 5: MIGRATION RECONCILE (consented reversion via adopt) --------
    #[test]
    fn migration_reconcile_with_adopt_records_consented_reanchor() {
        let db = TmpDb::new("g5");
        let (a, b) = (key(1), key(2));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.set_signing_key(a.clone());
        inject_chain_rotation(&mut s, &a, &b, 500); // chain A→B, env will be A

        // Env reverted to A while the chain's latest rotation is B → consented
        // adopt is required; it backfills the ledger AND logs a reanchor.
        assert_eq!(
            s.admit_signing_key(a.clone(), true, None, 1_000).unwrap(),
            KeyAdmission::AdoptedReanchor);
        let rows = s.audit_key_ledger_rows().unwrap();
        assert!(rows.iter().any(|r| r.role == "genesis" && r.key_id == kid(&a)),
            "genesis ledger row for A");
        assert!(rows.iter().any(|r| r.role == "backfill" && r.key_id == kid(&b)),
            "forensic backfill ledger row matching the pre-existing chain rotation to B");
        assert!(rows.iter().any(|r| r.role == "reanchor"
                && r.key_id == kid(&a)
                && r.prev_key_id.as_deref() == Some(kid(&b).as_str())),
            "consented reanchor row: A adopted over the chain's latest (B)");
        // The consented env key (A) is the active key.
        assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&a).as_str()));
    }

    // --- Migration hardening: reversion at first boot, no adopt → fail-closed
    #[test]
    fn migration_reversion_no_adopt_is_fail_closed() {
        let db = TmpDb::new("g5b");
        let (a, b) = (key(1), key(2));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.set_signing_key(a.clone());
        inject_chain_rotation(&mut s, &a, &b, 500); // chain A→B
        // Env = A (reverted to a pre-rotation key), no adopt → FAIL CLOSED.
        assert_eq!(
            s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(),
            KeyAdmission::MigrationReversionRejected {
                chain_latest_key_id: kid(&b),
                env_key_id: kid(&a),
            });
        // Fail-closed: nothing durable was written — no anchor, no ledger rows.
        assert!(s.audit_trust_anchor_genesis_id().unwrap().is_none(), "no anchor written on reject");
        assert!(s.audit_key_ledger_active_id().unwrap().is_none(), "no ledger row written on reject");
    }

    // --- Migration hardening: env matches the chain's latest rotation → OK ---
    #[test]
    fn migration_env_matches_latest_rotation_does_not_fire() {
        let db = TmpDb::new("g5c");
        let (a, b) = (key(1), key(2));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.set_signing_key(a.clone());
        inject_chain_rotation(&mut s, &a, &b, 500); // chain A→B
        // Env = B (correctly updated to the latest rotation) → normal backfill.
        assert_eq!(
            s.admit_signing_key(b.clone(), false, None, 1_000).unwrap(),
            KeyAdmission::BackfilledGenesis);
        assert_eq!(s.audit_trust_anchor_genesis_id().unwrap().as_deref(), Some(kid(&b).as_str()));
        assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&b).as_str()));
    }

    // --- Migration hardening: no rotations in chain → unaffected ------------
    #[test]
    fn migration_no_rotations_is_unaffected() {
        let db = TmpDb::new("g5d");
        let a = key(1);
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.set_signing_key(a.clone());
        // A signed non-rotation row, but NO KEY_ROTATION.
        {
            let sk = s.signing_key.clone();
            let tx = s.conn.transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(&tx, "TEST", "{}", 10, sk.as_ref()).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(),
            KeyAdmission::BackfilledGenesis);
        assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&a).as_str()));
    }

    // --- Test 6: ATOMICITY — chain row + ledger row both-or-neither ---------
    #[test]
    fn rotation_chain_row_and_ledger_row_are_atomic_across_reopen() {
        let db = TmpDb::new("g6");
        let (a, b) = (key(1), key(2));
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            s.admit_signing_key(a.clone(), false, None, 1).unwrap();
            let held = s.try_claim_epoch(0, "test-node", 0).unwrap().unwrap();
            s.record_key_rotation(b.clone(), "r", 2, held).unwrap();
        }
        let s = VerifierStore::new(db.path()).unwrap();
        let chain_rot: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type='KEY_ROTATION'", [], |r| r.get(0)).unwrap();
        let ledger_rot: i64 = s.durable_ref().query_row(
            "SELECT COUNT(*) FROM audit_key_ledger WHERE role='rotation'", [], |r| r.get(0)).unwrap();
        assert_eq!(chain_rot, 1, "the KEY_ROTATION chain row is durable across reopen");
        assert_eq!(ledger_rot, 1, "the ledger rotation row is durable across reopen");
        assert_eq!(chain_rot, ledger_rot, "both-present (single FULL transaction)");
    }

    // --- Regression: a rotated chain still verifies under the ledger seed ----
    #[test]
    fn rotated_chain_still_verifies_with_durable_seed() {
        let db = TmpDb::new("g7");
        let (a, b) = (key(1), key(2));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.admit_signing_key(a.clone(), false, None, 1).unwrap();
        let held = s.try_claim_epoch(0, "test-node", 0).unwrap().unwrap();
        // a signed row under A, rotate to B, a signed row under B.
        {
            let sk = s.signing_key.clone();
            let tx = s.conn.transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(&tx, "TEST", "{}", 10, sk.as_ref()).unwrap();
            tx.commit().unwrap();
        }
        s.record_key_rotation(b.clone(), "r", 20, held).unwrap();
        {
            let sk = s.signing_key.clone();
            let tx = s.conn.transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(&tx, "TEST", "{}", 30, sk.as_ref()).unwrap();
            tx.commit().unwrap();
        }
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(r.chain_intact, "hash chain intact across rotation");
        assert!(r.signature_valid, "all rows (A-signed AND B-signed) verify under the durable seed");
    }

    // --- #685: audit chain stays contiguous across both connections ----------
    // `audit_log_chain` is written from BOTH the NORMAL conn
    // (`save_posture_event_chained`) and the FULL `durable_conn`
    // (`record_key_rotation`); on a FILE-backed store those are DISTINCT
    // connections. This is a CONTIGUITY regression test for that two-connection
    // chain: interleave production writers from both connections and assert one
    // verifying, unbroken 0..N line across the connection boundary + rotation.
    //
    // Scope note (do not over-read): this does NOT force a statement-level
    // interleave. Under the single-process store mutex the two connections never
    // write concurrently, and most NORMAL-side writers (this one included) take
    // the WAL write lock on their domain-row INSERT *before* the audit tail read
    // regardless of DEFERRED/Immediate. `Immediate` (`audit_tx`) is what makes
    // the no-fork guarantee hold uniformly and independently of the store mutex
    // (and fixes the v2-migration path, where the audit append is the first
    // statement). This test guards the cross-connection invariant; it is not a
    // proof of the lock ordering itself.
    #[test]
    fn cross_connection_audit_chain_is_contiguous() {
        let db = TmpDb::new("s685");
        let (a, b) = (key(1), key(2));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.admit_signing_key(a.clone(), false, None, 1).unwrap();
        let held = s.try_claim_epoch(0, "test-node", 0).unwrap().unwrap();

        // NORMAL-conn audit writes (posture events) interleaved with a FULL-conn
        // audit write (key rotation), all appending to the same audit_log_chain.
        s.save_posture_event_chained("n1", "DEGRADED", "{}", None, 10).unwrap();
        s.save_posture_event_chained("n1", "NOMINAL", "{}", None, 20).unwrap();
        s.record_key_rotation(b.clone(), "scheduled", 30, held).unwrap(); // durable_conn
        s.save_posture_event_chained("n2", "DEGRADED", "{}", None, 40).unwrap();
        s.save_posture_event_chained("n2", "LOCKEDOUT", "{}", None, 50).unwrap();

        // (1) The chain verifies end-to-end across the connection boundary and the
        //     rotation (rows signed by A before, by B after).
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(r.chain_intact, "hash chain intact across the NORMAL/FULL connection boundary");
        assert!(r.signature_valid, "all rows verify (A-signed and B-signed)");
        assert_eq!(r.first_invalid_signature_index, None);

        // (2) NO FORK: sequences are contiguous 0..N with no gap, duplicate, or
        //     branch — exactly what a tail read off a stale previous_hash breaks.
        let seqs: Vec<i64> = {
            let mut stmt = s
                .conn
                .prepare("SELECT sequence FROM audit_log_chain ORDER BY id ASC")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
            rows.map(|x| x.unwrap()).collect()
        };
        let expected: Vec<i64> = (0..seqs.len() as i64).collect();
        assert_eq!(seqs, expected, "audit sequences are contiguous 0..N — chain did not fork");
        assert!(seqs.len() >= 5, "at least the 5 rows this test appended are present");
    }

    // --- Test 7: WCET — verdict path is independent of the key ledger --------
    #[test]
    fn wcet_verdict_path_does_not_touch_key_ledger() {
        // The per-command verdict (validate_vehicle_command) is a pure function
        // of (command, contract) — it takes no store, no connection, no key.
        // #165 work is entirely boot-time (admit_signing_key) + rotation-time
        // (record_key_rotation), off the hot path. This compiles & runs with no
        // VerifierStore in scope, demonstrating the independence.
        use crate::gateway::kinematics_contract::{
            validate_vehicle_command, EnforceAction, ProposedVehicleCommand,
            VehicleKinematicsContract,
        };
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,   // zero implied accel
            delta_time_s: 0.05,
            steering_angle_deg: 1.0,
            current_steering_angle_deg: 1.0, // zero steering rate
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }
}

// ---------------------------------------------------------------------------
// SG-010 (ASIL B) — Audit Chain Tamper Detection
// ---------------------------------------------------------------------------
//
// Verifies: SG-010. `verify_audit_chain_full` is the mechanism: every row binds
// to its predecessor through a recomputed hash AND (when signing is enabled) an
// Ed25519 signature over that hash, so any out-of-band edit to a stored row is
// detected — `chain_intact` goes false and `first_invalid_signature_index`
// pinpoints the first tampered row.
//
// These tests use a FILE-BACKED DB (tempfile): a SQLite `:memory:` database is
// per-connection, so to model a real tamperer we open a SECOND connection to the
// same file and mutate a row the FIRST connection wrote (via the `raw_conn`
// test seam), then verify through the original connection.
//
// SCOPE / HONEST GAP: SG-010's full statement also requires that audit-chain
// verification runs AUTOMATICALLY on service startup BEFORE the listener binds.
// That mechanism does NOT exist today: `src/bin/kirra_verifier_service.rs` runs
// only `check_startup_invariants` (admin-token / WAL / watchdog / posture-engine)
// before `TcpListener::bind`, and verifies the chain only on demand via the
// `/system/audit/verify` endpoint (plus a durable checkpoint on shutdown). Wiring
// a verify-and-abort into startup is a BEHAVIOR change, out of scope for a
// test-only change, so it is reported as a mechanism gap rather than asserted
// here. See RTM_GAP_REPORT.md (SG-010).
#[cfg(test)]
mod sg_010_audit_tamper_tests {
    use crate::verifier_store::*;
    use ed25519_dalek::SigningKey;

    /// Writes three signed audit rows to a file-backed store, returning the
    /// store, its verifying key, and the DB path string. The `TempDir` is
    /// returned so the caller keeps it alive (drop = cleanup of db + -wal/-shm).
    fn signed_chain_on_disk() -> (tempfile::TempDir, String, ed25519_dalek::VerifyingKey) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.sqlite");
        let path_str = path.to_str().unwrap().to_string();

        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();

        let mut writer = VerifierStore::new(&path_str).expect("writer store");
        writer.set_signing_key(sk);
        writer.save_posture_event_chained("n1", "E1", "{}", None, 100).unwrap();
        writer.save_posture_event_chained("n1", "E2", "{}", None, 200).unwrap();
        writer.save_posture_event_chained("n1", "E3", "{}", None, 300).unwrap();

        // Control: the untampered chain verifies clean. This proves the tamper —
        // not some pre-existing breakage — is what trips the later assertions.
        let clean = writer.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(clean.chain_intact, "freshly written chain must be intact");
        assert!(clean.signature_valid, "freshly written rows must all verify");
        assert_eq!(clean.first_invalid_signature_index, None,
            "no tampered index in a clean chain");
        assert_eq!(clean.total_entries, 3, "exactly the three rows we wrote");

        (dir, path_str, vk)
    }

    /// Tampering a previously-written row (out of band, via a SECOND connection)
    /// is detected: chain_intact == false AND the first tampered index is named.
    #[test]
    fn test_tamper_via_second_connection_detected_with_first_index() {
        let (_dir, path_str, vk) = signed_chain_on_disk();

        // A separate connection to the SAME file — the "attacker with disk
        // access". On :memory: this row would be invisible to it; file-backed,
        // it sees and can mutate what the writer committed.
        let mut tamperer = VerifierStore::new(&path_str).expect("tamperer store");
        let (tampered_id, ordinal): (i64, u64) = {
            let conn = tamperer.raw_conn();
            // Target the MIDDLE row (E2) so we assert the index points at it,
            // not trivially at row 0.
            let id: i64 = conn
                .query_row(
                    "SELECT id FROM audit_log_chain ORDER BY id ASC LIMIT 1 OFFSET 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            // 0-based position in id order = the index verify_audit_chain_full reports.
            let ord: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM audit_log_chain WHERE id < ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            // Back-date the event: created_at_ms is bound into BOTH the record
            // hash and the signature payload, so this single edit breaks the
            // hash chain and invalidates the signature on exactly this row.
            conn.execute(
                "UPDATE audit_log_chain SET created_at_ms = created_at_ms + 99999 WHERE id = ?1",
                [id],
            )
            .unwrap();
            (id, ord as u64)
        };

        // Verify through a FRESH store on the same file (independent of the
        // tamperer) to prove the tamper is durable, not connection-local.
        let reader = VerifierStore::new(&path_str).expect("reader store");
        let r = reader.verify_audit_chain_full(Some(&vk)).unwrap();

        assert!(!r.chain_intact,
            "back-dating row id={tampered_id} must break the hash chain");
        assert!(!r.signature_valid,
            "the tampered row's signature must no longer verify");
        assert_eq!(r.first_invalid_signature_index, Some(ordinal),
            "verify must pinpoint the FIRST tampered row's index ({ordinal})");
    }

    /// Even an unsigned chain detects tampering via the hash linkage alone
    /// (chain_intact), independent of signatures.
    #[test]
    fn test_hash_linkage_detects_tamper_without_signing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit_unsigned.sqlite");
        let path_str = path.to_str().unwrap().to_string();

        // No signing key set ⇒ unsigned rows.
        let mut store = VerifierStore::new(&path_str).expect("store");
        store.save_posture_event_chained("n1", "E1", "{}", None, 100).unwrap();
        store.save_posture_event_chained("n1", "E2", "{}", None, 200).unwrap();

        let clean = store.verify_audit_chain_full(None).unwrap();
        assert!(clean.chain_intact, "unsigned chain still hash-links");

        // Tamper the payload of the first row via the raw connection.
        store
            .raw_conn()
            .execute(
                "UPDATE audit_log_chain SET event_json = '{\"x\":1}' WHERE id = \
                 (SELECT id FROM audit_log_chain ORDER BY id ASC LIMIT 1)",
                [],
            )
            .unwrap();

        let r = store.verify_audit_chain_full(None).unwrap();
        assert!(!r.chain_intact,
            "tampering event_json must break the recomputed hash even with no signatures");
    }
}

// ---------------------------------------------------------------------------
// Issue #79 — in-transaction HA epoch fence (closes the residual gate TOCTOU).
//
// These tests prove that a node SUPERSEDED between the request-path gate check
// and the durable commit cannot land even one stale top-tier write: the fence
// re-reads `ha_state.epoch` inside the write transaction and rejects on any
// mismatch, dropping the transaction with NO partial mutation. The legitimate
// path (held == durable) still commits — so the fence is demonstrably the only
// thing rejecting in the race case.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod epoch_fence_79_tests {
    use crate::verifier_store::*;
    use ed25519_dalek::SigningKey;

    fn report(nonce: &str) -> FederatedTrustReport {
        FederatedTrustReport {
            source_controller_id: "ctrl-A".to_string(),
            asset_id: "asset-1".to_string(),
            posture: crate::verifier::FleetPosture::Nominal,
            issued_at_ms: 1_000,
            expires_at_ms: 9_000,
            nonce_hex: nonce.to_string(),
            signature_b64: "sig".to_string(),
        }
    }

    /// Claim the first epoch (held == durable == 1) — what an Active node holds.
    fn claimed(s: &mut VerifierStore) -> u64 {
        s.try_claim_epoch(0, "self", 0)
            .unwrap()
            .expect("first epoch claim wins on a fresh store")
    }

    /// CORE PROOF — race closure: held == 1 (the request-path gate would pass),
    /// then a concurrent instance claims and the durable epoch advances to 2.
    /// A top-tier durable write with the now-stale held == 1 is REJECTED with
    /// `EpochSuperseded`, and NOTHING partial lands (nonce not burned, no report
    /// row, `ha_state` untouched). Contrast with the legitimate-path test below,
    /// where the identical write commits because held == durable — so the fence
    /// is the sole reason for rejection here; the TOCTOU window is closed.
    #[test]
    fn fenced_federation_write_superseded_lands_no_partial() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        let held = claimed(&mut s); // held == durable == 1
        assert_eq!(held, 1);

        // Concurrent claim advances the durable epoch to 2 — we are now stale.
        assert_eq!(s.try_claim_epoch(1, "other", 5).unwrap(), Some(2));
        assert_eq!(s.current_epoch().unwrap(), 2);

        let err = s
            .save_federated_report_chained(&report("cafe"), None, 9_000, held)
            .unwrap_err();
        match err {
            DurableWriteError::Fenced(FenceError::EpochSuperseded { held: h, durable: d }) => {
                assert_eq!((h, d), (1, 2), "fence reports stale-held vs durable epoch");
            }
            other => panic!("expected EpochSuperseded, got {other:?}"),
        }

        assert!(
            !s.has_seen_federation_nonce("cafe").unwrap(),
            "fenced write must NOT burn the nonce"
        );
        assert!(
            s.load_federated_reports_for_asset("asset-1").unwrap().is_empty(),
            "fenced write must NOT persist the report row"
        );
        assert_eq!(s.current_epoch().unwrap(), 2, "fenced attempt must not touch ha_state");
    }

    /// LEGITIMATE PATH: held == durable → the identical write commits. The only
    /// delta from the race test is the matching epoch, isolating the fence as
    /// the cause of the rejection there.
    #[test]
    fn legitimate_federation_write_commits_when_held_matches_durable() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        let held = claimed(&mut s);
        s.save_federated_report_chained(&report("beef"), None, 9_000, held)
            .unwrap();
        assert!(
            s.has_seen_federation_nonce("beef").unwrap(),
            "held == durable must commit and burn the nonce"
        );
    }

    /// FAIL-CLOSED when the durable epoch is unreadable (ha_state row absent):
    /// the fence returns `EpochUnreadable` and the write never proceeds blind.
    #[test]
    fn fenced_fail_closed_when_epoch_unreadable() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        let held = claimed(&mut s);
        s.conn.execute("DELETE FROM ha_state WHERE id = 1", []).unwrap();

        let err = s
            .save_federated_report_chained(&report("f00d"), None, 9_000, held)
            .unwrap_err();
        assert!(
            matches!(err, DurableWriteError::Fenced(FenceError::EpochUnreadable)),
            "absent ha_state row must fail closed (EpochUnreadable), not write blind"
        );
        assert!(!s.has_seen_federation_nonce("f00d").unwrap());
    }

    /// NEVER-CLAIMED (held == 0) is fenced even at genesis (durable == 0): a node
    /// that never legitimately claimed an epoch must not perform a top-tier write.
    #[test]
    fn fenced_when_never_claimed_held_zero() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        assert_eq!(s.current_epoch().unwrap(), 0, "genesis durable epoch is 0");

        let err = s
            .save_federated_report_chained(&report("0000"), None, 9_000, 0)
            .unwrap_err();
        assert!(
            matches!(
                err,
                DurableWriteError::Fenced(FenceError::EpochSuperseded { held: 0, durable: 0 })
            ),
            "held == 0 must be fenced even when durable == 0"
        );
        assert!(!s.has_seen_federation_nonce("0000").unwrap());
    }

    /// The SECOND fenced site — `record_key_rotation` — is covered too: a
    /// superseded rotation is rejected and swaps NOTHING (no KEY_ROTATION chain
    /// row, in-memory signing key unchanged).
    #[test]
    fn fenced_key_rotation_superseded_lands_no_partial() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        let a = SigningKey::from_bytes(&[1; 32]);
        s.set_signing_key(a.clone());
        let held = claimed(&mut s); // held == durable == 1

        assert_eq!(s.try_claim_epoch(1, "other", 5).unwrap(), Some(2)); // superseded
        let b = SigningKey::from_bytes(&[2; 32]);

        let err = s.record_key_rotation(b.clone(), "fenced", 9, held).unwrap_err();
        assert!(matches!(
            err,
            DurableWriteError::Fenced(FenceError::EpochSuperseded { held: 1, durable: 2 })
        ));

        let rotations: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'KEY_ROTATION'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rotations, 0, "fenced rotation must not append a KEY_ROTATION row");
        assert_eq!(
            s.signing_key.as_ref().unwrap().verifying_key(),
            a.verifying_key(),
            "fenced rotation must NOT swap the in-memory signing key"
        );
    }
}

// ---------------------------------------------------------------------------
// #77 — signed anchor-HEAD high-water mark: tail-truncation / deletion + head
// tamper detection, and the #74 power-loss interaction.
//
// The per-row chain walk cannot see a TRUNCATED tail: deleting the last k rows
// leaves the surviving prefix internally hash-consistent. The signed head closes
// that gap by recording the highest committed (sequence, record_hash). These
// tests mutate the chain/head out-of-band via the `raw_conn` seam (the same
// thing a tamperer with disk access would do).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod audit_anchor_head_77_tests {
    use crate::verifier_store::*;
    use rusqlite::params;
    use ed25519_dalek::SigningKey;
    use base64::engine::general_purpose::STANDARD as b64e;

    fn signed_store() -> (VerifierStore, ed25519_dalek::VerifyingKey) {
        let mut s = VerifierStore::new(":memory:").expect("store");
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();
        s.set_signing_key(sk);
        (s, vk)
    }

    fn append3(s: &mut VerifierStore) {
        s.save_posture_event_chained("n", "E1", "{}", None, 100).unwrap();
        s.save_posture_event_chained("n", "E2", "{}", None, 200).unwrap();
        s.save_posture_event_chained("n", "E3", "{}", None, 300).unwrap();
    }

    fn read_head(s: &mut VerifierStore) -> (i64, String, Option<String>, Option<String>) {
        s.raw_conn()
            .query_row(
                "SELECT sequence, record_hash_hex, signature_b64, key_id \
                 FROM audit_anchor_head WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    }

    /// LEGITIMATE: a freshly-written signed chain verifies clean AND the head
    /// matches the tail.
    #[test]
    fn clean_chain_head_verifies() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(r.chain_intact && r.signature_valid, "control: chain itself is intact");
        assert!(r.head_verified, "head must match the tail on a clean chain");
        assert_eq!(r.head_status, "OK");
        assert_eq!(r.total_entries, 3);
    }

    /// #690: a row whose SIGNATURE is tampered (its signed payload / record hash
    /// untouched) leaves the hash chain internally consistent — `chain_intact ==
    /// true` — but the signature no longer verifies, so the AUTHORITATIVE verdict
    /// `verified()` must be false. This is exactly the gap a caller keying off
    /// `chain_intact` alone would miss (the motivating case being a failed
    /// `KEY_ROTATION` row, which also strands every later row signed by the
    /// un-absorbed rotated key).
    #[test]
    fn tampered_signature_keeps_chain_intact_but_fails_verified_690() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        // Corrupt ONLY the signature of the middle row (sequence 1) by flipping its
        // first base64 char to a guaranteed-different one. The record hash does not
        // cover `signature_b64`, so the hash walk still links cleanly.
        let changed = s
            .raw_conn()
            .execute(
                "UPDATE audit_log_chain \
                 SET signature_b64 = CASE WHEN substr(signature_b64,1,1)='A' THEN 'B' ELSE 'A' END \
                                     || substr(signature_b64, 2) \
                 WHERE sequence = 1",
                [],
            )
            .unwrap();
        assert_eq!(changed, 1, "exactly the middle row's signature was altered");

        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(r.chain_intact, "hash linkage is untouched — chain_intact stays true");
        assert!(!r.signature_valid, "the tampered signature must fail to verify");
        assert!(
            !r.verified(),
            "the authoritative verdict folds in signatures → a hash-intact, bad-signature chain is NOT verified"
        );
        assert!(r.first_invalid_signature_index.is_some(), "the offending row is named");
    }

    /// EMPTY chain → no head required, clean.
    #[test]
    fn empty_chain_needs_no_head() {
        let (s, vk) = signed_store();
        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert_eq!(r.total_entries, 0);
        assert!(r.head_verified);
        assert_eq!(r.head_status, "EMPTY_CHAIN");
    }

    /// TAIL TRUNCATION: delete the last row out-of-band, leaving the head
    /// pointing past the surviving tail → DETECTED. The surviving prefix is still
    /// internally consistent (`chain_intact == true`), which is exactly the gap
    /// the head closes.
    #[test]
    fn tail_truncation_is_detected() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        // Delete the last row (E3, sequence 2) but NOT the head (still seq 2).
        s.raw_conn()
            .execute(
                "DELETE FROM audit_log_chain \
                 WHERE id = (SELECT MAX(id) FROM audit_log_chain)",
                [],
            )
            .unwrap();

        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(
            r.chain_intact,
            "the surviving 2-row prefix is still hash-consistent — the walk alone cannot see the truncation"
        );
        assert!(!r.head_verified, "the head high-water mark must detect the deleted tail");
        assert_eq!(r.head_status, "TRUNCATION_DETECTED");
        assert_eq!(r.total_entries, 2, "only the prefix survived");
    }

    /// Deleting MORE than one tail row is still caught.
    #[test]
    fn multi_row_truncation_is_detected() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        s.raw_conn()
            .execute(
                "DELETE FROM audit_log_chain WHERE id IN \
                 (SELECT id FROM audit_log_chain ORDER BY id DESC LIMIT 2)",
                [],
            )
            .unwrap();
        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(!r.head_verified);
        assert_eq!(r.head_status, "TRUNCATION_DETECTED");
        assert_eq!(r.total_entries, 1);
    }

    /// HEAD TAMPER: corrupt the head signature → verify fails closed.
    #[test]
    fn head_signature_tamper_is_detected() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        // A well-formed but WRONG signature (64 zero bytes) — decodes, never verifies.
        let bogus = b64e.encode([0u8; 64]);
        s.raw_conn()
            .execute(
                "UPDATE audit_anchor_head SET signature_b64 = ?1 WHERE id = 1",
                params![bogus],
            )
            .unwrap();
        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(!r.head_verified, "a tampered head signature must fail closed");
        assert_eq!(r.head_status, "HEAD_SIGNATURE_INVALID");
    }

    /// HEAD ABSENT on a non-empty chain → fail closed (deleted head / unmigrated).
    #[test]
    fn absent_head_on_nonempty_chain_fails_closed() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        s.raw_conn().execute("DELETE FROM audit_anchor_head", []).unwrap();
        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(!r.head_verified);
        assert_eq!(r.head_status, "HEAD_ABSENT");
    }

    /// #74 POWER-LOSS interaction: the last commit (row + its head update) is lost
    /// TOGETHER on an ungraceful cut. Simulate by dropping the last row AND
    /// restoring the head to its prior (committed) value. Verify must PASS — head
    /// stays consistent with the recovered tail → NO false truncation alarm.
    #[test]
    fn power_loss_of_last_commit_does_not_false_alarm() {
        let (mut s, vk) = signed_store();
        s.save_posture_event_chained("n", "E1", "{}", None, 100).unwrap();
        s.save_posture_event_chained("n", "E2", "{}", None, 200).unwrap();
        // Head as committed after E2 (the state the head reverts to on rollback).
        let head_after_e2 = read_head(&mut s);
        // E3 commits: row + head→E3 atomically.
        s.save_posture_event_chained("n", "E3", "{}", None, 300).unwrap();

        // Ungraceful power loss of E3's single commit: row AND head update vanish
        // together (same NORMAL transaction). Recover to the post-E2 state.
        {
            let c = s.raw_conn();
            c.execute(
                "DELETE FROM audit_log_chain WHERE id = (SELECT MAX(id) FROM audit_log_chain)",
                [],
            )
            .unwrap();
            c.execute(
                "UPDATE audit_anchor_head \
                 SET sequence = ?1, record_hash_hex = ?2, signature_b64 = ?3, key_id = ?4 \
                 WHERE id = 1",
                params![head_after_e2.0, head_after_e2.1, head_after_e2.2, head_after_e2.3],
            )
            .unwrap();
        }

        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(r.chain_intact, "recovered 2-row prefix is intact");
        assert!(
            r.head_verified,
            "head and tail lost the SAME commit together → consistent → NO false alarm (#74)"
        );
        assert_eq!(r.head_status, "OK");
        assert_eq!(r.total_entries, 2);
    }

    /// BACKFILL: a chain that has rows but no head (a pre-#77 on-disk chain) gets
    /// a signed head from `ensure_audit_anchor_head`, after which it verifies clean.
    #[test]
    fn ensure_audit_anchor_head_backfills_legacy_chain() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        // Model a pre-#77 chain: rows present, head missing.
        s.raw_conn().execute("DELETE FROM audit_anchor_head", []).unwrap();
        assert!(!s.verify_audit_chain_full(Some(&vk)).unwrap().head_verified,
            "precondition: no head → fail closed");

        s.ensure_audit_anchor_head(999).unwrap();

        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(r.head_verified, "backfilled signed head must verify");
        assert_eq!(r.head_status, "OK");
        // Idempotent: a second call is a no-op and stays clean.
        s.ensure_audit_anchor_head(1000).unwrap();
        assert!(s.verify_audit_chain_full(Some(&vk)).unwrap().head_verified);
    }
}

// --- #87: fabric causal-log forensic chain tests ---------------------------
//
// The KEY WIN over the prior in-memory log: the record hash binds the causality
// edges, so tampering an edge (caused_by / affects_assets / fabric_generation)
// is DETECTED by `verify_causal_chain_integrity`. These tests mutate the chain
// out-of-band via the `raw_conn` seam (what a tamperer with disk access does).
#[cfg(test)]
mod causal_chain_87_tests {
    use crate::verifier_store::*;
    use rusqlite::params;
    use ed25519_dalek::SigningKey;

    fn signed_store() -> (VerifierStore, ed25519_dalek::VerifyingKey) {
        let mut s = VerifierStore::new(":memory:").expect("store");
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();
        s.set_signing_key(sk);
        (s, vk)
    }

    /// Append three causal events signed by the store's key.
    fn append3_causal(s: &mut VerifierStore) {
        let sk = s.signing_key.clone();
        let id1 = "entry-1".to_string();
        s.append_causal_event(&CausalEventInput {
            entry_id: "entry-1", asset_id: "leader", event_type: "FAULT", payload: "{}",
            caused_by: &[], affects_assets: &["follower".to_string()],
            fabric_generation: 1, timestamp_ms: 100,
        }, sk.as_ref()).unwrap();
        s.append_causal_event(&CausalEventInput {
            entry_id: "entry-2", asset_id: "follower", event_type: "DEGRADE", payload: "{}",
            caused_by: std::slice::from_ref(&id1), affects_assets: &[],
            fabric_generation: 1, timestamp_ms: 200,
        }, sk.as_ref()).unwrap();
        s.append_causal_event(&CausalEventInput {
            entry_id: "entry-3", asset_id: "follower", event_type: "STOP", payload: "{}",
            caused_by: &[id1], affects_assets: &["leader".to_string()],
            fabric_generation: 2, timestamp_ms: 300,
        }, sk.as_ref()).unwrap();
    }

    #[test]
    fn clean_signed_causal_chain_verifies() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(r.chain_intact, "chain must be intact");
        assert!(r.signature_valid, "all sigs must verify");
        assert!(r.head_verified, "head must verify: {}", r.head_status);
        assert_eq!(r.head_status, "OK");
        assert_eq!(r.total_entries, 3);
        assert_eq!(r.signed_entries, 3);
    }

    /// KEY WIN: tampering `caused_by` breaks the recomputed record hash.
    #[test]
    fn tampering_caused_by_edge_is_detected() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        // Precondition: clean.
        assert!(s.verify_causal_chain_integrity(Some(&vk)).unwrap().chain_intact);
        // Rewrite the caused_by edge of the middle row.
        s.raw_conn().execute(
            "UPDATE fabric_causal_log SET caused_by = ?1 WHERE entry_id = 'entry-2'",
            params![r#"["forged-cause"]"#],
        ).unwrap();
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(!r.chain_intact, "tampered caused_by edge MUST break chain_intact");
    }

    /// KEY WIN: tampering `affects_assets` breaks the recomputed record hash.
    #[test]
    fn tampering_affects_assets_edge_is_detected() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        assert!(s.verify_causal_chain_integrity(Some(&vk)).unwrap().chain_intact);
        s.raw_conn().execute(
            "UPDATE fabric_causal_log SET affects_assets = ?1 WHERE entry_id = 'entry-1'",
            params![r#"["forged-asset"]"#],
        ).unwrap();
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(!r.chain_intact, "tampered affects_assets edge MUST break chain_intact");
    }

    /// KEY WIN: tampering `fabric_generation` breaks the recomputed record hash.
    #[test]
    fn tampering_fabric_generation_edge_is_detected() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        assert!(s.verify_causal_chain_integrity(Some(&vk)).unwrap().chain_intact);
        s.raw_conn().execute(
            "UPDATE fabric_causal_log SET fabric_generation = 99 WHERE entry_id = 'entry-3'",
            [],
        ).unwrap();
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(!r.chain_intact, "tampered fabric_generation edge MUST break chain_intact");
    }

    /// TRUNCATION: delete the tail row; the surviving prefix is internally
    /// consistent but the signed head still points past it → detected.
    #[test]
    fn truncation_of_causal_tail_is_detected() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        s.raw_conn().execute(
            "DELETE FROM fabric_causal_log WHERE id = (SELECT MAX(id) FROM fabric_causal_log)",
            [],
        ).unwrap();
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(r.chain_intact, "surviving 2-row prefix is still hash-consistent");
        assert!(!r.head_verified, "the signed head must detect the deleted tail");
        assert_eq!(r.head_status, "TRUNCATION_DETECTED");
    }

    /// HEAD SIGNATURE TAMPER: a well-formed but wrong head signature fails closed.
    #[test]
    fn causal_head_signature_tamper_is_detected() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        let bogus = b64e.encode([0u8; 64]);
        s.raw_conn().execute(
            "UPDATE fabric_causal_anchor_head SET signature_b64 = ?1 WHERE id = 1",
            params![bogus],
        ).unwrap();
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(!r.head_verified);
        assert_eq!(r.head_status, "HEAD_SIGNATURE_INVALID");
    }

    /// PERSISTENCE ROUND-TRIP: append on a temp-file DB, drop, reopen, reload.
    #[test]
    fn causal_entries_persist_across_reopen() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kirra_causal_test_{}.sqlite", std::process::id()));
        let path_str = path.to_str().unwrap().to_string();
        // Clean any prior artifact.
        let _ = std::fs::remove_file(&path);

        let entry_id;
        {
            let mut s = VerifierStore::new(&path_str).expect("file store");
            let e = s.append_causal_event(&CausalEventInput {
                entry_id: "persist-1", asset_id: "a", event_type: "EVT", payload: "{}",
                caused_by: &[], affects_assets: &["x".to_string()],
                fabric_generation: 3, timestamp_ms: 1000,
            }, None).unwrap();
            entry_id = e.entry_id.clone();
            s.append_causal_event(&CausalEventInput {
                entry_id: "persist-2", asset_id: "b", event_type: "EVT2", payload: "{}",
                caused_by: std::slice::from_ref(&entry_id), affects_assets: &[],
                fabric_generation: 3, timestamp_ms: 2000,
            }, None).unwrap();
        } // drop closes the connection

        {
            let s = VerifierStore::new(&path_str).expect("reopen file store");
            let rows = s.load_causal_entries().unwrap();
            assert_eq!(rows.len(), 2, "rows must survive reopen");
            assert_eq!(rows[0].entry_id, "persist-1");
            assert_eq!(rows[0].affects_assets, vec!["x".to_string()]);
            assert_eq!(rows[1].caused_by, vec![entry_id]);
            // Chain still verifies after reopen.
            let r = s.verify_causal_chain_integrity(None).unwrap();
            assert!(r.chain_intact && r.head_verified, "reopened chain must verify");
        }

        let _ = std::fs::remove_file(&path);
        // WAL sidecar files.
        let _ = std::fs::remove_file(format!("{path_str}-wal"));
        let _ = std::fs::remove_file(format!("{path_str}-shm"));
    }
}

// ---------------------------------------------------------------------------
// #329 v2 — generation-ordered federation reconciliation, store round-trip.
//
// Proves the persistence half of the v2 wiring: source_generation survives a
// save → load_v2 round-trip, and the typed loader feeds authoritative_posture so
// the higher-generation report wins even when it is the LESS severe posture
// (generation, not severity, is the primary tie-breaker). A v1 report (no
// generation) round-trips as NULL and falls back to timestamp ordering.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod federation_v2_wiring_tests {
    use crate::verifier_store::*;
    use crate::federation_reconciliation::authoritative_posture;
    use crate::verifier::FleetPosture;

    fn rep(nonce: &str, asset: &str, posture: FleetPosture, issued_at_ms: u64) -> FederatedTrustReport {
        FederatedTrustReport {
            source_controller_id: "ctrl-A".to_string(),
            asset_id: asset.to_string(),
            posture,
            issued_at_ms,
            expires_at_ms: issued_at_ms + 30_000,
            nonce_hex: nonce.to_string(),
            signature_b64: "sig".to_string(),
        }
    }

    fn claimed(s: &mut VerifierStore) -> u64 {
        s.try_claim_epoch(0, "self", 0).unwrap().expect("first epoch claim wins")
    }

    #[test]
    fn generation_round_trips_and_drives_authoritative_posture() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        let held = claimed(&mut s);

        // Two reports for the same asset: a LATER-issued Nominal at a LOW generation,
        // and an EARLIER-issued Degraded at a HIGH generation. Pure timestamp ordering
        // would pick the Nominal; generation ordering must pick the Degraded.
        s.save_federated_report_chained(
            &rep("a1", "lidar_front", FleetPosture::Nominal, 2_000), Some(100), 2_100, held,
        ).unwrap();
        s.save_federated_report_chained(
            &rep("b2", "lidar_front", FleetPosture::Degraded, 1_000), Some(412), 1_100, held,
        ).unwrap();

        let v2s = s.load_federated_report_v2s_for_asset("lidar_front").unwrap();
        assert_eq!(v2s.len(), 2, "both reports must be loaded");
        assert!(v2s.iter().any(|r| r.source_generation == Some(100)));
        assert!(v2s.iter().any(|r| r.source_generation == Some(412)));

        // Higher generation (412 → Degraded) is authoritative over the newer-but-lower
        // generation (100 → Nominal).
        assert_eq!(
            authoritative_posture(&v2s), Some(FleetPosture::Degraded),
            "the higher-generation report must win regardless of issue time/severity",
        );

        // The JSON loader also surfaces the generation for API consumers.
        let json_rows = s.load_federated_reports_for_asset("lidar_front").unwrap();
        assert!(json_rows.iter().any(|v| v["source_generation"] == serde_json::json!(412)));
    }

    #[test]
    fn v1_report_round_trips_as_null_generation() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        let held = claimed(&mut s);

        // A v1 report (no generation) persists with NULL source_generation.
        s.save_federated_report_chained(
            &rep("c3", "camera_front", FleetPosture::LockedOut, 1_000), None, 1_100, held,
        ).unwrap();

        let v2s = s.load_federated_report_v2s_for_asset("camera_front").unwrap();
        assert_eq!(v2s.len(), 1);
        assert_eq!(v2s[0].source_generation, None, "a v1 report must load as None generation");
        assert_eq!(authoritative_posture(&v2s), Some(FleetPosture::LockedOut));

        let json_rows = s.load_federated_reports_for_asset("camera_front").unwrap();
        assert_eq!(json_rows[0]["source_generation"], serde_json::Value::Null,
            "a v1 report's generation must serialize as null");
    }
}

// ---------------------------------------------------------------------------
// Industrial-message replay protection — per-source monotonic sequence gate.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod industrial_seq_tests {
    use crate::verifier_store::VerifierStore;

    #[test]
    fn first_message_from_a_source_is_accepted_then_monotonic() {
        let s = VerifierStore::new(":memory:").unwrap();
        // First message establishes the baseline.
        assert_eq!(s.industrial_seq_check_and_advance("plc-a", 10, 1_000), Ok(true));
        // Strictly-greater advances.
        assert_eq!(s.industrial_seq_check_and_advance("plc-a", 11, 1_001), Ok(true));
        assert_eq!(s.industrial_seq_check_and_advance("plc-a", 50, 1_002), Ok(true));
        // Equal = replay; lower = regress — both rejected, mark NOT advanced.
        assert_eq!(s.industrial_seq_check_and_advance("plc-a", 50, 1_003), Ok(false), "equal seq is a replay");
        assert_eq!(s.industrial_seq_check_and_advance("plc-a", 20, 1_004), Ok(false), "lower seq is a regress");
        // After rejects, the mark is still 50 → 51 advances.
        assert_eq!(s.industrial_seq_check_and_advance("plc-a", 51, 1_005), Ok(true));
    }

    #[test]
    fn sources_are_independent() {
        let s = VerifierStore::new(":memory:").unwrap();
        assert_eq!(s.industrial_seq_check_and_advance("plc-a", 100, 1_000), Ok(true));
        // A different source starts fresh at its own baseline.
        assert_eq!(s.industrial_seq_check_and_advance("plc-b", 1, 1_001), Ok(true));
        assert_eq!(s.industrial_seq_check_and_advance("plc-b", 2, 1_002), Ok(true));
        // plc-a's mark (100) is unaffected by plc-b.
        assert_eq!(s.industrial_seq_check_and_advance("plc-a", 100, 1_003), Ok(false));
    }

    #[test]
    fn replayed_sequence_stays_rejected_across_reopen() {
        // Durability: a per-source mark persists, so a replay cannot ride a restart.
        let path = format!(
            "{}/kirra_seq_{}.sqlite",
            std::env::temp_dir().display(),
            std::process::id()
        );
        {
            let s = VerifierStore::new(&path).unwrap();
            assert_eq!(s.industrial_seq_check_and_advance("plc-a", 42, 1_000), Ok(true));
        }
        {
            let s = VerifierStore::new(&path).unwrap();
            // 42 was already seen before the reopen → still a replay.
            assert_eq!(s.industrial_seq_check_and_advance("plc-a", 42, 2_000), Ok(false),
                "a replay must stay rejected across restart");
            assert_eq!(s.industrial_seq_check_and_advance("plc-a", 43, 2_001), Ok(true));
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{path}-wal"));
        let _ = std::fs::remove_file(format!("{path}-shm"));
    }
}
