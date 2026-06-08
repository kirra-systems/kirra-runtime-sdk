// CERT-003 — RTM Gap Test Stubs
//
// One #[ignore]'d stub per Safety Goal that currently has zero
// corresponding test coverage in the codebase. The RTM names specific
// tests for each goal but those tests do not exist yet. Stubs here
// preserve the goal-ID-to-test mapping so the gaps are visible to
// `cargo test`, `--list`, and CI without falsifying coverage numbers.
//
// Each stub uses `todo!()` so that removing the `#[ignore]` without
// implementing the body causes an immediate panic rather than a silent
// pass. Tracked in CERT-003 (work/decisions.md ADL-011).
//
// Safety goal definitions: docs/safety/SAFETY_GOALS.md
// Traceability matrix:    docs/safety/REQUIREMENTS_TRACEABILITY.md
// Gap report:             docs/safety/RTM_GAP_REPORT.md

// SG-003 — Sensor Timeout Fault Detection (ASIL D): IMPLEMENTED (CERT-003).
// The RTM-named tests live in `src/telemetry_watchdog.rs` mod `sg_003_cert_tests`
// (test_watchdog_marks_node_untrusted_after_timeout,
//  test_watchdog_detection_latency_within_bound,
//  test_watchdog_triggers_posture_recalculation). They drive `watchdog_sweep_once`,
// which is `pub(crate)`, so they must be in-crate unit tests — not this external
// integration file. The mechanism carries a `// Verifies: SG-003` tag.

#[test]
#[ignore = "Implemented in tests/fault_injection.rs — test_safety_goal_sg_006_unknown_command_denial"]
fn test_safety_goal_sg_006_unknown_command_denial() {
    // See tests/fault_injection.rs for full implementation (CERT-004).
}

// SG-007 — Cross-Asset Fleet Lockout Propagation (ASIL D): the propagation
// SAFETY PROPERTY (leader LockedOut → all Nominal followers Degraded within one
// synchronous fabric pass) is IMPLEMENTED in tests/fault_injection.rs
// (test_safety_goal_sg_007_cross_asset_lockout_propagation); the mechanism
// (FabricRouter::propagate_cross_asset_trust) carries a `// Verifies: SG-007` tag.
//
// REMAINING SUB-GAP — the RTM also names a causal-log assertion, but
// propagate_cross_asset_trust does NOT record propagation events to any causal
// log (FabricCausalLog lives on ServiceState and is not wired into the router).
// Satisfying it requires adding audit/causal-log wiring to the propagation
// mechanism (a code change, not just a test). Kept as an explicit ignored stub
// so the gap stays visible and honest.
#[test]
#[ignore = "TODO(CERT-003): SG-007 propagation→causal-log recording not yet wired (see note above)"]
fn test_safety_goal_sg_007_causal_log_records_propagation_event() {
    todo!("wire propagate_cross_asset_trust to FabricCausalLog, then assert the leader→follower event")
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
#[test]
#[ignore = "Implemented in src/standby_monitor.rs — mod sg_009_promotion_act_tests"]
fn test_safety_goal_sg_009_ha_standby_promotion_within_timeout() {
    // See src/standby_monitor.rs mod sg_009_promotion_act_tests (CERT-003).
}

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
#[test]
#[ignore = "Implemented in src/verifier_store.rs — mod sg_010_audit_tamper_tests (startup-verify sub-gap OPEN, see note)"]
fn test_safety_goal_sg_010_audit_chain_tamper_detection() {
    // See src/verifier_store.rs mod sg_010_audit_tamper_tests (CERT-003).
}

// SG-012 — DNP3 Broadcast Command Mandatory Audit (ASIL B): MECHANISM GAP (OPEN).
//
// Investigated (CERT-003, 2026-06-08): the property is NOT testable today because
// the mechanism does not exist. `src/adapters/dnp3.rs::Dnp3Adapter::evaluate` is a
// PURE classifier — it returns a `Dnp3Evaluation { is_broadcast, is_control, ... }`
// and nothing else. There is:
//   - no audit-chain write on the DNP3 path (broadcast or otherwise),
//   - no control-output application at this layer, hence
//   - no "audit-before-control" ordering and no "block control on audit-write
//     failure" fail-closed path to assert.
// The only caller, `protocol_adapter.rs` (the `/industrial/dnp3/evaluate` route),
// likewise just classifies. Writing a passing test here would assert nothing
// (DO NOT FAKE COVERAGE). Closing SG-012 requires ADDING the mandatory-audit-
// before-control mechanism — a behavior change, out of scope for a test-only
// increment. Reported as an open mechanism gap. See RTM_GAP_REPORT.md (SG-012).
#[test]
#[ignore = "MECHANISM GAP (CERT-003): DNP3 audit-before-control does not exist; see note + RTM_GAP_REPORT.md"]
fn test_safety_goal_sg_012_dnp3_broadcast_mandatory_audit() {
    todo!("SG-012 mechanism (mandatory audit before control output) not implemented — see note above")
}

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
    use kirra_runtime_sdk::recovery_hysteresis::{
        evaluate_recovery_report, HysteresisDecision, AV_RECOVERY_STREAK_THRESHOLD,
        AV_RECOVERY_WINDOW_MS,
    };
    use kirra_runtime_sdk::verifier_store::VerifierStore;

    assert_eq!(AV_RECOVERY_STREAK_THRESHOLD, 5, "spec pins the threshold at 5");

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
                HysteresisDecision::StreakBuilding { current, required, .. } => {
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
                assert_eq!(streak, AV_RECOVERY_STREAK_THRESHOLD,
                    "the 5th in-window healthy report must confirm recovery at streak=5");
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
        store.reset_recovery_streak(node, base + 450).expect("reset on fault");

        let mut last = None;
        for i in 0..4 {
            last = Some(evaluate_recovery_report(&store, node, base + 500 + i * 100)); // streak 1..4
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

#[test]
#[ignore = "Implemented in tests/fault_injection.rs — test_safety_goal_sg_014_federation_report_replay_prevention"]
fn test_safety_goal_sg_014_federation_report_replay_prevention() {
    // See tests/fault_injection.rs for full implementation (CERT-004).
    // Note: nonce-burn replay prevention (persistence-layer) remains
    // out of scope for the in-memory unit; covered separately by an
    // integration test against VerifierStore in a future increment.
}

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
#[test]
#[ignore = "Implemented in src/security.rs — mod sg_015_admin_token_tests"]
fn test_safety_goal_sg_015_admin_token_absent_fail_closed() {
    // See src/security.rs mod sg_015_admin_token_tests (CERT-003).
}

#[test]
#[ignore = "Implemented in tests/fault_injection.rs — test_safety_goal_sg_016_dds_actuator_volatile_durability"]
fn test_safety_goal_sg_016_dds_actuator_volatile_durability() {
    // See tests/fault_injection.rs for full implementation (CERT-004).
}
