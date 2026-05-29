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

#[test]
#[ignore = "TODO(CERT-003): implement test for SG-003"]
fn test_safety_goal_sg_003_sensor_timeout_fault_detection() {
    // Safety Goal: SG-003 — Sensor Timeout Fault Detection (ASIL D)
    // This test must verify: spawn_telemetry_watchdog marks any sensor
    // node Untrusted within AV_TELEMETRY_TIMEOUT_MS (2000 ms) +
    // AV_WATCHDOG_SWEEP_MS (100 ms) of last telemetry, and sends a
    // PostureRecalcTrigger within the same sweep cycle.
    // Currently unimplemented — tracked in CERT-003
    todo!("implement SG-003 verification")
}

#[test]
#[ignore = "Implemented in tests/fault_injection.rs — test_safety_goal_sg_006_unknown_command_denial"]
fn test_safety_goal_sg_006_unknown_command_denial() {
    // See tests/fault_injection.rs for full implementation (CERT-004).
}

#[test]
#[ignore = "TODO(CERT-003): implement test for SG-007"]
fn test_safety_goal_sg_007_cross_asset_lockout_propagation() {
    // Safety Goal: SG-007 — Cross-Asset Fleet Lockout Propagation (ASIL D)
    // This test must verify: when a leader asset transitions to LockedOut,
    // propagate_cross_asset_trust degrades all follower assets within one
    // fabric governor tick (target <= 500 ms), and the propagation event
    // is recorded in the fabric causal log.
    // Currently unimplemented — tracked in CERT-003
    todo!("implement SG-007 verification")
}

#[test]
#[ignore = "TODO(CERT-003): implement test for SG-008"]
fn test_safety_goal_sg_008_process_fail_closed_on_crash() {
    // Safety Goal: SG-008 — Process Fail-Closed on Crash (ASIL D)
    // This test must verify: startup_sentinel aborts the process if any
    // safety invariant fails (admin token missing, watchdog not spawned,
    // posture engine not running, SQLite not in WAL mode), and the TCP
    // listener never binds before all invariants pass.
    // Currently unimplemented — tracked in CERT-003
    todo!("implement SG-008 verification")
}

#[test]
#[ignore = "TODO(CERT-003): implement test for SG-009"]
fn test_safety_goal_sg_009_ha_standby_promotion_within_timeout() {
    // Safety Goal: SG-009 — HA Standby Promotion Within PROMOTION_TIMEOUT_MS (ASIL B)
    // This test must verify: spawn_promotion_monitor promotes the standby
    // (mode_active.compare_exchange(false, true, ...)) within
    // PROMOTION_TIMEOUT_MS (10000 ms) of last heartbeat from the primary,
    // and the promoted instance begins writing heartbeats and recalculating
    // posture immediately.
    // Currently unimplemented — tracked in CERT-003
    todo!("implement SG-009 verification")
}

#[test]
#[ignore = "TODO(CERT-004): SG-010 needs file-backed DB; in-memory store is per-connection"]
fn test_safety_goal_sg_010_audit_chain_tamper_detection() {
    // Safety Goal: SG-010 — Audit Chain Tamper Detection (ASIL B)
    // This test must verify: AuditChainLinker detects any modification to a
    // previously written entry (prev_hash mismatch), the
    // /system/audit/verify endpoint returns the first tampered index, and
    // verification runs automatically on service startup before the
    // listener binds.
    //
    // INFRASTRUCTURE NEEDED:
    // - File-backed SQLite (not `:memory:`) so a second connection can
    //   tamper rows after the first connection wrote them.
    //   `:memory:` databases are per-connection — a tamper-via-second-
    //   connection approach is not viable against in-memory stores.
    //   Alternative: add a `#[cfg(test)] pub fn raw_conn(&mut self) -> &mut Connection`
    //   helper to VerifierStore that exposes the connection for tampering.
    // - tempfile crate (or std::env::temp_dir + std::fs::remove_file) for
    //   the DB path; add to dev-dependencies in a follow-up.
    // - Use verifier_store::VerifierStore::verify_audit_chain_full(None)
    //   to assert chain_intact == false after tampering.
    todo!("implement SG-010 verification")
}

#[test]
#[ignore = "TODO(CERT-003): implement test for SG-012"]
fn test_safety_goal_sg_012_dnp3_broadcast_mandatory_audit() {
    // Safety Goal: SG-012 — DNP3 Broadcast Command Mandatory Audit (ASIL B)
    // This test must verify: the DNP3 adapter writes an audit chain entry
    // for every message to DNP3_BROADCAST_ADDRESS *before* any control
    // output is applied, and an audit write failure on a broadcast command
    // blocks the control output (fail-closed audit ordering).
    // Currently unimplemented — tracked in CERT-003
    todo!("implement SG-012 verification")
}

#[test]
#[ignore = "TODO(CERT-004): SG-013 needs VerifierStore + multi-tick driver with controlled clock"]
fn test_safety_goal_sg_013_recovery_hysteresis_streak_and_window() {
    // Safety Goal: SG-013 — Recovery Hysteresis Streak and Window Enforcement (ASIL B)
    // This test must verify: evaluate_recovery_report requires exactly
    // AV_RECOVERY_STREAK_THRESHOLD (5) consecutive healthy reports inside a
    // single AV_RECOVERY_WINDOW_MS (10000 ms) window before transitioning
    // a node Untrusted -> Trusted; any gap or unhealthy report resets the
    // streak counter to 0.
    //
    // INFRASTRUCTURE NEEDED:
    // - `evaluate_recovery_report(store, node_id, now_ms)` loads streak
    //   state from VerifierStore — it does not take an "is_healthy" arg,
    //   so driving the unhealthy/streak-reset path requires a separate
    //   API call (or direct streak-table mutation) we don't yet expose.
    // - Test scenarios to cover:
    //     a. 4 calls within window → StreakBuilding{current:4}
    //     b. 5th call within window → RecoveryConfirmed{streak:5}
    //     c. 4 calls + 11s gap + 4 calls → still StreakBuilding (window reset)
    //     d. 4 calls + injected unhealthy report + 4 calls → still StreakBuilding
    // - Currently no public "report unhealthy" entry point; need either
    //   a new public fn or expose store streak helpers under #[cfg(test)].
    // - Should reuse the temporal_scenario_tests.rs VirtualClock pattern.
    todo!("implement SG-013 verification")
}

#[test]
#[ignore = "Implemented in tests/fault_injection.rs — test_safety_goal_sg_014_federation_report_replay_prevention"]
fn test_safety_goal_sg_014_federation_report_replay_prevention() {
    // See tests/fault_injection.rs for full implementation (CERT-004).
    // Note: nonce-burn replay prevention (persistence-layer) remains
    // out of scope for the in-memory unit; covered separately by an
    // integration test against VerifierStore in a future increment.
}

#[test]
#[ignore = "TODO(CERT-004): SG-015 needs subprocess isolation (env-var manipulation is unsafe in Rust 1.94+)"]
fn test_safety_goal_sg_015_admin_token_absent_fail_closed() {
    // Safety Goal: SG-015 — Admin Token Absent Fail-Closed (ASIL B)
    // This test must verify: require_admin_token returns HTTP 503 when
    // KIRRA_ADMIN_TOKEN is absent or empty, all mutation route handlers
    // call require_admin_token, and token comparison uses
    // constant_time_compare (never the `==` operator).
    //
    // INFRASTRUCTURE NEEDED:
    // - `std::env::set_var` and `remove_var` became `unsafe` in Rust 1.80+
    //   and CRITICAL SECURITY INVARIANT #13 forbids env-var mutation in
    //   any multithreaded context. The default `cargo test` runner is
    //   multithreaded, so this test must either:
    //     a. Spawn a subprocess via `std::process::Command` invoking a
    //        helper binary that sets/clears KIRRA_ADMIN_TOKEN and runs
    //        the assertion in its own process address space, OR
    //     b. Use the `serial_test` crate (dev-dependency add — currently
    //        not allowed by the scope of this commit) to serialize.
    // - `require_admin_token` is an axum middleware fn taking
    //   `(Request, Next) -> Result<Response, StatusCode>`. The test must
    //   either construct a minimal axum Router with the middleware applied
    //   and exercise it via `tower::ServiceExt::oneshot`, or extract the
    //   env-var check pattern into a smaller `pub(crate) fn` that's
    //   testable in isolation.
    // - Both routes (e.g. `/system/backup/export`) must round-trip 503
    //   when KIRRA_ADMIN_TOKEN is absent.
    todo!("implement SG-015 verification")
}

#[test]
#[ignore = "Implemented in tests/fault_injection.rs — test_safety_goal_sg_016_dds_actuator_volatile_durability"]
fn test_safety_goal_sg_016_dds_actuator_volatile_durability() {
    // See tests/fault_injection.rs for full implementation (CERT-004).
}
