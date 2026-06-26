# Kirra Runtime SDK — Principal Engineering Review

**Reviewer role:** Principal Systems Architect / Runtime & Real-Time Systems / Functional Safety
**Scope:** Full workspace — `kirra-verifier` root crate + 12 sibling crates + the separate `parko/` ML workspace (~98k LOC Rust, 262 files).
**Method:** Seven parallel subsystem deep-dives, each evidence-based (`file:line` + code quote). Findings are tagged **[Observed]** (a defect/behaviour read directly in code), **[Inferred]** (a consequence that follows from the code but was not executed), or **[Speculative]** (a forward-looking enhancement).
**Disposition of this document:** review only — no source files were modified.

---

## 1. Executive Summary

Kirra is a fail-closed runtime legitimacy engine and safety governor implementing a **Doer/Checker** architecture: an untrusted planner (the *doer*) proposes trajectories/commands; an independent kinematic + RSS checker (the *checker*) bounds them. The codebase is, for its stage, **exceptionally mature on safety discipline and exceptionally under-tuned on runtime mechanics.**

What stands out positively:

- The **safety-case scaffolding is real**, not decorative: HARA, IEC 61508 / ISO 26262 / UL 4600 / ASTM F3269 / ISO-IEC TR 5469 mappings, a WCET methodology doc, requirements-traceability matrices, and a 9-job CI with dedicated WCET, fault-injection (CERT-004), branch-coverage, and clippy `-D warnings` gates.
- The **checker is structurally independent of the doer** — `validate_trajectory_slow*` never sees a `ProposalKind`, intent, or planner handle; `PlanOutput::safe_stop` is always constructible.
- The **cryptographic core is fail-closed and free of the historical P0s** — no trust mock, verify-then-consume nonce ordering, atomic nonce burn via a UNIQUE-constraint TOCTOU close, tamper-evident hash-chained audit with tail-truncation detection.
- **RSS primitives are textbook fail-closed** — every divisor finite-guarded, `RSS_FAILSAFE_DISTANCE_M` instead of NaN→0 sinks, per-field NaN/Inf rejection codes on the verdict path.
- **Testability is the standout dimension** — pure decision functions with injected `now_ms`, property tests, MC/DC-style boundary tests, real-DB fence tests, poison-recovery regressions.

What needs work, in priority order:

1. **Runtime/concurrency hygiene on the verifier service** — 65 blocking `store.with()` calls on tokio worker threads, a DashMap iterate-then-`get` deadlock hazard, no SQLite `busy_timeout`, and per-node per-100ms disk reads in the watchdog. These are the highest-probability sources of a production stall.
2. **The deployed transport QoS does not match the safety intent** — actuator-output QoS is enforced on a *model struct* with no real DDS writer, while every safety-critical *ingress* subscription runs RMW-default `Reliable + KeepLast(10)` QoS, the exact stale-drain hazard the output side forbids.
3. **A real ≤2s two-writer window on the actuator path** during HA failover — the actuator-motion gate is fenced only by a cached (heartbeat-cadence-stale) epoch, with no in-transaction re-check.
4. **WCET evidence is mislabeled** — the "WCET gate" asserts p99.9 (not max) and benches the kernel function, not the deployed serde+Tower path that actually feeds the FTTI claim.
5. **A predictive-RSS silent fail-open** — when prediction modes are supplied but unevaluable (equal timestamps / short horizon), the cut-in detector evaluates nothing yet returns "safe."

None of these undermine the *architecture*, which is sound. They are the gap between "a rigorously-reasoned safety kernel" and "a 24/7 hard-real-time product."

**Overall maturity: strong architecture and safety reasoning (8/10), held back by runtime/IPC tuning and a handful of fail-closed seams that leak (≈6/10 on production-hardening).**

---

## 2. Overall Architecture Assessment

The system is a **layered, vendor-neutral safety kernel** with a clean separation between:

- **Trust/legitimacy plane** (`verifier.rs`, `posture_engine*.rs`, `attestation.rs`, `federation*.rs`, `audit_chain.rs`) — a fleet DAG, posture lattice (`Nominal/Degraded/LockedOut`), cryptographic attestation, and federated cross-controller trust.
- **Command-gating plane** (`gateway/`, `kirra_core.rs`, `kinematics_contract.rs`) — Tower middleware that classifies a command by `(path, method)` → `OperationalCommand`, gates it by posture, then bounds it kinematically.
- **Doer/Checker plane** (`crates/kirra-ros2-adapter`, `kirra-planner`, `kirra-core`, `kirra-taj`, `kirra-map`, `parko/`) — the planner proposes, the checker (containment + per-pose kinematics + RSS) bounds.
- **Protocol/transport edges** (`protocol_adapter.rs`, `adapters/`, `dds_bridge.rs`, `fleet-transport`, `fabric/`) — SCADA/industrial decoders and fleet transport.

**Strengths of the structure.** Dependency direction is correct: nearly everything depends on the lean `kirra-core` (no heavy deps), not on the heavy ROS adapter. The `ros2` and `tpm` features keep heavyweight deps out of the default build. The `parko/` ML workspace is deliberately isolated behind a `path =` dependency so the inference pipeline can't pull into the safety-kernel build. `crate-type = ["rlib", "cdylib"]` plus `panic = "abort"` in release gives a fail-closed-by-process-death model that is coherently argued in `Cargo.toml`.

**Coupling/cohesion concerns.**

- **The service binary is a monolith** — `src/bin/kirra_verifier_service.rs` is **6,189 lines** holding every route handler. This is the single largest maintainability liability: it concentrates auth, store access, posture, federation, and industrial-evaluation logic in one file, and it is where the 65 blocking `store.with()` calls live (§7). It should be decomposed into per-domain handler modules (`handlers/attestation.rs`, `handlers/federation.rs`, …).
- **Two industrial-protocol abstraction seams** rather than one (`protocol_adapter.rs:150` vs the `IndustrialAdapter` trait) — Modbus/OPC-UA bypass the `BoundSpec` path that CANopen/DNP3/CIP use (§13).
- **A parallel QoS model** in `dds_bridge.rs` that is never bound to a real writer (§9).
- **`verifier_store.rs` is 254 KB / ~6k lines** — a god-object store. It is well-organized internally but is a refactor target for splitting by table-domain.

**Extensibility / IoC.** The trait surface is good — `SafetyContract`, `SafetyGovernor`, `ProtocolAdapter`, `IndustrialAdapter`, `CorridorSource`, `Planner`, `Clock` (with `SystemClock`/`VirtualClock`) — and the `Clock` injection is the backbone of the strong testability. The plugin story for *planners* (doers) is excellent; for *protocols* and *transports* it is half-done.

---

## 3. Strengths

1. **Fail-closed is the default, pervasively.** Stale/empty/poisoned posture cache → LockedOut; absent admin token → 503; undecodable industrial payload → deny; audit read error → propagate, never fork-to-genesis. Multiple independent reviewers found *no authentication/authorization fail-open path*.
2. **The checker is genuinely independent of the doer.** `validate_trajectory_slow_capped` (`crates/kirra-ros2-adapter/src/validation.rs:168`) consumes only geometry + perception + posture; `ProposalKind` is audit-only.
3. **RSS math is rigorously guarded.** `parko/crates/parko-core/src/rss.rs:168,237,409` re-checks `is_finite()` after arithmetic and uses `RSS_FAILSAFE_DISTANCE_M`; `validate_vehicle_command` Priority-0 rejects all five non-finite fields before any arithmetic (`gateway/kinematics_contract.rs:443`).
4. **Crypto is sound and regression-pinned.** CSPRNG nonces (`getrandom`), `verify_strict` everywhere, length-prefixed domain-separated signing payloads (property-tested for injectivity), atomic nonce burn via `nonce_hex PRIMARY KEY` UNIQUE-violation TOCTOU close (`verifier_store.rs:1953`), anchor-head high-water mark detecting tail truncation.
5. **HA fencing fundamentals are well-built.** Monotonic-token heartbeat (skew-immune), durable epoch CAS (`verifier_store.rs:3074`), in-transaction `assert_epoch_held` for federation/key-rotation writes, disk-first ordering (`save_node` before `nodes.insert`).
6. **Testability and traceability.** Pure functions, DI clock, fault-injection CI gate, byte-identity tests on audit payloads, real-file fence tests. 183 test-bearing files.
7. **The `<=` replay rule is consistent across all three QNX/iceoryx2/UDP judges** (`equal = replay, lower = regress`), and the high-water mark only advances on a Fresh verdict so a rejected proposal can't poison it.
8. **Honest engineering culture.** Host timing is explicitly labeled INDICATIVE (`wcet_status = TBD-QNX-TARGET`); the iceoryx2 work is labeled a spike; deferred work is documented with reserved vocabulary rather than half-implemented.

---

## 4. Weaknesses

1. **Blocking SQLite on the async runtime** — 65× `store.with()` in async handlers vs 5× `.call()` (the `spawn_blocking` seam exists but is unused). **[Observed]**
2. **DashMap iterate-then-`get` re-entrancy** in `recalculate_and_broadcast` (`posture_engine.rs:77`) — deadlock/hang hazard on the safety engine. **[Inferred]**
3. **Default QoS on safety-critical ROS 2 ingress** — `r2r::QosProfile::default()` (Reliable + KeepLast(10)) on trajectory/odometry/objects (`node.rs:219+`). **[Observed]**
4. **Actuator-path HA fence uses a ≤2s-stale cached epoch** with no in-transaction re-check (`gateway/policy_layer.rs:491`). **[Observed/Inferred]**
5. **WCET gate measures p99.9 of the wrong function** — kernel, not deployed serde+Tower path (`wcet_gate.rs:219`). **[Observed]**
6. **Predictive-RSS silent fail-open** on supplied-but-unevaluable modes (`validation.rs:587`). **[Observed]**
7. **Fabricated `dt = 0.050`** fed to the scalar rate governor on the live Modbus path defeats rate-of-change limiting (`gateway/mod.rs:236`). **[Observed]**
8. **No SQLite `busy_timeout`** on writer/durable connections (`verifier_store.rs:406,708`). **[Observed]**
9. **Per-node disk read every 100 ms sweep** in the watchdog, contradicting its own header (`telemetry_watchdog.rs:299`). **[Observed]**
10. **Hand-rolled CDR missing alignment padding** — interop break with any spec-compliant DDS reader (`dds_bridge.rs:126`). **[Inferred]**
11. **QNX shim torn-read mechanism is compiler-fence-only** (`atomic_signal_fence`), not the odd/even seqlock the spec mandates — unsound on aarch64 (`tools/qnx-rtm-harness/kirra_shim.cpp:33`). **[Inferred]**
12. **Two monolith files** (`kirra_verifier_service.rs` 6.2k lines, `verifier_store.rs` ~6k lines). **[Observed]**

---

## 5. High-Impact Performance Improvements

Ordered by expected impact on tail latency / throughput / CPU.

| # | Change | Evidence | Expected impact |
|---|--------|----------|-----------------|
| P1 | Move async-handler store access to `StoreHandle::call`/`call_read` (`spawn_blocking`). Add a CI grep gate forbidding bare `.store.with(` in `bin/`. | `store_handle.rs:162`; 65 call sites in `kirra_verifier_service.rs` | Removes executor starvation: under writer contention, `/health`, SSE, and posture broadcast currently stall behind a `synchronous=FULL` commit. Eliminates a whole class of tail-latency spikes. **[Inferred, high confidence]** |
| P2 | Add `busy_timeout` (~250 ms, as the read replica already does) to `conn` and `durable_conn`. | `verifier_store.rs:406,708,744` | Absorbs WAL-checkpoint `SQLITE_BUSY` transients that currently surface as fail-closed errors (heartbeat/epoch read failures). Improves availability without weakening durability. |
| P3 | Share one `black`/`gray` memo across the whole fleet recalc instead of a fresh DFS per node. | `verifier.rs:441`, called per-node at `posture_engine.rs:79` | Turns fleet recalc from O(N·(N+E)) into ~O(N+E). A node depended on by 100 others is currently re-traversed 100×. Recalc fires on every sensor fault/recovery + ~2.5 s refresh. |
| P4 | Watchdog: read per-node last-seen from an in-memory map (DashMap), reserve SQLite for the 30 s refresh. | `telemetry_watchdog.rs:299` | Removes N writer-mutex acquisitions / 100 ms. At N=100 that is 1,000 lock-takes/s contending the same mutex as the HTTP handlers. |
| P5 | Intern node IDs as `Arc<str>`; stop cloning strings/Vecs per node per recalc. | `verifier.rs:502,511,521,547` | Cuts steady-state allocation in the posture engine; compounds with P3. |
| P6 | Industrial proxy: stop holding the single `Mutex<KirraKernelGovernor>` across encode + the 1-in-100 `VecDeque<JournalEntry>` clone. | `gateway/mod.rs:235,273` | The industrial proxy is effectively single-threaded today, with a 100× lock-hold spike every 100th frame. Shorten the critical section / shard per-asset. |
| P7 | Replace per-tick JSON audit payload on non-transition refreshes. | `posture_engine.rs:145` | Removes steady-state `serde_json::json!` + `format!` allocs every ~2.5 s no-op refresh. |

---

## 6. Bug Findings

Severity uses Critical/High/Med/Low. All are **[Observed]** in code unless marked.

### High

- **B1 — DashMap iterate-then-`get` deadlock hazard.** `posture_engine.rs:77` holds an `app.nodes.iter()` shard guard across `calculate_posture()`, which re-locks `self.nodes`/`self.dependency_graph` (`verifier.rs:504`). On the same shard under a queued writer this can self-deadlock and **hang the safety engine**. *Fix:* snapshot keys first (`let ids: Vec<_> = app.nodes.iter().map(|e| e.key().clone()).collect();`), drop the iterator, then traverse. **[Inferred — needs a contention repro]**
- **B2 — Unbounded PLC-response slice panic.** `gateway/mod.rs:291` indexes `plc_buf[6..6+p_len]` with `p_len: u16` from the wire and no `p_len > 500` guard (the client side *does* guard, `:227`). `plc_buf` is `[0u8; 512]`, so `p_len > 506` panics → with `panic=abort`, process death. A malicious/buggy PLC crashes the worker. *Fix:* mirror the client-side `p_len > 500` guard.
- **B3 — Predictive-RSS silent fail-open.** `validation.rs:587` `continue`s on `dt <= 0` and on `nearest_in_time → None`. If a producer emits equal timestamps or a horizon shorter than the prediction match tolerance, the cut-in detector (the *only* layer catching a mid-band cut-in the snapshot filters out) evaluates nothing and returns `false` (=safe). *Fix:* distinguish "no modes supplied" (legit no-op) from "modes supplied but all unevaluable" → MRC-floor derate.

### Medium

- **B4 — Fabricated `dt = 0.050` defeats the rate governor.** `gateway/mod.rs:236` calls `gov.evaluate(demand, 0.050)` with a constant timestep, so the rate-of-change limiter measures fictional rates on the live industrial path. This is the exact failure mode the `kirra_core.rs:166` comment claims was fixed. *Fix:* pass real elapsed `Instant` delta.
- **B5 — `clamp(min, max)` panic on misconfigured contract.** `f64::clamp` panics if `min > max`. `KirraKernelGovernor::new` takes `cap_min`/`cap_max` unvalidated; a typo (`cap_min=2.0, cap_max=-2.0`) aborts the process on the Degraded actuator path. *Fix:* validate `cap_min <= cap_max` and `min_bound <= max_bound` in `new()` / startup sentinel; finite-check contract bounds.
- **B6 — Generation counter set, not `fetch_max`, at boot.** `posture_engine.rs:21` does `POSTURE_GENERATION.store(last+1)`; if any recalc already ran, this can move the counter *backwards*, violating the federation monotonicity invariant. *Fix:* `fetch_max(last+1)`.
- **B7 — Worker holds `rx` mutex across `.await` for the task lifetime.** `posture_engine_v2.rs:406` — a supervised re-spawn blocks forever on `rx.lock().await` unless the prior future is fully dropped first (an unstated coupling). *Fix:* lock per-iteration or move ownership into the spawn closure.
- **B8 — Depth bound vs unregistered dep IDs.** `verifier.rs:451` `max_depth = nodes.len().max(10)`, but `dependency_graph` may reference IDs absent from `nodes`; a deep acyclic chain of unregistered IDs can exceed the bound → spurious `LockedOut`. *Fix:* bound by `(nodes.len() + dependency_graph.len()).max(10)`.
- **B9 — Heading-wrap not normalized in steering derivation.** `validation.rs:796` feeds raw `Δheading` into `atan2`; a trajectory crossing ±π yields a spurious ~2π delta → spurious `ClampSteering`. The angular channel normalizes; this path doesn't. *Fix:* apply the same `raw - TAU·round(raw/TAU)` normalization.
- **B10 — Horizon-cap mismatch → unconditional MRC.** Containment rejects `> MAX_TRAJECTORY_HORIZON` (50) outright (`containment.rs:217`); a 51-point proposal becomes instant MRC with no truncation. *Fix:* truncate at the checker boundary (conservative/safe) or make it a tested hard contract.

### Low (selected)

- **B11 — Wall-clock staleness fails *open* on backward NTP skew.** `posture_cache.rs:135` `now_ms.saturating_sub(generated_at_ms)` saturates to 0 on a backward jump → entry treated as fresh indefinitely. *Fix:* use a monotonic source for staleness, wall clock only for audit timestamps.
- **B12 — `>=` vs `>` staleness/expiry inconsistency.** `posture_cache.rs:136` (`>=`) vs `posture_tracker.rs:163` (`>`) vs nonce expiry `verifier.rs:558` (`>`). 1 ms boundary divergence across gates that all claim the same semantics. *Fix:* standardize on `>=`.
- **B13 — CIP `Set_Attribute_Single` bounds only the prefix.** `ethernet_ip.rs:187` decodes leading `width()` bytes; trailing garbage is silently accepted. *Fix:* require `data.len() == ty.width()`.
- **B14 — DNP3 Write/Freeze g41 bypasses the magnitude bound.** `dnp3.rs:101` restricts bounding to FC `0x03..=0x06`; a g41 carried on a `Write` (0x02) is posture-gated but not magnitude-bounded. *Fix:* include 0x02-with-g41 or document the exclusion.
- **B15 — Modbus `i64 → f64` unfaithful width.** `protocol_adapter.rs:76` casts an `i64` straight to the setpoint with no `0..=65535` register-range check, unlike the faithful-by-declared-width binary adapters. (Backstopped by the kinematic envelope, so not fail-open.)

---

## 7. Runtime Optimizations

- **Async/blocking boundary (P1).** This is the dominant runtime issue. Every `store.with()` in an `async fn` pins a tokio worker for lock-acquire + SQLite I/O. Convert to `spawn_blocking` via the existing `.call`/`.call_read`. Heavy reads (`verify_audit_chain_full`, `backup/export`, history/flapping scans) and durable writes (`save_federated_report_chained`, `record_key_rotation`) are the priority.
- **Coalesce posture recalcs at the source (already partly done).** The `PostureEngineSender` worker coalesces bursts — good. Combine with shared-memo traversal (P3) so each coalesced recalc is itself linear.
- **Lock-hold discipline.** Two hot Mutexes (`Mutex<VerifierStore>`, `Mutex<KirraKernelGovernor>`) serialize unrelated work. Shorten critical sections (don't clone the flight-recorder `VecDeque` under the governor lock); consider per-asset sharding for the industrial proxy.
- **Atomics.** All-`SeqCst` is correct but over-strong; `Acquire/Release` suffices for the posture flags. The perf delta is negligible here, so this is a clarity change, not a hot-path win. The `node.rs` freshness counters correctly use `Relaxed` (single-writer monotonic age comparison).
- **Executor jitter.** `node.rs:819` `spin_once(10ms)` adds up to a full 10 ms cycle of ingress-stamp jitter on a 100 Hz loop. Drive r2r on a dedicated thread with a 1 ms spin (or event-driven) and add the missing shutdown channel.

---

## 8. Memory Optimizations

- **Stop cloning IDs in traversal (P5).** `Arc<str>` interning for node IDs removes per-node `to_string()`/`clone()` in `recursive_calculate`.
- **Reusable scratch buffers in the slow checker loop.** `validation.rs:203` builds three `Vec`s (`left_kernel`/`right_kernel`/`poses`) per 10 Hz call via `.map().collect()`. Bounded (≤128×2 + 50) but real; a reused scratch buffer (or direct iteration) removes the per-cycle allocation. Note these in the WCET budget regardless.
- **`Arc<Vec<…>>` snapshot swap in `AdaptorState`.** `state.rs:518/542/563` clone the whole object vectors every slow-loop tick (3 unbounded clones/cycle) for lock hygiene. An `Arc`-swap keeps the lock-hygiene property without the copy.
- **Object pools / arenas — [Speculative].** The verdict path is already zero-alloc (`MitigationCode`/`DenyCode` are `Copy`). On the QNX target the in-partition channel should use a fixed `#[repr(C)]` POD frame (already the iceoryx2-spike design) — no allocator on the safety path at all. Don't add a general arena; the win is in not allocating, which is already the kernel's posture.

---

## 9. IPC Optimization Opportunities

**Per-mechanism verdict (good / not-good / better):**

| Mechanism | Where | Good | Not good | Better? |
|-----------|-------|------|----------|---------|
| **DDS (modeled)** | `dds_bridge.rs` | QoS *intent* is exemplary: `actuator_admissibility()` enforces Volatile + KeepLast(1) + non-zero lifespan *before* framing (INV-10). | **No real writer** — `DdsPublisherBridge` is a ZST; QoS is enforced on a struct that never reaches a wire (`D1`, HIGH-arch). `deadline_ms` set but never validated (`D2`). Hand-rolled CDR lacks alignment padding (`C1`) — interop break. | Real CycloneDDS via `cyclonedds-rs`/rmw + a real CDR codec when the path ships; validate readback QoS, not requested QoS. |
| **r2r subscriptions** | `node.rs:219+` | Bounded mpsc bridge, drop-on-full, monotonic-clock staleness. | **All `QosProfile::default()`** (Reliable + KeepLast(10)) on safety ingress — the stale-drain hazard the output side forbids (`N1`, HIGH). No Deadline/Liveliness. | Explicit per-topic sensor-data QoS: KeepLast(1) + BestEffort (or Reliable if planner requires) + Deadline = staleness budget + Liveliness lease. |
| **iceoryx2 (spike)** | `tools/iceoryx2-spike` | **Best-in-class**: true zero-copy SHM, torn-read elimination *by construction* (owned `Sample`), no unsafe on the command path, per-fault-class p50/p99/max measured. | Host-timing only (honestly gated); not yet adopted. | **This is the better alternative** — adopt for in-partition transport (ADR-0006 Clause 1). |
| **UDP + bincode + HMAC** | `kirra-governor-service` | MAC-authenticated, replay-gated, zero heavy deps, QNX-friendly; honestly labeled QM/prototype. | Single-threaded blocking `recv_from`; UDP loss → silent drop (undocumented). | Appropriate for the two-box prototype; superseded by the SHM channel for cert. |
| **Zenoh `put()`** | `fleet-transport` | Correct trust model (Ed25519 verify-before-surface); right tool for the intermittent fleet lane. | **No QoS** — default congestion control is *Drop*, so a clearance grant can be silently dropped. JSON per report where bincode is used elsewhere. | Keep Zenoh; add `CongestionControl::Block` + `Priority::RealTime` + `Reliable` for grants/reports. |
| **SQLite (grant ingest)** | `fleet-transport → VerifierStore` | Correct durability termination for low-rate grants. | Would be wrong on a command hot path (it isn't). | None needed. |

**Latency framing (indicative, consistent with the repo's host-vs-QNX stance):** iceoryx2 zero-copy SHM ≈ **1–2 µs** publish→receive vs DDS-over-RTPS ≈ 50–300 µs intra-host (serialization + reliable-ACK) — the **~50–150×** win that justifies iceoryx2 on the in-partition WCET-critical edge. The architecture's *lane assignment is correct* (UDP/Zenoh/JSON on latency-tolerant QM lanes; iceoryx2 + frozen SHM `GovernorContractView` on the WCET-critical edge). The defects are **configuration** (QoS) and **soundness** (the shim fence), not mechanism mismatch.

**Seqlock soundness (`S2`, MED).** `tools/qnx-rtm-harness/kirra_shim.cpp:33` uses `std::atomic_signal_fence` (a *compiler* fence — no CPU barrier) + `volatile` reads for its double-read-compare. On aarch64 (the actual QNX target) `volatile` is not an inter-thread ordering primitive and the signal fence emits no barrier, so the tear-detection is not provably sound against a concurrent producer; it is also susceptible to ABA. The hypervisor spec (`HYPERVISOR_CONTRACT_CHANNEL.md:154`) *explicitly mandates an odd/even generation seqlock instead* — so the harness does not exercise the protocol it traces to (an RTM gap as much as a memory-model one). *Fix:* implement the generation seqlock, or use `atomic_thread_fence(acquire)` and correct the RTM mapping to state the host harness uses a different mechanism than the target.

**Do NOT adopt** DPDK or io_uring here — **[Speculative, rejected]**. The WCET-critical path is intra-host cross-partition SHM, where iceoryx2/seqlock is the right primitive; DPDK (kernel-bypass NIC) and io_uring (async disk/socket batching) address bottlenecks this system doesn't have. Recommending them would be gold-plating against the real budgets.

---

## 10. Doer/Checker Architecture Review

**Separation of authority — clean [Observed].** The checker (`validate_trajectory_slow*`) is the sole safety authority and never receives doer intent/handles. `PlanOutput::safe_stop` is always constructible; every Mick intent branch fails closed to it on non-finite input. The doer is swappable (geometric / learned / LLM) and never trusted for safety. This is the load-bearing thesis and it holds.

**Where independence leaks (subtle, fixable):**

- **D/C-1 — Motion-source split [Observed, MED].** The snapshot RSS derives object lateral motion from scalar `velocity_mps × sin(heading)` (`validation.rs:435`) while the predictive pass and redundancy use the *vector* `obj.vel` (`prediction.rs:69`). `PerceivedObject` carries both with nothing enforcing consistency. A doer/upstream that populates one coherently and the other stale makes the two RSS passes evaluate different motions. *Fix:* derive one from the other at ingest, or assert consistency and fail closed on mismatch.
- **D/C-2 — Snapshot RSS is all-pairs, not time-matched [Observed, MED].** `validation.rs:359` compares every object's *current* snapshot against *every* pose, including far-future poses — geometrically meaningless and biased toward spurious MRC. The predictive pass got this right (`nearest_in_time`). *Fix:* match each object to the nearest-in-time/space pose.
- **D/C-3 — Predictive-RSS silent fail-open** (B3 above) — the most important checker-independence gap: a layer that exists specifically to catch what the snapshot misses can be silently neutralized by malformed timing.

**Timing/latency.** The Nominal per-pose `validate_vehicle_command` path is straight-line, branch-bounded, zero-alloc, no recursion/sort — confirmed unchanged and WCET-clean. The slow loop (10 Hz) has bounded-but-real allocations (§8). The **end-to-end** doer→checker→actuator latency that feeds FTTI is *not yet measurable* because the fast-loop output is still a `tracing::debug!` placeholder (`node.rs:757`) — flag for the safety case: loop-closure WCET is not demonstrable until the output edge exists.

**Recommendation to strengthen independence while reducing latency:** drive the checker on the in-partition iceoryx2 SHM channel (zero-copy, ~1–2 µs, torn-read-free) with the frozen `GovernorContractView`, so the doer (Autoware guest) and checker (Rust on the safety partition) share *only* a pointer-free, generation-seqlock'd snapshot — physically isolating fault domains while cutting ingress latency ~50–150× vs RTPS.

---

## 11. Safety Assessment

**Posture/fail-closed model — strong.** Stale/empty/poisoned → LockedOut; sticky lockouts; supervisor override; `Unknown` command denied in all postures including Nominal; Degraded = controlled decel-to-stop-and-hold with non-increasing speed, no re-initiation, reversal-through-stop denial. The decel-to-stop logic (`kinematics_contract.rs:370`) is correct and its edge cases (STOP_EPSILON, signum-on-zero ordering dependency) are handled — though the reversal check's correctness *depends on branch ordering* (`F3`); pin it with a comment or an order-independent both-nonzero test.

**Certification posture.** The IEC 61508 / ISO 26262 / UL 4600 / ASTM F3269 / ISO-IEC TR 5469 mappings, HARA, TARA, DFA, SOTIF docs and the requirements-traceability matrices are a genuine head start toward an ASIL/SIL safety case. Patterns that *simplify* certification and should be preserved/extended: pure decision functions (analyzable), injected clocks (deterministic replay), `&'static str` reason codes (stable audit vocabulary), the WCET structural-boundedness argument, and the fault-injection CI gate.

**Safety gaps to close before a credible safety case:**

1. **Actuator-path two-writer window (`1b`, MED [Observed/Inferred]).** `gateway/policy_layer.rs:491` fences `ActuatorMotion` only against `cached_db_epoch` (≤ `HEARTBEAT_INTERVAL_MS` = 2 s stale) with no in-transaction `assert_epoch_held` (that re-check exists only for federation/key-rotation). During failover the old primary can pass actuator commands for ~2 s after the new primary claims the epoch. For a governor whose *entire purpose* is gating actuators, this is the highest-consequence residual. *Fix:* read the live `current_epoch()` on the actuator admit path, or drive the cached-epoch refresh well below the actuator command period.
2. **Disk-wedge non-demotion (`1`, MED [Observed]).** `standby_monitor.rs:233` — a primary that cannot *read* its epoch (I/O hang) never self-demotes; it also can't refresh `cached_db_epoch`, so it stays Active and unfenced. *Fix:* self-demote after N consecutive epoch-read/heartbeat-write failures (fence-uncertainty → fail closed).
3. **WCET evidence mislabeled (`F7`, MED [Observed]).** `wcet_gate.rs:219` gates on **p99.9, not max**, and benches `validate_vehicle_command` directly — not the deployed path, which adds `serde_json` deser + Tower `to_bytes().await` + re-serialize on clamp (`policy_layer.rs:208`), none of it measured. The measured thing is not the deployed thing. *Fix:* gate on max-with-multiplier (or rename to a regression-smoke gate and stop citing it as WCET evidence), and add an end-to-end actuator-path bench.
4. **Predictive-RSS fail-open (`B3`)** and **motion-source split (`D/C-1`)** — close both so the checker can't be silently degraded by malformed perception timing.
5. **Audit gaps on overload are not themselves recorded (`4.2`, LOW [Observed]).** `audit_writer.rs:154` drops on a full queue with a log but no in-chain marker, so the tamper-evident ledger can have *unrecorded* gaps. *Fix:* emit an in-chain `AUDIT_GAP` marker / persisted dropped-count.

---

## 12. Robotics & Autonomous Systems Recommendations

- **Fix ingress QoS first (`N1`).** Default `Reliable + KeepLast(10)` on trajectory/odometry/objects is the single biggest robotics-correctness gap: it lets the adapter validate up to 10 stale samples after a stall. Mirror the `critical_actuator_profile` discipline on ingress with explicit sensor-data QoS + Deadline + Liveliness.
- **Adopt iceoryx2 for in-partition transport** (justified by the spike's measured ~1–2 µs and by torn-read elimination by construction). This is the highest-value robotics/IPC adoption.
- **Executor/scheduling.** Replace the 10 ms blocking `spin_once` with a dedicated-thread tighter spin or event-driven wait; add the shutdown channel (currently a Phase-4 TODO). For ROS 2, prefer a deterministic/static executor (e.g. callback-group isolation) so the slow/fast dual-rate loops have bounded servicing latency.
- **Wire the real output edge** (`node.rs:757` is a debug placeholder) so end-to-end FTTI becomes measurable.
- **Single CDR codec.** Replace hand-rolled framing with a real CDR codec / IDL-generated typesupport before any live CycloneDDS/Autoware subscriber consumes the frames (`C1`/`R1` are a guaranteed off-by-4 interop break otherwise).
- **Cyclone vs Fast DDS — [Speculative].** For Autoware integration, CycloneDDS + the SHM (iceoryx) PSMX transport aligns best with the in-partition zero-copy direction; standardize on it rather than carrying a parallel hand-rolled model.

---

## 13. SCADA & Critical-Infrastructure Recommendations

The industrial subsystem is the **most consistently fail-closed** part of the codebase (faithful-decode-or-refuse #85, undecodable/segmented/strict-unconfigured all deny, replay+freshness before eval, no slice panics — all binary-frame decoders are length-guarded). Recommendations:

- **Unify the two abstraction seams (`4.1`, MED).** Migrate Modbus/OPC-UA off the `IndustrialEvent`/action-claim path onto the `IndustrialAdapter` trait so all five protocols share one classification + `BoundSpec` bounding contract. A `ModbusMessage` carrying declared register width/type also resolves the `i64→f64` unfaithful-width issue (`B15`).
- **Close the narrow decode bypasses:** CIP length-equality (`B13`), DNP3 g41-on-Write (`B14`), and explicit `MAX_OBJECTS`/`MAX_DATA_LEN` caps on the unbounded DNP3/CANopen/CIP `Vec<u8>` payloads (`3.1`) to harden against resource-exhaustion from a malformed field device.
- **Verify the handler renders malformed frames as denials (`2.1`).** `dispatch_adapter` returns `Err(String)`; confirm + test that the handler at `kirra_verifier_service.rs:1245` produces `allowed:false`, not an ambiguous 500.
- **IEC 61850 extensibility — [Speculative].** The `IndustrialAdapter` trait is a low-friction seam for MMS/GOOSE: define a `Message`, implement `verdict` + `bound_magnitude`, add one enum variant + dispatch line. Do the unification (above) first so 61850 lands on one contract, not two.
- **Fabric peer-seed residual (`5.1`).** A registered-but-never-fed peer stays `Degraded` (limited motion) indefinitely; track toward the documented end-state of seeding every asset `LockedOut` once per-asset feeds exist.
- **Redundancy/determinism.** The control-loop mapping is deterministic (source-blind verdict, order-preserving first-breach-wins, clock-step-safe `saturating_*` freshness). For IEC 62443, the protocol decoders are the trust boundary and are well-guarded; the remaining hardening is the payload-size caps above.

---

## 14. Refactoring Opportunities

- **Split the 6.2k-line service binary.** Decompose `src/bin/kirra_verifier_service.rs` into `handlers/{attestation,federation,industrial,fleet,system}.rs`. This is also the natural moment to convert `store.with()` → `call/call_read` (P1) and add the CI grep gate.
- **Split `verifier_store.rs` (~6k lines) by table-domain** (`store/{nodes,audit,federation,epoch,av}.rs`) behind the existing `VerifierStore` facade.
- **Single source of truth for shared constants.** `RSS_REACTION_TIME_S` (and lateral-eps) are duplicated across `validation.rs` and `parko-core` — the two governors the design says must agree. Hoist into `kirra-core`.
- **Before/after — fleet recalc memoization (P3):**

  ```rust
  // BEFORE: fresh DFS per node, memo discarded between nodes — O(N·(N+E))
  let node_postures: Vec<FleetNodePosture> = app.nodes
      .iter()
      .map(|entry| app.calculate_posture(entry.key()))   // re-traverses shared deps N times
      .collect();

  // AFTER: snapshot keys (also fixes B1 deadlock), one shared memo — ~O(N+E)
  let ids: Vec<String> = app.nodes.iter().map(|e| e.key().clone()).collect();
  let mut black: HashMap<String, FleetNodePosture> = HashMap::new();
  let node_postures: Vec<FleetNodePosture> = ids.iter()
      .map(|id| app.calculate_posture_memoized(id, &mut black))
      .collect();
  ```

- **Before/after — clamp panic guard (B5):**

  ```rust
  // BEFORE: panics (→ abort in release) if a misconfigured cap inverts the range
  core_bounded_demand.clamp(self.constraint_cap_min, self.constraint_cap_max)

  // AFTER: order-tolerant, never panics; validate once in new()
  core_bounded_demand
      .max(self.constraint_cap_min.min(self.constraint_cap_max))
      .min(self.constraint_cap_min.max(self.constraint_cap_max))
  ```

- **Replace stringly-typed adapter `details: serde_json::Value`** with a typed observability channel (low priority; trades against the uniform dispatch).

---

## 15. Modern Technology Adoption Roadmap

Only technologies with a measurable, in-budget benefit here:

| Tech | Adopt? | Rationale |
|------|--------|-----------|
| **iceoryx2** | **Yes — highest value** | ~1–2 µs zero-copy in-partition SHM; torn-read elimination by construction; the spike already proves it. In-partition WCET-critical edge. |
| **Frozen `#[repr(C)]` SHM `GovernorContractView` + odd/even seqlock** | **Yes** | Cross-partition doer/checker isolation; fixes the `S2` shim-fence soundness gap; matches the spec. |
| **CycloneDDS + iceoryx PSMX** | **Yes (when DDS path ships)** | Real writer/reader + SHM transport; replaces the hand-rolled CDR model; aligns with Autoware. |
| **Linux PREEMPT_RT** | **Yes (host lanes)** | Bounds scheduler latency on the non-QNX deployments; complements the QNX partition story. |
| **`parking_lot::RwLock`** | **Consider** | Removes poison-wedge on the posture cache (B-class availability), keeping explicit fail-closed checks. |
| **SIMD** | **Niche** | Per-pose containment/RSS is O(50×256) edge tests — SIMD could help the corridor scan, but it's the slow loop, not the WCET path. Low priority. |
| **eBPF** | **Observability only — [Speculative]** | Could trace the actuator path latency in production without instrumenting the hot path. Not a control-path tech. |
| **DPDK / RDMA / io_uring** | **No** | No matching bottleneck (control path is SHM, fleet lane is latency-tolerant, grant I/O is low-rate). Gold-plating. |
| **QUIC** | **No / maybe fleet lane** | Zenoh already covers the intermittent fleet lane; QUIC adds nothing the lane needs. |
| **FlatBuffers / Cap'n Proto** | **Maybe (fleet reports)** | Zero-copy wire for federation/posture reports vs current JSON; only worth it if report rate grows. The control path should stay POD-`repr(C)`, not a serialization framework. |

---

## 16. Prioritized Improvement Plan

### Quick wins (days; high value, low risk)
1. **P1** Convert async-handler `store.with()` → `call/call_read`; add CI grep gate. *(runtime starvation)*
2. **P2 / B-class** Add `busy_timeout` to writer + durable connections. *(availability under checkpoint contention)*
3. **B1** Snapshot DashMap keys before traversal. *(deadlock hazard + enables P3)*
4. **B2** Bound `p_len` on the PLC-response path. *(remote crash)*
5. **B5** Validate `cap_min <= cap_max` / contract finiteness in `new()`/sentinel. *(config-typo abort)*
6. **B4** Pass real `dt` to the scalar governor on the Modbus path. *(rate limiter is decorative today)*
7. **N1** Set explicit sensor-data QoS on r2r ingress subscriptions. *(stale-drain on safety ingress)*
8. **B6** `fetch_max` the generation counter at boot. *(monotonicity invariant)*

### Medium-term (weeks)
9. **P3/P5** Shared-memo fleet recalc + `Arc<str>` interning.
10. **P4** Watchdog reads last-seen from memory, not disk-per-sweep.
11. **1b** Fence the actuator path on the live epoch (close the ~2 s two-writer window).
12. **1** Self-demote a primary that cannot read/confirm its epoch (disk-wedge).
13. **B3 / D/C-3** Predictive-RSS fails closed on supplied-but-unevaluable modes.
14. **D/C-1, D/C-2, B9, B10** Motion-source consistency, time-matched snapshot RSS, heading-wrap normalization, horizon truncation at the checker boundary.
15. **F7** Re-label/realign the WCET gate (max + multiplier) and add an end-to-end actuator-path bench.
16. **4.1 / B13–B15** Unify the industrial abstraction onto `IndustrialAdapter`; close CIP/DNP3 decode bypasses; add payload-size caps.
17. **Decompose** the service binary and `verifier_store.rs`.

### Long-term (quarters; certification-grade)
18. **Adopt iceoryx2** for in-partition transport; implement the odd/even-seqlock SHM `GovernorContractView`; wire the real fast-loop output edge → end-to-end FTTI becomes measurable on the QNX target.
19. **Replace the hand-rolled DDS/CDR model** with a real CycloneDDS writer + CDR codec; validate read-back QoS.
20. **Close the safety-case items**: in-chain audit-gap markers, federation per-controller generation high-water + canonical-payload byte-stability test, PREEMPT_RT on host lanes, and the QNX-target WCET measurement that converts the structural argument into target evidence.

---

## 17. Final Engineering Scorecard

Per-subsystem (1–10), from the seven deep-dives:

| Subsystem | Maint. | Perf. | Determ. | Safety | Test. |
|-----------|:---:|:---:|:---:|:---:|:---:|
| Verifier core / posture engine | 7 | 5 | 7 | 8 | 9 |
| Doer/Checker validation (RSS/containment) | 8 | 7 | 8 | 7 | 9 |
| Governor / kinematics / gateway | 7 | 6 | 6 | 7 | 8 |
| Security / attestation / federation / audit | 8 | 7 | 7 | 9 | 9 |
| SCADA / protocol adapters | 8 | 9 | 9 | 8 | 9 |
| IPC / DDS / transport / QNX | 8 | 6 | 5 | 7 | 9 |
| Persistence / HA / watchdog | 8 | 4 | 8 | 7 | 9 |
| **Workspace (weighted)** | **8** | **6** | **7** | **8** | **9** |

**Cross-cutting dimensions:**

| Dimension | Score | To reach 10 |
|-----------|:---:|---|
| **Maintainability** | 8 | Split the two monolith files; single source of truth for shared safety constants; unify the industrial abstraction. |
| **Performance** | 6 | P1–P7: get SQLite off the async runtime, add `busy_timeout`, share recalc memo, fix watchdog disk reads, shorten Mutex hold times. |
| **Determinism** | 7 | Monotonic-clock staleness; explicit ingress QoS + Deadline; tighter executor spin; odd/even seqlock; real `dt`. |
| **Safety** | 8 | Close the actuator two-writer window + disk-wedge demotion; predictive-RSS fail-closed; realign the WCET gate; in-chain audit-gap markers. |
| **Scalability** | 7 | Shared-memo/iterative DAG (stack-safe at fleet scale); per-asset sharding of the industrial proxy; `Arc<str>` interning. |
| **Testability** | 9 | Already excellent. Add: concurrent-recalc/deep-graph DAG tests, an end-to-end actuator-path WCET bench, and a predictive-RSS-unevaluable test. |
| **Extensibility** | 7 | One protocol seam (not two); a real transport binding behind the QoS model; planner plugin story is already strong. |

**Bottom line.** The architecture, safety reasoning, and test discipline are genuinely benchmark-grade for this domain. The distance to "industry benchmark" is not conceptual — it is **runtime hardening** (async/SQLite boundary, lock discipline), **transport realization** (real QoS on a real writer, iceoryx2 in-partition), and **closing a small number of fail-closed seams that currently leak** (actuator HA fence, predictive-RSS, WCET labeling). Every one of those is a bounded, well-scoped engineering task, and the codebase's own testability makes them safe to land.
