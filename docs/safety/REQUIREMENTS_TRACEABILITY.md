# Aegis Safety Kernel — Requirements Traceability Matrix

Document ID: AEGIS-RTM-001
Version: 1.0.0
Status: Draft
Classification: ISO 26262 Part 4 / Part 8
Date: 2026-05-23

---

## 1. Overview

This Requirements Traceability Matrix (RTM) links each Safety Goal (AEGIS-SG-001) to one or more Technical Requirements (TR), the precise implementation location within the `aegis-runtime-sdk` crate, and the test(s) that demonstrate compliance. All 16 safety goals are covered.

Traceability flows in both directions:
- **Forward:** Safety Goal -> Technical Requirement -> Implementation -> Test
- **Backward:** Test -> Implementation -> Technical Requirement -> Safety Goal

---

## 2. Traceability Matrix

### SG-001 — Velocity Envelope Enforcement (ASIL D)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-001 | `validate_vehicle_command` shall return `ClampLinear(max_speed_mps)` when `linear_velocity_mps.abs() > max_speed_mps` for the active kinematic contract | `src/gateway/kinematics_contract.rs:validate_vehicle_command` (Priority 2) | `test_speed_above_ceiling_triggers_clamp_linear` ✓ |
| TR-001a | The hard velocity clamp (Priority 2) shall execute before the rate-of-change limiter in `AegisKernelGovernor`; no ordering inversion shall be permitted | `src/gateway/kinematics_contract.rs` Priority 2 precedes `src/aegis_core.rs` rate limiter | Code inspection + proptest kinematics suite |
| TR-001b | `validate_vehicle_command` shall return `ClampLinear(0.0)` when the kinematic contract requires a stopped state (zero-velocity fence, Priority 1) | `src/gateway/kinematics_contract.rs:validate_vehicle_command` (Priority 1) | `test_zero_velocity_fence_enforced` ✗ |

---

### SG-002 — Lateral Acceleration Envelope Enforcement (ASIL D)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-002 | `validate_vehicle_command` shall compute the bicycle model lateral acceleration as `linear_velocity_mps^2 / turn_radius_m` and shall return `ClampSteering` when the result exceeds `max_lateral_accel_mps2` | `src/gateway/kinematics_contract.rs:validate_vehicle_command` (Priority 6) | `test_nominal_highway_speed_high_steering_clamps_steering` ✓ |
| TR-002a | `validate_vehicle_command` shall return `ClampAngular(max_yaw_rate_radps)` when `angular_velocity_radps.abs() > max_yaw_rate_radps` | `src/gateway/kinematics_contract.rs:validate_vehicle_command` (Priority 4) | `test_yaw_rate_above_ceiling_triggers_clamp_angular` ✗ |
| TR-002b | The kinematic forward simulator (`VehicleState`, `apply_enforcement`, `run_simulation`) shall validate the post-clamp trajectory for lateral acceleration constraint satisfaction | `src/kinematics_sim.rs:apply_enforcement`, `src/kinematics_sim.rs:run_simulation` | `test_forward_sim_confirms_lateral_accel_constraint` ✗ |

---

### SG-003 — Sensor Timeout Fault Detection (ASIL D)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-003 | `spawn_telemetry_watchdog` shall detect absence of telemetry for any registered AV sensor node when the elapsed time since last telemetry exceeds `AV_TELEMETRY_TIMEOUT_MS = 2000` ms and shall transition that node to `Untrusted` | `src/telemetry_watchdog.rs:spawn_telemetry_watchdog` | `test_watchdog_marks_node_untrusted_after_timeout` ✗ |
| TR-003a | The watchdog sweep interval shall be `AV_WATCHDOG_SWEEP_MS = 100` ms; the maximum detection latency shall not exceed `AV_TELEMETRY_TIMEOUT_MS + AV_WATCHDOG_SWEEP_MS = 2100` ms | `src/telemetry_watchdog.rs` — constant `AV_WATCHDOG_SWEEP_MS` | `test_watchdog_detection_latency_within_bound` ✗ |
| TR-003b | After the watchdog transitions a node to `Untrusted`, it shall send a `PostureRecalcTrigger` to the posture engine worker channel within the same sweep cycle | `src/telemetry_watchdog.rs` trigger send | `test_watchdog_triggers_posture_recalculation` ✗ |

---

### SG-004 — NaN and Inf Rejection (ASIL C)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-004 | `validate_vehicle_command` shall check all f64 fields of the vehicle command for `!f64::is_finite()` as the first check (Priority 0) before any arithmetic operation | `src/gateway/kinematics_contract.rs:validate_vehicle_command` (Priority 0) | `test_nan_linear_velocity_is_denied` ✗, `test_inf_linear_velocity_is_denied` ✓ |
| TR-004a | The `ros2_adapter` shall reject any command with a non-finite f64 field before publishing to the ROS2 topic | `src/ros2_adapter.rs` — NaN/Inf check | `test_ros2_adapter_rejects_nan_command` ✗ |
| TR-004b | The proptest suite shall generate commands with arbitrary f64 values including `f64::NAN`, `f64::INFINITY`, and `f64::NEG_INFINITY` and verify that all such commands are denied at Priority 0 | `src/gateway/kinematics_proptest.rs` | `proptest_nan_inf_always_denied` ✗ |

---

### SG-005 — Posture Cache Staleness Fail-Closed (ASIL D)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-005 | `resolve_posture_with_reason` shall return `LockedOut(PostureCacheStale)` when `now_ms - cache.generated_at_ms >= POSTURE_CACHE_TTL_MS` where `POSTURE_CACHE_TTL_MS = 5000` | `src/posture_engine_v2.rs:resolve_posture_with_reason` | `test_stale_cache_fails_closed_after_virtual_clock_advance` ✓ |
| TR-005a | `should_route_command` shall return `false` when the cache staleness condition is met, before evaluating the posture state | `src/posture_cache.rs:should_route_command` (staleness check step 2) | `test_stale_cache_blocks_all_commands` ✗ |
| TR-005b | The `SharedPostureCache` TTL value `POSTURE_CACHE_TTL_MS` shall be a compile-time constant and shall not be overridable at runtime without an explicit configuration update | `src/posture_cache.rs` — `POSTURE_CACHE_TTL_MS = 5_000` constant | Code inspection |

---

### SG-006 — Unknown Command Denial in All Posture States (ASIL D)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-006 | `should_route_command` shall return `false` for `OperationalCommand::Unknown` as an unconditional early return before any posture evaluation | `src/posture_cache.rs:should_route_command` (Unknown early return, step 1) | `test_unknown_command_denied_in_all_posture_states` ✗ |
| TR-006a | The `AegisPolicyLayer` Tower middleware shall map any HTTP request that does not match a known path+method combination to `OperationalCommand::Unknown` via `classify_command` | `src/gateway/policy.rs:classify_command`, `src/gateway/policy_layer.rs:AegisPolicyLayer` | `test_unrecognized_path_classified_as_unknown` ✗ |
| TR-006b | The `ActionFilter` shall deny any `ActionClaim` that maps to `OperationalCommand::Unknown` regardless of the current fleet posture | `src/action_filter.rs:ActionFilter` | `test_action_filter_denies_unknown_action_type` ✗ |

---

### SG-007 — Cross-Asset Fleet Lockout Propagation (ASIL D)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-007 | `propagate_cross_asset_trust` shall set the posture of all follower assets registered to a leader asset to `Degraded` when the leader asset's posture is `LockedOut` | `src/fabric/router.rs:propagate_cross_asset_trust` (Rule 1) | `test_convoy_leader_lockout_degrades_followers` ✗ |
| TR-007a | `propagate_cross_asset_trust` shall set follower asset postures to `Degraded` when the leader asset posture is `Degraded` and all followers are currently `Nominal` | `src/fabric/router.rs:propagate_cross_asset_trust` (Rule 2) | `test_convoy_leader_degraded_degrades_nominal_followers` ✗ |
| TR-007b | The fabric causal log (`src/fabric/causal_log.rs`) shall record every cross-asset trust propagation event with a causal timestamp | `src/fabric/causal_log.rs` | `test_causal_log_records_propagation_event` ✗ |

---

### SG-008 — Process Fail-Closed on Crash (ASIL D)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-008 | `startup_sentinel` shall verify all safety invariants (admin token present, watchdog spawned, posture engine running, SQLite WAL mode active) before the TCP listener binds; on any invariant failure, the process shall abort | `src/startup_sentinel.rs`, `src/bin/aegis_verifier_service.rs` | Integration smoke test, `test_startup_aborts_without_admin_token` ✗ |
| TR-008a | The `aegis_verifier_service` binary shall bind the TCP listener only after `startup_sentinel` succeeds; no route shall be served before all invariants pass | `src/bin/aegis_verifier_service.rs` — listener bind ordering | Integration test confirming no response before startup_sentinel completes |
| TR-008b | The `AegisPolicyLayer` Tower middleware shall be applied to the router at construction time; there shall be no code path that serves a request without passing through the middleware | `src/gateway/policy_layer.rs:AegisPolicyLayer`, `src/bin/aegis_verifier_service.rs` | Integration test confirming all routes pass through middleware |

---

### SG-009 — HA Standby Promotion Within PROMOTION_TIMEOUT_MS (ASIL B)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-009 | `spawn_promotion_monitor` shall call `mode_active.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)` when the primary heartbeat timestamp age exceeds `PROMOTION_TIMEOUT_MS = 10000` ms | `src/standby_monitor.rs:spawn_promotion_monitor` | `test_standby_promotes_after_primary_timeout` ✗ |
| TR-009a | `spawn_heartbeat_writer` shall write the current timestamp to the `posture_engine_state` table at intervals not exceeding `HEARTBEAT_INTERVAL_MS = 2000` ms | `src/standby_monitor.rs:spawn_heartbeat_writer` | `test_heartbeat_written_within_interval` ✗ |
| TR-009b | After promotion, the newly active instance shall immediately begin writing heartbeats and shall start a new posture recalculation cycle | `src/standby_monitor.rs` post-promotion logic | `test_promoted_instance_begins_heartbeat_and_recalculation` ✗ |

---

### SG-010 — Audit Chain Tamper Detection (ASIL B)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-010 | `AuditChainLinker` shall reject (detect as tampered) any entry where `SHA-256(serialized(previous_entry))` does not equal the `prev_hash` field of the current entry | `src/audit_chain.rs:AuditChainLinker` | `test_audit_chain_tamper_detection` ✗ |
| TR-010a | The `/system/audit/verify` endpoint shall perform a full chain verification and return a structured result indicating the first tampered entry index and its computed vs. expected hash | `src/bin/aegis_verifier_service.rs` — audit verify route handler | `test_audit_verify_endpoint_detects_corruption` ✗ |
| TR-010b | Audit chain verification shall be performed automatically on service startup before the TCP listener binds | `src/startup_sentinel.rs` or `src/bin/aegis_verifier_service.rs` startup sequence | Integration test confirming startup verification |

---

### SG-011 — CANOpen NMT State Change Triggers Posture Recalculation (ASIL C)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-011 | The CANOpen protocol adapter shall set `triggers_recalculation = true` when the NMT command byte (`data[0]`) is `0x02` (Stop), `0x80` (Pre-Operational), `0x81` (Reset Node), or `0x82` (Reset Communication) | `src/adapters/canopen.rs:CanOpenAdapter::evaluate` | `test_canopen_nmt_stop_triggers_posture_recalculation` ✓ |
| TR-011a | NMT commands with other `data[0]` values (e.g., `0x01` Start) shall not set `triggers_recalculation = true` | `src/adapters/canopen.rs:CanOpenAdapter::evaluate` | `test_canopen_nmt_start_does_not_trigger_recalculation` ✗ |
| TR-011b | The posture engine worker shall process the `PostureRecalcTrigger` generated by the CANOpen adapter within one channel drain cycle (target: 200 ms) | `src/posture_engine_v2.rs:start_posture_engine_worker` | `test_canopen_recalc_trigger_processed_within_cycle` ✗ |

---

### SG-012 — DNP3 Broadcast Command Mandatory Audit (ASIL B)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-012 | The DNP3 protocol adapter and verifier service shall write an audit chain entry for every DNP3 message with `dest_address == DNP3_BROADCAST_ADDRESS` before applying any control output | `src/adapters/dnp3.rs`, `src/bin/aegis_verifier_service.rs:evaluate_dnp3_adapter` | `test_dnp3_broadcast_always_audited` ✗ |
| TR-012a | If the audit chain write fails for a DNP3 broadcast command, the control output shall be blocked and an error shall be returned to the caller | `src/adapters/dnp3.rs` or `src/bin/aegis_verifier_service.rs` audit-before-action ordering | `test_dnp3_broadcast_blocked_on_audit_write_failure` ✗ |
| TR-012b | Non-broadcast DNP3 commands (unicast) shall also be audited, but an audit write failure for a non-broadcast command shall not block the control output | `src/adapters/dnp3.rs` | `test_dnp3_unicast_audit_failure_non_fatal` ✗ |

---

### SG-013 — Recovery Hysteresis Streak and Window Enforcement (ASIL B)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-013 | `evaluate_recovery_report` shall return `HysteresisDecision::Recover` only when `AV_RECOVERY_STREAK_THRESHOLD = 5` consecutive healthy reports have been received and the elapsed time between the first and last report in the streak does not exceed `AV_RECOVERY_WINDOW_MS = 10000` ms | `src/recovery_hysteresis.rs:evaluate_recovery_report` | `test_recovery_requires_full_streak` ✗ |
| TR-013a | `evaluate_recovery_report` shall return `HysteresisDecision::Reset` and set the streak counter to 0 when any gap between consecutive healthy reports exceeds `AV_RECOVERY_WINDOW_MS` | `src/recovery_hysteresis.rs:evaluate_recovery_report` | `test_streak_resets_on_gap` ✗ |
| TR-013b | `evaluate_recovery_report` shall return `HysteresisDecision::Reset` and set the streak counter to 0 when a report with `hw_fault = true` or `confidence < floor` is received during the recovery streak | `src/recovery_hysteresis.rs:evaluate_recovery_report` | `test_unhealthy_report_resets_streak` ✗ |

---

### SG-014 — Federation Report Replay Prevention (ASIL B)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-014 | `reconcile_reports` shall reject any `FederatedTrustReportV2` with a `generation` field less than or equal to the last accepted `generation` from the same peer controller, identified by controller ID | `src/federation_reconciliation.rs:reconcile_reports` | `test_federation_replay_rejected` ✗ |
| TR-014a | Accepted federation report nonces shall be stored in the `federation_report_nonces` SQLite table and checked before acceptance to prevent replay within `FEDERATION_REPLAY_WINDOW_MS = 5000` ms | `src/federation.rs:has_seen_federation_nonce`, `src/verifier_store.rs` | `test_federation_nonce_burn_prevents_replay` ✗ |
| TR-014b | `evaluate_federated_report` shall verify the Ed25519 signature using the public key registered for the peer controller before accepting any report | `src/federation.rs:verify_federated_report_signature` | `test_federation_invalid_signature_rejected` ✗ |

---

### SG-015 — Admin Token Absent Fail-Closed (ASIL B)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-015 | `require_admin_token` shall return HTTP 503 when `std::env::var("AEGIS_ADMIN_TOKEN")` returns `Err` or an empty string; it shall never return a 200-series or 400-series response in this condition | `src/bin/aegis_verifier_service.rs:require_admin_token` | `test_admin_token_absent_returns_503` ✗ |
| TR-015a | `require_admin_token` shall be called on every mutation route handler; no mutation route shall be reachable without passing through `require_admin_token` | `src/bin/aegis_verifier_service.rs` — all admin route handlers | Code inspection + integration test matrix covering all mutation routes |
| TR-015b | Token comparison in `require_admin_token` shall use `constant_time_compare` from `src/security.rs`; standard `==` operator shall not be used on the token bytes | `src/security.rs:constant_time_compare`, `src/bin/aegis_verifier_service.rs:require_admin_token` | Code inspection |

---

### SG-016 — DDS Actuator Topic Volatile Durability (ASIL C)

| TR ID | Technical Requirement | Implementation | Test(s) |
|-------|-----------------------|----------------|---------|
| TR-016 | All DDS actuator topics created in `src/dds_bridge.rs` shall be configured with `DurabilityPolicy::Volatile`; `DurabilityPolicy::TransientLocal` shall not be used for any actuator topic | `src/dds_bridge.rs` — topic creation calls | Code inspection (static invariant) |
| TR-016a | The `startup_sentinel` shall assert the DDS bridge topic durability configuration at startup and shall abort if `TransientLocal` durability is detected | `src/startup_sentinel.rs` | `test_startup_aborts_on_transient_local_durability` ✗ |
| TR-016b | The CDR encapsulation logic in `src/dds_bridge.rs` shall not include any sequence number or history cache that would allow stale commands to be replayed to reconnecting subscribers | `src/dds_bridge.rs` | Code inspection |

---

## 3. Coverage Summary

| Safety Goal | ASIL | TR Count | Implementation Files | Test Count |
|-------------|------|----------|----------------------|------------|
| SG-001 | D | 3 (TR-001, TR-001a, TR-001b) | src/gateway/kinematics_contract.rs, src/aegis_core.rs | 3+ |
| SG-002 | D | 3 (TR-002, TR-002a, TR-002b) | src/gateway/kinematics_contract.rs, src/kinematics_sim.rs | 3+ |
| SG-003 | D | 3 (TR-003, TR-003a, TR-003b) | src/telemetry_watchdog.rs | 3+ |
| SG-004 | C | 3 (TR-004, TR-004a, TR-004b) | src/gateway/kinematics_contract.rs, src/ros2_adapter.rs | 3+ |
| SG-005 | D | 3 (TR-005, TR-005a, TR-005b) | src/posture_engine_v2.rs, src/posture_cache.rs | 2+ |
| SG-006 | D | 3 (TR-006, TR-006a, TR-006b) | src/posture_cache.rs, src/gateway/policy.rs, src/action_filter.rs | 3+ |
| SG-007 | D | 3 (TR-007, TR-007a, TR-007b) | src/fabric/router.rs, src/fabric/causal_log.rs | 2+ |
| SG-008 | D | 3 (TR-008, TR-008a, TR-008b) | src/startup_sentinel.rs, src/bin/aegis_verifier_service.rs | 3+ |
| SG-009 | B | 3 (TR-009, TR-009a, TR-009b) | src/standby_monitor.rs | 3+ |
| SG-010 | B | 3 (TR-010, TR-010a, TR-010b) | src/audit_chain.rs, src/bin/aegis_verifier_service.rs | 2+ |
| SG-011 | C | 3 (TR-011, TR-011a, TR-011b) | src/adapters/canopen.rs | 3+ |
| SG-012 | B | 3 (TR-012, TR-012a, TR-012b) | src/adapters/dnp3.rs, src/bin/aegis_verifier_service.rs | 3+ |
| SG-013 | B | 3 (TR-013, TR-013a, TR-013b) | src/recovery_hysteresis.rs | 3+ |
| SG-014 | B | 3 (TR-014, TR-014a, TR-014b) | src/federation_reconciliation.rs, src/federation.rs | 3+ |
| SG-015 | B | 3 (TR-015, TR-015a, TR-015b) | src/bin/aegis_verifier_service.rs, src/security.rs | 2+ |
| SG-016 | C | 3 (TR-016, TR-016a, TR-016b) | src/dds_bridge.rs, src/startup_sentinel.rs | 1+ |

**Total Technical Requirements:** 48
**Total Test Suite Size:** 306 passing (as of v1.5.0)

---

## 4. Untraced Items

The following items require additional traceability work before ASIL-D certification submission:

1. ROS2 interlock node behavior (ros2_ws/src/aegis_safety/) — functional requirements exist but TR linkage to safety goals is pending.
2. EtherNet/IP adapter (src/adapters/ethernet_ip.rs) — safety-relevant behavior not yet assessed in HARA; addendum HARA update planned.
3. Modbus and OPC-UA adapters (src/protocol_adapter.rs) — partial coverage; audit requirements to be confirmed.
4. CARLA integration (src/bin/aegis_carla_client.rs) — not in scope for vehicle safety case; separate assessment for simulation environment.

---

## 5. Document Control

| Field | Value |
|-------|-------|
| Prepared by | Aegis Engineering |
| Review status | Pending TUV pre-assessment |
| Next review | 2026-11-23 |
| Supersedes | None |
