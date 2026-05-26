# Aegis Safety Kernel — Safety Architecture

Document ID: AEGIS-SA-001
Version: 1.0.0
Status: Draft
Classification: ISO 26262 Part 4 / Part 6
Date: 2026-05-23

---

## 1. Overview

The Aegis safety architecture is organized as three independent, sequentially ordered enforcement layers. A vehicle command must pass through all three layers before it can reach a downstream actuator. Each layer independently implements one or more safety goals and has its own failure mode and mitigation strategy. In addition, supplementary safety mechanisms provide cross-cutting protections that apply to multiple layers and safety goals.

This document references source file paths within the `aegis-runtime-sdk` crate (v1.5.0). All paths are relative to the crate root at `src/`.

---

## 2. Three-Layer Safety Architecture

```
AI Planner / LLM Controller
          |
          v
+---------------------------------------------+
|  Layer 1: Trust Graph Layer                  |
|  src/verifier.rs, src/telemetry_watchdog.rs  |
|  Evaluates node trust states via gray/black  |
|  DAG traversal; detects sensor timeouts      |
+---------------------------------------------+
          |
          v  (posture: Nominal / Degraded / LockedOut)
+---------------------------------------------+
|  Layer 2: Posture Derivation Layer           |
|  src/posture_engine.rs                       |
|  src/posture_engine_v2.rs                    |
|  src/posture_cache.rs                        |
|  Derives fleet posture; caches with TTL;     |
|  fails closed on stale cache                 |
+---------------------------------------------+
          |
          v  (posture-aware command gating)
+---------------------------------------------+
|  Layer 3: Enforcement Layer                  |
|  src/gateway/kinematics_contract.rs          |
|  src/gateway/policy_layer.rs                 |
|  src/posture_cache.rs                        |
|  Validates kinematic envelope; routes by     |
|  posture; denies Unknown unconditionally     |
+---------------------------------------------+
          |
          v
      Actuator Interface
```

---

## 3. Layer 1 — Trust Graph Layer

### 3.1 Purpose

Layer 1 maintains the per-node trust state for all registered fleet nodes and sensors, and derives the graph-level trust topology that feeds into fleet posture derivation. It is the authoritative source of trust state for all downstream layers.

### 3.2 Mechanism: Gray/Black DAG Traversal

**Mechanism ID:** M-001
**Safety Goals implemented:** SG-003, SG-007, SG-008
**Implementation location:** `src/verifier.rs` — `AppState::recursive_calculate()`

The trust graph is stored as two DashMaps on `AppState`:
- `AppState.nodes`: `DashMap<String, RegisteredNode>` — per-node trust state and metadata
- `AppState.dependency_graph`: `DashMap<String, Vec<String>>` — directed edges (node -> dependencies)

The DAG traversal algorithm uses two sets per evaluation:
- **Gray set:** nodes currently on the active call stack (cycle detection)
- **Black set:** nodes fully evaluated in this traversal (memoization, handles diamond DAGs)

Traversal rules:
- If a node is in the Gray set when encountered: cycle detected; the entire traversal result is `FleetPosture::LockedOut` with tag `CYCLE_DETECTED`.
- If traversal depth reaches or exceeds `MAX_DEPENDENCY_DEPTH = 10`: result is `FleetPosture::LockedOut`.
- If any dependency evaluates to `LockedOut`: the dependent node is also `LockedOut` (not merely `Degraded`).
- If all dependencies are `Trusted` and the node itself is `Trusted`: node posture is `Nominal`.
- If any dependency is `Untrusted` or `Unknown` but none are `LockedOut`: posture is `Degraded`.

**Test coverage:** DAG traversal unit tests including cycle detection, diamond DAG memoization, depth-limit lockout, and mixed trust state propagation.
**Failure mode:** DAG traversal produces incorrect posture due to concurrent modification of `AppState.nodes`.
**Mitigation:** `DashMap` provides shard-level locking; traversal reads a consistent snapshot per shard. For correctness during concurrent trust updates, the posture engine worker coalesces burst recalculations.

### 3.3 Mechanism: Telemetry Watchdog

**Mechanism ID:** M-002
**Safety Goals implemented:** SG-003
**Implementation location:** `src/telemetry_watchdog.rs` — `spawn_telemetry_watchdog()`

The telemetry watchdog is a dedicated async task that polls the last-seen telemetry timestamp for each registered AV sensor node at `AV_WATCHDOG_SWEEP_MS = 100 ms` intervals.

Key constants:
- `AV_TELEMETRY_WARN_MS = 1000`: log a warning if no telemetry received for 1 second
- `AV_TELEMETRY_TIMEOUT_MS = 2000`: transition node to `Untrusted` if no telemetry for 2 seconds
- `AV_WATCHDOG_NODE_REFRESH_MS = 30000`: refresh the monitored node list from SQLite every 30 seconds

On timeout detection, the watchdog transitions the node to `Untrusted(reason)` and sends a `PostureRecalcTrigger` to the posture engine worker channel, causing a posture recalculation within one channel drain cycle.

**Test coverage:** Unit test `test_watchdog_marks_node_untrusted_after_timeout` with VirtualClock injection.
**Failure mode:** Watchdog task panics or is not spawned.
**Mitigation:** `startup_sentinel` verifies that the watchdog task is spawned before the TCP listener binds. A panic in the watchdog task causes the tokio runtime to log an error; the missing watchdog is detectable via health check divergence.

---

## 4. Layer 2 — Posture Derivation Layer

### 4.1 Purpose

Layer 2 takes the per-node trust states from Layer 1, derives the aggregate fleet posture (Nominal / Degraded / LockedOut), maintains a time-limited cache of the derived posture, and fails closed (LockedOut) when the cache becomes stale.

### 4.2 Mechanism: Posture Engine

**Mechanism ID:** M-003
**Safety Goals implemented:** SG-003, SG-005, SG-007
**Implementation location:**
- `src/posture_engine.rs` — `derive_fleet_posture()`, `recalculate_and_broadcast()`
- `src/posture_engine_v2.rs` — `start_posture_engine_worker()`

`derive_fleet_posture()` applies the gray/black DAG traversal to all root nodes and aggregates the result into the three-state `FleetPosture` enum (`Nominal` / `Degraded` / `LockedOut`).

`recalculate_and_broadcast()` calls `derive_fleet_posture()`, increments the monotonic generation counter, writes the new posture to the `SharedPostureCache`, and broadcasts the posture event to SSE subscribers.

`start_posture_engine_worker()` runs an mpsc channel receiver loop that coalesces burst recalculation triggers (channel capacity: 128). The worker drains all pending `PostureRecalcTrigger` messages and then calls `recalculate_and_broadcast()` once, preventing multiple simultaneous DAG traversals when sensors fault in rapid succession.

The generation counter (`POSTURE_GENERATION` AtomicU64) is persisted to the `posture_engine_state` SQLite table via `save_last_generation()` and restored on restart via `init_generation_from_store()`. This ensures generation monotonicity across restarts, which federation peers rely on for replay prevention.

**Test coverage:** Posture engine unit tests, generation persistence round-trip test, burst coalescing test.
**Failure mode:** Posture engine worker task exits or channel becomes full (capacity 128).
**Mitigation:** Channel-full condition is logged as an error; the failing sender falls back to a direct recalculation call. Worker task restart is handled by the tokio runtime supervision hierarchy.

### 4.3 Mechanism: Posture Cache with TTL

**Mechanism ID:** M-004
**Safety Goals implemented:** SG-005
**Implementation location:** `src/posture_cache.rs` — `SharedPostureCache`, `CachedFleetPosture`

`SharedPostureCache` is defined as `Arc<tokio::sync::RwLock<Option<CachedFleetPosture>>>`.

`CachedFleetPosture` contains:
- `posture`: the derived `FleetPosture`
- `generated_at_ms`: the timestamp at which this posture was computed
- `ttl_ms`: the maximum age before this entry is considered stale (set to `POSTURE_CACHE_TTL_MS = 5000`)
- `generation`: the monotonic generation counter at the time of derivation

`resolve_posture_with_reason()` (in `src/posture_engine_v2.rs`) reads the cache and evaluates staleness:
- If `now_ms - generated_at_ms >= POSTURE_CACHE_TTL_MS`: returns `LockedOut(PostureCacheStale)`
- Otherwise: returns the cached posture

This ensures that clock skew, posture engine worker failure, or any other condition that prevents timely recalculation results in a fail-closed (LockedOut) posture rather than a stale Nominal pass-through.

**Test coverage:** Unit test `test_stale_cache_fails_closed_after_virtual_clock_advance` using VirtualClock injection.
**Failure mode:** Clock skew causes premature staleness, unnecessary lockout.
**Mitigation:** POSTURE_CACHE_TTL_MS is set conservatively (5000 ms) relative to the expected recalculation period (less than 200 ms under normal conditions). Spurious lockout is a safe failure mode.

---

## 5. Layer 3 — Enforcement Layer

### 5.1 Purpose

Layer 3 applies posture-aware command routing and hard kinematic envelope enforcement to every command before it reaches the actuator interface. This layer is the final defense against out-of-envelope commands and ensures that posture state is reflected in command routing.

### 5.2 Mechanism: Kinematic Contract Validation

**Mechanism ID:** M-005
**Safety Goals implemented:** SG-001, SG-002, SG-004
**Implementation location:** `src/gateway/kinematics_contract.rs` — `validate_vehicle_command()`

`validate_vehicle_command()` applies eight priority-ordered checks to each `VehicleKinematicsContract` against a proposed vehicle command. Priority ordering ensures that the most critical checks execute first and cannot be bypassed by later checks:

| Priority | Check | Safety Goal |
|----------|-------|-------------|
| 0 | NaN/Inf guard: all f64 fields checked with `!is_finite()` before any arithmetic | SG-004 |
| 1 | Zero-velocity fence: enforce absolute stop if contract requires stopped state | SG-001 |
| 2 | Linear velocity clamp: `linear_velocity_mps.abs() > max_speed_mps` results in ClampLinear | SG-001 |
| 3 | Reverse lockout: deny negative velocity if contract forbids reverse | SG-001 |
| 4 | Yaw rate clamp: `angular_velocity_radps.abs() > max_yaw_rate_radps` results in ClampAngular | SG-002 |
| 5 | Forward-only steering restriction | SG-002 |
| 6 | Bicycle model lateral acceleration: compute `v^2 / R` and clamp steering if result exceeds `max_lateral_accel_mps2` | SG-002 |
| 7 | Kinematics forward simulation validation | SG-001, SG-002 |

The hard velocity clamp (Priority 2) always executes before the rate-of-change limiter (implemented in `src/aegis_core.rs`), satisfying the invariant that the envelope cap always wins over rate priority.

**Test coverage:** Unit test suite including `test_speed_above_ceiling_triggers_clamp_linear`, `test_nominal_highway_speed_high_steering_clamps_steering`, `test_nan_linear_velocity_is_denied`, `test_inf_linear_velocity_is_denied`, plus the full proptest suite in `src/gateway/kinematics_proptest.rs`.
**Failure mode:** Incorrect contract loaded for current posture (e.g., Nominal contract applied during Degraded posture).
**Mitigation:** Contract selection is tied to posture resolution; SG-005 ensures stale posture fails closed, which bounds the window during which an incorrect contract could be applied.

### 5.3 Mechanism: Posture-Aware Command Routing

**Mechanism ID:** M-006
**Safety Goals implemented:** SG-005, SG-006
**Implementation location:** `src/posture_cache.rs` — `should_route_command()`

`should_route_command(cache, now_ms, command)` implements the posture-action matrix:

1. **Unknown early return:** If `command == OperationalCommand::Unknown`, return `false` immediately, before any posture evaluation.
2. **Stale cache check:** If `now_ms - cache.generated_at_ms >= POSTURE_CACHE_TTL_MS`, return `false` (fail-closed).
3. **LockedOut:** Return `false` for all commands.
4. **Degraded:** Return `true` only for `ReadTelemetry`; `false` for all others.
5. **Nominal:** Return `true` for all commands except `Unknown` (already handled in step 1).

The `OperationalCommand::Unknown` early return (step 1) is a protected invariant and must not be conditioned on posture state or removed.

**Test coverage:** Unit test `test_unknown_command_denied_in_all_posture_states` verifying all three posture states.
**Failure mode:** `should_route_command` invoked with a stale `now_ms` value (e.g., from a cached timestamp).
**Mitigation:** `now_ms` must be sourced from the injected Clock trait, never cached. The function signature forces the caller to provide `now_ms` explicitly.

### 5.4 Mechanism: Tower Policy Middleware

**Mechanism ID:** M-007
**Safety Goals implemented:** SG-006, SG-008
**Implementation location:** `src/gateway/policy_layer.rs` — `AegisPolicyLayer`

`AegisPolicyLayer` is a Tower middleware that wraps the axum HTTP service. For each incoming request, it:

1. Calls `classify_command()` (`src/gateway/policy.rs`) to map the HTTP path and method to an `OperationalCommand`.
2. Calls `should_route_command()` with the classified command and the current posture cache.
3. If routing is denied, returns an appropriate HTTP error response before the request reaches any route handler.
4. If routing is permitted, passes the request to the inner service (route handler).

This ensures that posture-based command gating is applied uniformly to all requests and cannot be bypassed by individual route handlers.

**Test coverage:** Integration tests confirming that requests to gated routes are blocked when posture is Degraded or LockedOut.
**Failure mode:** Middleware is not applied to a newly added route.
**Mitigation:** The middleware is applied at the router level, not per-route, so new routes are automatically covered. Code review requirement for any router modification (see AEGIS-CG-001).

---

## 6. Supplementary Safety Mechanisms

### 6.1 Action Filter

**Mechanism ID:** M-008
**Safety Goals implemented:** SG-006
**Implementation location:** `src/action_filter.rs` — `ActionFilter<C>`

The Action Filter evaluates `ActionClaim` objects from LLM-generated outputs against the current fleet posture. It provides a typed, structured evaluation path for agent frameworks (OpenAI function calling, LangChain tools) that bypasses the raw HTTP path classification used by `classify_command()`.

The Action Filter enforces the same posture-action matrix as `should_route_command()` and additionally validates the semantic content of the action claim against the registered action policy (`src/action_policy.rs`).

**Test coverage:** Action filter unit tests for each posture state and action type combination.
**Failure mode:** Action Filter not invoked for LLM-generated commands that arrive via a direct HTTP path (not the /action_filter/evaluate endpoint).
**Mitigation:** Documentation and API design encourage use of the action_filter endpoint for all LLM agent integrations.

### 6.2 Audit Chain

**Mechanism ID:** M-009
**Safety Goals implemented:** SG-010, SG-012
**Implementation location:** `src/audit_chain.rs` — `AuditChainLinker`

The audit chain provides a tamper-evident record of all safety-relevant events using SHA-256 hash chaining and Ed25519 signing. Each audit entry contains:
- The event payload (command, decision, reason, timestamp)
- `prev_hash`: SHA-256 of the previous serialized entry
- An Ed25519 signature over the entry content

`AuditChainLinker` verifies the chain by recomputing `SHA-256(previous_entry)` and comparing it to the `prev_hash` field in the subsequent entry. Any modification to any entry breaks the chain at that point, making the tamper detectable.

The audit chain is stored in the `audit_log_chain` SQLite table using WAL mode, ensuring writes are durable before in-memory state is updated.

**Test coverage:** Unit test `test_audit_chain_tamper_detection`, fuzz test for corrupted entries.
**Failure mode:** Audit write fails (disk full, SQLite error); the safety event is not recorded.
**Mitigation:** Audit write errors are logged at ERROR level but are not fatal to the enforcement decision (enforcement proceeds; the audit failure is itself audited to the application log). For DNP3 broadcast commands (SG-012), the audit write failure is fatal: the control output is blocked if the audit write fails.

### 6.3 Recovery Hysteresis

**Mechanism ID:** M-010
**Safety Goals implemented:** SG-013
**Implementation location:** `src/recovery_hysteresis.rs` — `evaluate_recovery_report()`

The recovery hysteresis evaluator enforces a two-dimensional gate on sensor node recovery:
- **Streak:** Exactly `AV_RECOVERY_STREAK_THRESHOLD = 5` consecutive healthy reports required
- **Window:** All reports in the streak must arrive within `AV_RECOVERY_WINDOW_MS = 10000 ms`

Both conditions must be satisfied simultaneously. A streak of 5 reports spread over 11 seconds does not satisfy the window condition and does not promote the node to Trusted.

The `HysteresisDecision` enum returns one of:
- `Recover`: streak and window satisfied; promote to Trusted
- `Continue`: streak in progress; remain Untrusted
- `Reset`: unhealthy report or window expired; reset streak to 0; remain Untrusted

**Test coverage:** Unit tests `test_recovery_requires_full_streak`, `test_streak_resets_on_gap`, temporal integration tests with VirtualClock.
**Failure mode:** Replay attack submits fabricated healthy reports at artificially high frequency to satisfy streak in minimal time.
**Mitigation:** The window condition bounds the maximum recovery rate. Nonce validation and Ed25519 signing on federation-sourced reports prevent fabrication for cross-controller scenarios.

### 6.4 HA Promotion Monitor

**Mechanism ID:** M-011
**Safety Goals implemented:** SG-009
**Implementation location:** `src/standby_monitor.rs` — `spawn_promotion_monitor()`

The HA promotion monitor implements the passive standby promotion logic:
- The primary instance calls `spawn_heartbeat_writer()`, which writes the current timestamp to the `posture_engine_state` table every `HEARTBEAT_INTERVAL_MS = 2000 ms`.
- The standby instance calls `spawn_promotion_monitor()`, which polls the heartbeat timestamp every 1000 ms.
- If the heartbeat age exceeds `PROMOTION_TIMEOUT_MS = 10000 ms`, the standby calls `mode_active.compare_exchange(false, true)` to atomically promote itself to the active role.
- After promotion, the standby begins full enforcement and starts writing heartbeats of its own.

The `compare_exchange` ensures that only one standby instance promotes in a split-brain scenario.

**Test coverage:** Unit test `test_standby_promotes_after_primary_timeout` with VirtualClock injection.
**Failure mode:** Both primary and standby lose access to the shared SQLite database simultaneously.
**Mitigation:** Both instances fail closed: without access to the shared state they cannot verify posture, and the fail-closed posture cache TTL causes LockedOut within 5 seconds.

### 6.5 Fabric Cross-Asset Trust Propagation

**Mechanism ID:** M-012
**Safety Goals implemented:** SG-007
**Implementation location:** `src/fabric/router.rs` — `propagate_cross_asset_trust()`

The fabric router applies four trust propagation rules when evaluating cross-asset dependencies:

1. If a leader asset posture is `LockedOut`, all registered follower assets are set to `Degraded`.
2. If a leader asset posture is `Degraded` and all followers are currently `Nominal`, followers are set to `Degraded`.
3. If a leader asset recovers to `Nominal` and all followers were `Degraded` due to leader state, followers may recover to `Nominal` after the leader's recovery is confirmed.
4. Isolated asset failures (not in a leadership role) do not propagate to followers.

The propagation result is fed into the posture engine worker channel, triggering a posture recalculation for affected assets.

**Test coverage:** Integration test `test_convoy_leader_lockout_degrades_followers`, fabric unit tests.
**Failure mode:** Fabric governor tick interval too long; propagation latency exceeds SG-007 FTTI.
**Mitigation:** Fabric governor tick interval is configurable; the default is set to meet the SG-007 FTTI target of 500 ms or less.

### 6.6 NaN/Inf Guard

**Mechanism ID:** M-013
**Safety Goals implemented:** SG-004
**Implementation location:** `src/gateway/kinematics_contract.rs` — `validate_vehicle_command()` Priority 0

The NaN/Inf guard checks every f64 field in the vehicle command for finiteness using `f64::is_finite()` before any arithmetic is performed. This is implemented as Priority 0 in the eight-check sequence, ensuring no subsequent check can operate on a non-finite value.

Checked fields include at minimum: `linear_velocity_mps`, `angular_velocity_radps`, and any additional f64 fields present in the command structure.

**Test coverage:** `test_nan_linear_velocity_is_denied`, `test_inf_linear_velocity_is_denied`, proptest generating random f64 including NaN/Inf.
**Failure mode:** New f64 field added to command structure without updating the guard.
**Mitigation:** Code review checklist item for any modification to the vehicle command struct; proptest coverage over the full command struct.

---

## 7. Safety Architecture Properties

### 7.1 Independence of Layers

The three enforcement layers are independent in the sense that:
- Layer 1 failure (incorrect trust state) causes posture to fail closed (Degraded or LockedOut) rather than fail open.
- Layer 2 failure (stale cache) causes posture to fail closed (LockedOut via PostureCacheStale) rather than allowing stale Nominal.
- Layer 3 failure (incorrect kinematic contract) is bounded by the contract parameters, which are validated at startup.

No single-point failure in any layer can cause a command to pass all three layers in an unsafe state.

### 7.2 Fail-Closed Property

Every mechanism is designed to fail closed:
- Missing admin token: 503, not pass-through
- Stale posture cache: LockedOut, not stale Nominal
- Unknown command: denied before posture check
- DAG cycle: LockedOut, not Nominal
- Audit write failure: logged, not silently dropped (except for non-broadcast DNP3 where it is tolerated)

### 7.3 Determinism

Time-dependent functions (`resolve_posture_with_reason`, `should_route_command`, `evaluate_recovery_report`) accept a `now_ms: u64` parameter rather than calling the system clock internally. This enables deterministic testing with VirtualClock injection and prevents hidden clock dependencies in the enforcement path.

---

## 8. Document Control

| Field | Value |
|-------|-------|
| Prepared by | Aegis Engineering |
| Review status | Pending TUV pre-assessment |
| Next review | 2026-11-23 |
| Supersedes | None |
