# Aegis Safety Kernel — Safety Goals

Document ID: AEGIS-SG-001
Version: 1.0.0
Status: Draft
Classification: ISO 26262 Part 4
Date: 2026-05-23

---

## 1. Overview

This document defines the Safety Goals (SG) derived from the Hazard Analysis and Risk Assessment (AEGIS-HARA-001). Each safety goal is assigned an ASIL level, a precise and testable goal statement, a safe state, a Fault Tolerant Time Interval (FTTI), and a verification method.

Safety goals are allocated to the `aegis-runtime-sdk` v1.5.0 software item and are the primary input to safety requirements decomposition.

---

## 2. Safety Goal Definitions

---

### SG-001 — Velocity Envelope Enforcement

**ASIL:** D
**Derived from:** H-001, H-015
**Goal statement:** The Aegis safety kernel shall prevent any command with an absolute linear velocity (linear_velocity_mps) exceeding the active kinematic contract's max_speed_mps from being delivered to a downstream actuator. The hard envelope cap shall be applied before any rate-of-change limiter, such that no transient overspeed command can pass through in any enforcement cycle.
**Safe state:** The commanded velocity is clamped to max_speed_mps; the enforcement decision is logged to the audit chain; the original command is not forwarded.
**FTTI:** 50 ms (one enforcement cycle; must be enforced on the command as received, not deferred)
**Verification method:** Unit test (test_speed_above_ceiling_triggers_clamp_linear), property-based test (proptest kinematics suite), integration test with CARLA simulator, code inspection of priority ordering in validate_vehicle_command

---

### SG-002 — Lateral Acceleration Envelope Enforcement

**ASIL:** D
**Derived from:** H-002
**Goal statement:** The Aegis safety kernel shall compute the implied lateral acceleration for every vehicle command using the bicycle kinematic model and shall prevent delivery of any command that would cause lateral acceleration exceeding the active contract's max_lateral_accel_mps2.
**Safe state:** Steering angle is clamped such that the resulting lateral acceleration is at or below max_lateral_accel_mps2; the enforcement decision is logged; the original command is not forwarded.
**FTTI:** 50 ms
**Verification method:** Unit test (test_nominal_highway_speed_high_steering_clamps_steering), property-based test (kinematics_proptest.rs), forward simulator validation (kinematics_sim.rs)

---

### SG-003 — Sensor Timeout Fault Detection

**ASIL:** D
**Derived from:** H-003
**Goal statement:** The Aegis telemetry watchdog shall detect the absence of telemetry from any registered AV sensor node within AV_TELEMETRY_TIMEOUT_MS (2000 ms) and shall transition that node to the Untrusted trust state, triggering fleet posture recalculation, within one watchdog sweep interval (AV_WATCHDOG_SWEEP_MS = 100 ms) after the timeout expires.
**Safe state:** The sensor node is marked Untrusted; fleet posture is recalculated; if posture transitions to Degraded or LockedOut, the posture cache is updated and all subsequent commands are evaluated under the new posture.
**FTTI:** AV_TELEMETRY_TIMEOUT_MS + AV_WATCHDOG_SWEEP_MS = 2100 ms
**Verification method:** Unit test (test_watchdog_marks_node_untrusted_after_timeout), temporal integration test via ScenarioRunner with VirtualClock injection, inspection of spawn_telemetry_watchdog loop interval

---

### SG-004 — NaN and Inf Rejection

**ASIL:** C
**Derived from:** H-005
**Goal statement:** The Aegis gateway shall reject any vehicle command containing a non-finite (NaN or Inf) value in any f64 field before performing any arithmetic on that command. The rejection shall occur as the first check (Priority 0) in validate_vehicle_command, prior to all other enforcement logic.
**Safe state:** The command is rejected with an appropriate error variant; no arithmetic is performed on the non-finite value; the rejection is logged to the audit chain.
**FTTI:** 50 ms
**Verification method:** Unit tests (test_nan_linear_velocity_is_denied, test_inf_linear_velocity_is_denied), property-based test generating arbitrary f64 combinations including NaN/Inf

---

### SG-005 — Posture Cache Staleness Fail-Closed

**ASIL:** D
**Derived from:** H-004
**Goal statement:** The Aegis posture resolution function shall return a LockedOut result with reason PostureCacheStale when the cached fleet posture age meets or exceeds POSTURE_CACHE_TTL_MS (5000 ms). The system shall never evaluate a vehicle command against a stale posture cache.
**Safe state:** Commands are blocked (fail-closed) until the posture cache is refreshed with a valid recalculation result; the stale-cache lockout reason is logged.
**FTTI:** POSTURE_CACHE_TTL_MS = 5000 ms (cache staleness is the bound; lockout is immediate upon detection)
**Verification method:** Unit test (test_stale_cache_fails_closed_after_virtual_clock_advance) using VirtualClock injection, integration test confirming no command passes during stale window

---

### SG-006 — Unknown Command Denial in All Posture States

**ASIL:** D
**Derived from:** H-009
**Goal statement:** The Aegis command router shall deny OperationalCommand::Unknown before performing any posture evaluation, in all posture states including Nominal. The early return on Unknown shall be the first check in should_route_command and shall never be conditioned on posture state.
**Safe state:** The command is denied; the denial is logged; the fleet posture is unaffected.
**FTTI:** Synchronous; enforced within the request handling cycle (< 10 ms)
**Verification method:** Unit test (test_unknown_command_denied_in_all_posture_states), code inspection of should_route_command to verify unconditional early return, mutation testing to confirm the check cannot be removed without test failure

---

### SG-007 — Cross-Asset Fleet Lockout Propagation

**ASIL:** D
**Derived from:** H-012
**Goal statement:** When a leader asset in a multi-asset fabric transitions to LockedOut posture, the Aegis fabric router shall propagate a Degraded posture state to all follower assets registered in the cross-asset dependency group within one propagation cycle (defined by the fabric governor tick interval).
**Safe state:** Follower assets receive Degraded posture; their command enforcement applies the Degraded contract (ReadTelemetry only); motion commands are blocked on all followers until the leader recovers.
**FTTI:** One fabric governor tick interval (implementation-defined; target <= 500 ms)
**Verification method:** Integration test (test_convoy_leader_lockout_degrades_followers), fabric unit tests in src/fabric/router.rs

---

### SG-008 — Process Fail-Closed on Crash

**ASIL:** D
**Derived from:** H-006
**Goal statement:** The Aegis verifier service shall perform a startup invariant check via startup_sentinel before accepting any command traffic. If any invariant check fails, the process shall abort rather than enter a fail-open state. The service architecture shall not permit command forwarding to bypass the posture enforcement layer.
**Safe state:** Process aborts cleanly; no commands are forwarded; the upstream AI planner receives connection-refused or timeout errors and must handle the absence of the enforcement layer.
**FTTI:** Startup check must complete before the TCP listener binds; no commands accepted until all invariants pass.
**Verification method:** Integration smoke test confirming service startup with valid configuration, negative test confirming abort on missing AEGIS_ADMIN_TOKEN, code inspection of startup_sentinel invariant list

---

### SG-009 — HA Standby Promotion Within PROMOTION_TIMEOUT_MS

**ASIL:** B
**Derived from:** H-007
**Goal statement:** In a high-availability deployment, the passive standby instance shall detect the absence of a primary heartbeat and promote itself to the active enforcement role within PROMOTION_TIMEOUT_MS (10000 ms) of the last valid heartbeat from the primary instance.
**Safe state:** The standby instance promotes and begins enforcing posture; enforcement coverage is restored within the promotion timeout; the promotion event is logged to the audit chain.
**FTTI:** PROMOTION_TIMEOUT_MS = 10000 ms (enforcement gap is bounded by this interval)
**Verification method:** Unit test (test_standby_promotes_after_primary_timeout) using VirtualClock injection, integration test with simulated primary crash

---

### SG-010 — Audit Chain Tamper Detection

**ASIL:** B
**Derived from:** H-008
**Goal statement:** The Aegis audit chain linker shall detect any modification to a previously written audit entry by verifying that the prev_hash field of each entry matches SHA-256(previous_entry_serialized). Verification shall be available on demand via the /system/audit/verify endpoint and shall be performed automatically on startup.
**Safe state:** Tampered entries are detected and reported; the verification result is logged; the system continues operating but flags the chain integrity failure for operator review.
**FTTI:** Verification is on-demand; tamper detection latency is bounded by the time between audit operations.
**Verification method:** Unit test (test_audit_chain_tamper_detection), code inspection of AuditChainLinker hash verification logic, fuzz test injecting corrupted bytes at random entry positions

---

### SG-011 — CANOpen NMT State Change Triggers Posture Recalculation

**ASIL:** C
**Derived from:** H-010
**Goal statement:** The Aegis CANOpen protocol adapter shall interpret NMT commands with data[0] values of 0x02 (Stop), 0x80 (Pre-Operational), 0x81 (Reset Node), or 0x82 (Reset Communication) as posture-affecting events and shall set triggers_recalculation=true, causing the posture engine to recalculate fleet posture within one engine cycle.
**Safe state:** The posture engine recalculates posture reflecting the NMT state change; if posture degrades, subsequent commands are evaluated under the updated posture.
**FTTI:** One posture engine cycle (coalescing mpsc channel, target <= 200 ms)
**Verification method:** Unit test (test_canopen_nmt_stop_triggers_posture_recalculation), adapter integration test with synthetic CANOpen frames

---

### SG-012 — DNP3 Broadcast Command Mandatory Audit

**ASIL:** B
**Derived from:** H-011
**Goal statement:** The Aegis DNP3 protocol adapter and verifier service shall write an audit chain entry for every DNP3 message with a destination address equal to DNP3_BROADCAST_ADDRESS before any control output is applied. The audit entry shall be written atomically before the control effect is permitted.
**Safe state:** The audit entry is written before the control output; if the audit write fails, the control output is blocked (fail-closed audit ordering).
**FTTI:** Synchronous; audit write precedes control output within the same request handling cycle.
**Verification method:** Unit test (test_dnp3_broadcast_always_audited), code inspection confirming audit-before-action ordering in evaluate_dnp3_adapter

---

### SG-013 — Recovery Hysteresis Streak and Window Enforcement

**ASIL:** B
**Derived from:** H-013
**Goal statement:** The Aegis recovery hysteresis evaluator shall require exactly AV_RECOVERY_STREAK_THRESHOLD (5) consecutive healthy sensor reports, all arriving within a single AV_RECOVERY_WINDOW_MS (10000 ms) window, before transitioning a node from Untrusted to Trusted. Any gap between consecutive reports that exceeds AV_RECOVERY_WINDOW_MS shall reset the streak counter to zero. Any single unhealthy report during recovery shall reset the streak counter to zero.
**Safe state:** The node remains Untrusted until the full streak is satisfied; the fleet posture remains at the degraded level appropriate for the Untrusted node count.
**FTTI:** Recovery is bounded by AV_RECOVERY_STREAK_THRESHOLD * (inter-report interval) + AV_RECOVERY_WINDOW_MS; no premature recovery can occur before this bound.
**Verification method:** Unit tests (test_recovery_requires_full_streak, test_streak_resets_on_gap), temporal integration test with VirtualClock injection simulating various report patterns

---

### SG-014 — Federation Report Replay Prevention

**ASIL:** B
**Derived from:** H-014
**Goal statement:** The Aegis federation reconciliation engine shall reject any FederatedTrustReportV2 with a generation counter less than or equal to the last accepted generation counter from the same peer controller. Nonces from accepted reports shall be burned in the federation_report_nonces table to prevent replay within the FEDERATION_REPLAY_WINDOW_MS (5000 ms) window.
**Safe state:** The replayed or outdated report is rejected; the current posture state is unchanged; the rejection is logged to the audit chain.
**FTTI:** Synchronous; rejection occurs within the federation report evaluation pipeline.
**Verification method:** Unit test (test_federation_replay_rejected), code inspection of reconcile_reports generation comparison logic, nonce burning verification test

---

### SG-015 — Admin Token Absent Fail-Closed

**ASIL:** B
**Derived from:** H-016
**Goal statement:** The Aegis verifier service shall return HTTP 503 (Service Unavailable) for all mutation routes when the AEGIS_ADMIN_TOKEN environment variable is absent or resolves to an empty string. The require_admin_token function shall never be bypassed, commented out, or conditioned on a feature flag on any mutation route.
**Safe state:** All mutation operations are blocked; read-only and public routes remain available; the 503 response does not include any information about the token configuration.
**FTTI:** Synchronous; enforced at request ingress before any state mutation.
**Verification method:** Unit test (test_admin_token_absent_returns_503), integration test confirming all admin routes return 503 when AEGIS_ADMIN_TOKEN is unset, code inspection of require_admin_token usage on all mutation routes

---

### SG-016 — DDS Actuator Topic Volatile Durability

**ASIL:** C
**Derived from:** H-017
**Goal statement:** All DDS actuator topics created by the Aegis DDS bridge shall be configured with DurabilityPolicy::Volatile. No actuator topic shall use DurabilityPolicy::TransientLocal or any other durability policy that would cause historical commands to be delivered to reconnecting subscribers.
**Safe state:** Reconnecting actuator subscribers receive only fresh commands; historical commands from before the reconnect are not delivered; the actuator starts from a stopped or hold state until the next fresh command arrives.
**FTTI:** This is a static configuration invariant; there is no runtime FTTI. The property must be verified at startup and on each topic creation.
**Verification method:** Code inspection of src/dds_bridge.rs confirming DurabilityPolicy::Volatile in all topic creation calls, static analysis, startup assertion (if configurable topic durability is supported)

---

## 3. Safety Goal to ASIL Summary

| Safety Goal | ASIL | Primary Mechanism | Derived From |
|-------------|------|-------------------|--------------|
| SG-001 | D | validate_vehicle_command Priority 2 (velocity clamp) | H-001, H-015 |
| SG-002 | D | validate_vehicle_command Priority 6 (bicycle model lateral accel) | H-002 |
| SG-003 | D | spawn_telemetry_watchdog timeout detection | H-003 |
| SG-004 | C | validate_vehicle_command Priority 0 (NaN/Inf guard) | H-005 |
| SG-005 | D | resolve_posture_with_reason TTL check | H-004 |
| SG-006 | D | should_route_command Unknown early return | H-009 |
| SG-007 | D | propagate_cross_asset_trust leader lockout rule | H-012 |
| SG-008 | D | startup_sentinel invariant checks | H-006 |
| SG-009 | B | spawn_promotion_monitor heartbeat poll | H-007 |
| SG-010 | B | AuditChainLinker SHA-256 hash verification | H-008 |
| SG-011 | C | CanOpenAdapter NMT state mapping | H-010 |
| SG-012 | B | DNP3 adapter broadcast audit requirement | H-011 |
| SG-013 | B | evaluate_recovery_report streak and window | H-013 |
| SG-014 | B | reconcile_reports generation comparison and nonce burning | H-014 |
| SG-015 | B | require_admin_token fail-closed check | H-016 |
| SG-016 | C | DDS bridge Volatile durability configuration | H-017 |

---

## 4. Document Control

| Field | Value |
|-------|-------|
| Prepared by | Aegis Engineering |
| Review status | Pending TUV pre-assessment |
| Next review | 2026-11-23 |
| Supersedes | None |
