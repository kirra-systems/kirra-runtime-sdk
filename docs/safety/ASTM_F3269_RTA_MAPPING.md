# ASTM F3269-21 Bounded Operation Mapping

Document ID: KIRRA-RTA-001
Version: 1.0
Status: Draft
Standard: ASTM F3269-21 (Methods for Specifying the Bounded Operational
          Domain and Mission of UAS)
Date: 2026-05-29

## 1. Overview

ASTM F3269-21 defines requirements for specifying and enforcing the
Operational Design Domain (ODD) of autonomous systems — the conditions
under which the system is designed to operate safely. When operating
outside the ODD, the system must detect the violation and transition
to a safe state.

Kirra implements the Runtime Assurance (RTA) architecture defined in
ASTM F3269-21. It operates as the safety monitoring layer between AI
inference output and physical actuators — continuously evaluating
whether proposed commands fall within the operational envelope and
enforcing safe-state transitions when they do not.

This mapping applies to Kirra deployments on:
- Unmanned Aircraft Systems (UAS / drones)
- Unmanned Ground Vehicles (UGV)
- Autonomous robots operating in defined environments

This is a **self-assessment**. Independent third-party assessment by
an ASTM-recognized body has not yet been performed.

Relationship to the existing preliminary mapping in
`docs/safety/ASTM_F3269_MAPPING.md` (AEGIS-F3269-001 v1.0.0): that
earlier document established the initial claim shape under the prior
Aegis branding; this document (KIRRA-RTA-001) is the up-to-date
working mapping aligned with the current Kirra architecture, the
ODD specification, and CERT-track artifacts. The earlier document
remains in the safety case index for historical traceability.

## 2. Kirra as an RTA System

ASTM F3269-21 defines the Runtime Assurance Monitor as a component
that:

1. Monitors the primary (AI) system's outputs
2. Detects when outputs would violate the operational envelope
3. Activates the fallback (backup) system when violations are detected
4. Maintains an audit trail of all interventions

Kirra maps directly to this architecture:

| ASTM F3269-21 Component | Kirra Equivalent | Implementation |
|------------------------|-----------------|----------------|
| Primary System | AI inference layer (any `InferenceBackend`) | `parko/crates/parko-core/src/` |
| RTA Monitor | `KirraGovernor` + posture engine | `parko/crates/parko-kirra/src/lib.rs`, `src/verifier.rs` |
| Operational Envelope | Kinematic limits (velocity ceiling, accel limits) | `parko/crates/parko-kirra/src/lib.rs` |
| ODD Violation Detection | RSS safe-distance + telemetry watchdog + posture engine | `src/posture_cache.rs` |
| Fallback System | MRC fallback profile (Degraded) / Hard stop (LockedOut) | `parko/crates/parko-kirra/src/lib.rs` |
| Audit Trail | SHA-256 hash-chained audit log | `src/audit_chain.rs` |

## 3. Operational Design Domain (ODD) Specification

Per ASTM F3269-21 §5, the ODD must be explicitly specified. Kirra's
ODD is defined by the following operational parameters:

### 3.1 Kinematic Envelope (Hard Bounds)

| Parameter | Nominal Limit | MRC Limit (Degraded) | Hard Stop (LockedOut) |
|-----------|--------------|---------------------|----------------------|
| Linear velocity | 35.0 m/s ceiling | 5.0 m/s cap (`MRC_VELOCITY_CEILING_MPS`) | 0.0 m/s |
| Acceleration | Rate-limited (nominal profile) | Rate-limited (MRC profile) | 0.0 m/s² |
| Angular velocity | Governed | Governed | 0.0 rad/s |

Source: `parko/crates/parko-kirra/src/lib.rs`

### 3.2 Sensor Health Bounds

| Parameter | Bound | Violation Response |
|-----------|-------|--------------------|
| Telemetry freshness | `AV_TELEMETRY_TIMEOUT_MS` (2,000 ms) | Degraded posture |
| RSS longitudinal margin | ≥ `longitudinal_safe_distance` | Degraded posture |
| RSS lateral margin | ≥ `lateral_safe_distance` | Degraded posture |

Source: `src/verifier.rs`, `parko/crates/parko-core/src/rss.rs`

### 3.3 System Health Bounds

| Parameter | Bound | Violation Response |
|-----------|-------|--------------------|
| Dependency graph | Cycle-free, depth ≤ `MAX_DEPENDENCY_DEPTH` (10) | LockedOut |
| Node trust state | All trusted | Degraded or LockedOut |
| Governor reachability | Reachable | Degraded semantics |
| Admin token | Present and non-empty | 503 / fail-closed |

Source: `src/verifier.rs`, `src/posture_cache.rs`

## 4. ODD Violation Detection and Response Mapping

Per ASTM F3269-21 §6, the RTA monitor must detect all ODD violations
and respond within a defined time bound.

| ODD Violation | Detection Mechanism | Response | Time Bound |
|--------------|--------------------|---------|-----------|
| Command exceeds velocity ceiling | `KirraGovernor::evaluate()` | `ClampLinearVelocity` | Every tick (≤ 50 ms) |
| RSS safe-distance violated | `RssState.safe == false` | Degraded + MRC cap | Every tick |
| Sensor telemetry timeout | `AV_TELEMETRY_TIMEOUT_MS` watchdog | Degraded | ≤ 2.1 s |
| Dependency graph cycle | DAG traversal (`recursive_calculate`) | LockedOut | Every recalculation |
| Unknown command | `should_route_command` early return | Denied | Every command |
| `GovernorComparator` divergence | Δ > `COMPARATOR_TOLERANCE` (1e-9) | Deny (hard stop) | Every tick |
| Startup invariant failure | `startup_sentinel` pre-bind | Process abort | Startup |

## 5. Fallback System Specification

Per ASTM F3269-21 §7, the fallback system must be specified and must
be independent of the primary system.

### 5.1 Fallback Level 1: Minimum Risk Condition (Degraded)

- **Activation:** ODD violation detected, recoverable
- **Behavior:** `MRC_VELOCITY_CEILING_MPS` (5.0 m/s) cap
- **Independence:** MRC profile is independent of AI inference output —
  it applies regardless of what the AI requested
- **Recovery:** 5 consecutive clean ticks within 10 s window → Nominal
  (`AV_RECOVERY_STREAK_THRESHOLD` / `AV_RECOVERY_WINDOW_MS`)
- **Implements:** ASTM F3269-21 §7.2 "Minimum Risk Maneuver"

### 5.2 Fallback Level 2: Hard Stop (LockedOut)

- **Activation:** Critical ODD violation, non-recoverable
- **Behavior:** 0.0 m/s — full stop, no commands forwarded
- **Independence:** Hard stop is unconditional — no AI input can override
- **Recovery:** Human intervention required (`KIRRA_SUPERVISOR_RESET_KEY`)
- **Implements:** ASTM F3269-21 §7.3 "Minimum Risk Condition (terminal)"

## 6. RTA Independence Requirements

ASTM F3269-21 requires the RTA monitor to be independent of the
primary (AI) system. Kirra satisfies this through:

### 6.1 Software Independence

- `KirraGovernor` runs as a separate Rust crate (`parko-kirra`) with
  no dependency on any AI inference crate
- The safety decision path (posture engine + governor) cannot be
  overridden by AI model output

### 6.2 Execution Independence

- Kirra processes AI output **after** inference completes — the AI
  cannot modify Kirra's decision logic at runtime
- `OperationalCommand::Unknown` is denied before any posture check
  (SG-006 — cannot be bypassed by any AI output)

### 6.3 Redundancy

- `GovernorComparator` runs two independent governor instances and
  detects divergence (CERT-006)
- Two independent faults required to bypass both instances

## 7. Audit and Traceability Requirements

ASTM F3269-21 §8 requires a tamper-evident record of all RTA
interventions.

| Requirement | Implementation | Status |
|-------------|----------------|--------|
| Log all ODD violations | `RssViolationEvent`, `posture_events` table | ✅ Implemented |
| Tamper-evident audit | SHA-256 hash-chained; optional Ed25519 signed | ✅ Implemented |
| Replay capability | Scenario replay from audit log | ✅ Implemented |
| Startup audit verification | `/system/audit/verify` endpoint | ✅ Implemented |

Source: `src/audit_chain.rs`, `src/verifier_store.rs`

## 8. Gaps and Open Items

| Gap | Priority | Tracking |
|-----|----------|---------|
| ODD formally specified per platform (UAS, UGV, industrial) | High | Platform-specific config |
| Time-bound verification for detection latency (empirical benchmarks) | Medium | Not yet benchmarked |
| Independent V&V not performed | High | Pending assessment |
| ASTM F3269-21 notified-body assessment | High | Not started |
| ODD monitoring for environmental conditions (weather, GPS) | Low | Out of current scope |

## 9. Relationship to Other Standards

| Standard | Relationship |
|----------|-------------|
| ISO 26262 ASIL-D | Automotive deployment — see `SAFETY_GOALS.md` |
| IEC 61508 SIL 3 | Industrial / general deployment — see `IEC_61508_SIL3_MAPPING.md` |
| IEEE 2846 | RSS behavioral safety — integrated in posture engine |
| DO-178C | Avionics — not currently targeted |
| ROS REP-2004 | ROS2 safety requirements — relevant for PARK-037 |

## 10. Implementation References

- `KirraGovernor` (RTA monitor): `parko/crates/parko-kirra/src/lib.rs`
- `GovernorComparator`: `parko/crates/parko-kirra/src/comparator.rs`
- Posture engine: `src/posture_cache.rs`, `src/verifier.rs`
- RSS safe-distance: `parko/crates/parko-core/src/rss.rs`
- Audit chain: `src/audit_chain.rs`
- Startup sentinel: `src/bin/kirra_verifier_service.rs`
- Safe state specification: `docs/safety/SAFE_STATE_SPECIFICATION.md`
- IEC 61508 SIL 3 mapping: `docs/safety/IEC_61508_SIL3_MAPPING.md`
- Predecessor mapping: `docs/safety/ASTM_F3269_MAPPING.md` (AEGIS-F3269-001)
