// CERT-003 — RTM Coverage Reconciliation
//
// This file originally held `#[ignore]`/`todo!()` placeholders for RTM entries
// whose named tests did not exist. Those placeholders are gone for implemented
// goals: either the evidence lives here (when the public API is reachable from
// an integration test) or the comment below points to the in-crate / binary test
// that exercises a private seam. Do not add a placeholder for an implemented
// goal — add or reference real executable evidence instead. If a mechanism is
// genuinely missing, keep it documented in the RTM gap report rather than
// falsifying coverage with a passing stub.
//
// Safety goal definitions: docs/safety/SAFETY_GOALS.md
// Traceability matrix:    docs/safety/REQUIREMENTS_TRACEABILITY.md
// Historical gap report:  docs/safety/RTM_GAP_REPORT.md

// SG-003 — Sensor Timeout Fault Detection (ASIL D): IMPLEMENTED (CERT-003).
// The RTM-named tests live in `src/telemetry_watchdog.rs` mod `sg_003_cert_tests`
// (test_watchdog_marks_node_untrusted_after_timeout,
//  test_watchdog_detection_latency_within_bound,
//  test_watchdog_triggers_posture_recalculation). They drive `watchdog_sweep_once`,
// which is `pub(crate)`, so they must be in-crate unit tests — not this external
// integration file. The mechanism carries a `// Verifies: SG-003` tag.

// SG-006 — Unknown Command Denial in All Posture States (ASIL D): IMPLEMENTED.
// Evidence lives in `tests/fault_injection.rs`:
// `test_safety_goal_sg_006_unknown_command_denial`.

// SG-007 — Cross-Asset Fleet Lockout Propagation (ASIL D): CLOSED.
// The propagation SAFETY PROPERTY (leader LockedOut → all Nominal followers
// Degraded within one synchronous fabric pass) is in tests/fault_injection.rs
// (test_safety_goal_sg_007_cross_asset_lockout_propagation). The previously-open
// causal-log sub-gap is now closed: FabricRouter::propagate_and_record records a
// `cross_asset_trust_degrade` event per rule-firing into the FabricCausalLog
// (the propagation DECISIONS are byte-identical to propagate_cross_asset_trust).
// The test below asserts BOTH the decision and the recorded causal event.
#[test]
fn test_safety_goal_sg_007_causal_log_records_propagation_event() {
    use kirra_fabric_types::asset::{AssetPosture, AssetType, FabricAsset, KinematicProfileType};
    use kirra_verifier::fabric::causal_log::FabricCausalLog;
    use kirra_verifier::fabric::router::FabricRouter;
    use kirra_verifier::verifier::FleetPosture;
    use std::collections::HashMap;

    fn convoy_av(id: &str, role: &str) -> FabricAsset {
        let mut metadata = HashMap::new();
        metadata.insert("convoy_role".to_string(), role.to_string());
        FabricAsset {
            asset_id: id.to_string(),
            asset_type: AssetType::AutonomousVehicle,
            display_name: id.to_string(),
            kinematic_profile: KinematicProfileType::AutomotiveNominal,
            registered_at_ms: 1000,
            last_seen_ms: 1000,
            metadata,
        }
    }
    fn posture(id: &str, p: FleetPosture, gen: u64) -> AssetPosture {
        AssetPosture {
            asset_id: id.to_string(),
            posture: p,
            generation: gen,
            computed_at_ms: 1000,
            contributing_nodes: vec![],
            blocked_by: vec![],
        }
    }

    let router = FabricRouter::new();
    router.register_asset(&convoy_av("leader01", "leader"));
    router.register_asset(&convoy_av("follower01", "follower"));
    // Registration seeds Degraded; lift the follower to Nominal so the rule has
    // a transition to make, then lock out the leader.
    router.update_asset_posture(
        "follower01",
        posture("follower01", FleetPosture::Nominal, 1),
    );
    router.update_asset_posture("leader01", posture("leader01", FleetPosture::LockedOut, 2));

    let log = FabricCausalLog::new_in_memory(None);
    let fabric_generation = router.fabric_state().fabric_generation;
    let changes = router.propagate_and_record(&log, fabric_generation);

    // (a) decision unchanged: the LockedOut leader degrades the Nominal follower.
    assert!(
        changes.iter().any(|(id, p)| id == "follower01" && *p == FleetPosture::Degraded),
        "leader LockedOut must degrade the Nominal follower (propagation decision); changes={changes:?}"
    );

    // (b) the causal log recorded the propagation: asset_id = the LockedOut
    // leader, affects_assets containing the degraded follower.
    let entries = log.export(0, u64::MAX);
    assert!(
        entries.iter().any(|e| e.asset_id == "leader01"
            && e.event_type == "cross_asset_trust_degrade"
            && e.affects_assets.iter().any(|a| a == "follower01")
            && e.fabric_generation == fabric_generation),
        "FabricCausalLog must record the leader→follower propagation event \
         (asset_id=leader01, affects=follower01); entries={entries:?}"
    );
}

// SG-008 — Process Fail-Closed on Startup (ASIL D): IMPLEMENTED (CERT-003).
// The startup-invariant predicate `check_startup_invariants` + its
// `StartupContext` / `StartupInvariant` live in
// `src/bin/kirra_verifier_service.rs` and are tested by mod `sg_008_cert_tests`
// there (admin-token / WAL / watchdog / posture-engine violations, the
// all-present Ok case, the Active-vs-PassiveStandby distinction, and check-order
// stability). They are `pub(crate)` in the binary, so the tests must be in-bin —
// not in this external integration file. `main` evaluates the predicate
// immediately before `TcpListener::bind` and aborts on Err, so the listener
// never binds before invariants pass. The predicate carries `// Verifies: SG-008`.

// SG-009 — HA Standby Promotion Within PROMOTION_TIMEOUT_MS (ASIL B): IMPLEMENTED.
// `spawn_promotion_monitor`'s spawned task uses wall-clock time, so the per-poll
// promotion DECISION is extracted into `promotion_decision` (the real gate the
// loop calls) and the promotion ACT is `perform_promotion` — both module-private
// in `src/standby_monitor.rs`, hence tested IN-CRATE in mod `sg_009_promotion_act_tests`
// (decision: stale→promote / fresh→hold / inclusive boundary / clock-skew-safe;
// ACT: mode_active false→true, durable promotion record + audit event, posture
// recalc populates the cache). `promotion_decision` carries `// Verifies: SG-009`.

// SG-009 executable evidence lives in `src/standby_monitor.rs`
// `sg_009_promotion_act_tests`.
// SG-010 — Audit Chain Tamper Detection (ASIL B): tamper-detection IMPLEMENTED;
// startup-verification sub-gap OPEN.
//
// Tamper detection is verified IN-CRATE in `src/verifier_store.rs` mod
// `sg_010_audit_tamper_tests` (file-backed tempfile DB + the `#[cfg(test)]
// pub(crate) raw_conn` seam): a second connection back-dates a previously
// written row and `verify_audit_chain_full` reports `chain_intact == false` and
// `first_invalid_signature_index == Some(<first tampered index>)`; an unsigned
// chain is still caught by the hash linkage alone. In-crate because `raw_conn`
// is `#[cfg(test)] pub(crate)` (invisible to this external crate).
//
// OPEN sub-gap (mechanism does not exist — reported, not faked): SG-010 also
// requires audit-chain verification to run AUTOMATICALLY at startup BEFORE the
// listener binds. Today the bin runs only `check_startup_invariants` before
// `TcpListener::bind` and verifies the chain on demand via `/system/audit/verify`
// (plus a shutdown checkpoint). Wiring verify-and-abort into startup is a
// behavior change, out of scope for a test-only increment.
// SG-012 — DNP3 Broadcast Command Mandatory Audit (ASIL B): IMPLEMENTED.
// The dedicated DNP3 handler and the unified industrial path now both write a
// tamper-evident audit record before returning an admitted broadcast verdict, and
// block a broadcast if that mandatory audit write is unavailable. Evidence lives
// in `src/bin/kirra_verifier_service.rs`:
//   - `sg_012_dnp3_broadcast_audit_tests::test_dnp3_broadcast_always_audited`
//   - `sg_012_dnp3_broadcast_audit_tests::test_store_recovers_after_poison_broadcast_still_evaluates`
// plus the unified-path tests in `src/bin/kirra_verifier_service/industrial.rs`
// and `protocol_adapter::unified_tests`.

// SG-013 — Recovery Hysteresis Streak and Window Enforcement (ASIL B): IMPLEMENTED
// here (external) — the whole closure is reachable through the PUBLIC API
// (`evaluate_recovery_report`, `HysteresisDecision`, the `AV_RECOVERY_*`
// constants, and `VerifierStore::{new,register_av_subsystem_meta,
// reset_recovery_streak}`), so per the placement rule it stays in this external
// crate rather than moving in-crate.
//
// `evaluate_recovery_report` takes `now_ms` directly, so time is injected
// deterministically (the VirtualClock principle: controlled timestamps, no wall
// clock). The "injected unhealthy report" is driven through the EXACT store
// mutation the production unhealthy path performs — `reset_recovery_streak`
// (see the degraded branch of `handle_sensor_fault_report`) — not a test-only
// backdoor.
#[test]
fn test_safety_goal_sg_013_recovery_hysteresis_streak_and_window() {
    use kirra_persistence::VerifierStore;
    use kirra_verifier::recovery_hysteresis::{
        evaluate_recovery_report, HysteresisDecision, AV_RECOVERY_STREAK_THRESHOLD,
        AV_RECOVERY_WINDOW_MS,
    };

    assert_eq!(
        AV_RECOVERY_STREAK_THRESHOLD, 5,
        "spec pins the threshold at 5"
    );

    // Fresh in-memory store with one registered AV node (single connection ⇒
    // streak increments/loads are self-consistent).
    fn store_with_node(node: &str) -> VerifierStore {
        let store = VerifierStore::new(":memory:").expect("memory store");
        store
            .register_av_subsystem_meta(node, "LIDAR", "hw-0001", 0.7, 0)
            .expect("register subsystem");
        store
    }

    // --- Scenarios a + b: streak builds to 4, then the 5th in-window confirms.
    {
        let node = "lidar_a";
        let store = store_with_node(node);
        let base = 1_000u64;
        for i in 0..(AV_RECOVERY_STREAK_THRESHOLD - 1) {
            let d = evaluate_recovery_report(&store, node, base + (i as u64) * 100);
            match d {
                HysteresisDecision::StreakBuilding {
                    current, required, ..
                } => {
                    assert_eq!(current, i + 1, "streak must advance one per healthy report");
                    assert_eq!(required, AV_RECOVERY_STREAK_THRESHOLD);
                }
                other => panic!("report {} must be StreakBuilding, got {other:?}", i + 1),
            }
        }
        // (a) after exactly 4 healthy reports the streak is StreakBuilding{4}:
        // the 4th call above asserted current == 4. (b) the 5th in-window confirms.
        let fifth = evaluate_recovery_report(&store, node, base + 400);
        match fifth {
            HysteresisDecision::RecoveryConfirmed { streak } => {
                assert_eq!(
                    streak, AV_RECOVERY_STREAK_THRESHOLD,
                    "the 5th in-window healthy report must confirm recovery at streak=5"
                );
            }
            other => panic!("5th in-window report must be RecoveryConfirmed, got {other:?}"),
        }
    }

    // --- Scenario c: a gap longer than the window resets the streak; the second
    //     run of 4 reports never reaches confirmation.
    {
        let node = "lidar_c";
        let store = store_with_node(node);
        let base = 1_000u64;
        for i in 0..4 {
            evaluate_recovery_report(&store, node, base + i * 100); // streak 1..4, start=base
        }
        // First report AFTER an > AV_RECOVERY_WINDOW_MS gap → window expired.
        let after_gap = base + AV_RECOVERY_WINDOW_MS + 1_000; // 11s after streak start
        let expired = evaluate_recovery_report(&store, node, after_gap);
        match expired {
            HysteresisDecision::WindowExpired { old_streak } => {
                assert_eq!(old_streak, 4, "the stale 4-streak must be discarded");
            }
            other => panic!("post-gap report must be WindowExpired, got {other:?}"),
        }
        // Three more in the fresh window → back up to 4, never 5 → no confirm.
        let mut last = None;
        for i in 1..4 {
            last = Some(evaluate_recovery_report(&store, node, after_gap + i * 100));
        }
        match last.unwrap() {
            HysteresisDecision::StreakBuilding { current, .. } => {
                assert_eq!(current, 4,
                    "after a window reset the streak rebuilds to 4 (8 total reports), not confirmed");
            }
            other => panic!("a gap must prevent confirmation; got {other:?}"),
        }
    }

    // --- Scenario d: an injected unhealthy report (production reset path) resets
    //     the streak; the following 4 healthy reports never confirm.
    {
        let node = "lidar_d";
        let store = store_with_node(node);
        let base = 1_000u64;
        for i in 0..4 {
            evaluate_recovery_report(&store, node, base + i * 100); // streak 1..4
        }
        // Unhealthy report ⇒ the degraded branch calls reset_recovery_streak.
        store
            .reset_recovery_streak(node, base + 450)
            .expect("reset on fault");

        let mut last = None;
        for i in 0..4 {
            last = Some(evaluate_recovery_report(&store, node, base + 500 + i * 100));
            // streak 1..4
        }
        match last.unwrap() {
            HysteresisDecision::StreakBuilding { current, .. } => {
                assert_eq!(current, 4,
                    "an unhealthy report resets the streak to 0; 4 fresh reports rebuild to 4, not confirmed");
            }
            other => panic!("an unhealthy report must prevent confirmation; got {other:?}"),
        }
    }
}

// SG-014 — Federation Report Replay Prevention (ASIL B): IMPLEMENTED.
// Evidence lives in `tests/fault_injection.rs`
// (`test_safety_goal_sg_014_federation_report_replay_prevention`) and the
// VerifierStore/fleet-transport nonce-burn tests.

// SG-015 — Admin Token Absent Fail-Closed (ASIL B): IMPLEMENTED.
// The env-var check is factored out of the `require_admin_token` middleware into
// the pure `security::admin_token_ok(provided, configured)` (which uses
// `constant_time_compare`, never `==`), so the fail-closed truth table is tested
// without mutating process env vars (forbidden in the multithreaded runner —
// INVARIANT #13). Tests live IN-CRATE in `src/security.rs` mod
// `sg_015_admin_token_tests` (absent/empty configured → deny → caller maps to
// 503; absent/mismatched provided → deny → 401; exact token → allow). The
// middleware still maps configured-absent/empty → 503 and provided-absent/
// mismatch → 401 (behavior unchanged); it now calls `admin_token_ok` for the
// comparison. `admin_token_ok` carries `// Verifies: SG-015`.
// SG-015 executable evidence lives in `src/security.rs`
// `sg_015_admin_token_tests`.

// SG-016 executable evidence lives in `tests/fault_injection.rs`:
// `test_safety_goal_sg_016_dds_actuator_volatile_durability`.
