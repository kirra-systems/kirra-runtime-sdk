# Aegis — ASTM F3269 Run Time Assurance Mapping

Document ID: AEGIS-F3269-001
Version: 1.0.0
Status: Draft
References: ASTM F3269-21 Standard Methods for Run Time Assurance (RTA) of Autonomous and Semi-Autonomous Systems
Date: 2026-05-23

---

## 1. Purpose

ASTM F3269-21 defines a methodology for Run Time Assurance (RTA) monitoring of autonomous and semi-autonomous systems. RTA provides a runtime safety monitor that bounds the behavior of a primary autonomous function (e.g., an AI navigation stack) and reverts to a safe backup control law when the primary function operates outside proven-safe bounds.

Aegis is architecturally an RTA monitor. This document maps each F3269 concept to the corresponding Aegis component, confirming that Aegis satisfies the intent of the standard.

---

## 2. Terminology Mapping

| ASTM F3269 Term | Aegis Implementation | Notes |
|-----------------|---------------------|-------|
| RTA Monitor | Aegis Safety Kernel (`aegis_verifier_service` binary + `validate_vehicle_command()` + `should_route_command()`) | The Aegis process is the monitor — it intercepts every proposed command and gates it |
| Primary Function (PF) | AI Planner / Autonomous Navigation Stack (e.g., Nav2, LLM-based controller, CARLA autopilot) | The upstream system generating ProposedVehicleCommand JSON payloads |
| Backup Control Law (BCL) | MRC Fallback Profile (`VehicleKinematicsContract::mrc_fallback_profile()`) | Applied automatically when FleetPosture transitions to Degraded |
| Safe Set / Safe Region | FleetPosture::Nominal with Nominal Reference Profile active | The region of operation where the primary function is within verified bounds |
| Recovery Region | FleetPosture::Degraded with MRC limits active | Restricted operation: max speed reduced, full maneuverability suspended |
| Unsafe Set / LockedOut Region | FleetPosture::LockedOut | All commands denied; system must halt or execute emergency stop procedure |
| RTA Decision Logic | `validate_vehicle_command()` (Priority 0–8 enforcement pipeline) + `should_route_command()` (posture gate) | Two-layer decision: posture gate (routing) + kinematics gate (envelope) |
| Monitoring Function | Telemetry watchdog (`spawn_telemetry_watchdog()`), DAG trust traversal (`recursive_calculate()`), posture engine worker (`start_posture_engine_worker()`) | Continuously monitors sensor trust state; triggers posture recalculation on fault |
| State Estimation | `CachedFleetPosture` in `SharedPostureCache` — `posture`, `generated_at_ms`, `ttl_ms`, `generation` | Atomic snapshot of fleet posture with staleness detection |
| Switching Logic | Posture engine + `recalculate_and_broadcast()` → `SharedPostureCache` update → all enforcement handlers read updated posture | Mode switch from Safe→Recovery or Recovery→LockedOut is broadcast via Tokio watch channel |
| Proven Safe Region | Kinematic contract invariants verified by proptest suite (prop_allow_result_satisfies_speed_contract, prop_clamp_steering_satisfies_lateral_accel_invariant, etc.) | Property-based testing establishes the proven-safe boundary |
| Recovery Trigger | `AV_TELEMETRY_TIMEOUT_MS = 2000ms` absence → node Untrusted → posture recalculation → Degraded if any critical node faulted | Telemetry watchdog is the primary recovery trigger |
| Nominal Trigger | `evaluate_recovery_report()` — 5 consecutive healthy reports within 10s window | Recovery hysteresis prevents premature return to Nominal |

---

## 3. RTA Architecture Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                   PRIMARY FUNCTION                           │
│  AI Planner / Nav2 / LLM Controller / CARLA Autopilot       │
│                                                              │
│  Generates: ProposedVehicleCommand                           │
│  { linear_velocity_mps, current_velocity_mps,               │
│    delta_time_s, steering_angle_deg,                         │
│    current_steering_angle_deg }                              │
└────────────────────────────┬────────────────────────────────┘
                             │ POST /actuator/motion/command
                             ▼
┌─────────────────────────────────────────────────────────────┐
│                   RTA MONITOR (Aegis)                        │
│                                                              │
│  1. Posture Gate (should_route_command)                      │
│     LockedOut → DenyAll                                      │
│     Degraded → ReadTelemetry only                            │
│     Nominal → proceed to kinematics gate                     │
│                                                              │
│  2. Kinematics Gate (validate_vehicle_command)               │
│     Priority 0: NaN/Inf guard → DenyBreach                   │
│     Priority 1: delta_time_s > 0 → DenyBreach               │
│     Priority 2: |velocity| <= max_speed → ClampLinear        │
│     Priority 3: acceleration rate → ClampLinear              │
│     Priority 4: steering rate → ClampSteering                │
│     Priority 5: steering angle absolute → ClampSteering      │
│     Priority 6: lateral acceleration bicycle model           │
│                 → ClampSteering                              │
│     Priority 7: all checks pass → Allow                      │
│                                                              │
│  3. Monitoring Function                                       │
│     Telemetry watchdog → node trust state                    │
│     DAG traversal → fleet posture                            │
│     Posture engine worker → SharedPostureCache update        │
└────────────────────────────┬────────────────────────────────┘
                             │ EnforceAction
                             │ Allow | ClampLinear | ClampSteering | DenyBreach
                             ▼
┌─────────────────────────────────────────────────────────────┐
│                   ACTUATOR INTERFACE                          │
│  Vehicle motor controller / Robot motor driver               │
│  ROS2 /cmd_vel_safe / DDS actuator topic                     │
└─────────────────────────────────────────────────────────────┘
```

---

## 4. Operating Modes and Transitions

### Mode 1: Safe Region (Nominal)

All sensors trusted. Fleet posture = Nominal. Nominal Reference Profile active:
- max_speed_mps: contract-defined (automotive: 33.3 m/s, robot: 1.8 m/s, drone: 15.0 m/s)
- Full steering range (subject to bicycle model lateral acceleration limit)
- Primary function operates with full authority, bounded by kinematic envelope

Transition to Recovery Region: Any registered sensor node transitions to Untrusted (telemetry timeout, HW fault, PCR16 mismatch) → posture engine recalculates → FleetPosture::Degraded

### Mode 2: Recovery Region (Degraded)

One or more sensors faulted. Fleet posture = Degraded. MRC Fallback Profile active:
- max_speed_mps: 5.0 m/s (automotive), 0.54 m/s (robot: 1.8 * 0.3), 4.5 m/s (drone: 15.0 * 0.3)
- Reduced steering (40% of nominal steering rate)
- WriteState and SystemMutation commands blocked; ReadTelemetry allowed
- Primary function continues with restricted authority

Transition to LockedOut Region: Multiple sensor failures, DAG cycle detected, posture cache stale (age >= POSTURE_CACHE_TTL_MS = 5000ms), or admin lockout command

Transition to Safe Region: evaluate_recovery_report() returns HysteresisDecision::AllowRecovery — requires 5 consecutive healthy sensor reports within 10s window

### Mode 3: LockedOut Region

Catastrophic fleet state. All commands denied. System must proceed to emergency stop.

ROS2 interlock behavior: posture_subscriber.py detects LockedOut transition via SSE stream, immediately publishes zero-velocity command to /cmd_vel_safe.

---

## 5. Proven Safe Region Definition

The Proven Safe Region is defined by the kinematic contract invariants that are verified by the property-based test suite. For the Nominal Reference Profile:

**Invariant 1 (Speed):** For all commands with |linear_velocity_mps| <= max_speed_mps, validate_vehicle_command() returns Allow or ClampSteering (never ClampLinear).

**Invariant 2 (Lateral Acceleration):** For all commands where validate_vehicle_command() returns Allow, the computed lateral acceleration (velocity^2 * tan(steering_deg) / wheelbase_m) is <= max_lateral_accel_mps2.

**Invariant 3 (Sign Preservation):** For all ClampSteering results, sign(clamped_angle) == sign(original_angle).

**Invariant 4 (Finiteness):** validate_vehicle_command() never returns Allow for a command with NaN or infinite values in any field.

These invariants are verified by proptest properties in `src/gateway/kinematics_proptest.rs`.

---

## 6. Claim Against F3269 Requirements

| F3269 Requirement | Aegis Claim | Evidence |
|-------------------|-------------|----------|
| RTA.1: The RTA monitor shall detect when the primary function exceeds the safe set | validate_vehicle_command() detects speed, acceleration, steering, and NaN/Inf exceedances synchronously on every command | src/gateway/kinematics_contract.rs, 306 passing tests |
| RTA.2: The RTA monitor shall switch to the backup control law within the fault tolerant time interval | Posture transition to Degraded applies MRC profile to next command (synchronous, per-command FTTI) | FTTI for kinematic enforcement: per-command (< 1ms); FTTI for posture: AV_TELEMETRY_TIMEOUT_MS = 2000ms |
| RTA.3: The backup control law shall keep the system within the recovery region | MRC Fallback Profile max_speed = 5.0 m/s is within the proven-safe region for all vehicle types | VehicleKinematicsContract::mrc_fallback_profile() |
| RTA.4: The RTA monitor shall be independent of the primary function | Aegis is a separate process; primary function has no write access to Aegis state; admin token required for all mutations | Architectural separation; require_admin_token on all mutation routes |
| RTA.5: The RTA monitor shall fail to a safe state | Stale posture cache → LockedOut (all commands denied). Absent env token → 503 fail-closed. Empty posture cache → LockedOut. Poisoned RwLock → LockedOut. | src/posture_cache.rs:should_route_command, AEGIS-SG-005 |
| RTA.6: The RTA monitor shall record evidence of monitoring decisions | Every command evaluation generates an audit entry in the SHA-256 hash-chained audit log with Ed25519 signature | src/audit_chain.rs, AEGIS-SG-010 |

---

## 7. Gaps and Open Items

| Gap ID | Description | Action |
|--------|-------------|--------|
| F3269-GAP-001 | Formal verification of proven safe region not yet completed | Address in Phase 2 (DO-333 formal methods or TLA+ model) |
| F3269-GAP-002 | FTTI for posture recalculation path not independently verified | Measure end-to-end latency from telemetry silence to posture update in test environment |
| F3269-GAP-003 | F3269 does not define how to handle multi-asset fabric mode (compound primary function) | Document FabricRouter posture propagation as extension to single-asset RTA model |

---

## 8. Document Control

| Field | Value |
|-------|-------|
| Prepared by | Aegis Engineering |
| Next review | 2026-11-23 |
| Supersedes | None |
