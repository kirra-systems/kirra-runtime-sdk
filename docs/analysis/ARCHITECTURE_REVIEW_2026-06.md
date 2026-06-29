# Kirra Runtime SDK — Principal-Engineer Architecture & Implementation Review

**Date:** 2026-06-29
**Reviewer:** Automated principal-engineer review (Claude Opus 4.8)
**Scope:** Full workspace (~111K LOC Rust + console frontend + infra), reviewed incrementally by subsystem.
**Method:** Divide-and-conquer. Breadth-first repository/architecture mapping, then adversarial per-subsystem deep reviews, then cross-cutting synthesis. Findings carry severity + confidence and `file:line` evidence.

> This document is a point-in-time engineering assessment, not a formal safety certification. "Critical" denotes engineering risk to correctness/safety/security, not an ISO 26262 ASIL determination.

---

## Phase 1 — Repository Discovery

### Shape
- **Build system:** Cargo workspace, resolver 2, edition 2021. Root crate `kirra-verifier` (lib `rlib`+`cdylib` + 2 bins). 15 member crates under `crates/*`. `parko/` is a **separate** Cargo workspace (own resolver) consumed via `path=` deps.
- **Languages:** Rust (~111K LOC) dominant; `console/` frontend (~74 JS/TS/HTML files); Python/C++ test artifacts (`hil_replay_proof.py`, `native_test.cpp`); shell (`install.sh` 30KB); Helm/Docker/compose infra.
- **Release profile:** `panic = "abort"` (fail-closed by process death). Dev/test left on unwind for `#[should_panic]`.

### LOC by subsystem (Rust, approximate)
| Area | LOC | Notes |
|---|---|---|
| `src/` (root `kirra-verifier`) | 38.3K | HTTP service, verifier core, posture engine, persistence, adapters, gateway, HA |
| `crates/kirra-planner` | 12.1K | Occy geometric DOER + Mick intent seam |
| `crates/kirra-core` | 6.5K | Lean shared types (corridor/trajectory/containment/kinematics) |
| `crates/kirra-ros2-adapter` | 6.1K | ROS2 integration layer + node; re-exports checker modules from `kirra-trajectory` |
| `crates/kirra-trajectory` | 3.7K | **The CHECKER** (RSS validation core) + trajectory/state/validation primitives |
| `crates/kirra-map` | 2.8K | Lanelet2-lite lane graph |
| `crates/kirra-collector` | 2.4K | Capture/collector data path |
| `crates/kirra-mick`, `kirra-taj`, `kirra-fleet-transport`, `kirra-contract-channel`, others | ~6K | |
| `parko/` (separate ws) | 26.7K | parko-ros2 9.3K, parko-core 7.6K, parko-kirra 7.1K, inference backends |
| `tests/` (root integration) | 3.5K | fault_injection, ha_tests, posture_gate, rss, temporal, clause2 trust-chain |

**Largest single files (review-attention magnets):** `src/bin/kirra_verifier_service.rs` (3906 — monolithic router/handlers), `src/verifier_store/tests.rs` (2097), `src/standby_monitor.rs` (1498), `src/telemetry_watchdog.rs` (1391), `src/verifier_store/audit.rs` (1268), `src/verifier.rs` (1092), `src/gateway/policy_layer.rs` (1009).

### Entry points / executables
- **Primary product:** `kirra_verifier_service` (`src/bin/kirra_verifier_service.rs`) — axum HTTP governor on `0.0.0.0:8090`.
- **Secondary:** `kirra_carla_client` (CARLA sim integration).
- Dev/optional bins: console demo seed, two-box UDP governor (`kirra-governor-service` + `kirra-proposal-bench` + `kirra-wire-client`), parko ROS2 node.

### CI/CD
`.github/workflows/`: `ci.yml` (large, multi-job), `release.yml`, `docker.yml`, `deploy-pages.yml`. CI jobs: `test` (+ fault-injection CERT-004, capture-feature), `coverage` (MC/DC pending #65), `wcet-gate`, `parko-safety`, `parko-ros2-backends`, `parko-onnx`, `parko-openvino`, `static-analysis` (clippy `-D warnings`, cargo-audit, shellcheck, helm lint), `parko-install-framework`, `ros2-adapter-build` (`--features ros2` under sourced Jazzy). Docker: multistage, non-root `USER kirra`, no secrets baked.

### Documentation
Strong: `CLAUDE.md` (35KB architecture bible), `README.md` (40KB), `AGENTS.md`, `INSTALL.md`, `SECURITY.md`, **29 ADRs** under `docs/adr/`, plus `docs/safety/` (safe-state spec, WCET methodology, hypervisor contract channel), `docs/testing/`, `docs/roadmap/`.

---

## Phase 2 — Architecture Mapping

### The load-bearing thesis
**Doer–Checker separation.** A planner (the DOER: geometric/learned/LLM) *proposes* a trajectory or command; Kirra (the CHECKER) *bounds* it. The doer is swappable and never trusted for safety; the checker is the invariant. Fail-closed everywhere: absent positive trust evidence ⇒ `LockedOut` ⇒ deny.

### Two top-level products
1. **`kirra-verifier` (fleet-legitimacy engine + governor service)** — distributed trust/posture authority. HTTP service, SQLite-backed registry, gray/black DAG dependency posture, Ed25519 attestation + federation, industrial protocol bounding, HA standby. (root `src/`)
2. **Doer–checker AV stack** — `kirra-ros2-adapter` (the per-trajectory RSS checker), `kirra-planner`/`kirra-map`/`kirra-taj` (doer + perception + maps), `parko/` (ML inference + diverse-governor comparison).

### Runtime / process / boundaries
- **Service process:** single axum process; `ServiceState { app: Arc<AppState>, posture_cache, posture_engine_sender }` shared across handlers. Background tokio tasks: posture-engine worker (mpsc-coalesced recalc), telemetry watchdog, HA heartbeat writer + promotion monitor, capture writer, SSE broadcaster.
- **Persistence:** rusqlite (bundled SQLite, WAL), single `Arc<Mutex<VerifierStore>>`. 11 tables (nodes, dependencies, posture_events, av_subsystem_meta, posture_engine_state, audit_log_chain, federation tables, attestation registry).
- **IPC / networking:** HTTP (axum 0.8) for fleet API; SSE for posture stream; Ed25519-signed federation reports between controllers; Zenoh fleet transport (`kirra-fleet-transport`); DDS/CycloneDDS actuator bridge (Volatile durability); industrial protocol adapters (Modbus/OPC-UA, CANopen, DNP3, EtherNet/IP). EPIC #270: governor command path moving to Rust-end-to-end on a QNX partition with an iceoryx2/seqlock contract channel (`kirra-contract-channel`, host spikes under `tools/`).
- **Config flow:** env-var driven (fail-closed: `KIRRA_ADMIN_TOKEN`/`KIRRA_SUPERVISOR_RESET_KEY` absent ⇒ abort/503). `src/config.rs` + `startup_sentinel.rs` pre-flight invariant checks.
- **Startup:** sentinel invariant checks → store open → generation restore from SQLite → spawn background workers → bind axum. **Cold-start fleet is empty ⇒ `LockedOut` ⇒ all non-exempt routes 503** (intentional fail-closed).
- **Shutdown:** SIGINT/SIGTERM graceful handler (can linger seconds).

### Coupling / smells (architecture level)
- **Monolithic router file** `src/bin/kirra_verifier_service.rs` at ~3.9K LOC concentrates all route wiring + many handlers — a maintainability and review-surface hotspot (partially decomposed into submodules `industrial.rs`/`fleet.rs`/`console.rs`/`operators.rs`).
- **De-monolithication in progress:** several `src/*.rs` are now thin re-export shims into `kirra-core` (`kinematics_sim`, `capture`, `gateway/perception_monitor`) and `kirra-fleet-types` (`federation*`). Good direction; transitional duplication/indirection cost remains.
- **Global mutable statics** for posture generation (`POSTURE_GENERATION` AtomicU64) introduce process-global ordering coupling between `force_lockout` and concurrent recalc (see F-VC4).
- **Single store Mutex** is a structural serialization point for all persistence (see persistence findings).
- `parko/` separate-workspace boundary is deliberate and clean (keeps ML deps out of the safety-kernel build).

---

## Phase 3 — Subsystem Reviews

### 3.1 — Verifier Core: DAG posture + posture engine + cache
**Files:** `src/verifier.rs`, `src/posture_cache.rs`, `src/posture_engine.rs`, `src/posture_engine_v2.rs`
**Purpose:** The fail-closed trust core. Gray/black DAG dependency traversal → `FleetPosture`; cache + generation-monotonic broadcast; command-routing gate.

**Strengths**
- Backward-clock fail-closed is correct and centralized: `is_stale_with_ttl` uses `checked_sub` (`None ⇒ stale`); both the gate and the engine route through it (`posture_cache.rs:149`).
- DAG deadlock-avoidance via id snapshot to an owned `Vec` before re-entrant `nodes.get()` — avoids DashMap shard self-deadlock; applied consistently (`verifier.rs:526`, `posture_engine.rs:99`).
- Audit-commit-gated cache write: `recalculate_and_broadcast` returns *without* touching cache/broadcast if `save_posture_event_chained` fails (`posture_engine.rs:261`) — true fail-closed ordering. `Unknown` early-return (INV-9) and Degraded `ActuatorMotion` deferral (ADR-0011) both present & correct.

**Findings**
| ID | Title | File:line | Sev | Conf |
|---|---|---|---|---|
| VC1 | Nonce expiry uses `>` (exclusive) while staleness uses `>=` — boundary inconsistency, benign (accepts nonce *at* `expires_at_ms`) | `verifier.rs:646` | Low | High |
| VC2 | Gray-node cycle early-return fabricates `local_status: Unknown` for a node that may be Trusted/Untrusted — posture still fail-closed LockedOut; observability-only | `verifier.rs:554` | Low | Med |
| VC3 | `next_generation` mixes `fetch_add` (returns pre-increment) with `fetch_max(last+1)` restore — correct only because the *consumed* value is persisted; fragile contract worth a restart-during-recalc test | `posture_engine.rs:35` | Med | High |
| **VC4** | **Immediate-lockout race:** `force_lockout` and concurrent `recalculate_and_broadcast` both acquire a global generation then CAS strict-greater; a slower recalc holding a *higher* gen can land after a supervisor `force_lockout` and cause the lower-gen LockedOut to be dropped. Mitigated (eventually) by sticky `supervisor_tripped`, but the *immediate* guarantee is not race-free | `posture_engine.rs:325`, `293` | Med | Med |
| VC5 | Stale-cache detection compares two independent `SystemTime` reads (engine vs gate); forward clock skew < TTL could read stale as fresh — AOU-TIMESYNC-001, not a code bug | `posture_cache.rs:258` | Low | Med |
| VC6 | Worker `spawn_blocking` panic after `apply_rss_state` mutated streak counters leaves streaks half-updated (escalation still forces LockedOut) | `posture_engine_v2.rs:476` | Low | Med |

**Risk: Medium · Confidence: High.** Core fail-closed posture is sound; the actionable item is **VC4** (immediate-lockout under concurrent recalc) and a **VC3** restart-during-recalc test.

---

### 3.2 — Persistence: SQLite store + audit hash-chain
**Files:** `src/verifier_store/*`, `src/audit_chain.rs`, `src/audit_writer.rs`
**Purpose:** rusqlite (bundled, WAL) registry + SHA-256 hash-chained tamper-evident ledger, single `Arc<Mutex<VerifierStore>>`.

**Strengths**
- Genuinely atomic federation commit: report INSERT + generation high-water gate + nonce burn (plain INSERT mapping UNIQUE→`NonceReplay`) + audit append + retention prune all ride **one `Immediate` transaction** with the epoch fence as the first statement — partial commit impossible; double-accept race closed.
- Fail-closed prev-hash read: `QueryReturnedNoRows` is the *only* path to genesis; all other read errors propagate instead of forking a fresh chain (fixes prior "fork-to-genesis on error").
- Mutex poisoning handled, not panicking: `StoreHandle::with/call` use `unwrap_or_else(|p| p.into_inner())`; single-writer audit task never holds the lock across `.await`.

**Findings**
| ID | Title | File:line | Sev | Conf |
|---|---|---|---|---|
| **PS1** | **Audit chain split across two connections.** `audit_log_chain`/`audit_anchor_head` written from both NORMAL `conn` (posture events, `transaction()`=DEFERRED) and FULL `durable_conn` (KEY_ROTATION, `Immediate`). Tail-read-then-INSERT under asymmetric isolation can link a new row off a stale `previous_hash`/`sequence` and fork the chain (caught only later by verify). The store Mutex mostly hides it within one process | `audit_chain.rs:268`, `audit.rs:976` | Critical | Med |
| PS2 | `verify_audit_chain_full`: a KEY_ROTATION row failing row-signature leaves later rows "unknown key → fail", but `chain_intact` is hash-only — callers keying off `chain_intact` alone miss `signature_valid=false` | `audit.rs:594`,`706`,`769` | High | Med |
| PS3 | Cross-connection durability inversion on the shared singleton `audit_anchor_head` (asymmetric synchronous levels) — fragile though likely consistent; treat as documented #74 boundary | `audit.rs:976` | High→Med | Med |
| PS4 | `unchecked_transaction()` on `&self` bypasses rusqlite's nested-tx guard (safe only because the Mutex serializes; removes a future re-entrancy backstop) | `fabric.rs:92`, `nodes.rs:123` | Med | High |
| PS5 | Migration idempotency detected by `to_string().contains("duplicate column name")` — a library/locale message change turns benign re-run into a startup outage | `mod.rs:475`, `audit.rs:404` | Med | High |
| PS6 | `save_last_generation` monotonic UPSERT silently no-ops (0 rows) on a lower generation — caller can't distinguish rejection from success | `posture.rs:176` | Med | High |
| PS7 | `burn_federation_nonce` uses wall-clock `SystemTime::now()` not injected `now_ms` (diagnostic field only) | `federation.rs:256` | Low | High |
| PS8 | `load_fabric_assets`/`causal_entry_from_row` swallow deserialize errors to defaults (`unwrap_or(Unknown)`), masking corruption as empty/Unknown — inconsistent with fail-closed *skip* used elsewhere | `fabric.rs:44`,`219` | Low | Med |

**Risk: High · Confidence: Medium.** The single store Mutex is a correctness crutch *and* a scalability ceiling. **PS1** is the headline: the two-connection audit-chain design is the one place where the Mutex's serialization is the only thing preventing a chain fork — any future change that lets the two connections write concurrently breaks tamper-evidence. Recommend a single audit-writer connection (or a dedicated audit mutex) so atomicity does not depend on the global store lock.

---

### 3.3 — High Availability, Watchdog, Recovery, Industrial Adapters
**Files:** `src/standby_monitor.rs`, `src/telemetry_watchdog.rs`, `src/recovery_hysteresis.rs`, `src/adapters/{canopen,dnp3,ethernet_ip}.rs`, `src/protocol_adapter.rs`

**Strengths**
- Monotonic heartbeat-freshness (`HeartbeatFreshness`): treats the primary's timestamp as an opaque change-token timed on the standby's own `Instant` — structurally eliminates cross-machine clock skew / future-dated heartbeat.
- Durable epoch fence (`try_claim_epoch`): split-brain prevention as a SQLite-serialized CAS; tests use a shared temp-file DB (not `:memory:`) so the race is genuinely exercised.
- Recovery hysteresis fail-closed arms precise: time-window AND count both required; store failures degrade to `StreakBuilding`; boundary `>= threshold` tested both sides.
- Type-driven (not frame-width) industrial decode with config-supplied signedness; unconfigured/invalid configs → fail-closed deny. **No panics/unwrap/overflow** found in decode paths (checked `from_le_bytes` over length-guarded slices, `saturating_sub` for time).

**Findings**
| ID | Title | File:line | Sev | Conf |
|---|---|---|---|---|
| HA1 | CANopen non-size-indicated expedited SDO (`s=0`) bypasses width check: a 4-byte payload against configured `i16` decodes leading 2 bytes, silently ignores 2 attacker bytes, no `WIDTH_MISMATCH`. "Decode by exact width" only holds for `s=1` | `adapters/canopen.rs:221` | Med | High |
| HA2 | DNP3 analog setpoint uses `len() >= N` not `==`; over-long object decoded from leading bytes, rest dropped silently (by-design per doc but inconsistent with deny-on-width-mismatch elsewhere; value still bounded) | `adapters/dnp3.rs:152` | Low | High |
| HA3 | CIP `Set_Attribute_Single` never compares data length to configured width; over-long payload admitted from leading bytes (CIP frames lack self-described width — documented limitation) | `adapters/ethernet_ip.rs:171` | Low | High |
| **HA4** | **HA split-brain window unguarded for env-tuned timings.** Const-assert guards only the *defaults*; `KIRRA_HEARTBEAT_INTERVAL`/`KIRRA_PROMOTION_TIMEOUT` read independently with no cross-check. If `3×interval ≥ promotion_timeout`, a wedged primary self-demotes *after* the standby promoted — both active until the old primary's next tick + epoch read. Epoch fence is the real backstop but leaves a transient two-`mode_active` window | `standby_monitor.rs:92`,`213`,`470` | Med | Med |
| HA5 | Watchdog `MissedTickBehavior::Delay` — under runtime starvation the advertised "TIMEOUT + one SWEEP" detection-latency bound (SG-003) is best-effort; clock math is still fail-closed (a stalled sensor never reads healthy) | `telemetry_watchdog.rs:198` | Low | High |

**Risk: Medium · Confidence: High.** Decode paths are robust and fail-closed; the residual industrial gap (HA1–HA3) is *trailing-byte tolerance*, not a bounds bypass of the decoded value. **HA4** (validate env-derived HA timings at startup, mirroring the const-assert) is the one concrete fix.

### 3.4 — HTTP service, auth, gateway/policy
**Files:** `src/bin/kirra_verifier_service.rs` (+ submodules), `src/gateway/*`, `src/security.rs`

**Adversarial checks that PASSED (verified-correct — a notable security strength):**
- `require_admin_token` present on every mutation group (admin/actuator/identity-gated); env-only; absent/empty → **503** (fail-closed, distinct from 401).
- No `==` on secrets anywhere — admin, supervisor, and key-container comparisons all route through `constant_time_compare`, whose impl is accumulator-based, no early return, covers `max(len,64)`, and folds a length mask (prior 64-byte-prefix fail-open closed + regression-tested).
- Command classification is an **exact-match/fixed-prefix allowlist**; unrecognized POST/PUT → `Unknown` → denied in ALL postures; query/fragment stripped; `/actuator/motion/command` matched exactly (siblings can't inherit the Degraded relaxation). No trailing-slash/case/prefix confusion.
- Handlers: no `unwrap`/`panic!` on attacker input; bodies bounded (413), parse errors → 400, store/lock errors → generic 500 (no info leak); all use `State<Arc<ServiceState>>`. HA epoch fencing re-asserted in-transaction on the actuator path.

**Findings**
| ID | Title | File:line | Sev | Conf |
|---|---|---|---|---|
| HT1 | CORS preflight (`OPTIONS`) denied fleet-wide: outer posture gate classifies `OPTIONS`→`Unknown`→503, so browser preflight to non-exempt paths fails. Availability bug, not auth bypass | `kirra_verifier_service.rs:1526`, `gateway/policy.rs:77` | Low | High |
| HT2 | Over-permissive CORS `allow_origin(Any)/methods(Any)/headers(Any)`; no `allow_credentials` and Bearer-not-cookie auth so no CSRF, but any web origin can read public read-endpoints. Tighten to allowlist | `kirra_verifier_service.rs:1490` | Low | High |
| HT3 | Gate clock duplication: actuator middleware local `now_ms()` vs `posture_cache::now_ms` — both `SystemTime`, consistent today, but violates the injectable-`now_ms` testability convention | `gateway/policy_layer.rs:38`, `posture_cache.rs:211` | Low | Med |

**Risk: Low · Confidence: High.** This is the strongest-reviewed layer — auth is comprehensively fail-closed. Only the CORS preflight (HT1) is worth fixing (exempt `OPTIONS` or handle preflight before the posture gate).

---

### 3.5 — Cryptography: attestation, federation, TPM, key registry
**Files:** `src/attestation.rs`, `src/key_registry.rs`, `src/tpm_quote.rs`, `src/tpm.rs`, `crates/kirra-fleet-types/src/federation*.rs`, service handlers

**Strengths (verified-correct, high assurance):**
- Attestation is genuinely **node-proven & fail-closed**: `verify_attestation_proof_with_pcr16` requires a registered AK, parses via vetted `spki`, uses `verify_strict` (rejects malleability/small-order), and **folds PCR16 into the signed payload** so it's authenticated not asserted. Domain-separated, length-prefixed `node_id`; 4000-case proptest proves payload injectivity over `(node_id, nonce)`. Legacy admin-HMAC proof explicitly removed (INV-3 satisfied).
- Nonce lifecycle correct: CSPRNG (`getrandom`, panics rather than weak fallback), volatile node-keyed DashMap, TTL-pruned, **atomic `remove` consume** (closes concurrent-verify race), **verify-then-consume** so a bad proof can't burn a victim's nonce.
- Federation crypto canonicalized & double-guarded: `source_generation` inside the signed payload (can't be stripped); v1/v2 byte-stability pinned (no version key-confusion); `verify_strict` throughout; replay enforced by both pre-check and a durable PRIMARY-KEY claim with generation-regress fence (TOCTOU loser → clean reject, no double-burn). `constant_time_compare` on clearance nonce.

**Findings**
| ID | Title | File:line | Sev | Conf |
|---|---|---|---|---|
| CR1 | Reconciliation makes generation strictly authoritative *before* severity — a higher-gen `Nominal` peer masks a lower-gen `LockedOut` peer. **Bounded to the advisory `/federation/reports/:asset_id` read view; does NOT feed actuator-gating posture.** Confirm acceptable | `federation_reconciliation.rs:161` | Med | High |
| CR2 | Federation `expires_at_ms` is attacker-chosen with no max-TTL clamp; practically bounded by the 5s `issued_at_ms` window + nonce burn | `federation.rs:76` | Low | High |
| CR3 | TPM `qualifiedSigner` (AK Name) parsed but never checked vs registered AK — not exploitable (AK Ed25519 sig over quote is sound) but a missing defense-in-depth check | `tpm_quote.rs:157` | Low | High |
| CR4 | `verify_attestation` reads AK/PCR16 from in-memory DashMap while TPM-quote policy reads from SQLite — split read-source for one decision; latent risk if cache/disk diverge (INV-12 says disk authoritative) | `attestation.rs:102`,`145` | Low | Med |

**Risk: Low · Confidence: High.** No critical/high crypto defect. The one ticket worth opening is **CR1** (confirm the advisory federated view may present Nominal over a peer's LockedOut).

---

### 3.6 — Doer–Checker: RSS trajectory validation (THE safety authority)
**Files:** `crates/kirra-trajectory/src/validation.rs` (the real validator), `parko/crates/parko-core/src/rss.rs`, `prediction.rs`, `perception_redundancy.rs`, `kirra-core/src/{containment,kinematics_contract,perception_monitor}.rs`
> Note: the live checker is in `kirra-trajectory`, not the `kirra-ros2-adapter` path implied by CLAUDE.md — a **doc drift** worth correcting.

**Strengths**
- RSS primitives rigorous: return `RSS_FAILSAFE_DISTANCE_M` (1e6) on any non-finite/non-positive brake/accel, with post-arithmetic `is_finite()` re-checks (the `NaN.max(0.0)==0.0` sink is defended). `ABSENT vs KnownEmpty` scene distinction correct.
- `validate_vehicle_command` envelope ordering correct: P0 NaN/Inf guard before all arithmetic (per-field deny codes); hard speed ceiling checked **before** rate limits (INV-8); bicycle model guards `v²>1e-6` (no div-by-zero).
- Sharp fail-closed catch: `validation.rs:752` returns MRCFallback when a non-empty mode set yields zero evaluable windows (closes a silent fail-open). Object-finiteness guard + heading-wrap normalization well-reasoned.

**Findings**
| ID | Title | File:line | Sev | Conf |
|---|---|---|---|---|
| **DC1** | **Mid-lateral-band false-accept (RSS §4 gating hole).** Longitudinal RSS fires only when `|dy_ego| < 2.5 m`; lateral RSS fires only when `dx_ego ≤ 8.0 m`. With `rss_lateral_alignment_tolerance_m = 4.0`, an object in `2.5 ≤ |dy_ego| < 4.0` that is longitudinally unsafe but >8 m ahead trips **neither** axis and is admitted. The predictive pass reuses the same gates and a stationary object's CV mode doesn't move, so it doesn't close it either — containment is the only remaining guard | `validation.rs:450`,`473` | High | High |
| **DC2** | **8 m lateral-conjunction ceiling clips high-speed cut-ins.** `dx_ego ≤ 8.0 m` gate is a fixed scalar not scaled by closing speed; a genuine cut-in originating >8 m ahead is filtered before `lateral_safe_distance` is consulted. At the 22.35 m/s ODD cap, reaction-time travel alone (~11 m) exceeds the gate | `validation.rs:473`, `parko-core/rss.rs:46` | Med | Med |
| DC3 | Occlusion cap non-conservative slack: `velocity > cap + 0.1` slack combined with chord (`hypot`) vs arc-length `traveled` over-estimates remaining visibility distance on curves, nudging the cap up. Small magnitude | `validation.rs:561` | Low | Med |
| DC4 | Predictive `point_on_polyline` doesn't finite-check lane-path vertices/yaw at producer; ultimately caught downstream by sample-finiteness (fail-closed) but guard placement implicit | `prediction.rs:107` | Low | Med |

**Risk: High · Confidence: High.** This is the most safety-significant subsystem and **DC1/DC2 are genuine false-accept paths** in the core RSS §4 conjunction. They widen with both lateral offset and closing speed. **Strongly recommend** re-deriving the longitudinal/lateral gate bounds from the alignment tolerance and closing speed (the longitudinal gate should be ≥ alignment tolerance; the lateral conjunction ceiling should scale with closing-speed reaction distance), and adding targeted false-accept tests for the `2.5–4.0 m` band and >8 m high-speed cut-in.

---

### 3.7 — parko ML / diverse-governor
**Files:** `parko/crates/parko-core/src/{scheduler,comparator,rss}.rs`, `parko-kirra/*`, `parko-ros2/src/tick_pipeline.rs`, inference backends

**Strengths**
- Escalation monotonicity proven: `SafetyPosture::escalate` takes max severity; tick only escalates; no in-tick de-escalation path.
- Fail-closed pervasive & tested: NaN/Inf at every layer; backends refuse silent CPU fallback (ONNX/TRT `error_on_failure`); stale-frame MRC; fault-injection tests assert *safe-stop*, not merely "no panic."
- Honest diverse-governor diversity: genuinely different algebra (interval containment vs signed-accel); the spec-level shared-assumption limit is documented; 10k-case proptest pins no-false-divergence. MRC decel-to-stop-and-HOLD faithfully duplicated across primary + diverse on both linear and angular channels.

**Findings**
| ID | Title | File:line | Sev | Conf |
|---|---|---|---|---|
| PK1 | Built-in degraded clamp `min(max_linear)` caps only positive bound — a reverse (negative) command passes unclamped. Mitigated: production node always attaches comparator governor, so only reachable for a governorless caller | `parko-core/scheduler.rs:268` | Med | High |
| PK2 | Comparator accumulator ceiling `2×LOCKOUT_LEVEL` can leave LockedOut lingering after divergence stops; intended hysteresis but no documented max stuck-duration bound | `comparator.rs:73`,`537` | Low | Med |
| PK3 | `recommended_posture()` escalation is advisory — an integrator calling `evaluate()` without re-reading it gets the clamp but not the fleet escalation (silent under-wiring risk) | `tick_pipeline.rs:170` | Low | High |
| PK4 | Time-based lockout fallback uses `Instant::now()` not injected clock — non-deterministic/untestable per the `now_ms` convention | `comparator.rs:456` | Low | Med |

**Risk: Medium · Confidence: High.** Solid fail-closed ML governor. Fix the reverse-velocity clamp (PK1) defensively even though the live path is covered.

---

### 3.8 — Build / CI / Supply-Chain
**Files:** `.github/workflows/*`, `Cargo.toml`, `Dockerfile`, `install.sh`, `SECURITY.md`

**Strengths**
- `Cargo.lock` committed; tests run `--locked` (reproducible test builds).
- `install.sh` mandatory fail-closed checksum verification (refuses if SHA256SUMS absent/missing the archive).
- Strong least-privilege runtime: non-root `USER kirra`; systemd `NoNewPrivileges`/`ProtectSystem=strict`/scoped `ReadWritePaths`/`MemoryMax`; env file `chmod 640`.
- CI safety coverage genuinely thorough: separate gating jobs for `parko/`, the ros2-gated build, fault-injection (CERT-004), WCET gate, runtime-isolation guard.

**Findings**
| ID | Title | File:line | Sev | Conf |
|---|---|---|---|---|
| **BD1** | **Supply-chain controls are documented but not enforced.** `SECURITY.md` mandates 40-hex SHA-pinned actions + digest-pinned base images; every workflow uses floating tags (`actions/checkout@v7`, `dtolnay/rust-toolchain@stable`, `docker/build-push-action@v7`). `release.yml` runs `contents: write`, Docker `packages: write` → a hijacked tag executes with write tokens | `.github/workflows/*`, `SECURITY.md:34` | High | High |
| **BD2** | **`cargo audit` is non-gating:** piped to `tee`, swallowing the exit code — a vuln advisory never fails CI | `ci.yml:411` | High | High |
| BD3 | No `rust-toolchain.toml` / `deny.toml` / `.cargo/audit.toml`; toolchain floats on `@stable`/`@nightly` | repo root | Med | High |
| BD4 | Build tools unpinned/from-network: `cargo install cargo-audit/llvm-cov` unversioned; `cross` from `--git` HEAD into the **release** pipeline | `release.yml:80` | Med | High |
| BD5 | `tpm` and `cyclonedds` feature builds never compiled in CI → silent rot of `tss-esapi`/native bindings | `Cargo.toml:48` | Med | High |
| BD6 | Docker base images float (`rust:1-alpine`/`alpine:3`); image `cargo build` omits `--locked` so the image can drift from `Cargo.lock` | `Dockerfile:17`,`24` | Med | High |
| BD7 | `install.sh` documents `curl | sudo bash` from mutable `main`; the script itself is unverified (only the downloaded binary is checksummed) | `install.sh:8` | Med | Med |
| BD8 | Release/install artifacts SHA256-checksummed but **not signed** (no cosign/GPG); SHA256SUMS travels the same channel → integrity not authenticity | `release.yml`, `install.sh` | Med | Med |
| BD9 | `SECURITY.md:28` claims `verify_attestation` uses real HMAC-SHA256 — stale/contradicts INV-3 (Ed25519, HMAC removed) | `SECURITY.md:28` | Low | High |

**Risk: High · Confidence: High.** Functionally the CI is strong; the gap is *supply-chain enforcement vs. documented policy*. BD1 + BD2 are cheap, high-value fixes.

---

## Phase 4 — Cross-Cutting Concerns

**Concurrency / lifetime.** The single `Arc<Mutex<VerifierStore>>` serializes all persistence — it is simultaneously the project's biggest *correctness crutch* (PS1: it's the only thing preventing the two-connection audit chain from forking) and its biggest *scalability ceiling*. No deadlocks found (locks not held across `.await`; DashMap id-snapshot avoids shard self-deadlock). Mutex poisoning is handled gracefully (`into_inner`), not panic-cascaded.

**Time / clocks.** A recurring smell: several modules read `SystemTime::now()`/`Instant::now()` directly (PS7, HT3, PK4, federation nonce burn) despite the codebase's own injectable-`now_ms` testability invariant. The safety-critical posture/staleness paths *do* follow it and are fail-closed on backward clocks (`checked_sub`). Cross-host clock skew is an AOU (AOU-TIMESYNC-001), not closed in code (VC5).

**Generation monotonicity.** Process-global `AtomicU64` + SQLite persistence is mostly sound but has a race (VC4: immediate-lockout vs concurrent recalc) and a fragile `fetch_add`/`fetch_max` contract (VC3).

**Error propagation / silent failure.** Mostly fail-closed, but two inconsistencies: loader-level deserialize-to-default coercion (PS8) vs fail-closed skip elsewhere; `chain_intact` (hash-only) diverging from `signature_valid` (PS2).

**Security.** Auth, secret comparison, attestation, and federation crypto are the strongest parts of the system — comprehensively fail-closed and well-tested. The weak link is **supply chain** (BD1/BD2), not the runtime auth model. Industrial decoders are robust but tolerate trailing bytes (HA1–HA3).

**Observability.** `tracing` + JSON subscriber, `/metrics`, SHA-256 audit chain, SSE posture stream — good. Gap: signal-loss cases (PS6 silent no-op, PS2 chain_intact) can mask state.

**Testing.** Genuinely strong: proptests (injectivity, no-false-divergence, kinematics), fault-injection asserting safe-state, shared-file-DB epoch-fence race tests, WCET gate, cross-backend equivalence. Gaps: MC/DC coverage pending (#65); the RSS false-accept band (DC1/DC2) is untested; restart-during-recalc (VC3) untested; `tpm`/`cyclonedds` feature builds uncovered (BD5).

---

## Phase 5 — Deep Inspection: highest-value targets

1. **RSS §4 conjunction (DC1/DC2)** — the load-bearing safety algorithm has two real false-accept geometries. Re-derive gate bounds from alignment tolerance + closing speed; add band/cut-in tests. *This is the single most important finding in the review.*
2. **Two-connection audit chain (PS1)** — tamper-evidence integrity currently depends on the global store Mutex. Consolidate to one audit-writer connection / dedicated mutex.
3. **Immediate-lockout race (VC4)** — supervisor `force_lockout` can be transiently dropped under concurrent recalc; verify the sticky-flag eventual-consistency is acceptable for the FTTI claim, or make lockout generation-independent.
4. **HA split-brain window (HA4)** — validate env-derived heartbeat/promotion timings at startup (the const-assert only guards defaults); the epoch fence backstops but leaves a transient two-`mode_active` window.
5. **Supply chain (BD1/BD2)** — make `cargo audit` gating; SHA-pin actions / digest-pin images or remove the SECURITY.md claims.

---

## Executive Summary

Kirra is a **mature, unusually disciplined safety-engineering codebase**. The fail-closed philosophy is not a slogan — it is implemented consistently and defended by an exceptional test suite (proptests, fault-injection asserting *safe-state*, race tests on shared-file DBs, WCET gating, cross-backend equivalence). The doer–checker separation is architecturally clean, the cryptographic attestation/federation layer is genuinely node-proven and well-canonicalized, and the HTTP auth boundary is comprehensively fail-closed. 29 ADRs and a 35KB architecture bible show real design rigor.

The risks are concentrated and specific, not systemic. The **most important** is that the core RSS §4 conjunction — the actual safety authority — has two demonstrable false-accept geometries (mid-lateral-band and high-speed cut-in beyond the fixed 8 m gate). Second is that audit-chain tamper-evidence integrity currently leans on the global store Mutex (a two-connection design that forks if the connections ever write concurrently). Third is a documented-but-unenforced supply-chain posture. None of these is a sign of careless engineering; they are the kind of subtle, geometry- and concurrency-dependent gaps that only surface under adversarial review.

### Top 10 Risks
1. **DC1 — RSS mid-lateral-band false-accept** (High): object at 2.5–4.0 m lateral, >8 m ahead, longitudinally unsafe, admitted by all snapshot + predictive branches.
2. **DC2 — Fixed 8 m lateral-conjunction ceiling** (Med-High): clips genuine high-speed cut-ins; not scaled by closing speed.
3. **PS1 — Two-connection audit chain fork** (Critical/Med): tamper-evidence depends on the store Mutex; concurrent writes link off a stale prev-hash.
4. **VC4 — Immediate-lockout race** (Med): supervisor `force_lockout` transiently droppable under concurrent recalc.
5. **HA4 — HA split-brain window for env-tuned timings** (Med): no startup validation of heartbeat/promotion budget; transient two-active window.
6. **BD1 — Unpinned Actions + write-token release pipeline** (High): supply-chain policy documented, not enforced.
7. **BD2 — `cargo audit` non-gating** (High): advisories never fail CI.
8. **PS2 — `chain_intact` (hash-only) diverges from `signature_valid`** (High/Med): callers can miss signature failures.
9. **CR1 — Federation reconciliation: higher-gen Nominal masks lower-gen LockedOut** (Med): bounded to advisory read view — confirm acceptable.
10. **PK1 / HA1 — Reverse-velocity clamp gap & CANopen `s=0` width bypass** (Med): defense-in-depth holes in non-primary paths.

### Top 10 Strengths
1. Pervasive, *tested* fail-closed semantics (fault-injection asserts safe-state, not absence of panic).
2. Node-proven Ed25519 attestation with PCR16 folded into the signed payload (+ injectivity proptest).
3. Atomic single-transaction federation commit with epoch fence + dual replay guard.
4. Comprehensive HTTP auth: env-only tokens, 503-fail-closed, constant-time compares, exact-match command allowlist.
5. Backward-clock fail-closed staleness (`checked_sub`) centralized across gate + engine.
6. Monotonic heartbeat-freshness + durable SQLite epoch fence for HA (race-tested on file DB).
7. DAG deadlock-avoidance via id-snapshot; root-independent memo proven by equivalence test.
8. Honest diverse-governor with genuinely different algebra + 10k-case no-false-divergence proptest.
9. RSS primitives defend the `NaN.max(0)` sink with post-arithmetic finiteness re-checks.
10. Strong documentation discipline: 29 ADRs, safety specs, WCET methodology, explicit AOUs.

### Quick Wins
- Make `cargo audit` gating (drop the `tee`) and add `deny.toml` (BD2/BD3).
- Exempt `OPTIONS`/handle CORS preflight before the posture gate (HT1).
- Validate env-derived HA timings at startup, mirroring the const-assert (HA4).
- Fix the reverse-velocity `.min()` clamp in the scheduler fallback (PK1).
- Surface `save_last_generation` rejection instead of silent no-op (PS6); reconcile `chain_intact`/`signature_valid` reporting (PS2).
- Correct stale docs: SECURITY.md HMAC claim (BD9), CLAUDE.md checker-location drift (DC note).

### Long-Term Refactors
- **Re-derive the RSS §4 gate constants** from alignment tolerance + closing speed; add a property test asserting no admit when longitudinally-unsafe within `alignment_tolerance` at any range, and a closing-speed-scaled lateral ceiling (DC1/DC2).
- **Consolidate audit-chain writes** to a single dedicated connection/mutex so tamper-evidence no longer depends on the global store lock (PS1).
- **Decompose the 3.9K-LOC service binary** into route modules behind a typed router builder.
- **Adopt the injectable clock universally** (eliminate residual `SystemTime/Instant::now()` in time-dependent logic).
- **SHA-pin/digest-pin** the entire CI/release supply chain and sign artifacts (BD1/BD6/BD8).

### Architectural Debt
- Single store Mutex as both correctness crutch and throughput ceiling.
- Transitional re-export shims (de-monolith in progress) add indirection.
- Process-global generation atomic couples force-lockout and recalc ordering.
- Doc/code drift (checker location, SECURITY.md HMAC) — docs are extensive but can lag fast-moving safety code.

---

## Scorecard

| Dimension | Score (1–10) | Rationale |
|---|---|---|
| **Production Readiness** | **7** | Strong fail-closed runtime + tests; held back by DC1/DC2 safety gaps, PS1 audit-integrity dependency, and unenforced supply chain. |
| **Maintainability** | **7** | Excellent docs/ADRs and modular crates; dragged by the 3.9K-LOC binary, transitional shims, and doc drift. |
| **Reliability** | **8** | Pervasive tested fail-closed behavior, graceful poisoning, HA epoch fence; minor races (VC4, HA4 window). |
| **Scalability** | **6** | Single store Mutex serializes all persistence; coalesced posture engine helps, but the write path is a clear ceiling. |
| **Security** | **8** | Auth/attestation/federation crypto are exemplary and well-tested; primary gap is supply-chain enforcement, not the runtime model. |
| **Testability** | **8** | Proptests, fault-injection, race tests, WCET gate, cross-backend equivalence; MC/DC pending, some false-accept/restart cases untested. |

**Overall Engineering Grade: A− / B+.** This is top-decile safety-systems engineering with a small number of concentrated, high-value gaps. Closing DC1/DC2 (RSS false-accepts), PS1 (audit-chain consolidation), and the supply-chain enforcement (BD1/BD2) would move it to a clear A.

> **Confidence in this review: Medium-High.** Findings are evidence-based with `file:line` citations from full-file reads. The highest-confidence items are the verified-correct security strengths and DC1, HT-layer, and BD findings; the audit-chain (PS1) and reconciliation (CR1) severities depend on runtime concurrency/deployment assumptions that warrant a confirming test before remediation priority is fixed.
