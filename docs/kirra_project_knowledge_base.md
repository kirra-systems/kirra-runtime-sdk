# Kirra — Project Knowledge Base

*Paste this document into your Claude project's knowledge base or custom instructions to give any conversation in that project full context about the Kirra system.*

---

## What Kirra Is

Kirra is a distributed runtime legitimacy engine and safety governor for AI-driven robotic and edge systems. It enforces fail-closed trust semantics across a heterogeneous fleet of nodes — preventing unsafe or unauthorized commands from reaching actuators regardless of what an AI model, LLM output, or upstream orchestration layer instructs.

**The core problem it solves**: When an AI agent or LLM is in the loop controlling physical systems (robots, drones, industrial machinery), you cannot trust that every output it generates is safe or authorized. Kirra sits between the AI and the actuators as a cryptographically grounded gatekeeper that continuously evaluates the trust state of the entire fleet before letting any command through.

---

## The Three Layers

### 1. Safety Governor (the original kernel)
The `KirraKernelGovernor` clamps actuator commands to hard engineering boundaries before applying rate-of-change limits. The absolute ceiling always wins over rate priority — a command that exceeds the ceiling is rejected even if its rate of change is nominal. LLM-generated intents are parsed through a type-safe struct boundary (`UnstructuredTextParser`) that strips prompt-injection artifacts before they can influence motion primitives.

### 2. Trust Verification Layer
An axum HTTP service backed by WAL-mode SQLite. It maintains a registry of nodes in the fleet, each with a trust state (`Trusted`, `Untrusted`, `Unknown`) derived from:
- **Cryptographic attestation**: HMAC-SHA256 challenge-response with 30-second nonce TTL
- **TPM quotes**: PCR16 digest verification (when TPM hardware is present)
- **Dependency graph evaluation**: A gray/black two-set DAG algorithm that propagates trust failures upward through dependency chains

Every state mutation is written to disk before memory is updated (fail-closed disk-before-memory ordering). Every posture event is written to a SHA-256 hash-chained audit log whose integrity can be verified end-to-end.

### 3. Gateway Policy Layer
A Tower middleware service (`KirraPolicyLayer`) that intercepts all control traffic. Commands are classified into four bins:
- **ReadTelemetry** — allowed in Nominal and Degraded
- **WriteState** — allowed in Nominal only
- **SystemMutation** — allowed in Nominal only
- **Unknown** (any unrecognized method/path) — denied in ALL states including Nominal

A stale posture cache (TTL exceeded) blocks everything regardless of last-known posture.

---

## Fleet Posture States

- **Nominal**: All nodes trusted, no dependency failures. All command classes flow.
- **Degraded**: One or more nodes Untrusted or Unknown; no cycles. ReadTelemetry only.
- **LockedOut**: A cycle detected in the dependency graph, OR a LockedOut dependency propagated from below. Everything blocked, including reads.

Posture is computed on-demand using a gray/black DAG traversal with memoization. Diamond DAGs (A→B→D, A→C→D) are correctly handled — D is evaluated once and cached; the second path through C returns the memoized result rather than triggering a false cycle alarm. Maximum traversal depth is 10.

---

## Federation

Multiple Kirra controllers can share trust observations across organizational boundaries. Each federated trust report must:
1. Carry a valid Ed25519 signature over a canonical JSON payload
2. Include a nonce that has never been seen before (burned atomically on acceptance)
3. Have an `issued_at_ms` within a 5-second replay window of the receiving controller's clock
4. Come from a controller whose Ed25519 public key is registered in the trusted controller registry

The 5-step acceptance pipeline: structural validation → identity lookup → signature verification → nonce uniqueness check → atomic commit (report + nonce burn + audit chain entry, all in one SQLite transaction).

---

## Real-Time Posture Stream

`GET /system/posture/stream` is a Server-Sent Events endpoint that broadcasts posture change notifications in real time. It is backed by a bounded broadcast channel (capacity 1024). Slow subscribers that fall behind are dropped automatically rather than stalling mutation handlers. The stream fires after every successful state mutation (`NODE_STATUS_CHANGED`, `DEPENDENCY_GRAPH_MUTATED`, `NODE_IDENTITY_PROVISIONED`).

---

## Security Model

**Fail-closed everywhere**: Unknown states, missing config, absent tokens, stale caches, and unrecognized commands all resolve to denial, never to permissiveness.

**Auth tiers**:
- **Identity-gated routes** (posture stream, federation submit, action filter, industrial evaluate): require admin Bearer token AND a valid `x-kirra-client-id` header — enforces that requests arrive through an authorized mesh proxy, not a raw caller who obtained the token
- **Admin-only routes** (node registration, backup, audit verify, controller registration, hardware identity registration): require admin Bearer token only
- **Challenge-response routes** (issue challenge, verify attestation): unauthenticated — the protocol provides its own cryptographic guarantee
- **Public routes** (health, ready, status reads): no auth

**Timing-safe comparisons**: All token comparisons use `constant_time_compare` to prevent oracle attacks.

**Nonce hygiene**: Attestation challenge nonces are volatile (never persisted), expire in 30 seconds, and are consumed on first use. Federation nonces are burned atomically in the same SQLite transaction that commits the report.

**Audit chain**: Every posture event and federation report acceptance is written to a SHA-256 hash-chained ledger. Each record's hash covers: `SHA256(previous_hash || payload || timestamp)`. Chain integrity can be verified end-to-end via `GET /system/audit/verify`.

---

## Hardware Identity Registry (Patch 1)

Each node can have a hardware fingerprint (AK public key digest) registered via `POST /attestation/identity/register`. Registration is atomic: the fingerprint is written and a `NODE_IDENTITY_REGISTERED` event is chained into the audit ledger in a single transaction. Fingerprints can be rotated (INSERT OR REPLACE); each rotation produces a new audit chain entry.

## Transport Identity Enforcement (Patch 2)

`TransportIdentityConfig` reads two env vars at startup:
- `KIRRA_TRUSTED_INGRESS_MODE` (`true`/`1` to enable) — fail-closed: disabled by default
- `KIRRA_CLIENT_ID_HEADER` (default: `x-kirra-client-id`)

`validate_client_identity_headers` is a pure function with no side effects — it checks only that the mode is enabled, the header is present, and the value is non-blank.

---

## Operational Modes

- **Active** (default): All mutation routes open, subject to auth
- **PassiveStandby** (`KIRRA_VERIFIER_MODE=passive`): Mutation routes return 503 — HA hot-spare that prevents split-brain writes

HA deployment: Docker Compose with primary on port 8088 (active) and standby on 8089 (passive_standby). Helm chart uses `Recreate` rollout strategy to protect SQLite from concurrent write locks.

---

## Current State (as of last session)

- **Test count**: 66 passing, 0 failing, 0 warnings
- **Language**: Rust, edition 2021
- **Crate name**: `kirra-runtime-sdk`
- **Key dependencies**: axum 0.8, tokio 1, tower 0.5, dashmap 6, rusqlite 0.31 (bundled), ed25519-dalek 2, tokio-stream 0.1
- **SQLite tables**: nodes, dependencies, posture_events, audit_log_chain, federated_trust_reports, trusted_federation_controllers, federation_report_nonces, attestation_identity_registry

### Feature Milestones Built
1. KirraKernelGovernor — scalar clamping and rate limiting
2. LLM intent parsing with injection stripping
3. ROS2 adapter with NaN/Inf rejection
4. DDS bridge with CDR encapsulation and Volatile durability
5. TPM attestation (challenge-response, PCR16, 30s nonce TTL)
6. Gray/black DAG posture computation
7. Gateway policy Tower middleware
8. `OperationalCommand::Unknown` — denied in all postures including Nominal
9. SQLite persistence (WAL mode, write-through ordering)
10. SHA-256 hash-chained audit log
11. HA active/passive mode with PassiveStandby 503 guard
12. Flap detection (≥3 events in 5 minutes)
13. cmd_vel validation with hard kinematic limits
14. Industrial protocol adapter (Modbus, OPC-UA)
15. Ed25519 signed federation with nonce replay prevention
16. Real-time posture SSE stream with backpressure containment
17. Attestation identity registry with atomic audit chaining
18. Transport identity enforcement with pure functional validator and two-tier router

### Documentation in `kirra/docs/`
- `v1_security_invariants.md`
- `v1_route_authorization_matrix.md`
- `v1_dr_drill_transcript.md`
- `v1_active_passive_runbook.md`
- `v1_gateway_policy_matrix.md`
- `v1_tpm_attestation_proof.md`
- `v1_dag_cycle_depth_report.md`
- `v1_release_notes_and_architecture.md`

---

## Things Claude Must Never Do in This Codebase

1. Comment out, bypass, or remove `require_admin_token` from any mutation route
2. Use `let status = NodeTrustState::Trusted` as a mock in `verify_attestation`
3. Replace the gray/black DAG algorithm with a simplified mock
4. Remove `pending_challenges: DashMap<String, ChallengeEntry>` from `AppState`
5. Use `State<Arc<AppState>>` in handlers — correct type is `State<Arc<ServiceState>>`
6. Import `FleetPosture` from `crate::gateway::posture_cache` — correct path is `crate::verifier::FleetPosture`
7. Add hardcoded fallbacks for `KIRRA_ADMIN_TOKEN` or `KIRRA_SUPERVISOR_RESET_KEY`
8. Use `TransientLocal` DurabilityPolicy on DDS topics
9. Remove the `Unknown` early-return from `should_route_command`
10. Write SQLite to memory before disk (memory must always lag disk, never lead)
