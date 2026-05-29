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
#[ignore = "TODO(CERT-003): implement test for SG-006"]
fn test_safety_goal_sg_006_unknown_command_denial() {
    // Safety Goal: SG-006 — Unknown Command Denial in All Posture States (ASIL D)
    // This test must verify: should_route_command returns false for
    // OperationalCommand::Unknown unconditionally — before any posture
    // evaluation, in all three posture states (Nominal, Degraded, LockedOut).
    // Currently unimplemented — tracked in CERT-003
    todo!("implement SG-006 verification")
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
#[ignore = "TODO(CERT-003): implement test for SG-010"]
fn test_safety_goal_sg_010_audit_chain_tamper_detection() {
    // Safety Goal: SG-010 — Audit Chain Tamper Detection (ASIL B)
    // This test must verify: AuditChainLinker detects any modification to a
    // previously written entry (prev_hash mismatch), the
    // /system/audit/verify endpoint returns the first tampered index, and
    // verification runs automatically on service startup before the
    // listener binds.
    // Currently unimplemented — tracked in CERT-003
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
#[ignore = "TODO(CERT-003): implement test for SG-013"]
fn test_safety_goal_sg_013_recovery_hysteresis_streak_and_window() {
    // Safety Goal: SG-013 — Recovery Hysteresis Streak and Window Enforcement (ASIL B)
    // This test must verify: evaluate_recovery_report requires exactly
    // AV_RECOVERY_STREAK_THRESHOLD (5) consecutive healthy reports inside a
    // single AV_RECOVERY_WINDOW_MS (10000 ms) window before transitioning
    // a node Untrusted -> Trusted; any gap or unhealthy report resets the
    // streak counter to 0.
    // Currently unimplemented — tracked in CERT-003
    todo!("implement SG-013 verification")
}

#[test]
#[ignore = "TODO(CERT-003): implement test for SG-014"]
fn test_safety_goal_sg_014_federation_report_replay_prevention() {
    // Safety Goal: SG-014 — Federation Report Replay Prevention (ASIL B)
    // This test must verify: reconcile_reports rejects any
    // FederatedTrustReportV2 with generation <= last accepted generation
    // from the same peer controller, nonces are burned in the
    // federation_report_nonces table to prevent replay within
    // FEDERATION_REPLAY_WINDOW_MS (5000 ms), and Ed25519 signatures are
    // verified before acceptance.
    // Currently unimplemented — tracked in CERT-003
    todo!("implement SG-014 verification")
}

#[test]
#[ignore = "TODO(CERT-003): implement test for SG-015"]
fn test_safety_goal_sg_015_admin_token_absent_fail_closed() {
    // Safety Goal: SG-015 — Admin Token Absent Fail-Closed (ASIL B)
    // This test must verify: require_admin_token returns HTTP 503 when
    // KIRRA_ADMIN_TOKEN is absent or empty, all mutation route handlers
    // call require_admin_token, and token comparison uses
    // constant_time_compare (never the `==` operator).
    // Currently unimplemented — tracked in CERT-003
    todo!("implement SG-015 verification")
}

#[test]
#[ignore = "TODO(CERT-003): implement test for SG-016"]
fn test_safety_goal_sg_016_dds_actuator_volatile_durability() {
    // Safety Goal: SG-016 — DDS Actuator Topic Volatile Durability (ASIL C)
    // This test must verify: every DDS actuator topic created in
    // src/dds_bridge.rs uses DurabilityPolicy::Volatile, startup_sentinel
    // aborts on any TransientLocal durability, and the CDR encapsulation
    // logic does not retain a history cache that could replay stale
    // commands to reconnecting subscribers.
    // Currently unimplemented — tracked in CERT-003
    todo!("implement SG-016 verification")
}
