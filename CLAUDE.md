# Kirra — Claude Code Context

## Project Identity

- **Crate**: `kirra-runtime-sdk` (lib + bin dual-crate, `crate-type = ["rlib", "cdylib"]`)
- **Edition**: 2021
- **Primary binary**: `kirra_verifier_service` (`src/bin/kirra_verifier_service.rs`)
- **Secondary binary**: `kirra_carla_client` (`src/bin/kirra_carla_client.rs`)
- **Test suite**: `cargo test`
- **Remote**: `justinlooney/kirra-runtime-sdk`
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
├── kinematics_sim.rs         — VehicleState, SimulationResult, apply_enforcement, run_simulation
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
├── gateway/
│   ├── mod.rs
│   ├── policy.rs             — classify_command (path+method → OperationalCommand)
│   ├── policy_layer.rs       — Tower KirraPolicyLayer/KirraPolicyService
│   ├── cmd_vel.rs            — CmdVel validation, DEFAULT_CMD_VEL_LIMITS
│   ├── interceptor.rs        — gateway interceptor
│   ├── kinematics_contract.rs — VehicleKinematicsContract, validate_vehicle_command
│   └── kinematics_proptest.rs — property-based tests for validate_vehicle_command
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
- `Degraded` → allows `ReadTelemetry` only
- `Nominal` → allows all except `Unknown`

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
