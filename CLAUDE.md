# Kirra — Claude Code Context

## Project Identity

- **Workspace**: a Cargo workspace. The ROOT member is the **`kirra-verifier`** crate (the fleet-legitimacy engine + governor service, documented in the bulk of this file). The **doer-checker / planning / perception** side lives under `crates/*` (see **Workspace Crates** below). `parko/` is a **separate** workspace (the ML + diverse-governor side). Most AV/Occy work happens in `crates/kirra-planner`, `kirra-map`, `kirra-taj`, `kirra-ros2-adapter`, and `parko/`.
- **Root crate**: `kirra-verifier` (lib ident `kirra_verifier`; lib + bin dual-crate, `crate-type = ["rlib", "cdylib"]`). Renamed from `kirra-runtime-sdk` once nothing lean depended on it (the GitHub repo remains `kirra-runtime-sdk`).
- **Edition**: 2021
- **Primary binary**: `kirra_verifier_service` (`src/bin/kirra_verifier_service.rs`)
- **Secondary binary**: `kirra_carla_client` (`src/bin/kirra_carla_client.rs`)
- **Test suite**: `cargo test` (root). For a scoped crate use `cargo test -p <crate>`. `parko/` is its own workspace: `cd parko && cargo test`. The `kirra-ros2-adapter` `node.rs` is `#[cfg(feature = "ros2")]` and needs a sourced ROS 2 toolchain (`r2r`) — it is built ONLY by CI's `ros2 adapter build (--features ros2)` job, never by a default build.
- **Remote**: `kirra-systems/kirra-runtime-sdk`
- **Repo root in prompts**: use `~/kirra-runtime-sdk` (not `/home/user/...` or `/home/user/aegis`)

---

## What This System Is

Kirra is a distributed runtime legitimacy engine and safety governor for AI-driven robotic and edge systems. It enforces fail-closed trust semantics across a heterogeneous fleet — preventing unsafe or unauthorized commands from reaching actuators regardless of what an AI model, LLM output, or upstream orchestration layer instructs.

---

## CRITICAL SECURITY INVARIANTS — NEVER VIOLATE THESE

These have been blocked or reverted multiple times. Any submission that violates them must be rejected outright.

1. **`require_admin_token` must never be commented out, bypassed, or removed** from any mutation route. It reads `KIRRA_ADMIN_TOKEN` from env; if absent or empty it returns 503 (fail-closed), never fail-open.

2. **`constant_time_compare` must be used** for all token comparisons. Standard `==` is forbidden on security-critical byte sequences.

3. **`verify_attestation` must never mock trust** (`let status = NodeTrustState::Trusted` without verification). It MUST cryptographically verify a per-node proof: the node's Ed25519 signature over the `(node_id, nonce)` challenge payload, checked against the registered per-node `ak_public_pem` via `attestation::verify_attestation_proof` (issue #73). Fail-closed — no registered AK / malformed key / malformed proof / bad signature → reject; never accept by default. (The prior `HMAC(KIRRA_ADMIN_TOKEN, nonce)` proof was admin-asserted, not node-proven, and is removed. PCR16 measured-boot quote verification is a tracked follow-up.)

4. **`FleetNodePosture` and the gray/black two-set DAG algorithm must never be replaced with a mock**. The real traversal in `AppState::recursive_calculate` must remain intact.

5. **`pending_challenges: DashMap<String, ChallengeEntry>` must never be removed**. Nonces are volatile, never persisted, and expire after `CHALLENGE_TTL_MS = 30_000` ms.

6. **`KIRRA_ADMIN_TOKEN` must come from env var only**. No hardcoded fallbacks. Absent or empty → 503.

7. **`KIRRA_SUPERVISOR_RESET_KEY` must come from env var, no hardcoded fallbacks**. Must be present, non-empty, and ≤ 64 bytes.

8. **The governor must clamp to the absolute hard boundary first**, then apply rate-of-change limits. Envelope cap always wins over rate priority.

9. **`OperationalCommand::Unknown` is denied in ALL posture states including Nominal**. The early return `if command == OperationalCommand::Unknown { return false; }` in `should_route_command` must never be removed.

10. **DDS actuator topics must use `DurabilityPolicy::Volatile`**, never `TransientLocal`.

11. **All handlers use `State<Arc<ServiceState>>`**, not `State<Arc<AppState>>`. `ServiceState` has `app: Arc<AppState>` and `posture_cache: SharedPostureCache`. Accessing app state in handlers: `svc.app.*`.

12. **SQLite writes go to disk before memory** (fail-closed ordering). `persist_and_insert_node` calls `save_node` then `nodes.insert` — never reverse this.

13. **`no std::env::set_var` in multithreaded context**.

---

## Architecture

### Key Types and Locations

| Type | File | Notes |
|------|------|-------|
| `AppState` | `src/verifier.rs:169` | DashMap nodes/dependency_graph/challenges, Arc<Mutex<VerifierStore>>, mode_active AtomicBool, posture_tx |
| `RegisteredNode` | `src/verifier.rs:34` | `node_id`, `status: NodeTrustState`, `registered_at_ms`, `last_trust_update_ms`, `ak_public_pem`, `expected_pcr16_digest_hex` |
| `ServiceState` | `src/posture_cache.rs` | Wraps `Arc<AppState>` + `SharedPostureCache`; is the axum router state |
| `FleetPosture` | `src/verifier.rs` | `Nominal` / `Degraded` / `LockedOut` |
| `FleetNodePosture` | `src/verifier.rs` | Per-node posture with `blocked_by` list |
| `NodeTrustState` | `src/verifier.rs` | `Trusted` / `Untrusted(String)` / `Unknown` |
| `OperationalCommand` | `src/posture_cache.rs` | `ReadTelemetry` / `WriteState` / `SystemMutation` / `Unknown` |
| `VerifierOperationMode` | `src/verifier.rs` | `Active` / `PassiveStandby`; runtime state held in `mode_active: Arc<AtomicBool>` |
| `VerifierStore` | `src/verifier_store.rs` | rusqlite WAL-mode SQLite; wrapped in `Arc<Mutex<VerifierStore>>` in AppState |
| `PostureStreamEvent` | `src/verifier.rs` | Broadcast channel payload for SSE stream |
| `TransportIdentityConfig` | `src/verifier.rs` | `trusted_ingress_mode` + `client_id_header` from env |
| `FederatedTrustReport` | `src/federation.rs` | Ed25519-signed cross-controller trust report |
| `FederatedTrustReportV2` | `src/federation_reconciliation.rs` | Generation-ordered v2 report with reconciliation |
| `AuditChainLinker` | `src/audit_chain.rs` | SHA-256 hash-chained tamper-evident ledger |
| `SharedPostureCache` | `src/posture_cache.rs` | `Arc<tokio::sync::RwLock<Option<CachedFleetPosture>>>` |
| `CachedFleetPosture` | `src/posture_cache.rs` | Atomic snapshot: `posture`, `generated_at_ms`, `ttl_ms`, `generation` |
| `LockoutReason` | `src/posture_engine_v2.rs` | Structured fail-closed reason codes (`DagLockedOut`, `PostureCacheStale`, etc.) |
| `PostureRecalcTrigger` | `src/posture_engine_v2.rs` | Typed trigger for posture engine worker channel |
| `PostureEngineSender` | `src/posture_engine_v2.rs` | `mpsc::Sender<PostureRecalcTrigger>` — add to ServiceState |
| `KirraPolicyLayer` | `src/gateway/policy_layer.rs` | Tower middleware; gates commands by posture |
| `VehicleKinematicsContract` | `src/gateway/kinematics_contract.rs` | Hard envelope limits for vehicle commands |
| `VirtualClock` / `SystemClock` | `src/clock.rs` | Clock abstraction for deterministic testing |
| `ScenarioRunner` | `src/scenario_runner.rs` | Deterministic temporal test harness |
| `KinematicContract` | `src/kinematics_contract.rs` | Scalar clamping contract for kinematics |

### Module Map

```
src/
├── verifier.rs               — AppState, FleetPosture, DAG traversal, TransportIdentityConfig
├── verifier_store.rs         — SQLite persistence (all tables; WAL mode)
├── posture_cache.rs          — SharedPostureCache, CachedFleetPosture, ServiceState,
│                               OperationalCommand, should_route_command, POSTURE_CACHE_TTL_MS
├── posture_engine.rs         — recalculate_and_broadcast, derive_fleet_posture,
│                               next_generation, init_generation_from_store, POSTURE_GENERATION
├── posture_engine_v2.rs      — LockoutReason, PostureRecalcTrigger, PostureEngineSender,
│                               start_posture_engine_worker, resolve_posture_with_reason
├── recovery_hysteresis.rs    — evaluate_recovery_report, HysteresisDecision,
│                               AV_RECOVERY_STREAK_THRESHOLD (5), AV_RECOVERY_WINDOW_MS (10s)
├── telemetry_watchdog.rs     — spawn_telemetry_watchdog; AV_TELEMETRY_TIMEOUT_MS (2s),
│                               AV_TELEMETRY_WARN_MS (1s), AV_WATCHDOG_SWEEP_MS (100ms)
├── clock.rs                  — Clock trait, SystemClock, VirtualClock, SharedClock
├── scenario_runner.rs        — ScenarioRunner, ScenarioEvent, PostureAssertion, AssertionResult
├── standby_monitor.rs        — spawn_heartbeat_writer, spawn_promotion_monitor,
│                               HEARTBEAT_INTERVAL_MS (2s), PROMOTION_TIMEOUT_MS (10s)
├── federation.rs             — FederatedTrustReport, Ed25519 verify, evaluate_federated_report
├── federation_reconciliation.rs — FederatedTrustReportV2, reconcile_reports,
│                               ReconciliationOutcome, authoritative_posture
├── audit_chain.rs            — SHA-256 hash-chained audit log
├── kinematics_contract.rs    — KinematicContract, scalar clamping
├── kinematics_sim.rs         — re-export shim → kirra_core::kinematics_sim (relocated
│                               Stage 7; VehicleState, apply_enforcement, run_simulation)
├── capture.rs                — re-export shim → kirra_core::capture (relocated Stage 7;
│                               record_from_verdict, spawn_capture_writer; needs the
│                               kirra-core `capture` feature, enabled in the SDK manifest)
├── action_filter.rs          — ActionFilter<C>, ActionClaim, evaluate_action_claim
├── action_policy.rs          — UnstructuredTextParser (LLM JSON → typed AgentAction)
├── security.rs               — constant_time_compare
├── protocol_adapter.rs       — Modbus/OPC-UA industrial event mapping
├── kirra_core.rs             — KirraKernelGovernor (scalar clamping, rate limiting)
├── ros2_adapter.rs           — NaN/Inf rejection before ROS2 publish
├── dds_bridge.rs             — CDR encapsulation, Volatile durability
├── standby_monitor.rs        — HA heartbeat writer and promotion monitor
├── startup_sentinel.rs       — Pre-flight invariant checks at startup
├── config.rs                 — Configuration loading helpers
├── audit_log.rs              — Audit log helpers
├── metrics.rs                — Metrics collection
├── health.rs                 — Health check utilities
├── tpm.rs                    — TPM attestation support (optional feature)
├── ffi.rs                    — C FFI bindings
├── wcet_gate.rs              — Governor verdict WCET CI guard (O(1) structural
│                               boundedness argument; GOVERNOR_VERDICT_WCET_*_MICROS)
├── gateway/
│   ├── mod.rs
│   ├── policy.rs             — classify_command (path+method → OperationalCommand)
│   ├── policy_layer.rs       — Tower KirraPolicyLayer/KirraPolicyService
│   ├── cmd_vel.rs            — CmdVel validation, DEFAULT_CMD_VEL_LIMITS
│   ├── interceptor.rs        — gateway interceptor
│   ├── kinematics_contract.rs — VehicleKinematicsContract, validate_vehicle_command
│   ├── kinematics_proptest.rs — property-based tests for validate_vehicle_command
│   └── perception_monitor.rs — re-export shim → kirra_core::perception_monitor (relocated
│                               Stage 7; KinematicPlausibilityContract, apply_perception_cap)
└── bin/
    ├── kirra_verifier_service.rs  — axum HTTP service, all route handlers
    └── kirra_carla_client.rs      — CARLA simulator integration client
```

### SQLite Tables

| Table | Purpose |
|-------|---------|
| `nodes` | Registered node registry (trust state, AK PEM, PCR16) |
| `dependencies` | Dependency graph edges |
| `posture_events` | Time-series posture event log |
| `av_subsystem_meta` | AV sensor confidence floors, recovery streaks, last telemetry timestamps |
| `posture_engine_state` | Persistent generation counter + arbitrary key-value store for engine state |
| `audit_log_chain` | SHA-256 hash-chained tamper-evident ledger |
| `federated_trust_reports` | Accepted cross-controller reports |
| `trusted_federation_controllers` | Ed25519 public key registry |
| `federation_report_nonces` | Burned nonces (replay prevention) |
| `attestation_identity_registry` | Hardware fingerprint (AK public key digest) per node |

---

## Workspace Crates — the doer-checker / planning / perception side

Everything above documents the **`kirra-verifier`** root crate. The AV/Occy stack lives in
sibling crates. The load-bearing thesis: **a planner (the DOER) PROPOSES a trajectory; KIRRA
(the CHECKER) BOUNDS it.** The doer is swappable (geometric, learned, LLM-driven) and is never
trusted for safety; the checker is the invariant.

| Crate | Role |
|-------|------|
| `crates/kirra-core` | Lean shared types (no heavy deps): `corridor` (`CorridorSource`, `Point`, `MockCorridorSource`), `trajectory` (`PerceivedObject`, `Pose`, `TrajectoryPoint`, `TrajectoryVerdict`), `containment` (`MAX_TRAJECTORY_HORIZON`), `FleetPosture`, `kinematics_sim`, `capture`, `perception_monitor`, `KirraKernelGovernor`. Almost everything else depends on this, NOT on the heavy adapter. |
| `crates/kirra-ros2-adapter` | **The #131 Option-B CHECKER (re-export wiring) + ROS 2 node.** The checker modules named below — `validation.rs`, `prediction.rs`, `perception_redundancy.rs`, `state.rs` — actually live in the lean **`crates/kirra-trajectory`** crate and are re-exported here; the adapter itself adds only the `ros2`-gated `node.rs`. `validation.rs` — `validate_trajectory_slow` / `validate_trajectory_slow_capped`: containment + per-pose kinematics + **RSS** (the §4 conjunction: danger needs BOTH longitudinal AND lateral unsafe) + **occlusion (RSS Rule 4)** + **multi-modal predictive RSS** (`predictive_rss_breach` over `PredictedMode`s). `prediction.rs` — the multi-modal **mode producer** (`predicted_modes_from_objects` / `slow_loop_modes`: CV always, CTRV when a tracker yaw is fresh). `perception_redundancy.rs` — the True-Redundancy `cross_check` + `resolve_redundancy_cap`. `state.rs` — `AdaptorState` (primary + secondary object channels, yaw channel). `node.rs` (**ros2-gated**) — slow/fast dual-rate loops, subscriptions. |
| `crates/kirra-planner` | **Occy, the geometric DOER + the Mick intent seam.** `GeometricPlanner` / `Planner` trait / `PlanInput` / `PlanOutput`. `mick.rs` — `plan_for_intent` grounds a `MickIntent` (`GoTo` / `LaneChange` / `Cruise` / `Overtake` / `PullOver` / `TurnAt` / **`RouteTo`** multi-junction). `learned.rs` — `LearnedPlanner` (speed-only Hydra-MDP) + **`LearnedManeuverPlanner`** (2-D lateral×speed vocabulary, routes around). `behavior.rs` — `TrafficControl` (signs/signals + **`OccludedApproach`** speed cap). `fast_loop.rs`, `mick_llm.rs`, `mick_capture.rs`. |
| `crates/kirra-map` | **Lanelet2-lite lane graph** (`kirra_map::lanemap`). `LaneGraph` (`route` Dijkstra, `route_corridor` / `route_drivable` stitch a multi-junction corridor, `route_to_point`, right-of-way / `junction_context`, **occlusion `sight_distance`**), `Lane`, `LaneCorridor`, `LineType` / `lane_lines`. Re-exported by `kirra-planner`. |
| `crates/kirra-taj` | **Taj, the R2 perception layer (ADR-0015).** Phase-A geometric corridor/objects from lidar; Phase-B semantic fusion (`clip_corridor_to_hazards` / `binding_hazard` / `hazard_clip_x` — water/obstacle hazards tighten the corridor); **`SemanticEvalSummary`** — the safety-weighted perception eval harness (`UnsafeMiss` / `OverConservative` / `Correct`, `hazard_recall`). |
| `crates/kirra-mick` | Mick examples / eval harness binaries (`mick_intersection`, `mick_eval`). |
| `crates/kirra-fleet-transport` | Zenoh-backed fleet transport (ADR-0007). |
| `crates/kirra-governor-service`, `kirra-proposal-bench`, `kirra-wire-client` | Two-box prototype tools (UDP governor + proposal sweep + shared wire mirror; ADR-0001). |
| `crates/kirra-capture-schema`, `kirra-collector` | Governor-correction capture wire schema + collector (supervised-learning data path). |
| `parko/` (separate workspace) | **The ML + diverse-governor side.** `parko-core` (`SafetyGovernor` trait, `SafetyPosture` with `escalate()`, RSS, `InferenceLoop` scheduler, `detector`), `parko-kirra` (`KirraGovernor`, `GovernorComparator` — two diverse governors, divergence accumulator → `recommended_posture()`), `parko-ros2` (`run_pipeline_tick` — divergence escalates the effective posture), `parko-onnx`/`parko-openvino`/`parko-tensorrt` (inference backends, hardware/CI-gated). |

### Doer-checker key algorithms (planner / checker side)

**RSS §4 conjunction** (`validate_trajectory_slow`): a collision needs the object unsafe
LONGITUDINALLY **and** LATERALLY at once. The lateral side-RSS fires only when abreast
(`lon_unsafe`) OR the object is closing laterally (a cut-in). This admits a safe stationary
queue / a stopped lead the ego halts behind (was over-rejected by the reaction-time swerve term).

**Multi-modal predictive RSS** (gap #3, LIVE): the snapshot RSS evaluates an object at its
CURRENT position; the predictive pass rolls each `PredictedMode` forward in TIME and checks the
time-matched ego pose — catching a cut-in / turn-in the snapshot filtered as laterally clear.
Worst-case over modes (one dangerous hypothesis refuses). Producer: `predicted_modes_from_objects`
(CV always; CTRV when the tracker yaw feed is fresh — stale yaw degrades to CV-only, not a fault).

**Perception-divergence monitor** (gap #2b, True-Redundancy, LIVE): `cross_check` requires two
independent perception channels to AGREE; a divergence (phantom / miss / speed mismatch) OR a
silent secondary (redundancy lost) → `resolve_redundancy_cap` → `Some(0.0)` MRC-floor cap,
composed into the Track-C `apply_perception_cap` derate. Env-gated (`KIRRA_PERCEPTION_REDUNDANCY_ENABLED`).

**Occlusion-aware speed bound at junctions** (gap #1): `behavior::OccludedApproach` caps the
approach speed to the assured-clear-distance speed (RSS Rule 4) for the junction's sight distance,
so the ego CREEPS into a blind junction. Sight distance carried per approach-lane on `LaneGraph`.

**Multi-junction routing** (`MickIntent::RouteTo`): resolve ego + destination lanes, `route_to_point`
(Dijkstra picks the turn at each junction), materialize the stitched `route_corridor`, follow it.
Re-resolved from the ego pose each tick (receding horizon). KIRRA bounds the corridor.

**Learned doer** (`learned.rs`): a fixed trajectory vocabulary scored by a seeded-ES-fit MLP,
distilled from a `Teacher` (`SafetyAware` vs `ProgressOnly`). `LearnedManeuverPlanner` adds a 2-D
(lateral offset × speed) vocabulary so the net can ROUTE AROUND — KIRRA admits a band-clearing
pass that fits the corridor, rejects one that doesn't or a misaligned straight-through.

### Doer-checker invariants (NEVER violate)

- The planner only **PROPOSES**; the checker (`validate_trajectory_slow*`) is the sole safety
  authority. A planner change must keep its nominal output **checker-admissible**.
- `PlanOutput::safe_stop` (the always-available MRC proposal) must always exist — a planner with
  no stop output deadlocks the loop.
- The RSS §4 **conjunction** (lateral fires only on abreast-OR-cut-in) must not regress to
  lateral-on-proximity-alone (it over-rejects safe stationary objects).
- New predictive bounds (occlusion, multi-modal, divergence) are **derate-only / fail-closed**:
  absent input → no-op (byte-identical Nominal WCET path); a fault → an MRC-floor cap via
  `apply_perception_cap`, never a relaxation. The WCET-critical per-pose `validate_vehicle_command`
  path is UNCHANGED.

---

## Route Authorization Matrix

### Tier 1 — Identity-gated (admin token + `x-kirra-client-id` header)
- `GET  /system/posture/stream` — SSE broadcast of posture events
- `POST /federation/reports/submit` — Submit signed federated trust report
- `POST /action_filter/evaluate` — Evaluate action claim against posture
- `POST /industrial/evaluate` — Evaluate Modbus/OPC-UA industrial event

### Tier 2 — Admin-only (Bearer `KIRRA_ADMIN_TOKEN`)
- `POST /attestation/register` — Register a node
- `POST /fleet/dependencies` — Register dependency graph edges
- `POST /system/backup/export` — Full state dump
- `GET  /system/audit/verify` — Verify audit chain integrity
- `POST /federation/controllers/register` — Register trusted peer controller
- `POST /attestation/identity/register` — Register hardware fingerprint

### Unauthenticated (challenge-response provides its own guarantee)
- `POST /attestation/challenge/:node_id`
- `POST /attestation/verify`

### Public read-only
- `GET /health`, `GET /ready`
- `GET /attestation/status/:node_id`
- `GET /fleet/posture`, `GET /fleet/posture/:node_id`
- `GET /fleet/history/:node_id`, `GET /fleet/flapping/:node_id`
- `GET /federation/reports/:asset_id`

---

## Key Constants

```rust
// verifier.rs
MAX_DEPENDENCY_DEPTH        = 10          // DAG traversal depth limit
CHALLENGE_TTL_MS            = 30_000      // nonce expiry (30 seconds)
POSTURE_BROADCAST_CAPACITY  = 1024        // SSE broadcast ring buffer

// posture_cache.rs (re-exported via posture_engine.rs)
POSTURE_CACHE_TTL_MS        = 5_000       // cache staleness TTL (5 seconds)

// federation.rs
FEDERATION_REPLAY_WINDOW_MS = 5_000       // max report age (5 seconds)

// recovery_hysteresis.rs
AV_RECOVERY_STREAK_THRESHOLD = 5          // consecutive healthy reports required
AV_RECOVERY_WINDOW_MS        = 10_000     // streak window (10 seconds)

// telemetry_watchdog.rs
AV_WATCHDOG_SWEEP_MS         = 100        // sweep interval
AV_TELEMETRY_WARN_MS         = 1_000      // warn threshold (1 second silence)
AV_TELEMETRY_TIMEOUT_MS      = 2_000      // fault threshold (2 seconds silence)
AV_WATCHDOG_NODE_REFRESH_MS  = 30_000     // node list refresh from SQLite

// standby_monitor.rs
HEARTBEAT_INTERVAL_MS        = 2_000      // primary→standby heartbeat
PROMOTION_TIMEOUT_MS         = 10_000     // standby promotes if primary silent 10s
```

---

## Key Algorithms

**Gray/Black DAG Traversal** (`AppState::recursive_calculate`):
- Gray set = nodes currently on the active call stack (cycle detection)
- Black set = nodes fully evaluated (memoization, handles diamond DAGs)
- Cycle or depth ≥ 10 → `FleetPosture::LockedOut` with `CYCLE_DETECTED` tag
- LockedOut dep propagates LockedOut (not Degraded) upward

**`should_route_command(cache, now_ms, command)`**:
- `Unknown` → `false` immediately (before posture check)
- Stale cache (TTL exceeded) → `false`
- `LockedOut` → blocks everything
- `Degraded` → allows `ReadTelemetry` AND `ActuatorMotion` only (Option A / ADR-0011):
  `ActuatorMotion` is the one write classification (`POST /actuator/motion/command`,
  exact match) mounted behind the inner `enforce_actuator_safety_envelope` decel gate,
  so the outer gate defers its Degraded verdict to that gate instead of 503-ing it.
  Every other `WriteState` / `SystemMutation` is still denied in Degraded.
- `Nominal` → allows all except `Unknown`

**Degraded = Controlled Decel-to-Stop-and-HOLD** (`enforce_degraded_decel_to_stop`, issue #70):
- Degraded is NOT a sustained reduced-speed crawl. The kinematic Governor admits a
  command in Degraded ONLY if all hold: (a) within the MRC envelope (the
  *decel-trajectory bound*, via `validate_vehicle_command` against the MRC contract);
  (b) non-increasing speed `|proposed| <= |current|` → else `DenyCode::DegradedSpeedIncreaseDenied`;
  (c) no re-initiation — if `|current| <= STOP_EPSILON_MPS` (0.05), any `|proposed| > STOP_EPSILON_MPS`
  → `DenyCode::DegradedReinitiationDenied` (HOLD at zero); a reversal through a stop is also re-initiation.
- A denied command → MRC controlled stop; the Governor never authors re-acceleration.
- Implemented at four enforcement points: gateway `enforce_actuator_safety_envelope`,
  fabric `AssetGovernor::evaluate_command`, ros2-adapter `validate_trajectory_slow`,
  parko-kirra `KirraGovernor::apply_mrc_profile` (the last also gates an independent
  angular-velocity channel via `STOP_EPSILON_RAD_S` for differential drive). **REACHABILITY
  (#405 / ADR-0011, Option A adopted):** all four enforcement points are now live. The three
  *direct* callers (fabric / parko-kirra / ros2-adapter) invoke the gate directly; the gateway
  `enforce_actuator_safety_envelope` branch is now **reachable on the HTTP
  `/actuator/motion/command` path** because the outer `enforce_posture_routing` gate classifies
  that exact route as `OperationalCommand::ActuatorMotion` and `should_route_command` admits it
  under Degraded (deferring the verdict to the inner decel gate) — every OTHER `WriteState`
  stays 503 under Degraded. The `503 → 0.0` consumer safe-stop (#405) remains the defense-in-depth
  safety floor for the LockedOut/stale 503s and any non-gated write. Auth note: on the assembled
  router the actuator route is still admin-gated, so the auth-free Degraded-deferral proof lives
  in `tests/posture_gate_integration.rs` (INV-13 forbids `set_var` in the binary-internal test).
- The Nominal WCET-critical `validate_vehicle_command` path is UNCHANGED.
- "MRC" disambiguation: Degraded MRC = decel-to-stop *envelope* (bounds a converging
  command); LockedOut MRC fallback = safe-stop *maneuver* (all commands denied).
- Motivation: Cruise SF Oct-2023 post-stop pullover-drag (~3 m/s, under a 5 m/s crawl
  ceiling). Recovery is AUTOMATIC on return to Nominal (contrast LockedOut human-reset).
  See `docs/safety/SAFE_STATE_SPECIFICATION.md` SS-002.

**AV Recovery Hysteresis** (`evaluate_recovery_report`):
- Node is `Untrusted` after a fault (hw_fault or confidence < floor)
- Recovery requires `AV_RECOVERY_STREAK_THRESHOLD` (5) consecutive healthy reports
- All reports must arrive within `AV_RECOVERY_WINDOW_MS` (10s) — window expiry resets streak
- A new fault during recovery resets the streak to 0

**Posture Engine Worker** (`start_posture_engine_worker`):
- Replaces direct `recalculate_and_broadcast()` calls with mpsc channel sends
- Worker drains all buffered triggers (coalescing) then calls recalculate once
- Prevents burst recalculations when multiple sensors fault simultaneously
- Channel capacity: 128 triggers; full channel returns `Err` to sender

**Generation Persistence** (`init_generation_from_store`, `posture_engine_state` table):
- `POSTURE_GENERATION` AtomicU64 survives restarts by persisting to SQLite
- On boot: `init_generation_from_store` loads last value and sets the atomic
- On each recalculation: generation is written back via `save_last_generation`
- Prevents generation time-reversal across restarts (federation peers rely on monotonicity)

**Ed25519 Federation Verification** (5-step pipeline):
1. `evaluate_federated_report` — structural + freshness + replay window
2. `load_trusted_federation_controller_key` — identity check
3. `verify_federated_report_signature` — Ed25519 cryptographic verification
4. `has_seen_federation_nonce` — replay prevention
5. `save_federated_report_chained` — atomic commit (burns nonce + audit chain)

**HA Promotion** (`standby_monitor.rs`):
- Primary writes a heartbeat timestamp to `posture_engine_state` every `HEARTBEAT_INTERVAL_MS`
- Standby polls for the heartbeat every `PROMOTION_POLL_MS` (1s)
- If primary is silent for `PROMOTION_TIMEOUT_MS` (10s), standby promotes via `mode_active.compare_exchange`

---

## Dependencies (key versions)

```toml
axum = "0.8"
tokio = { version = "1", features = ["full"] }
tower = { version = "0.5", features = ["util"] }
dashmap = "6"
rusqlite = { version = "0.31", features = ["bundled"] }
ed25519-dalek = { version = "2", features = ["rand_core"] }
base64 = "0.22"
tokio-stream = { version = "0.1", features = ["sync"] }
reqwest = { version = "0.12", features = ["blocking", "json"] }
hmac = "0.12"
sha2 = "0.10"
hex = "0.4"
http = "1"
tracing = "0.1"
proptest = "1"  # dev-dependency
```

---

## Environment Variables

| Variable | Required | Default | Purpose |
|----------|----------|---------|---------|
| `KIRRA_ADMIN_TOKEN` | Yes (mutation routes) | — | Bearer token; absent/empty → 503 |
| `KIRRA_VERIFIER_MODE` | No | `active` | `passive_standby` → read-only; runtime-mutable via `mode_active` AtomicBool |
| `KIRRA_DB_PATH` | No | `kirra_verifier.sqlite` | SQLite file path |
| `KIRRA_VERIFIER_ADDR` | No | `0.0.0.0:8090` | Listen address |
| `KIRRA_TRUSTED_INGRESS_MODE` | No | `false` | Enable client-id header enforcement |
| `KIRRA_CLIENT_ID_HEADER` | No | `x-kirra-client-id` | Header name for identity-gated routes |
| `KIRRA_INSTANCE_ID` | No | hostname | Unique ID for HA deployments (heartbeat key) |
| `KIRRA_HEARTBEAT_INTERVAL` | No | `2000` | HA heartbeat write interval (ms) |
| `KIRRA_PROMOTION_TIMEOUT` | No | `10000` | Standby promotes if primary silent this long (ms) |
| `KIRRA_SUPERVISOR_RESET_KEY` | Yes (reset ops) | — | Must be non-empty, ≤ 64 bytes |
| `KIRRA_CANOPEN_NODE_MAP` | No | — | CANopen node-id → fleet-node-id map (#84), `canid:fleet_node` comma-separated (e.g. `5:robot-01,6:robot-02`). Unset → every NMT-offline is unattributed (fail-closed) |
| `KIRRA_FABRIC_ASSET_ID` | No | — | Local fabric asset id fed by the verifier→fabric posture feed (#88). Unset/empty → feed inert (asset keeps its `Degraded` registration seed) |
| `KIRRA_DNP3_ANALOG_OUTPUT_ENVELOPE` | No | — | DNP3 Analog Output (g41) magnitude envelope as `min:max` (e.g. `-100.0:100.0`). A control write (Operate/Direct_Operate) whose decoded setpoint is outside the envelope is denied. Unset/invalid → analog control writes are **denied (fail-closed)**; faithfully-undecodable g41 payloads are also refused (never fabricated) |
| `KIRRA_CANOPEN_SDO_BOUNDS` | No | — | Per-target CANopen SDO expedited-download magnitude bounds, `node:index:subindex=type:min:max` comma-separated (e.g. `5:0x6042:0=i16:-500:500`). `type` ∈ {i8,u8,i16,u16,i32,u32,f32}. A download to a configured target is faithfully decoded **by the configured type** (the OD entry — the frame carries width at best, never type; #85) and bounded: out-of-range/undecodable/segmented/width-mismatch → denied. Unconfigured targets are posture-only. Unset → SDO writes are posture-only |
| `KIRRA_CANOPEN_STRICT_BOUNDS` | No | `false` | `1`/`true` → a CANopen SDO **download** to a target with NO configured bound is denied (high-assurance mode) instead of posture-only. Reads/uploads/non-SDO frames are unaffected |
| `KIRRA_CIP_ATTR_BOUNDS` | No | — | Per-attribute CIP (EtherNet/IP) magnitude bounds, `class:instance:attr=type:min:max` comma-separated (e.g. `0x0A:1:3=i16:-500:500`). All keys `u16` (decimal or `0x`-hex); `type` ∈ {i8,u8,i16,u16,i32,u32,f32,f64}. A `Set_Attribute_Single` (0x10) write to a configured target is faithfully decoded **by the configured type** (the CIP attribute's data type — the frame carries only bytes; #85) and bounded: out-of-range/undecodable → denied. Other services (reads / `Write_Tag` / `Execute_Service`) carry no faithfully-located scalar → posture-only. Unconfigured targets posture-only. Unset → CIP writes posture-only |
| `KIRRA_CIP_STRICT_BOUNDS` | No | `false` | `1`/`true` → a CIP `Set_Attribute_Single` to a target with NO configured bound is denied (high-assurance mode) instead of posture-only. Reads / other services are unaffected |

**`kirra-ros2-adapter` slow-loop env gates** (consumed in `node.rs`, opt-in, default off →
byte-identical prior behaviour): `KIRRA_PERCEPTION_DERATE_ENABLED` (Track-C perception-derate cap),
`KIRRA_PERCEPTION_REDUNDANCY_ENABLED` (the True-Redundancy divergence monitor — enables the
`~/input/objects_secondary` channel-B subscription; enabled-but-silent-B → fail closed),
`KIRRA_SUBSCRIPTION_STALENESS_MS` (subscription/channel freshness budget).

---

## EPIC #270 — Governor transport / QNX partition lane

The governor command path is moving to **Rust end-to-end** on a QNX safety
partition; the Autoware/ROS 2 planner is an isolated guest. The C ABI/FFI is
demoted to the C/C++ integration boundary (**ADR-0006 Clause 3**) — not the hot path.

- **`tools/qnx-rtm-harness/`** (#271/#272) — C++ **shim** (driver: header-tear /
  bounds / CRC) → Rust **judge** (checker: `kirra_judge.rs` — the contract verdict on
  a shim-stabilized snapshot). Built **g++ + rustc directly (no cargo)**; the judge is
  `no_std`, `panic=abort`, zero-alloc. The FDIT/RTM matrix gates on **VERDICT
  CORRECTNESS** only; every row is traced to the kernel RTM (`QNX_MAPPING.md`, #272).
  The concern split is load-bearing: memory faults die in the driver, contract faults
  reach the judge. Sequence rule mirrors the kernel: `sequence <= last_accepted ⇒
  reject` (equal = replay).
- **`tools/iceoryx2-spike/`** (#273) — host-side iceoryx2 feature-subset spike
  (seqlock-style owned snapshot; same `<=` replay rule; `src/judge.rs`).
- **`docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md`** (KIRRA-OCCY-HVCHAN-001, #278) —
  frozen `#[repr(C)]` pointer-free `GovernorContractView` over hypervisor shared
  memory; 7-step seqlock write/read trust chain; **two-clock-domain model (§5)** with
  the normative **non-mixing rule** (safety/boundary timing vs system timing).
- **`docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`** (KIRRA-OCCY-WCET-METH-001,
  #274/#279) — measurement-based timing-evidence strategy; `src/wcet_gate.rs` holds the
  O(1) structural boundedness argument + the CI guard.
- **`AOU-TIMESYNC-001`** (`ASSUMPTIONS_OF_USE.md`) — integrator timestamps must be
  synchronized/monotonic and **converted to the boundary clock domain before publish**.

**Invariant — host timing is INDICATIVE, never WCET.** Only QNX-target-under-FIFO
numbers feed an FTTI claim (the harness/spike banners + the methodology enforce this;
the harness CSV carries `wcet_status = TBD-QNX-TARGET`).

---

## Common Mistakes to Reject

- Using `State<Arc<AppState>>` in handlers — correct type is `State<Arc<ServiceState>>`
- Calling `should_route_command` with 2 args — signature is `(cache, now_ms, command)`
- Importing `FleetPosture` from `crate::gateway::posture_cache` — correct path is `crate::verifier::FleetPosture`
- Using `node.trust_state` — the field is `node.status` on `RegisteredNode`
- Using `app.deps` — the field is `app.dependency_graph` on `AppState`
- Calling `app.store.method()` directly — store is `Arc<Mutex<VerifierStore>>`; use `app.store.lock().unwrap().method()`
- Calling `cache.read().await` on `SharedPostureCache` in sync code — use `cache.blocking_read()` or restructure as async
- Replacing `admin_routes` router structure without accounting for all existing protected routes
- Using `PostureCache::new()` — type doesn't exist; use `Arc::new(tokio::sync::RwLock::new(Some(CachedFleetPosture::new(...))))`
- Adding `TransientLocal` durability to DDS topics
- Removing the `Unknown` early-return from `should_route_command`
- Calling `recalculate_and_broadcast` directly from a handler — route through `PostureEngineSender` to coalesce bursts
- Using `SystemTime::now()` inside time-dependent functions — accept a `now_ms: u64` parameter for testability
