# Aegis — Claude Code Context

## Project Identity

- **Crate**: `aegis-runtime-sdk` (lib + bin dual-crate, `crate-type = ["rlib", "cdylib"]`)
- **Edition**: 2021
- **Primary binary**: `aegis_verifier_service` (`src/bin/aegis_verifier_service.rs`)
- **Test suite**: `cargo test` — currently **66 passing, 0 failing**
- **Branch**: `master` (remote: `justinlooney/aegis`)

---

## What This System Is

Aegis is a distributed runtime legitimacy engine and safety governor for AI-driven robotic and edge systems. It enforces fail-closed trust semantics across a heterogeneous fleet — preventing unsafe or unauthorized commands from reaching actuators regardless of what an AI model, LLM output, or upstream orchestration layer instructs.

---

## CRITICAL SECURITY INVARIANTS — NEVER VIOLATE THESE

These have been blocked or reverted multiple times. Any submission that violates them must be rejected outright.

1. **`require_admin_token` must never be commented out, bypassed, or removed** from any mutation route. It reads `AEGIS_ADMIN_TOKEN` from env; if absent or empty it returns 503 (fail-closed), never fail-open.

2. **`constant_time_compare` must be used** for all token comparisons. Standard `==` is forbidden on security-critical byte sequences.

3. **`verify_attestation` must never use `let status = NodeTrustState::Trusted` mock**. The HMAC-SHA256 proof must be computed and compared.

4. **`FleetNodePosture` and the gray/black two-set DAG algorithm must never be replaced with a mock**. The real traversal in `AppState::recursive_calculate` must remain intact.

5. **`pending_challenges: DashMap<String, ChallengeEntry>` must never be removed**. Nonces are volatile, never persisted, and expire after `CHALLENGE_TTL_MS = 30_000` ms.

6. **`AEGIS_ADMIN_TOKEN` must come from env var only**. No hardcoded fallbacks. Absent or empty → 503.

7. **`AEGIS_SUPERVISOR_RESET_KEY` must come from env var, no hardcoded fallbacks**. Must be present, non-empty, and ≤ 64 bytes.

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
| `AppState` | `src/verifier.rs:168` | DashMap nodes/deps/challenges, store, mode, posture_tx, transport_identity |
| `ServiceState` | `src/bin/aegis_verifier_service.rs:67` | Wraps Arc<AppState> + SharedPostureCache; is the axum router state |
| `FleetPosture` | `src/verifier.rs` | Nominal / Degraded / LockedOut |
| `FleetNodePosture` | `src/verifier.rs` | Per-node posture with blocked_by list |
| `NodeTrustState` | `src/verifier.rs` | Trusted / Untrusted(String) / Unknown |
| `OperationalCommand` | `src/posture_cache.rs` | ReadTelemetry / WriteState / SystemMutation / Unknown |
| `VerifierOperationMode` | `src/verifier.rs` | Active / PassiveStandby (from AEGIS_VERIFIER_MODE env) |
| `VerifierStore` | `src/verifier_store.rs` | rusqlite WAL-mode SQLite; all persistence |
| `PostureStreamEvent` | `src/verifier.rs` | Broadcast channel payload for SSE stream |
| `TransportIdentityConfig` | `src/verifier.rs` | trusted_ingress_mode + client_id_header from env |
| `FederatedTrustReport` | `src/federation.rs` | Ed25519-signed cross-controller trust report |
| `AuditChainLinker` | `src/audit_chain.rs` | SHA-256 hash-chained tamper-evident ledger |
| `SharedPostureCache` | `src/posture_cache.rs` | `Arc<RwLock<Option<CachedFleetPosture>>>` |
| `AegisPolicyLayer` | `src/gateway/policy_layer.rs` | Tower middleware; gates commands by posture |

### Module Map

```
src/
├── verifier.rs           — AppState, FleetPosture, DAG traversal, TransportIdentityConfig
├── verifier_store.rs     — SQLite persistence (nodes, events, audit chain, federation, identity registry)
├── posture_cache.rs      — SharedPostureCache, OperationalCommand, should_route_command
├── federation.rs         — FederatedTrustReport, Ed25519 verify, evaluate_federated_report
├── audit_chain.rs        — SHA-256 hash-chained audit log
├── action_filter.rs      — ActionFilter<C>, ActionClaim, evaluate_action_claim
├── protocol_adapter.rs   — Modbus/OPC-UA industrial event mapping
├── security.rs           — constant_time_compare
├── gateway/
│   ├── policy.rs         — classify_command (path+method → OperationalCommand)
│   ├── policy_layer.rs   — Tower AegisPolicyLayer/AegisPolicyService
│   ├── cmd_vel.rs        — CmdVel validation, DEFAULT_CMD_VEL_LIMITS
│   └── interceptor.rs    — gateway interceptor
├── aegis_core.rs         — AegisKernelGovernor (scalar clamping, rate limiting)
├── kinematics_contract.rs
├── action_policy.rs      — UnstructuredTextParser (LLM JSON → typed AgentAction)
├── ros2_adapter.rs       — NaN/Inf rejection before ROS2 publish
├── dds_bridge.rs         — CDR encapsulation, Volatile durability
└── bin/
    └── aegis_verifier_service.rs  — axum HTTP service, all route handlers
```

### SQLite Tables

| Table | Purpose |
|-------|---------|
| `nodes` | Registered node registry (trust state, AK PEM, PCR16) |
| `dependencies` | Dependency graph edges |
| `posture_events` | Time-series posture event log |
| `audit_log_chain` | SHA-256 hash-chained tamper-evident ledger |
| `federated_trust_reports` | Accepted cross-controller reports |
| `trusted_federation_controllers` | Ed25519 public key registry |
| `federation_report_nonces` | Burned nonces (replay prevention) |
| `attestation_identity_registry` | Hardware fingerprint (AK public key digest) per node |

---

## Route Authorization Matrix

### Tier 1 — Identity-gated (admin token + `x-aegis-client-id` header)
- `GET  /system/posture/stream` — SSE broadcast of posture events
- `POST /federation/reports/submit` — Submit signed federated trust report
- `POST /action_filter/evaluate` — Evaluate action claim against posture
- `POST /industrial/evaluate` — Evaluate Modbus/OPC-UA industrial event

### Tier 2 — Admin-only (Bearer `AEGIS_ADMIN_TOKEN`)
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
MAX_DEPENDENCY_DEPTH       = 10          // verifier.rs — DAG traversal depth limit
CHALLENGE_TTL_MS           = 30_000      // verifier.rs — nonce expiry (30 seconds)
POSTURE_BROADCAST_CAPACITY = 1024        // verifier.rs — SSE broadcast ring buffer
FEDERATION_REPLAY_WINDOW_MS = 5_000     // federation.rs — max report age (5 seconds)
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
- `Degraded` → allows ReadTelemetry only
- `Nominal` → allows all except Unknown

**Ed25519 Federation Verification** (5-step pipeline):
1. `evaluate_federated_report` — structural + freshness + replay window
2. `load_trusted_federation_controller_key` — identity check
3. `verify_federated_report_signature` — Ed25519 cryptographic verification
4. `has_seen_federation_nonce` — replay prevention
5. `save_federated_report_chained` — atomic commit (burns nonce + audit chain)

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
hmac = "0.12"
sha2 = "0.10"
hex = "0.4"
http = "1"
tracing = "0.1"
```

---

## Environment Variables

| Variable | Required | Default | Purpose |
|----------|----------|---------|---------|
| `AEGIS_ADMIN_TOKEN` | Yes (mutation routes) | — | Bearer token; absent/empty → 503 |
| `AEGIS_VERIFIER_MODE` | No | `active` | `passive`/`passive_standby`/`standby` → read-only |
| `AEGIS_DB_PATH` | No | `aegis_verifier.sqlite` | SQLite file path |
| `AEGIS_VERIFIER_ADDR` | No | `0.0.0.0:8090` | Listen address |
| `AEGIS_TRUSTED_INGRESS_MODE` | No | `false` | Enable client-id header enforcement |
| `AEGIS_CLIENT_ID_HEADER` | No | `x-aegis-client-id` | Header name for identity-gated routes |
| `AEGIS_SUPERVISOR_RESET_KEY` | Yes (reset ops) | — | Must be non-empty, ≤ 64 bytes |

---

## Common Mistakes to Reject

- Using `State<Arc<AppState>>` in handlers — correct type is `State<Arc<ServiceState>>`
- Calling `should_route_command` with 2 args — signature is `(cache, now_ms, command)`
- Importing `FleetPosture` from `crate::gateway::posture_cache` — correct path is `crate::verifier::FleetPosture`
- Replacing `admin_routes` router structure without accounting for all existing protected routes
- Using `PostureCache::new()` — type doesn't exist; correct type is `SharedPostureCache = Arc<RwLock<Option<CachedFleetPosture>>>`
- Adding `TransientLocal` durability to DDS topics
- Removing the `Unknown` early-return from `should_route_command`
