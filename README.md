# Aegis Runtime SDK

A distributed runtime legitimacy engine and safety governor for AI-driven robotic and edge systems. Aegis enforces **fail-closed trust semantics** across a heterogeneous fleet — preventing unsafe or unauthorized commands from reaching actuators regardless of what an AI model, LLM output, or upstream orchestration layer instructs.

---

## Overview

Modern robotic and edge deployments increasingly rely on AI models to generate operational commands. Aegis sits between those models and the physical actuators, acting as a cryptographically-grounded safety layer that:

- **Attests** each fleet node via HMAC-SHA256 challenge/response
- **Tracks trust posture** per-node and fleet-wide using a gray/black DAG traversal algorithm
- **Gates commands** based on live posture — locking out unsafe operations before they reach hardware
- **Federates** trust across multiple controllers using Ed25519-signed reports
- **Audits** all state transitions via a SHA-256 hash-chained tamper-evident ledger

---

## Features

- **Fail-closed by design** — missing or invalid credentials yield `503`, never silent pass-through
- **Constant-time token comparison** — timing-safe token verification throughout
- **Gray/black DAG traversal** — cycle detection and diamond-DAG memoization for fleet dependency graphs
- **SSE posture broadcast** — real-time fleet posture stream for subscribers
- **Industrial protocol support** — Modbus and OPC-UA event evaluation
- **DDS bridge** — CDR-encapsulated actuator topics with `Volatile` durability
- **Ed25519 federation** — cross-controller trust reports with replay prevention and nonce burning
- **WAL-mode SQLite** — durable persistence with fail-closed write ordering (disk before memory)

---

## Architecture

```
src/
├── verifier.rs                — AppState, FleetPosture, DAG traversal
├── verifier_store.rs          — SQLite persistence layer
├── posture_cache.rs           — SharedPostureCache, command routing logic
├── federation.rs              — Ed25519 trust federation
├── audit_chain.rs             — SHA-256 hash-chained audit log
├── action_filter.rs           — ActionClaim evaluation
├── protocol_adapter.rs        — Modbus/OPC-UA industrial event mapping
├── security.rs                — constant_time_compare
├── aegis_core.rs              — AegisKernelGovernor (clamping + rate limiting)
├── action_policy.rs           — LLM JSON → typed AgentAction parser
├── ros2_adapter.rs            — NaN/Inf rejection before ROS2 publish
├── dds_bridge.rs              — CDR encapsulation, Volatile durability
└── gateway/
    ├── policy.rs              — classify_command (path + method → OperationalCommand)
    ├── policy_layer.rs        — Tower AegisPolicyLayer middleware
    ├── cmd_vel.rs             — CmdVel validation
    └── interceptor.rs         — gateway interceptor
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
export AEGIS_ADMIN_TOKEN="your-secret-token"
export AEGIS_SUPERVISOR_RESET_KEY="your-reset-key"
cargo run --bin aegis_verifier_service
```

The service listens on `0.0.0.0:8090` by default.

### Test

```bash
cargo test
```

Current status: **66 passing, 0 failing**.

---

## Configuration

All configuration is via environment variables.

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `AEGIS_ADMIN_TOKEN` | Yes (mutation routes) | — | Bearer token for admin endpoints. Absent or empty → `503`. |
| `AEGIS_SUPERVISOR_RESET_KEY` | Yes (reset ops) | — | Reset authorization key. Must be non-empty, ≤ 64 bytes. |
| `AEGIS_VERIFIER_MODE` | No | `active` | Set to `passive` / `passive_standby` / `standby` for read-only mode. |
| `AEGIS_DB_PATH` | No | `aegis_verifier.sqlite` | Path to the SQLite database file. |
| `AEGIS_VERIFIER_ADDR` | No | `0.0.0.0:8090` | Listen address. |
| `AEGIS_TRUSTED_INGRESS_MODE` | No | `false` | Enforce `x-aegis-client-id` header on identity-gated routes. |
| `AEGIS_CLIENT_ID_HEADER` | No | `x-aegis-client-id` | Header name for client identity. |

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

### Identity-Gated (admin token + `x-aegis-client-id`)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/system/posture/stream` | SSE stream of posture events |
| `POST` | `/federation/reports/submit` | Submit signed federated trust report |
| `POST` | `/action_filter/evaluate` | Evaluate an action claim against posture |
| `POST` | `/industrial/evaluate` | Evaluate a Modbus/OPC-UA industrial event |

### Admin-Only (`Authorization: Bearer <AEGIS_ADMIN_TOKEN>`)

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
- **No hardcoded secrets** — `AEGIS_ADMIN_TOKEN` and `AEGIS_SUPERVISOR_RESET_KEY` must come from environment variables. No fallback values exist in code.
- **Volatile DDS durability** — actuator topics are never persisted via `TransientLocal`.
- **Ordered SQLite writes** — disk persistence always precedes in-memory state updates.

---

## Dependencies

| Crate | Version | Purpose |
|-------|---------|--------|
| `axum` | 0.8 | HTTP framework |
| `tokio` | 1 | Async runtime |
| `tower` | 0.5 | Middleware (`AegisPolicyLayer`) |
| `dashmap` | 6 | Concurrent hashmaps |
| `rusqlite` | 0.31 (bundled) | WAL-mode SQLite persistence |
| `ed25519-dalek` | 2 | Federation signature verification |
| `hmac` + `sha2` | 0.12 / 0.10 | Attestation proof computation |
| `base64` | 0.22 | Encoding |
| `tokio-stream` | 0.1 | SSE broadcast |
| `tracing` | 0.1 | Structured logging |

---

## License

See [LICENSE](LICENSE) for details.
