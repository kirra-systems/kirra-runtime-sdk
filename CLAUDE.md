# Kirra — Claude Code Context

## Project Identity

- **Workspace**: a Cargo workspace. The ROOT member is the **`kirra-verifier`** crate (the fleet-legitimacy engine + governor service, documented in the bulk of this file). The **doer-checker / planning / perception** side lives under `crates/*` (see **Workspace Crates** below). `parko/` is a **separate** workspace (the ML + diverse-governor side). Most AV/Occy work happens in `crates/kirra-planner`, `kirra-map`, `kirra-taj`, `kirra-ros2-adapter`, and `parko/`.
- **Root crate**: `kirra-verifier` (lib ident `kirra_verifier`; lib + bin dual-crate, `crate-type = ["rlib", "cdylib"]`). Renamed from `kirra-runtime-sdk` once nothing lean depended on it (the GitHub repo remains `kirra-runtime-sdk`).
- **Edition**: 2021
- **Primary binary**: `kirra_verifier_service` (`src/bin/kirra_verifier_service.rs`)
- **Secondary binary**: `kirra_carla_client` (`src/bin/kirra_carla_client.rs`)
- **Test suite**: `cargo test` (root). For a scoped crate use `cargo test -p <crate>`. `parko/` is its own workspace: `cd parko && cargo test`. The `kirra-ros2-adapter` `node.rs` is `#[cfg(feature = "ros2")]` and needs a sourced ROS 2 toolchain (`r2r`) — it is built ONLY by CI's `ros2 adapter build (--features ros2)` job, never by a default build.
- **Build toolchain**: pinned to **1.94.1** via `rust-toolchain.toml` (reproducible builds); **MSRV is 1.88**, enforced on every PR by the `msrv` CI lane (`cargo +1.88.0 check --workspace --locked` on both lockfiles). Build on the pin; support down to the MSRV. See `docs/VERSIONING_POLICY.md` §3.
- **Hardening harnesses** (workspace-detached, own CI lanes): **loom** concurrency models — `crates/kirra-loom-models`, run `RUSTFLAGS="--cfg loom" cargo test -p kirra-loom-models --release` (models the posture-generation + #688 sticky-lockout protocols). **cargo-fuzz** targets — `fuzz/`, run `cargo +nightly fuzz run <target>` (decoders: `decode_verdict`, `dnp3_analog_setpoint`, `scalar_decode_le`, `llm_json_intent`). Both compile to nothing in a normal build. **Power-loss audit drill** (Gate C #2) — `tests/audit_chain_prefix_on_kill.rs`, run `cargo test --test audit_chain_prefix_on_kill`: a reexec child appends hash-chained audit entries in a loop, the parent `SIGKILL`s it mid-append, then reopens the WAL DB (recovery runs) and asserts the chain verifies INTACT with entries surviving — proving an abrupt power loss never leaves a torn/forked chain (only ever a valid prefix). Companion `committed_..._reverifies_after_reopen` checks committed-entry durability. Unix-only; the non-vacuousness of the positive assertion is anchored by the tamper-detection tests (`break_audit_chain_table_for_test`). **Two-node rollout harness** (Gate C #1) — `tests/two_node_rollout.rs`, run `cargo test --test two_node_rollout`: drives the verifier campaign engine (`Campaign`/`resolve_node_assignment`/`summarize_campaigns` over a real `VerifierStore`) AND the node-side `Installer` state machine (`decide_pull` → stage → trial → health-gated commit/rollback) against each other across two node identities — proving staged-% membership grows as the campaign advances, an out-of-cohort node is never assigned, a healthy node commits + is counted adopted, and an UNHEALTHY node rolls back to its baseline and is NOT counted. EP-13 extends it to the FULL Uptane flow: an anchored node verifies the assignment-carried signed metadata set against its durable trust state, installs only the authorized digest, persists the advanced rollback floor — then a re-served OLDER signed set (rollback attack) and a set-stripped assignment (downgrade-by-omission) are both REFUSED. (Root crate dev-deps `kirra-ota-installer` for this.) **HA failover + split-brain fence drill** (WS-4) — `tests/ha_failover.rs`, run `cargo test --test ha_failover`: two `VerifierStore`s share one file (the shared-file HA topology); the primary claims the durable epoch, the standby promotes by claiming the NEXT epoch (real `try_claim_epoch` CAS), and the revived old primary is then FENCED (`assert_actuator_epoch_held` errors + its stale-epoch re-claim is refused) — proving exactly one writer at a time. Deterministic (store-level, no async/10 s timers) + a pure invariant that the self-demote window closes before the promotion window opens. **Live-Postgres backend conformance** (EP-10/G-9) — `crates/kirra-verifier-pg` (workspace-detached like loom/fuzz): binds the `PgExecutor`/`EpochFence`/`NodeStore`/`PostureEngineStateStore`/`FederationStore`/`OperatorStore`/`PrincipalStore`/`CertPrincipalStore`/`FabricAssetStore`/`OtaCampaignStore`/`AvSubsystemStore` seams to the sync `postgres` client (`LivePgExecutor` = the promised ~10-line adapter; `PgVerifierStore` realizes the actuator fence transactionally with `SELECT … FOR UPDATE`) and runs the SAME `assert_fence_contract`/`assert_node_store_contract`/`assert_posture_engine_state_store_contract`/`assert_federation_store_contract`/`assert_operator_store_contract`/`assert_principal_store_contract`/`assert_cert_principal_store_contract`/`assert_fabric_asset_store_contract`/`assert_ota_campaign_store_contract`/`assert_av_subsystem_store_contract` conformance suites the root crate runs against SQLite, against a real server — plus PG-only drills (two-connection CAS race, migration future-schema refusal). The `FederationStore` nonce burn + per-source sequence gate use atomic Postgres upserts (`ON CONFLICT DO NOTHING` = `INSERT OR IGNORE`; conditional `DO UPDATE … WHERE EXCLUDED.last_sequence > …` = the strict-advance CAS). The `PostureEngineStateStore` monotonic high-water uses a single-statement conditional upsert on PG (`ON CONFLICT … WHERE (CASE WHEN value ~ '^[0-9]+$' THEN value::numeric ELSE 0 END) < EXCLUDED.value::numeric`) — race-safe like SQLite's atomic upsert, with the `CASE` giving the same heal-on-corrupt-save parity as SQLite's `CAST('garbage')=0` (a non-numeric existing value counts as 0 → a positive generation overwrites it). The comparison is `::numeric` (arbitrary precision), not `::bigint`, so a stored all-digits-but-out-of-domain value never overflows the cast (it compares greater than any real generation → monotonic keeps it, never a crash). The table is installed by a real v3 migration step (not the baseline; later PG migrations add further seam tables and advance `PG_SCHEMA_VERSION` accordingly). Only `load_last_generation` fails closed on a corrupt high-water (via `PgStoreError::CorruptGeneration`); `save` heals, matching every backend. The `OtaCampaignStore` seam (v9 migration installs `ota_campaigns` + `node_artifact_status`) persists campaigns with `cohorts`/`stages` JSON-in-TEXT and decodes rows FAIL-CLOSED (`PgStoreError::CorruptCampaignRow` on an unknown state/halt token or an out-of-range `stage_index`, exactly as SQLite's `map_campaign_row` — never a `Campaign` the engine could index out of bounds); the node-adoption upsert is the same monotonic (`WHERE EXCLUDED.reported_at_ms >= …`) + attested-per-digest CAS, with `attested` a native PG `BOOLEAN`. The audit-chaining of lifecycle mutations (`update_campaign`) is NOT part of this storage contract — it stays inherent on the SQLite backend via the `AuditAppender` seam. The `AvSubsystemStore` seam (v10 migration installs `av_subsystem_meta`) realizes the AV diagnostic-meta store: `register` is a faithful INSERT-OR-REPLACE (`ON CONFLICT DO UPDATE` that also zeroes the streak columns, matching SQLite's row-replace reset), `increment_recovery_streak` is a single-statement `UPDATE … RETURNING` whose `CASE` stamps `start_ms` only on the 0→1 edge and whose zero-rows-returned case fails closed (`PgStoreError::AvNodeNotRegistered`, the parity of SQLite's `QueryReturnedNoRows` + the in-memory `NodeNotRegistered`). Run `cd crates/kirra-verifier-pg && KIRRA_PG_URL=postgres://… cargo test` (tests SKIP loudly without the URL; the `postgres-conformance` CI lane provides a `services: postgres` container). **Kani proofs** (EP-15) — `verification/kani/` (workspace-detached): machine-checked (CBMC) invariants on the checker cores, with the shipped sources `#[path]`-included VERBATIM — the frozen kinematics talisman (blob `ed00f4da…`) is proved UNMODIFIED (never add proof modules inside it; its pin is a git blob hash). 12 properties: lease split-brain algebra for all u64 (L1–L4), the talisman's NaN/dt/P2-ceiling + Degraded decel-to-stop gates over f64 `is_finite` case-splits (K1–K5), RSS `longitudinal_safe_distance` totality + the closing-speed monotonicity `occlusion_limited_speed`'s bisection relies on, over integer-scaled grids (R1–R3). Honest scope: the P6 `tan`/`atan` path is excluded (transcendentals; covered by MC/DC + proptest). LANE SPLIT: R2 alone is behind the crate's `deep-proofs` feature and runs in the WEEKLY `kani-deep-weekly` lane (its exact-IEEE relational instance exceeds the per-PR 45-min budget on both CaDiCaL and kissat); its per-PR gate is the exhaustive concrete mirror (full speed-grid walk swept along all four param axes). Run `cd verification/kani && cargo test` (BLOCKING mirror tier, no Kani needed) / `cargo kani` (the per-PR proofs; `kani-proofs` CI lane, BLOCKING — install-flake tolerated: proofs skip, mirrors still gate) / `cargo kani --features deep-proofs` (adds R2). Cited from `docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md` §2.
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

3. **`verify_attestation` must never mock trust** (`let status = NodeTrustState::Trusted` without verification). It MUST cryptographically verify a per-node proof: the node's Ed25519 signature over the `(node_id, nonce)` challenge payload, checked against the registered per-node `ak_public_pem` via `attestation::verify_attestation_proof` (issue #73). Fail-closed — no registered AK / malformed key / malformed proof / bad signature → reject; never accept by default. (The prior `HMAC(KIRRA_ADMIN_TOKEN, nonce)` proof was admin-asserted, not node-proven, and is removed. PCR16 measured-boot: BOTH paths are LIVE — the self-report binding (`verify_attestation_proof_with_pcr16`) and the genuine TPM2 **quote**. `tpm_quote::verify_tpm_quote` (`src/tpm_quote.rs`) is wired into `/attestation/verify` under a per-node `require_tpm_quote` policy: a node required to quote must present one whose TPM-signed `pcrDigest` equals `SHA256(registered_pcr16_value)` (`expected_single_pcr_digest_hex`) over a bounded PCR16 selection, bound to the challenge nonce — `verify_strict`, fail-closed (401 auth / 403 boot-state), and nonce-preserving (runs before `consume_challenge`, so a failed quote is retryable). What remains for Gate C #3 is the on-device rooting only: Orin Secure Boot + dm-verity + a provisioned TPM AK (the quote sig is Ed25519 today → RSA/ECC on a real TPM).)

4. **`FleetNodePosture` and the gray/black two-set DAG algorithm must never be replaced with a mock**. The real traversal was moved VERBATIM to `kirra_safety_authority::dag::recursive_calculate` (ADR-0035 Stage 3d) and is invoked via the `AppState::calculate_posture*` / `calculate_fleet_posture` delegators — the algorithm is byte-identical and must remain intact (proven by `shared_memo_equivalence_tests`).

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
| `VerifierStore` | `crates/kirra-persistence` (extracted ADR-0035 slice 4; `lib.rs` + per-table submodules), re-exported via the `src/verifier_store.rs` shim | rusqlite WAL-mode SQLite; wrapped in `Arc<Mutex<VerifierStore>>` in AppState. The persistence crate depends only on the lean domain/audit leaf crates; `crate::verifier_store::*` / `kirra_verifier::verifier_store::*` paths resolve through the shim. Test-only `*_for_test` helpers are behind its `test-support` feature (root enables it as a dev-dep) |
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
| `Campaign` / `CampaignState` | `crates/kirra-ota-campaign` (re-exported via `src/ota_campaign.rs` shim) | WS-4 OTA rollout campaign + lifecycle state machine (Draft→Staged→Rolling→{Completed\|Halted}); `advance` fail-closed on posture. ADR-0035 Stage 2.5 C2 slice 1: relocated to a lean crate so `verifier_store` no longer C2-couples to it |

### Module Map

```
src/
├── verifier.rs               — AppState, FleetPosture, DAG traversal, TransportIdentityConfig
├── verifier_store/           — SQLite persistence (all tables; WAL mode); module dir:
│                               mod.rs + per-table submodules (nodes, attestation, audit,
│                               federation, epoch, principals, ota_campaigns, …).
│                               migrations.rs (WP-18/G-20): versioned schema framework
│                               over PRAGMA user_version — SCHEMA_VERSION, fail-closed
│                               assert_schema_not_future (refuse a newer-binary DB),
│                               run_migrations (apply registered steps + stamp); new()
│                               gates on it. VerifierStore::schema_version() reads it.
│                               WP-18 s2/3: the policy is now a dialect-agnostic engine
│                               (SchemaBackend trait + run_migrations_generic +
│                               validate_step_versions); SqliteBackend is one impl (the
│                               live path delegates, behaviour-preserving).
│                               migrations_postgres.rs: PostgresBackend<E: PgExecutor> —
│                               the same engine over a kirra_schema_version table + injected
│                               executor seam (no tokio-postgres dep; driver binding is
│                               the integrator's ~10-line adapter).
│                               WP-18 3/3: epoch.rs also defines the EpochFence storage
│                               trait (current_epoch/current_active_holder/try_claim_epoch
│                               CAS/assert_actuator_epoch_held) — the first VerifierStorage-
│                               family seam; VerifierStore impls it (inherent methods win
│                               resolution → non-breaking), InMemoryEpochFence is the
│                               portability-proof 2nd backend, a generic conformance test
│                               runs the at-most-one-writer contract against both.
│                               WP-18 store-trait 2/N: nodes.rs adds the NodeStore trait
│                               (save_node/load_node/load_nodes/node_exists/count_nodes)
│                               the same way — VerifierStore impls it + InMemoryNodeStore is
│                               the 2nd backend + a shared conformance test (upsert/load/count).
│                               federation.rs adds the FederationStore seam the same way
│                               (the PORTABLE subset: trusted-controller key registry +
│                               anti-replay primitives — burn_federation_nonce /
│                               has_seen_federation_nonce / industrial_seq_check_and_advance;
│                               the audit-chained save_federated_report_chained stays inherent).
│                               posture.rs adds the PostureEngineStateStore seam the same way
│                               (the posture_engine_state KV store + the MONOTONIC generation
│                               high-water — load/save_last_generation + load/save_engine_state;
│                               the audit-chained save_posture_event_chained* stays inherent)
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
├── lease.rs                  — WP-19/G-21 lease-based failover timing model (pure):
│                               LeaseParams::from_ttl (renew at half-life, promote at
│                               ttl+ttl/2), DEFAULT_LEASE_TTL_MS (3s → ≤5s failover,
│                               ≤ POSTURE_CACHE_TTL_MS); demote_before_promote split-
│                               brain invariant. EP-03: LIVE behind KIRRA_HA_LEASE_ENABLED
│                               (standby_monitor renews + lease-triggers promotion;
│                               epoch CAS stays the takeover authority; default off)
├── federation.rs             — FederatedTrustReport, Ed25519 verify, evaluate_federated_report
├── federation_reconciliation.rs — FederatedTrustReportV2, reconcile_reports,
│                               ReconciliationOutcome, authoritative_posture
├── audit_chain.rs            — SHA-256 hash-chained audit log
├── audit_shipper.rs          — WS-4/Track-3 WORM off-box audit shipping:
│                               ShippedAuditRecord, verify_shipped_chain (INDEPENDENT
│                               off-box hash-chain re-verifier, no source DB),
│                               AuditSink (InMemory/JsonlFile), ship_and_advance +
│                               cursor persistence (at-least-once, ship-then-advance),
│                               spawn_audit_shipper (env-gated background scheduler,
│                               AUDIT_SHIP_INTERVAL_MS; opt-in via KIRRA_AUDIT_SHIP_PATH)
├── verdicts.rs               — EP-17 explainable verdicts: mint_verdict_id /
│                               is_valid_verdict_id + the DenyCode→operator-sentence
│                               explanation table (kept OUT of the frozen talisman;
│                               a lock-step test walks the real enum). Deny arm binds
│                               the id into the chained payload + 400 body; the
│                               auditor-tier GET /verdicts/{id} handler renders it
├── ota_campaign.rs           — re-export shim → kirra_ota_campaign (ADR-0035 Stage
│                               2.5 C2 slice 1): the WS-4/Track-3 OTA governor-artifact
│                               campaign engine (PURE): Campaign, CampaignState machine,
│                               HaltReason, fail-closed posture_regression_halt (advance
│                               HALTS, never rolls, when fleet posture != Nominal).
│                               Relocated to crates/kirra-ota-campaign so verifier_store
│                               no longer C2-couples to it
├── campaign_monitor.rs       — WS-4/Track-3 background posture-sweep monitor:
│                               sweep_active_campaigns_once + spawn_campaign_monitor
│                               (CAMPAIGN_SWEEP_MS); auto-halts active campaigns on a
│                               CONFIRMED regression between advances (unavailable/
│                               stale posture is skipped, never a halt)
├── cert_expiry_monitor.rs    — WP-15/G-19 mTLS cert-principal expiry monitor:
│                               sweep_cert_expiry_once + spawn_cert_expiry_monitor
│                               (CERT_EXPIRY_SWEEP_MS / CERT_EXPIRY_WARN_WINDOW_MS);
│                               hourly census → WARN + hash-chained
│                               CertPrincipalExpiryWarning audit for lapsed/lapsing
│                               certs (observability only; auth already fail-closes)
├── kinematics_contract.rs    — KinematicContract, scalar clamping
├── kinematics_sim.rs         — re-export shim → kirra_core::kinematics_sim (relocated
│                               Stage 7; VehicleState, apply_enforcement, run_simulation)
├── capture.rs                — re-export shim → kirra_core::capture (relocated Stage 7;
│                               record_from_verdict, spawn_capture_writer; needs the
│                               kirra-core `capture` feature, enabled in the SDK manifest)
├── action_filter.rs          — ActionFilter<C>, ActionClaim, evaluate_action_claim
├── action_policy.rs          — UnstructuredTextParser (LLM JSON → typed AgentAction)
├── security.rs               — constant_time_compare
├── authz.rs                  — WS-1 (#G7) RBAC: ApiRole, scopes, authorize_request
│                               (pure fail-closed decision; store/env lifted out)
├── protocol_adapter.rs       — Modbus/OPC-UA industrial event mapping
├── kirra_core.rs             — KirraKernelGovernor (scalar clamping, rate limiting)
├── ros2_adapter.rs           — NaN/Inf rejection before ROS2 publish
├── dds_bridge.rs             — CDR encapsulation, Volatile durability
├── standby_monitor.rs        — HA heartbeat writer and promotion monitor
├── startup_sentinel.rs       — Pre-flight invariant checks at startup
├── execution_manager.rs      — WP-20/G-11 declarative execution manager: TASK_MANIFEST
│                               (the 7 supervised loops as data + deps + criticality +
│                               scheduling intent + deadline), resolve_startup_order
│                               (topological sort, FAIL-CLOSED on cycle/missing/dup),
│                               deadline_missed + DeadlineStats. Pure core — main()
│                               adoption + SCHED_FIFO/affinity syscalls are follow-up
├── config.rs                 — Configuration loading helpers (Modbus gateway
│                               file-config: KirraRuntimeConfig, versioned+validated)
├── env_config.rs             — WP-17/G-17 unified verifier ENV config: KIRRA_ENV_KEYS
│                               canonical registry (single source of truth for every
│                               KIRRA_* var), unknown_kirra_env_vars warn-only sweep,
│                               versioned EffectiveConfig + effective_digest (SHA-256);
│                               startup WARNs on unknown vars + commits an
│                               EffectiveConfigDigest audit event (drift-detectable)
├── audit_log.rs              — Audit log helpers
├── metrics.rs                — Metrics collection
├── health.rs                 — Health check utilities
├── tpm.rs                    — TPM attestation support (optional feature)
├── ffi.rs                    — C FFI bindings
├── wcet_gate.rs              — Governor verdict WCET CI guard (O(1) structural
│                               boundedness argument; GOVERNOR_VERDICT_WCET_*_MICROS)
├── gateway/
│   ├── mod.rs
│   ├── policy.rs             — re-export shim → kirra_policy_types (ADR-0035
│   │                           Stage 0a de-monolith): OperationalCommand +
│   │                           classify_http_command moved to the zero-dep
│   │                           kirra-policy-types leaf crate; existing
│   │                           crate::gateway::policy::* paths unchanged
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
| `api_principals` | WS-1 (#G7) per-principal scoped API tokens (SHA-256 hash + role; plaintext never stored) |
| `cert_principals` | WS-1 (#G7) Track 1.2 mTLS cert principals (client-cert SHA-256 leaf fingerprint + role; CA-verified at the TLS layer, pinned here). WP-15 (G-19): nullable `not_after_ms` (X.509 notAfter) → the auth path fail-closes a cert at/past expiry exactly as on revocation; renewal = re-pin in place with a later expiry, no restart |
| `ota_campaigns` | WS-4 (Track 3) OTA governor-artifact campaigns (artifact digest + WP-12 `artifact_signature_b64` release signature + EP-13 `uptane_metadata_json` signed Uptane metadata set (nullable; schema v2 migration) + cohorts + staged rollout schedule + lifecycle state + halt reason; the `crate::ota_campaign` state machine's durable backing) |
| `node_artifact_status` | WS-4 (Track 3) per-node adoption reports (node_id PK + applied_digest + campaign_id + version + reported_at_ms + `attested`; upsert monotonic on reported_at_ms, non-audit-chained observability; the fleet summary's `applied_nodes`/`attested_nodes` join source) |

---

## Workspace Crates — the doer-checker / planning / perception side

Everything above documents the **`kirra-verifier`** root crate. The AV/Occy stack lives in
sibling crates. The load-bearing thesis: **a planner (the DOER) PROPOSES a trajectory; KIRRA
(the CHECKER) BOUNDS it.** The doer is swappable (geometric, learned, LLM-driven) and is never
trusted for safety; the checker is the invariant.

| Crate | Role |
|-------|------|
| `crates/kirra-policy-types` | **ADR-0035 Stage 0a (de-monolith).** Zero-dependency leaf: `OperationalCommand` (the doer-agnostic command-classification enum) + `classify_http_command` (the pure, total method+path classifier — SG-006/#69 fail-closed allowlist). The verifier's `gateway::policy` is a `pub use` re-export shim. First slice of the layered split; `kirra-industrial` (Stage 1) will depend on this. |
| `crates/kirra-core` | Lean shared types (no heavy deps): `corridor` (`CorridorSource`, `Point`, `MockCorridorSource`), `trajectory` (`PerceivedObject`, `Pose`, `TrajectoryPoint`, `TrajectoryVerdict`), `containment` (`MAX_TRAJECTORY_HORIZON`), `FleetPosture`, `kinematics_sim`, `capture`, `perception_monitor`, `KirraKernelGovernor`. Almost everything else depends on this, NOT on the heavy adapter. |
| `crates/kirra-ros2-adapter` | **The #131 Option-B CHECKER (re-export wiring) + ROS 2 node.** The checker modules named below — `validation.rs`, `prediction.rs`, `perception_redundancy.rs` — actually live in the lean **`crates/kirra-trajectory`** crate and are re-exported here; `state.rs` (`AdaptorState`) stays adapter-local and re-exports only the checker contract types (`AcceptedTrajectory`, `EgoOdom`, …) from `kirra_trajectory::state`. The adapter's own code is the `ros2`-gated `node.rs` plus that thin `state.rs`. `validation.rs` — `validate_trajectory_slow` / `validate_trajectory_slow_capped`: containment + per-pose kinematics + **RSS** (the §4 conjunction: danger needs BOTH longitudinal AND lateral unsafe) + **occlusion (RSS Rule 4)** + **multi-modal predictive RSS** (`predictive_rss_breach` over `PredictedMode`s). `prediction.rs` — the multi-modal **mode producer** (`predicted_modes_from_objects` / `slow_loop_modes`: CV always, CTRV when a tracker yaw is fresh). `perception_redundancy.rs` — the True-Redundancy `cross_check` + `resolve_redundancy_cap`. `state.rs` — `AdaptorState` (primary + secondary object channels, yaw channel). `node.rs` (**ros2-gated**) — slow/fast dual-rate loops, subscriptions. |
| `crates/kirra-planner` | **Occy, the geometric DOER + the Mick intent seam.** `GeometricPlanner` / `Planner` trait / `PlanInput` / `PlanOutput`. `mick.rs` — `plan_for_intent` grounds a `MickIntent` (`GoTo` / `LaneChange` / `Cruise` / `Overtake` / `PullOver` / `TurnAt` / **`RouteTo`** multi-junction). `learned.rs` — `LearnedPlanner` (speed-only Hydra-MDP) + **`LearnedManeuverPlanner`** (2-D lateral×speed vocabulary, routes around). `behavior.rs` — `TrafficControl` (signs/signals + **`OccludedApproach`** speed cap). `fast_loop.rs`, `mick_llm.rs`, `mick_capture.rs`. |
| `crates/kirra-map` | **Lanelet2-lite lane graph** (`kirra_map::lanemap`). `LaneGraph` (`route` Dijkstra, `route_corridor` / `route_drivable` stitch a multi-junction corridor, `route_to_point`, right-of-way / `junction_context`, **occlusion `sight_distance`**), `Lane`, `LaneCorridor`, `LineType` / `lane_lines`. Re-exported by `kirra-planner`. |
| `crates/kirra-taj` | **Taj, the R2 perception layer (ADR-0015).** Phase-A geometric corridor/objects from lidar; Phase-B semantic fusion (`clip_corridor_to_hazards` / `binding_hazard` / `hazard_clip_x` — water/obstacle hazards tighten the corridor); **`SemanticEvalSummary`** — the safety-weighted perception eval harness (`UnsafeMiss` / `OverConservative` / `Correct`, `hazard_recall`). EP-20: `kirra_kpi_gate::differential` runs Phase-A vs Phase-B over the shared ground-truth corpus and the KPI gate carries differential rows — `MissedTighten` (required divergence absent, unsafe) / `PhantomTighten` (unjustified, availability) / `ForbiddenLoosen` (fusion may NEVER extend Phase-A; hard 0) — with per-fault-family negative controls that must breach. |
| `crates/kirra-mick` | Mick examples / eval harness binaries (`mick_intersection`, `mick_eval`). |
| `crates/kirra-sidecars` | **The shipped doer-side sidecar binaries** (promoted from `kirra-mick` examples): `planner_service` (Occy `POST /plan` — grounds a goal OR a typed Mick `intent` via the ONE fail-closed `MickIntent::from_llm_json` parse; hardened seam: finite/in-map bounds, rate limit, loopback bind policy; returns the checker verdict + the #893 narration reason), `taj_service` (`POST /perception`), and `mick_service` (`POST /intent` typed text → `LlmBrain(OllamaClient)` → fail-closed typed intent, published read-only on `GET /intent/last` for `occy_doer`; optional `GET /narration/last` relays the verifier's auditor-tier `GET /system/verdicts/last` via `KIRRA_MICK_AUDITOR_TOKEN` — never the admin token). 🔴 FENCED with `kirra-mick` by `ci/check_mick_actuation_fence.py`: NO dependency route to actuation (release-token / serial consumer / ROS-DDS) can compile into these binaries — Mick publishes intents, never commands. |
| `crates/kirra-inline-governor` | **EP-01 — the in-line SHM enforcement path (G-1 software half).** `GovernorStation` (seqlock read → `decide_cycle` → release token over the ENFORCED bytes) + `ActuatorStation` (verify-before-release: token → strict Ed25519 over exactly the presented bytes → strictly-advancing release sequence → decode; refusals never poison the watermark). No HTTP on the enforced path. FDIT fault matrix (12 rows) + cross-process POSIX-SHM tests + `inline_demo` bin; WCET CI gate extended to the assembled loop (`wcet_gate::regression_inline_loop_full_step`). QNX-target rooting is the recorded remainder (crate README). |
| `crates/kirra-fleet-transport` | Zenoh-backed fleet transport (ADR-0007). Untrusted carrier: Ed25519 verify-before-use on every ingest + `RejectionCounter`. `ingress_limit` (WS-4) — a pure token-bucket `IngressRateLimiter` (per-source + global backstop, memory-bounded map) gates ingest BEFORE the verify, dropping a flood cheaply (`RejectReason::RateLimited`) so a signature-verify DoS can't ride the carrier. |
| `crates/kirra-governor-service`, `kirra-proposal-bench`, `kirra-wire-client` | Two-box prototype tools (UDP governor + proposal sweep + shared wire mirror; ADR-0032). |
| `crates/kirra-capture-schema`, `kirra-collector` | Governor-correction capture wire schema + collector (supervised-learning data path). |
| `crates/kirra-replay` | **EP-19 deterministic replay** (incident reconstruction): feeds captured `CommandGateway` JSONL back through the REAL checker (`validate_vehicle_command` / `enforce_degraded_decel_to_stop` over the real class profiles) and the REAL `record_from_verdict` emit mapping, comparing verdicts BIT-identically (`f64::to_bits`). Incomplete-context records (slow-loop summaries, derate-enabled, LOCKED_OUT, NaN-input) are CLASSIFIED, never guessed. serde_json `float_roundtrip` is LOAD-BEARING (default parse is one ulp off). CLI: `kirra-replay --class <class> session.jsonl` (exit 1 on divergence). See `docs/REPLAY_INCIDENT_RECONSTRUCTION.md`. |
| `crates/kirra-ota-installer` | **WS-4/Track-3 node-side dual-slot (A/B) governor-artifact installer** (doer side). Device-AGNOSTIC core: `Installer<B>` slot state machine (`Idle→Staged→Trying→{commit\|retry\|rollback}`), fail-closed `verify_staged_artifact` (SHA-256 vs the campaign's signed digest — a mismatch never arms the slot), health-gated automatic rollback, and the `BootController` trait (the one hardware seam). Two controllers: **`FileBootController`** (app-level: JSON boot record + `plan_*` over `BootRecord{active,try_boot,trying}`) and **`NvbootctrlBootController`** (`nvbootctrl.rs`, rootfs-level: the Jetson bootloader's native A/B slots via the `NvbootctrlRunner` command seam — `Installer`-driven, unit-tested over a mock; fail-closed slot parse). **App-level A/B is live end-to-end** on the Orin: `kirra-ota-ctl` (`run`/`stage`/`commit`/`rollback`/`probe`/`pull`/`enroll` + systemd unit; drill in `docs/ota/ORIN_APP_LEVEL_AB_DRILL.md`). `probe` = `HealthGate` consecutive-success gate → auto commit-or-rollback; `pull` = poll the verifier's `/fleet/campaigns/assignment/{node_id}` (#829), download-by-digest, verify, stage (`decide_pull`/`AssignmentView`); EP-13: an Uptane-ANCHORED node (provisioned `UptaneTrustStore`) additionally runs `uptane_pull_gate` BEFORE the download — full `verify_update` of the assignment-carried metadata set (root-keyed role sigs, freshness, chain agreement, rollback floors) + digest-authorization check, fail-closed (anchored + missing/stale/unauthorized set → refuse; the rollback floor persists only AFTER a successful stage); `model-allowlist` can now FETCH its bundle (`--metadata-url`; `ModelMetadataBundle` = the shared `UptaneMetadataSet` wire type). `enroll` (WP-16/G-8) = one audited `POST /attestation/register` provisioning this node as measured-boot: AK public key (derived from the PKCS#8 private, which never ships) + expected PCR16 value + `require_tpm_quote=true`, so `/attestation/verify` then demands a hardware TPM quote. **Rootfs-level** design + Orin drill: `docs/ota/ROOTFS_AB_DESIGN.md`, `docs/ota/ORIN_ROOTFS_AB_DRILL.md` (two-phase reboot-spanning driver + Secure Boot/dm-verity/PCR16 = follow-up). |
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
Fail-closed is **per mode**, not per mode-SET: a mode with inter-sample windows that evaluates
none of them (samples out of the ego trajectory's time span, all non-monotonic, …) → MRC, so one
object's evaluable mode can never mask another object's unevaluable one (#824).

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

**WS-1 (#G7) scoped RBAC.** Each gated group requires a SCOPE, satisfied by EITHER
the break-glass `KIRRA_ADMIN_TOKEN` (Admin holds every scope — the tiers below are
back-compatible) OR a per-principal API token (`api_principals`; role ∈
{`admin`,`integrator`,`auditor`,`operator`}) whose role holds that scope. The gate
is `authz::authorize_request` (`src/authz.rs`); `require_admin_token` is preserved as
the `SCOPE_ADMIN` specialization (INVARIANT #1/#6 unchanged: absent/empty root → 503).
Mint/manage principals via the admin-scoped `POST/GET /system/principals` +
`POST /system/principals/{id}/revoke` (token returned once at mint; stored only as
its SHA-256).

### Tier 1 — Identity-gated (`SCOPE_INTEGRATION_EVALUATE` + `x-kirra-client-id` header)
Admin token or an `integrator`-role principal.
- `GET  /system/posture/stream` — SSE broadcast of posture events
- `POST /federation/reports/submit` — Submit signed federated trust report
- `POST /action_filter/evaluate` — Evaluate action claim against posture
- `POST /fleet/campaigns/report` — WS-4 node adoption report (a node reports the governor digest it is now running → the fleet summary's `applied_nodes`; a write, so identity-gated, unlike the open read-only assignment GET; upsert by node_id, monotonic on `reported_at_ms`, not audit-chained). Optionally attestation-SIGNED: a base64 Ed25519 signature over `attestation::adoption_report_signing_payload(node_id, applied_digest, reported_at_ms)` verified against the node's registered `ak_public_pem` → `attested=true` (unforgeable attribution; `summary.attested_nodes`); invalid sig / no AK → 401 fail-closed; unsigned → accepted but `attested=false`
- `POST /industrial/evaluate` — Evaluate Modbus/OPC-UA industrial event

**Trust-BOOTSTRAP posture exemption:** the node-onboarding + attestation-handshake
routes below — `POST /attestation/register`, `/attestation/identity/register`,
`/attestation/challenge/:node_id`, `/attestation/verify` — are POSTURE-EXEMPT
(`is_posture_exempt`, `src/gateway/policy_layer.rs`). They establish/prove a node's
trust identity and cannot actuate, so the posture gate (which blocks COMMANDS) must
not gate them. This is load-bearing for the M-9 empty-fleet policy: a fresh Active
verifier with zero nodes is forced LockedOut and auto-recovers "the instant a node
is registered" — which requires those routes to be reachable UNDER LockedOut (else
the only way out of the lockout is blocked by the lockout — a bootstrap deadlock).
Each keeps its own guarantee independent of posture (admin token / challenge-response).

### Tier 2 — Admin (`SCOPE_ADMIN`; Bearer `KIRRA_ADMIN_TOKEN` or `admin`-role principal)
- `POST /attestation/register` — Register a node (posture-exempt — trust bootstrap)
- `POST /fleet/dependencies` — Register dependency graph edges
- `POST /system/backup/export` — Full state dump (admin-only; NOT in the auditor tier)
- `POST /system/audit/rotate-signing-key` — Rotate the audit signing key
- `POST/GET /system/principals`, `POST /system/principals/{id}/revoke` — API principal registry
- `POST/GET /system/campaigns`, `GET /system/campaigns/summary`, `GET /system/campaigns/{id}`, `POST /system/campaigns/{id}/{arm,advance,halt}` — WS-4 OTA governor-artifact campaign control plane (EP-13: create accepts an optional `uptane_metadata` signed metadata set — validated to authorize the campaign digest, else 422 — stored and relayed to nodes) (each lifecycle mutation writes an R156-shaped audit entry; `advance` is fail-closed on fleet posture — non-Nominal → HALT). `summary` = fleet rollout observability (`summarize_campaigns`: counts by state + active-campaign stage progress + halted-with-reason + `applied_nodes` adoption numerator joined from `node_artifact_status`; read-only, static path wins over `{id}`)
- `POST/GET /system/cert-principals`, `POST /system/cert-principals/{id}/revoke` — mTLS cert-principal registry (pin a CA-verified client cert by SHA-256 fingerprint → role). WP-15: register accepts optional `not_after_ms` (X.509 notAfter, must be future); the auth path fail-closes a cert at/past expiry; renewal = re-pin in place with a later expiry; GET surfaces `not_after_ms`/`expired`/`valid`
- `POST /federation/controllers/register` — Register trusted peer controller
- `POST /attestation/identity/register` — Register hardware fingerprint

### Tier 2a — Auditor read-only (`SCOPE_AUDIT_READ`; admin token or `auditor`-role principal)
- `GET  /system/audit/verify` — Verify audit chain integrity
- `GET  /system/audit/causal/verify` — Verify the fabric causal chain
- `GET  /system/audit/export` — Export the audit chain (read-only; no mutation rights)
- `GET  /verdicts/{verdict_id}` — EP-17 explainable safety verdict: one DENIED actuator command as a signed, human-readable artifact (machine `DenyCode` → operator explanation → recorded inputs + `inputs_digest_sha256` over the exact chained bytes + the chain fields: sequence/hashes/signature/key id). The id is minted by the deny arm and returned in the 400 body (`verdict_id` / `verdict_uri`); auditor tier because it exposes the denied command's raw inputs

### Tier 2b — Actuator (`SCOPE_ACTUATOR_COMMAND`; admin token or `operator`-role principal)
- `POST /actuator/motion/command` — behind the decel safety envelope + posture gate

### Unauthenticated (challenge-response provides its own guarantee)
- `POST /attestation/challenge/:node_id`
- `POST /attestation/verify`

### Public read-only
All unauthenticated. The observability GETs marked **posture-exempt** below bypass
the posture-routing gate (`is_posture_exempt`, GET/HEAD only), so they survive
LockedOut and a cold/stale posture cache — a GET cannot actuate, and blocking it
would remove fleet observability exactly when an operator needs to distinguish
"LockedOut" from "service down" (Bug 2). Their sibling WRITES on the same prefix
(e.g. `POST /federation/reports/submit`) stay posture-gated.
- `GET /health`, `GET /ready` — posture-exempt (liveness)
- `GET /metrics` — Prometheus fleet-safety series (WS-0.5) + WS-4 OTA rollout series (`kirra_ota_campaigns_total{state}`, `kirra_ota_campaign_rollout_percent{campaign_id}`, `kirra_ota_campaign_applied_nodes{campaign_id}` via `campaign_metrics_prometheus`) + WP-15 cert-lifecycle census (`kirra_cert_principals{state="active|revoked|expired|expiring_soon|no_expiry"}` via `cert_expiry_prometheus`); posture-exempt so the scrape survives LockedOut
- `GET /attestation/status/:node_id` — posture-exempt (Bug 2)
- `GET /fleet/posture`, `GET /fleet/posture/:node_id` — posture-exempt (Bug 2)
- `GET /fleet/history/:node_id`, `GET /fleet/flapping/:node_id` — posture-exempt (Bug 2)
- `GET /fleet/campaigns/assignment/:node_id?cohorts=a,b` — WS-4 node-facing OTA artifact assignment (which signed governor digest this node should run under the active campaigns; **posture-GATED → denied under LockedOut**, deliberately NOT exempt — it drives a node's install decision, not observability). EP-13: relays the campaign's signed Uptane metadata set (`uptane_metadata`) when present — the verifier is an untrusted carrier; verification is end-to-end at the node
- `GET /federation/reports/:asset_id` — posture-exempt (Bug 2)

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

// campaign_monitor.rs
CAMPAIGN_SWEEP_MS            = 1_000      // OTA campaign posture-sweep interval

// cert_expiry_monitor.rs
CERT_EXPIRY_SWEEP_MS        = 3_600_000  // mTLS cert-expiry census interval (1h)
CERT_EXPIRY_WARN_WINDOW_MS  = 1_209_600_000 // "expiring soon" horizon (14 days)

// audit_shipper.rs
AUDIT_SHIP_INTERVAL_MS      = 5_000      // WORM off-box audit-ship cycle interval
```

---

## Key Algorithms

**Gray/Black DAG Traversal** (`kirra_safety_authority::dag::recursive_calculate`, ADR-0035 Stage 3d; `AppState::calculate_posture*` delegate to it):
- Gray set = nodes currently on the active call stack (cycle detection)
- Black set = nodes fully evaluated (memoization, handles diamond DAGs)
- Cycle (gray-set back-edge) → `FleetPosture::LockedOut` tagged `CYCLE_DETECTED`
- Depth backstop is a DYNAMIC bound `max(nodes+edges, MAX_DEPENDENCY_DEPTH)` (10 is
  a floor, not a fixed cap): exceeding it → `LockedOut` tagged `MAX_DEPTH_EXCEEDED`
  (distinct from the cycle tag; unreachable on a valid acyclic graph)
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

**WP-17 (G-17): the canonical machine-readable registry of every verifier `KIRRA_*`
var is `KIRRA_ENV_KEYS` in `src/env_config.rs`** — this table mirrors it. At startup
the service WARNs on any `KIRRA_*` env var NOT in the registry (a typo / stale var
that is not taking effect) and commits an `EffectiveConfigDigest` audit event (the
SHA-256 of the boot-config snapshot, so drift is detectable across restarts). Adding
a new `KIRRA_*` read means adding its `EnvKeySpec` row. **EP-12 (Config Slice B,
config v2):** the migrated module families — the gateway/actuator envelope class
(`contract_profiles`), HA (`standby_monitor` + `lease`: `KIRRA_INSTANCE_ID{,_FILE}`,
`KIRRA_HEARTBEAT_INTERVAL`, `KIRRA_PROMOTION_{TIMEOUT,POLL}`, `KIRRA_FORCE_PROMOTE`,
`KIRRA_HA_LEASE_ENABLED`), and the audit shipper (`KIRRA_AUDIT_SHIP_PATH`) — read
ONLY through the boot-validated `EffectiveConfig` snapshot (zero direct env reads,
greppable). **A malformed value in any migrated var now fails at BOOT
(`ConfigError` → startup abort), never silently defaulting at use** — including
`KIRRA_HA_LEASE_ENABLED`, where a typo previously fell back to the legacy path
with only an error log. Per-instance identity is carried on the snapshot but
`serde(skip)`ped out of the digest (fleet digests stay instance-independent).

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
| `KIRRA_HA_LEASE_ENABLED` | No | off | EP-03 lease-based failover trigger (`1`/`true`). Gate ON: the Active renews the durable `ha_state` lease at the half-life cadence (TTL 3 s → renew 1.5 s) and self-demotes on its own expiry; the standby promotes when BOTH the heartbeat token AND the lease stamp go unobserved-to-advance for `promote_after` (4.5 s) — ≤5 s failover, drill-proven (`tests/ha_two_process_drill.rs` gate-on test). Conjunctive trigger keeps a mixed-config fleet safe (a gate-off primary's advancing heartbeat blocks promotion). The durable epoch CAS remains the sole takeover authority. Default off = legacy ~12 s heartbeat-timeout path, byte-identical |
| `KIRRA_SUPERVISOR_RESET_KEY` | Yes (reset ops) | — | Must be non-empty, ≤ 64 bytes |
| `KIRRA_VEHICLE_CLASS` | Yes (#312) | — | Deployment vehicle class: `courier` \| `delivery-av` \| `robotaxi`. Selects the per-class kinematic contract in the actuator gate (`contract_for`/`mrc_fallback_for`, robotaxi = the frozen instance) AND the parko node's SG6 `impact_cfg_for_class`. **Fail-closed: there is no default class** — unset/empty/unknown aborts startup in BOTH the verifier service and the parko-ros2 node (a wrong class would select another class's envelope). See `docs/CONTRACT_PROFILES.md` |
| `KIRRA_CANOPEN_NODE_MAP` | No | — | CANopen node-id → fleet-node-id map (#84), `canid:fleet_node` comma-separated (e.g. `5:robot-01,6:robot-02`). Unset → every NMT-offline is unattributed (fail-closed) |
| `KIRRA_FABRIC_ASSET_ID` | No | — | Local fabric asset id fed by the verifier→fabric posture feed (#88). Unset/empty → feed inert (asset keeps its `Degraded` registration seed) |
| `KIRRA_DNP3_ANALOG_OUTPUT_ENVELOPE` | No | — | DNP3 Analog Output (g41) magnitude envelope as `min:max` (e.g. `-100.0:100.0`). A control write (Operate/Direct_Operate) whose decoded setpoint is outside the envelope is denied. Unset/invalid → analog control writes are **denied (fail-closed)**; faithfully-undecodable g41 payloads are also refused (never fabricated) |
| `KIRRA_CANOPEN_SDO_BOUNDS` | No | — | Per-target CANopen SDO expedited-download magnitude bounds, `node:index:subindex=type:min:max` comma-separated (e.g. `5:0x6042:0=i16:-500:500`). `type` ∈ {i8,u8,i16,u16,i32,u32,f32}. A download to a configured target is faithfully decoded **by the configured type** (the OD entry — the frame carries width at best, never type; #85) and bounded: out-of-range/undecodable/segmented/width-mismatch → denied. Unconfigured targets are posture-only. Unset → SDO writes are posture-only |
| `KIRRA_CANOPEN_STRICT_BOUNDS` | No | `false` | `1`/`true` → a CANopen SDO **download** to a target with NO configured bound is denied (high-assurance mode) instead of posture-only. Reads/uploads/non-SDO frames are unaffected |
| `KIRRA_CIP_ATTR_BOUNDS` | No | — | Per-attribute CIP (EtherNet/IP) magnitude bounds, `class:instance:attr=type:min:max` comma-separated (e.g. `0x0A:1:3=i16:-500:500`). All keys `u16` (decimal or `0x`-hex); `type` ∈ {i8,u8,i16,u16,i32,u32,f32,f64}. A `Set_Attribute_Single` (0x10) write to a configured target is faithfully decoded **by the configured type** (the CIP attribute's data type — the frame carries only bytes; #85) and bounded: out-of-range/undecodable → denied. Other services (reads / `Write_Tag` / `Execute_Service`) carry no faithfully-located scalar → posture-only. Unconfigured targets posture-only. Unset → CIP writes posture-only |
| `KIRRA_CIP_STRICT_BOUNDS` | No | `false` | `1`/`true` → a CIP `Set_Attribute_Single` to a target with NO configured bound is denied (high-assurance mode) instead of posture-only. Reads / other services are unaffected |
| `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` | No | — | Governor release-signing key source (`kirra_release_token::provisioning`, ADR-0031 Clause E): `file:<path>` (permission-checked, zeroized 32-byte Ed25519 seed) \| `dev-fixed` (well-known harness key, needs the ALLOW_DEV flag) \| `tpm:<handle>` (deferred → `TpmUnsealUnsupported`). **Unset/empty → refuse** (fail-closed; never mints an unpinnable key). See `docs/safety/GOVERNOR_KEY_PROVISIONING.md` |
| `KIRRA_GOVERNOR_SIGNING_KEY_ALLOW_DEV` | No | `false` | `1`/`true` → admit the `dev-fixed` governor key source. Absent → `dev-fixed` is refused (`DevKeyNotAllowed`) — a non-production key never loads by default |
| `KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT` | No | `false` (off) | WP-16 (MGA G-8) measured-boot fleet default. `1`/`true` → a `POST /attestation/register` that OMITS `require_tpm_quote` defaults to quote-required (the node's `/attestation/verify` then demands a hardware TPM quote). An EXPLICIT `require_tpm_quote` in the request always wins (a TPM-less node can still register `false`). Unset/off → omitted field is `false` (back-compat, byte-identical). Pure resolver `resolve_require_tpm_quote`; node-side one-call enrollment via `kirra-ota-ctl enroll` |
| `KIRRA_TLS_CERT_PATH` / `KIRRA_TLS_KEY_PATH` | No | — | Opt-in in-process TLS termination (WS-1 Track 1.2, `src/bin/kirra_verifier_service/tls.rs`): PEM cert-chain + private-key paths. **Both set** → verifier terminates TLS in-process (rustls, `ring` provider only — no `aws-lc-rs`). **Exactly one set** → fail-closed startup abort (a half-configured TLS listener must not fall back to plaintext). **Neither** → plaintext (default, byte-identical; mesh terminates TLS per ADR-0006 Clause 3). Cert/key validated before bind. See `docs/safety/TRANSPORT_SECURITY.md` §4 |
| `KIRRA_TLS_CLIENT_CA_PATH` | No | — | Opt-in **mTLS** (WS-1 Track 1.2). Set (server TLS must ALSO be on) → client certs are REQUIRED and CA-verified by rustls's `WebPkiClientVerifier`; the verified leaf's SHA-256 fingerprint resolves to a `cert_principals` principal when no bearer token is presented (same RBAC). Set WITHOUT server TLS → fail-closed startup abort. Unset → no client auth. See `docs/safety/TRANSPORT_SECURITY.md` §4 |
| `KIRRA_AUDIT_SHIP_PATH` | No | — | WS-4 WORM off-box audit shipping (`src/audit_shipper.rs`). Set to an append-only sink FILE path → the Active instance spawns a background shipper (`AUDIT_SHIP_INTERVAL_MS`) that appends each new hash-chained audit record there (a WORM volume / log-shipping agent carries it off-box; the shipped stream re-verifies independently via `verify_shipped_chain`). Ship-then-advance + fsync (at-least-once; consumer dedupes by `sequence`). Unset/empty → shipping OFF (default, byte-identical) |
| `KIRRA_HTTP_MAX_CONCURRENCY` | No | `512` | WP-03 control-plane backpressure (`src/bin/kirra_verifier_service/backpressure.rs`): the API plane's shared concurrency pool (one semaphore across every non-probe, non-console route). At capacity a request is load-shed as **429 + `Retry-After`** (never queued unbounded; 503 stays posture-denial's code). Probes (`/health`, `/ready`, `/metrics`) are exempt. Set-but-invalid (non-numeric/0) → fail-closed startup abort |
| `KIRRA_HTTP_CONSOLE_MAX_CONCURRENCY` | No | `64` | The operator console's OWN isolated pool (an API flood cannot starve the LockedOut recovery surface — clearance grants + the ADR-0013 e-stop request — and vice versa). Same shed/abort semantics as `KIRRA_HTTP_MAX_CONCURRENCY` |
| `KIRRA_HTTP_MAX_BODY_BYTES` | No | `262144` | Request-body cap on both pools (413 over the cap). Control-plane payloads are small (PEMs, campaign specs); the cap bounds memory under a slow-body flood. Same abort-on-invalid semantics |

**`kirra-ros2-adapter` slow-loop env gates** (consumed in `node.rs`, opt-in, default off →
byte-identical prior behaviour): `KIRRA_PERCEPTION_DERATE_ENABLED` (Track-C perception-derate cap),
`KIRRA_PERCEPTION_REDUNDANCY_ENABLED` (the True-Redundancy divergence monitor — enables the
`~/input/objects_secondary` channel-B subscription; enabled-but-silent-B → fail closed),
`KIRRA_VRU_CHANNEL_ENABLED` (#789 follow-up 1 — enables the `~/input/pedestrians` subscription
feeding the omnidirectional pedestrian bound; the pure `vru_channel::resolve_vru_channel` makes
the three-way decision DISARMED→checker no-op / armed+fresh→live `PedestrianScene` / armed+silent→
MRC-floor cap, so an armed-but-silent VRU sensor STOPS the ego rather than driving blind; producer
must publish at rate per AOU-VRU-RATE-001),
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
- **`tools/iceoryx2-spike/`** (#273 → WP-21b) — the iceoryx2 carrier tree (own
  isolated workspace; iceoryx2 never enters the SDK/parko dep tree). Three tiers:
  the original feature-subset spike (`src/judge.rs`, seqlock-style owned snapshot,
  same `<=` replay rule), the frozen-contract carrier (`src/frozen.rs`, #275/L2 —
  the production `GovernorContractView` + `validate()` over a real channel), and
  **WP-21b production adoption** (`src/inline.rs`): the ASSEMBLED EP-01 loop
  (`GovernorStation` → Ed25519 release token → `ActuatorStation`
  verify-before-release) consuming commands received over iceoryx2 —
  release/clamp/replay/CRC/NaN rows, both feature configs, gated by the
  `iceoryx2-spike` CI lane. Crate-level opt-in = the feature gate.
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

## Robot Side — R2 / Rabbit operator & doer layer (`robot/`)

`robot/` is the **doer-side operator layer** for the Rosmaster R2 on Jetson Orin
NX — Python scripts + install tooling, NOT a Cargo crate. It holds no safety
authority: every actuation goes through the ADR-0033 verify-token chokepoint, and
every conversational path is fenced.

**Governed drive chokepoint (ADR-0033):** `kirra_motor_consumer.py` (verifying
consumer — reads the Ed25519 release token, verifies via `kirra_ffi.py` ctypes →
`libkirra_consumer_ffi.so`, NO crypto in Python, then drives) + `r2_drive.py`
(the pure R2 Path-B Ackermann last-hop — `translate`/`odom_step`, host-tested in
`r2_drive_test.py`; `KIRRA_DRIVE_MODE=r2_ackermann` = set_motor + AKM steering,
bypassing the broken X3 firmware mixer). Consumer `/odom` gated by
`KIRRA_R2_ODOM_ENABLED`.

**Rabbit — the conversational operator layer** (`docs/hardware/RABBIT_CONVERSATION_DESIGN.md`).
Two output channels, only one reaches the wheels:
- **Channel A (SPEAK)** — pure speech, zero actuation authority: `rabbit_ask.py`
  (grounded Q&A), `rabbit_converse.py` (multi-turn persona + router), `rabbit_watch.py`
  (proactive posture/deny narration), `rabbit_boot.py` (posture-gated boot greeting
  + shutdown).
- **Channel B (ACT)** — the ONLY motion path: a movement directive's TEXT → mick
  `POST /intent` → `MickIntent::parse_llm_json` → Occy → the KIRRA checker. Rabbit
  never builds an intent, a Twist, or a token. 🔴 Single-door invariant: system
  commands (OTA) do NOT ride the movement door.
- `rabbit_persona.py` — shared stdlib-only helpers: the `{name}` slot
  (`operator_name`/`name_slot`), `speak()`, and the LLM model pin
  (`read/write_model_pin`, `classify_model_pin`).
- The doer LLM is local Ollama (`KIRRA_RABBIT_MODEL`, default `gemma3:4b`);
  STT = whisper.cpp, TTS = piper (`docs/hardware/RABBIT_AUDIO_STACK.md`, incl. the
  systemd-audio caveat). Deterministic (NON-LLM) speech — OTA (`rabbit_ota.py`),
  proactive (`rabbit_watch.py`), boot (`rabbit_boot.py`) — are templates fired by real
  events. Every spoken line is catalogued in `docs/rabbit/RABBIT_VOICE_LINES.md`.

**Model-swap discipline** (the doer LLM is swappable with ZERO safety re-review —
the checker is model-agnostic): `rabbit_model_smoketest.py` is the doer-contract
gate — fires canned utterances through the REAL router contract (`STAGE2_SYSTEM` +
`parse_reply`) and asserts JSON / drive→directive / chat→null / no-fabrication.
On PASS it pins the model's Ollama **digest + vetted-at timestamp**
(`~/.kirra_rabbit_model.pin`) — the "no version bump" stealth-update guard;
`--pin-check` compares, boot warns (voice line A5) on drift. Commands:
`RABBIT_BRINGUP_RUNBOOK.md` → "Swapping the LLM". Pure logic host-tested in
`rabbit_voice_test.py`.

**OTA voice check** (`rabbit_ota.py` + `kirra-ota-check.timer`): "stage and ask" —
`kirra-ota-ctl pull` stages only; applying is a deliberate health-gated `probe`
(`docs/hardware/R2_OTA_VOICE_CHECK.md`). Never touches LLM weights.

**Install** (`robot/install/`): `install_robot_units.sh` stages the Rabbit scripts +
systemd units (`kirra-ros-stack`, `kirra-rabbit-watch`, `kirra-rabbit-greet`,
`kirra-ota-check`) — STAGED, not enabled (validate wheels-up first:
`R2_AUTOSTART_CHECKLIST.md`). Base-image install (verifying consumer + FFI +
lidar): `robot/install/README.md`. Bring-up: `docs/hardware/RABBIT_BRINGUP_RUNBOOK.md`.

**Autoware distro isolation (ADR-0036):** the Autoware AV-stack doer stays pinned
to Ubuntu 22.04/Humble in its own container while the rest of the stack (ros2
adapter, checker, Occy/Taj) moves to 24.04/Jazzy; they meet only on 5 curated,
hash-verified boundary topics (`ros2_ws/src/autoware_*_msgs`). Occy does NOT
replace Autoware's L4 breadth (localization / control / mature fused perception /
HD-maps) — KIRRA is the checker that bounds it. Scaffold: `deploy/autoware-isolation/`
+ `scripts/curated_interface/crossdistro_hash_check.sh`.

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
