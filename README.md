# Kirra Runtime SDK

![Version](https://img.shields.io/badge/version-v1.1.1-blue)

A distributed runtime legitimacy engine and safety governor for AI-driven robotic and edge systems. Kirra enforces **fail-closed trust semantics** across a heterogeneous fleet — preventing unsafe or unauthorized commands from reaching actuators regardless of what an AI model, LLM output, or upstream orchestration layer instructs.

---

## AI Safety Integration

Kirra is the enforcement layer that prevents LLM hallucinations from reaching physical actuators — drop it between your AI agent and your robot fleet in minutes.

```
LLM output  →  Kirra Action Filter  →  Actuator
```

Every AI-generated command is evaluated against the live fleet posture before any hardware interaction occurs. A model that hallucinates a velocity of 999 m/s, invents a non-existent action type, or issues a kinetic command while the fleet is degraded is stopped at the software layer — and the attempt is permanently recorded in a SHA-256 hash-chained audit ledger.

### Posture-Action Matrix

| Action Type | Nominal | Degraded | LockedOut |
|-------------|---------|----------|-----------|
| `cmd_vel` (kinetic write) | ✓ with kinematics validation | ✗ | ✗ |
| `read_telemetry` | ✓ | ✓ | ✗ |
| Unknown / unrecognized | ✗ | ✗ | ✗ |

Compatible with **OpenAI function calling**, **LangChain tools**, or any agent framework that can make an HTTP POST.

**Docs:**
- [Action Filter Architecture](docs/action_filter.md) — pipeline, hallucination containment, API reference
- [LLM Integration Guide](docs/llm_integration_guide.md) — 5-minute quickstart, auth, agent loop patterns, SSE posture stream
- [OpenAI example](examples/openai_action_filter.py) — GPT function calling with Kirra safety filter
- [LangChain example](examples/langchain_action_filter.py) — `@tool` decorator pattern with Kirra safety filter

---

## Safety Certification

Kirra is targeting ASIL-D certification under ISO 26262 and SIL 3 under IEC 61508.

| Document | Doc ID | Status |
|----------|--------|--------|
| Hazard Analysis and Risk Assessment (HARA) | KIRRA-HARA-001 | Draft |
| Safety Goals | KIRRA-SG-001 | Draft |
| Safety Architecture | KIRRA-SA-001 | Draft |
| Requirements Traceability Matrix | KIRRA-RTM-001 | Draft |
| Coding Guidelines | KIRRA-CG-001 | Draft |
| Safety Standards Matrix (23 standards, 5 verticals) | KIRRA-STD-001 | Draft |
| ASTM F3269 Run Time Assurance Mapping | KIRRA-F3269-001 | Draft |
| IEC 61508 SIL 3 Preliminary Claim Mapping | KIRRA-61508-001 | Draft |

See [docs/safety/](docs/safety/) for the complete safety case foundation.
See [docs/safety/ROADMAP_TO_ASIL_D.md](docs/safety/ROADMAP_TO_ASIL_D.md) for the certification roadmap.
See [docs/safety/STANDARDS_MATRIX.md](docs/safety/STANDARDS_MATRIX.md) for the full 23-standard matrix with priority ratings and certification paths.

---

## Roadmap

Pre-execution architecture sketches for planned integrations. Each document
includes honest caveats, effort estimates, and explicit sequencing dependencies.

| Integration | Description | Status |
|-------------|-------------|--------|
| [Apollo AV Stack](docs/roadmap/APOLLO_KIRRA_INTEGRATION.md) | Cyber RT bridge between Apollo Control and Canbus — kinematic enforcement and lockout in the Apollo pipeline | Planned — after QNX + robot demo |
| [IEEE 2846 / RSS](docs/roadmap/RSS_KIRRA_INTEGRATION.md) | Behavioral safety invariants based on IEEE 2846 — safe distance enforcement given perception state | Planned — after Apollo integration |

See [docs/roadmap/](docs/roadmap/) for sequencing dependencies and execution plans.

---

## Overview

Modern robotic and autonomous deployments increasingly rely on AI models to generate operational commands. Kirra sits between those models and the physical actuators, acting as a cryptographically-grounded safety layer that:

- **Attests** each fleet node via HMAC-SHA256 challenge/response
- **Tracks trust posture** per-node and fleet-wide using a gray/black DAG traversal algorithm
- **Gates commands** based on live posture — locking out unsafe operations before they reach hardware
- **Monitors AV sensor health** with a configurable telemetry watchdog and hysteresis-based recovery
- **Enforces kinematics envelopes** — hard physical limits on velocity, acceleration, and yaw rate
- **Federates** trust across multiple controllers using Ed25519-signed reports
- **Audits** all state transitions via a SHA-256 hash-chained tamper-evident ledger
- **Supports HA deployments** with automatic passive-standby promotion

---

## Features

- **Fail-closed by design** — missing or invalid credentials yield `503`, never silent pass-through
- **Constant-time token comparison** — timing-safe token verification throughout
- **Gray/black DAG traversal** — cycle detection and diamond-DAG memoization for fleet dependency graphs
- **AV sensor watchdog** — per-node telemetry timeout detection (warn at 1 s, fault at 2 s)
- **Recovery hysteresis** — 5 consecutive healthy reports required over a 10 s window to restore trust
- **Posture engine worker** — mpsc channel coalesces burst faults into a single DAG recalculation
- **Generation persistence** — monotonic posture generation counter survives restarts via SQLite
- **Kinematics enforcement** — vehicle command envelope validation with forward simulation
- **SSE posture broadcast** — real-time fleet posture stream for subscribers
- **Industrial protocol support** — Modbus and OPC-UA event evaluation
- **DDS bridge** — CDR-encapsulated actuator topics with `Volatile` durability
- **Ed25519 federation** — cross-controller trust reports with replay prevention and nonce burning
- **Federation reconciliation** — generation-ordered conflict resolution for multi-controller deployments
- **HA standby/promotion** — heartbeat-based automatic promotion from passive standby to active
- **WAL-mode SQLite** — durable persistence with fail-closed write ordering (disk before memory)
- **Deterministic test harness** — `ScenarioRunner` with virtual clock injection for temporal integration tests
- **CARLA integration** — `kirra_carla_client` binary for AV simulator connectivity

---

## Architecture

```
src/
├── verifier.rs                — AppState, FleetPosture, DAG traversal, TransportIdentityConfig
├── verifier_store.rs          — SQLite persistence (all tables; WAL mode)
├── posture_cache.rs           — SharedPostureCache, CachedFleetPosture, ServiceState,
│                                OperationalCommand, should_route_command
├── posture_engine.rs          — recalculate_and_broadcast, derive_fleet_posture,
│                                generation counter, init_generation_from_store
├── posture_engine_v2.rs       — LockoutReason, PostureRecalcTrigger, PostureEngineSender,
│                                start_posture_engine_worker, resolve_posture_with_reason
├── recovery_hysteresis.rs     — evaluate_recovery_report, HysteresisDecision
├── telemetry_watchdog.rs      — spawn_telemetry_watchdog (AV sensor health monitoring)
├── clock.rs                   — Clock trait, SystemClock, VirtualClock (test injection)
├── scenario_runner.rs         — ScenarioRunner, ScenarioEvent, PostureAssertion
├── standby_monitor.rs         — spawn_heartbeat_writer, spawn_promotion_monitor
├── federation.rs              — FederatedTrustReport, Ed25519 verify pipeline
├── federation_reconciliation.rs — FederatedTrustReportV2, reconcile_reports
├── audit_chain.rs             — SHA-256 hash-chained audit log
├── kinematics_contract.rs     — KinematicContract, scalar envelope clamping
├── kinematics_sim.rs          — VehicleState, forward simulator, apply_enforcement
├── action_filter.rs           — ActionFilter<C>, ActionClaim evaluation
├── action_policy.rs           — LLM JSON → typed AgentAction parser
├── security.rs                — constant_time_compare
├── protocol_adapter.rs        — Modbus/OPC-UA industrial event mapping
├── kirra_core.rs              — KirraKernelGovernor (clamping + rate limiting)
├── ros2_adapter.rs            — NaN/Inf rejection before ROS2 publish
├── dds_bridge.rs              — CDR encapsulation, Volatile durability
├── gateway/
│   ├── policy.rs              — classify_command (path + method → OperationalCommand)
│   ├── policy_layer.rs        — Tower KirraPolicyLayer middleware
│   ├── cmd_vel.rs             — CmdVel validation, DEFAULT_CMD_VEL_LIMITS
│   ├── interceptor.rs         — gateway interceptor
│   ├── kinematics_contract.rs — VehicleKinematicsContract, validate_vehicle_command
│   └── kinematics_proptest.rs — property-based tests for kinematics validation
└── bin/
    ├── kirra_verifier_service.rs — axum HTTP service, all route handlers
    └── kirra_carla_client.rs     — CARLA AV simulator integration
```

### Fleet Posture States

| Posture | Command Routing |
|---------|----------------|
| `Nominal` | All commands allowed except `Unknown` |
| `Degraded` | `ReadTelemetry` only |
| `LockedOut` | All commands blocked |

### Trust Evaluation Pipeline

1. Node registers with an attestation key (AK) and PCR16 value
2. Verifier issues a time-limited HMAC challenge (TTL: 30 s)
3. Node responds with a proof; verifier computes and compares HMAC-SHA256
4. Trust state (`Trusted` / `Untrusted` / `Unknown`) stored to SQLite
5. Fleet posture recalculated via DAG traversal; broadcast over SSE

### AV Sensor Recovery Pipeline

1. Sensor reports arrive with confidence score and hardware fault flag
2. Below-floor confidence or `hw_fault=true` marks the node `Untrusted` immediately
3. Recovery requires **5 consecutive healthy reports** within a **10 s window**
4. A new fault during recovery resets the streak to 0
5. Telemetry watchdog independently monitors for silence — faults at 2 s, warns at 1 s

---

## Getting Started

### Prerequisites

- Rust 2021 edition toolchain (`rustup`)
- A writable path for the SQLite database

### Build

```bash
cargo build --release
```

### Run

```bash
export KIRRA_ADMIN_TOKEN="your-secret-token"
export KIRRA_SUPERVISOR_RESET_KEY="your-reset-key"
cargo run --bin kirra_verifier_service
```

The service listens on `0.0.0.0:8090` by default.

### Install (Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/justinlooney/kirra-runtime-sdk/main/install.sh | sudo bash
```

See [INSTALL.md](INSTALL.md) for full installation documentation including non-interactive mode, HA setup, and upgrade/uninstall instructions.

### Test

```bash
cargo test
```

---

## Configuration

All configuration is via environment variables.

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `KIRRA_ADMIN_TOKEN` | Yes (mutation routes) | — | Bearer token for admin endpoints. Absent or empty → `503`. |
| `KIRRA_SUPERVISOR_RESET_KEY` | Yes (reset ops) | — | Reset authorization key. Must be non-empty, ≤ 64 bytes. |
| `KIRRA_VERIFIER_MODE` | No | `active` | `passive_standby` → read-only. Runtime-promotable via HA monitor. |
| `KIRRA_DB_PATH` | No | `kirra_verifier.sqlite` | Path to the SQLite database file. |
| `KIRRA_VERIFIER_ADDR` | No | `0.0.0.0:8090` | Listen address. |
| `KIRRA_TRUSTED_INGRESS_MODE` | No | `false` | Enforce `x-kirra-client-id` header on identity-gated routes. |
| `KIRRA_CLIENT_ID_HEADER` | No | `x-kirra-client-id` | Header name for client identity. |
| `KIRRA_INSTANCE_ID` | No | hostname | Unique identifier for this instance in HA deployments. |
| `KIRRA_HEARTBEAT_INTERVAL` | No | `2000` | HA heartbeat write interval (ms). |
| `KIRRA_PROMOTION_TIMEOUT` | No | `10000` | Standby promotes if primary silent for this many ms. |

---

## API Reference

### Public / Unauthenticated

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Liveness check |
| `GET` | `/ready` | Readiness check |
| `GET` | `/fleet/posture` | Current fleet-wide posture |
| `GET` | `/fleet/posture/:node_id` | Per-node posture |
| `GET` | `/fleet/history/:node_id` | Posture event history |
| `GET` | `/fleet/flapping/:node_id` | Flap detection for a node |
| `GET` | `/attestation/status/:node_id` | Node trust state |
| `GET` | `/federation/reports/:asset_id` | Federation reports for an asset |
| `POST` | `/attestation/challenge/:node_id` | Issue attestation challenge |
| `POST` | `/attestation/verify` | Submit challenge response |

### Identity-Gated (admin token + `x-kirra-client-id`)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/system/posture/stream` | SSE stream of real-time posture events |
| `POST` | `/federation/reports/submit` | Submit signed federated trust report |
| `POST` | `/action_filter/evaluate` | Evaluate an action claim against posture |
| `POST` | `/industrial/evaluate` | Evaluate a Modbus/OPC-UA industrial event |

### Admin-Only (`Authorization: Bearer <KIRRA_ADMIN_TOKEN>`)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/attestation/register` | Register a node |
| `POST` | `/fleet/dependencies` | Register dependency graph edges |
| `POST` | `/system/backup/export` | Full state dump |
| `GET` | `/system/audit/verify` | Verify audit chain integrity |
| `POST` | `/federation/controllers/register` | Register a trusted peer controller |
| `POST` | `/attestation/identity/register` | Register a hardware fingerprint |

---

## Security Model

- **Fail-closed everywhere** — any missing token, expired nonce, or verification failure results in denial, never silent pass-through.
- **Constant-time comparisons** — all token verification uses `constant_time_compare`; standard `==` is never used on security-critical byte sequences.
- **No hardcoded secrets** — `KIRRA_ADMIN_TOKEN` and `KIRRA_SUPERVISOR_RESET_KEY` must come from environment variables. No fallback values exist in code.
- **Volatile DDS durability** — actuator topics are never persisted via `TransientLocal`.
- **Ordered SQLite writes** — disk persistence always precedes in-memory state updates.
- **Nonce burning** — federation report nonces are stored and checked before acceptance; replays are rejected.
- **Posture-gated routing** — `OperationalCommand::Unknown` is rejected in all posture states, including `Nominal`.

---

## High Availability

Kirra supports active/passive HA with automatic failover.

**Primary** (`KIRRA_VERIFIER_MODE=active`): writes a heartbeat to the shared database every 2 s.

**Standby** (`KIRRA_VERIFIER_MODE=passive_standby`): polls the heartbeat. If the primary is silent for 10 s (`KIRRA_PROMOTION_TIMEOUT`), the standby automatically promotes itself to active and begins enforcing posture.

Both instances must share the same SQLite database (NFS mount, shared block storage, or equivalent).

```bash
# Primary
KIRRA_VERIFIER_MODE=active KIRRA_INSTANCE_ID=kirra-primary ./kirra_verifier_service

# Standby
KIRRA_VERIFIER_MODE=passive_standby KIRRA_INSTANCE_ID=kirra-standby ./kirra_verifier_service
```

---

## Dependencies

| Crate | Version | Purpose |
|-------|---------|----------|
| `axum` | 0.8 | HTTP framework |
| `tokio` | 1 | Async runtime |
| `tower` | 0.5 | Middleware (`KirraPolicyLayer`) |
| `dashmap` | 6 | Concurrent hashmaps |
| `rusqlite` | 0.31 (bundled) | WAL-mode SQLite persistence |
| `ed25519-dalek` | 2 | Federation signature verification |
| `hmac` + `sha2` | 0.12 / 0.10 | Attestation proof computation |
| `base64` | 0.22 | Encoding |
| `tokio-stream` | 0.1 | SSE broadcast |
| `reqwest` | 0.12 | CARLA client HTTP |
| `tracing` | 0.1 | Structured logging |
| `proptest` | 1 | Kinematics property-based tests |

---

## Releases

### v1.1.1
- Complete Aegis → Kirra rename across all source files, binaries, systemd units, ROS2 packages, Docker images, Helm charts, and documentation
- 13 bug fixes including post-rename import cleanup, binary path corrections, and CI pipeline fixes

### v1.1.0
- Multi-Asset Safety Fabric
- ASIL-D and SOTIF safety case foundation documents
- Ed25519 log signing with export and key rotation
- Action Filter with LLM integration guide
- EtherNet/IP, CANOpen, DNP3 protocol adapters
- ROS2 safety interlock package
- Docker multi-platform images and Helm chart
- CARLA integration client
- 333 tests passing, 0 failures

---

## License

See [LICENSE](LICENSE) for details.
