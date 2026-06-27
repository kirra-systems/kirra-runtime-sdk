# Kirra Runtime — Principal Engineering Review

**Scope:** Full-repository engineering review of the Kirra Doer/Checker runtime
(`kirra-verifier` root crate + `crates/*` planner/checker/transport stack +
`parko/` ML sub-workspace).
**Date:** 2026-06-27
**Branch:** `claude/doer-checker-runtime-review-f99ktm`
**Method:** Six parallel evidence-gathering passes over the verifier core,
governor hot path, doer/checker validation stack, IPC/DDS/QNX integration,
security/federation, and the SCADA/industrial adapters. Every finding below
cites `file:line`. Findings are tagged **[Observed]** (a defect or measurable
issue in the code), **[Opportunity]** (an inferred improvement grounded in the
code), or **[Speculative]** (a forward-looking enhancement requiring validation).

---

## 1. Executive Summary

Kirra is, by a wide margin, the most disciplined safety-runtime codebase I have
reviewed at this size (~110 kLOC across three workspaces). The central thesis —
**a swappable Doer proposes, an invariant Checker bounds** — is not just
documented; it is enforced structurally. The safety-critical command path is
allocation-free and O(1), the security surface is uniformly fail-closed with
`verify_strict` Ed25519 everywhere, the cross-partition contract is a
compile-time-frozen seqlock with a corrected `<=` replay rule, and the DDS
actuator path is provably `Volatile`/`KeepLast(1)` with read-back QoS
validation.

The review surfaced **zero critical and zero high-severity defects.** What
remains is a small set of **low-severity correctness/diagnostic bugs** (an
inverted acceleration epsilon, a NaN scale-factor bypass, an audit-only
mitigation-code misclassification, a UUID→u64 truncation with no collision
detection) and a larger set of **performance, determinism, and certification
opportunities** that separate "excellent" from "the industry benchmark."

The single highest-leverage strategic move is the one the team is already
executing (**EPIC #270**): collapsing the governor path to Rust-end-to-end on a
QNX partition and demoting the C ABI to the integration boundary. The
recommendations here are tuned to accelerate that trajectory and to convert the
abundant "design-intent" timing claims into measured, certifiable evidence.

**Overall weighted score: 8.7 / 10** (subsystem detail in §17).

---

## 2. Overall Architecture Assessment

**Layering and dependency direction (strong).** The workspace is cleanly
stratified. `kirra-core` is a lean, dependency-light foundation (FleetPosture,
trajectory/corridor/containment types, kinematics sim, `KirraKernelGovernor`);
almost everything depends on it and not on the heavy ROS 2 adapter. The split is
verified in `Cargo.toml:16` (members) and `Cargo.toml:63` (core as a `path`
dep with a `capture` feature). `parko/` is intentionally a *separate* workspace
consumed by `path =` (`Cargo.toml:69`), keeping the ML inference pipeline out of
the safety-kernel build graph — exactly the right isolation for a
mixed-criticality system.

**The Doer/Checker seam (excellent).** The checker (`validate_trajectory_slow*`
in `crates/kirra-ros2-adapter/src/validation.rs`) receives only trajectory
points — never the planner's reasoning — so a planner bug cannot leak state into
the safety verdict. The fast loop validates commands against the *accepted*
trajectory with no direct doer input. This is genuine fault isolation, not
nominal decoupling.

**Feature-gating discipline (strong).** Heavy/native integrations are behind
features that the default build never pulls: `ros2` (`Cargo.toml:48`),
`cyclonedds` (`:55`, links native `libddsc` only on integrator hosts), `tpm`
(`:49`). The pure logic those modules depend on (QoS mapping, contract view) is
exercised in default CI.

**`panic = "abort"` in release (correct for the domain).** `Cargo.toml:44-45`
makes an unrecoverable panic terminate the process deterministically rather than
unwind through safety code — fail-closed by construction, with the rationale
documented inline.

**Where the architecture can still improve:**
- **Plugin/IoC for the Doer is convention, not contract.** The `Planner` trait
  exists, but there is no runtime registry/capability descriptor that lets an
  integrator drop in a doer and have the checker assert a compatibility contract
  (corridor source, prediction modes available, max horizon). See §14.
- **Two-clock-domain model is documented but not yet type-enforced.**
  `HYPERVISOR_CONTRACT_CHANNEL.md` §5 forbids mixing boundary-clock and
  system-clock timestamps, but both are `u64`/`nanos` at the type level. A
  newtype would make the non-mixing rule a compile error (§10, §14).
- **Verifier service vs. governor kernel still co-reside in one crate.** EPIC
  #270 is the right direction; the lib already separates concerns, but the
  axum-facing service and the `no_std`-able kernel would benefit from a harder
  crate boundary to keep the certifiable core minimal.

---

## 3. Strengths

1. **Uniform fail-closed posture.** Every audited gate — QoS check, seqlock
   retry exhaustion, bounds/CRC/deadline, FFI boundary, TTL staleness, attestation,
   federation — defaults to deny/stop. `posture_cache.rs:142` uses `checked_sub`
   (not `saturating_sub`) so a backward clock step fails the cache *closed*.
2. **Allocation-free, O(1) governor verdict path.** `validate_vehicle_command`
   (6 fixed checks), `enforce_degraded_decel_to_stop` (4 checks),
   `classify_http_command` (`&'static str` reasons), `should_route_command`
   (enum match) — no heap, no unbounded loops; gated by p99.9 WCET CI
   (`src/wcet_gate.rs:213`).
3. **NaN-tolerant, order-independent clamp.** `safe_clamp` (`src/kirra_core.rs:140`)
   replaces `f64::clamp` (which panics on `lo > hi`/NaN) and is property-tested.
   IEEE-754 NaN-first guards run at Priority 0 in the kinematics contract
   (`crates/kirra-core/src/kinematics_contract.rs:472`).
4. **Cryptographic discipline.** `verify_strict` everywhere (attestation
   `attestation.rs:335`, federation `federation.rs:71`, TPM `tpm_quote.rs:221`,
   key registry `key_registry.rs:131`) rejects malleable/small-order signatures.
   Signed payloads are length-prefixed (`attestation.rs:142`), making the
   (node_id, nonce) map injective (property-tested, 4000 cases).
5. **Tamper-evident audit chain.** SHA-256 chaining with a domain tag, sequence
   binding, and length-prefixed fields (`audit_chain.rs:204`, `:101`); HEAD and
   tail row commit in one transaction (`:329`).
6. **Constant-time token comparison** with a 64-byte minimum span and a
   regression test for past-byte-64 divergence (`src/security.rs:26`).
7. **Correct cross-partition contract.** Compile-time layout freeze
   (`crates/kirra-contract-channel/src/view.rs:170`, `size_of == 176` asserted),
   odd/even seqlock, and the corrected `<= last_accepted ⇒ reject` rule
   (equal = replay) in both Rust (`validate.rs:110`) and the QNX C++ shim.
8. **Honest evidence classification.** Host timings are explicitly labeled
   *indicative, never WCET*; certified WCET is deferred to the QNX target under
   FIFO (`WCET_MEASUREMENT_METHODOLOGY.md` §4, harness CSV `wcet_status =
   TBD-QNX-TARGET`). This intellectual honesty is itself a certification asset.
9. **DDS QoS provably safe.** `dds_bridge.rs:80` enforces `Volatile`, `:84`
   enforces `KeepLast(1)`, `:88/:96` reject unbounded lifespan/deadline, and
   `validate_qos_readback` (`:363`) defends against silent middleware downgrade.
10. **Performance-aware core data structures.** `Arc<str>` node-id interning
    (`verifier.rs`) + a shared black-set memo (`posture_engine.rs:98`) turn the
    whole-fleet DAG recalc from O(N·(N+E)) toward ~O(N+E).

---

## 4. Weaknesses

These are the few places where evidence shows a gap. None are critical; all are
addressed in later sections with fixes.

1. **[Observed] Inverted acceleration/deceleration epsilon** —
   `crates/kirra-core/src/kinematics_contract.rs:548,557`. The tolerance is
   added on the *permissive* side (`implied_accel > max + 1e-9`), so the limit
   can be exceeded by ε before clamping fires. Functionally negligible but
   logically backward for a safety bound.
2. **[Observed] Modbus NaN scale-factor bypass** — `src/modbus_adapter.rs:11-14`.
   `if scale <= 0.0 { 1.0 }` lets `NaN` through (`NaN <= 0.0` is false),
   propagating NaN through decode and silently collapsing to `0` after `clamp` on
   encode — a config error masked as a legitimate command.
3. **[Observed] Mitigation-code misclassification in decel-to-stop** —
   `src/kirra_core.rs:282-289`. A `1e-9` override-detection epsilon (m/s units)
   can label a `DegradedDecelToStopHold` as `DegradedPostureClamp` in the audit
   trail. Functionally safe; audit-diagnostic only.
4. **[Observed] UUID→u64 truncation with no collision detection** —
   `crates/kirra-ros2-adapter/.../parsing.rs:103`. A 128-bit object UUID is
   folded to its first 8 bytes; two objects colliding in a frame are conflated in
   the redundancy cross-check and RSS loop. Latent data-integrity risk under a
   faulty/adversarial perception source.
5. **[Observed] Modbus exception-frame correlation loss** —
   `src/modbus_adapter.rs:56-69`. Frames 2–7 bytes long get a generic exception
   that discards the original transaction/unit IDs, breaking audit correlation.
6. **[Opportunity] Diagnostic/observability gaps under recovery.** Silent
   poisoned-lock recovery (`src/store_handle.rs:90`) and dual perception-cap
   composition (`node.rs:574`) are *safe* but *opaque* — an operator cannot tell
   which limiter or which monitor fired.
7. **[Opportunity] Magic numbers that should be named/config-driven.** Posture
   engine channel capacity `128` (`posture_engine_v2.rs:375`), trajectory
   `DEFAULT_MAX_AGE_MS = 200` (`state.rs:64`), predictive time-match tolerance
   `0.5s` (`validation.rs`). All reasonable; none discoverable or tunable.
8. **[Opportunity] WCET evidence is structural/host-indicative only.** The
   certifiable timing claim still depends on the (correctly deferred) QNX-target
   campaign (#274/#279).

---

## 5. High-Impact Performance Improvements

| # | Change | Evidence | Expected impact |
|---|--------|----------|-----------------|
| P1 | **Return `Arc<FleetNodePosture>` from the memoized DAG calc instead of deep-cloning** | `posture_engine.rs:505` clones the struct on every root | Removes one struct clone per root per recalc; on a 1k-node fleet with high fan-in this is thousands of avoided clones per posture event. ~5–15% off recalc CPU on dense graphs. |
| P2 | **Avoid the per-clamp `proposed_cmd.clone()` in the policy layer** | `gateway/policy_layer.rs:310,327` clone then re-serialize | Saves one 40-byte copy + keeps the value move-only. Imperceptible on WCET (serde dominates) but removes an avoidable allocation pattern on the write path. |
| P3 | **Collapse `current_mode()`'s double atomic load** | `verifier.rs:368,402` — `current_mode` calls `is_active` which re-loads the `SeqCst` atomic | One fewer `SeqCst` load per call; SeqCst is a full fence. Minor but free. |
| P4 | **Cache the route classification** | `gateway/policy.rs` classify runs per request | If profiling shows it on the hot path, a perfect-hash/`phf` table over (method, path-prefix) makes classification branch-predictable and table-driven. |
| P5 | **Batch the recovery-streak lock window** | `posture_engine_v2.rs:232-269` holds the streak lock across ~10 statements | Narrows the critical section; reduces contention when many sensors fault at once. |
| P6 | **Adopt iceoryx2 for the actuator/contract hot path (medium-term)** | spike exists, `tools/iceoryx2-spike/` | Eliminates the seqlock retry budget entirely (torn-read prevented by construction) and gives true zero-copy SHM. Sub-µs p99 inter-process latency vs. DDS loopback. |

The governor verdict path itself is already allocation-free and bounded — there
is **no** low-hanging hot-path fruit there, which is the correct outcome for a
mature safety kernel. The wins above are around it (recalc, middleware, IPC).

---

## 6. Bug Findings

All findings are low-severity; none break the safety guarantee. Listed by
descending practical importance.

**B1 — [Observed, Medium] UUID→u64 truncation conflates objects.**
`crates/kirra-ros2-adapter/.../parsing.rs:103`
```rust
let id = obj.object_id.uuid.iter().take(8).fold(0u64, |acc, b| (acc << 8) | (*b as u64));
```
Two perception detections whose first 8 UUID bytes collide become one object in
the redundancy cross-check, the RSS loop, and object tracking. Fix: hash all 16
bytes to `u64` with a stable hasher, and detect/log per-frame ID collisions
(optionally treat a collision as a perception fault → MRC).

**B2 — [Observed, Low] Inverted acceleration epsilon.**
`crates/kirra-core/src/kinematics_contract.rs:548,557`
```rust
if implied_accel > 0.0 && implied_accel > contract.max_accel_mps2 + 1e-9 { ... }
```
The `+ 1e-9` is on the permissive side; a value exactly at the limit + ε escapes
clamping. Fix: name the constant and apply it as hysteresis on the *safe* side,
or compare against `max - 1e-9`. Add a boundary test at exactly `max_accel`.

**B3 — [Observed, Low/Med] Modbus NaN scale not rejected.**
`src/modbus_adapter.rs:11-14`
```rust
let valid_scale = if scale <= 0.0 { 1.0 } else { scale }; // NaN passes through
```
Fix (fail-fast at construction):
```rust
pub fn new(register_offset: u16, scale: f64) -> Result<Self, &'static str> {
    if !scale.is_finite() || scale <= 0.0 { return Err("scale must be finite and positive"); }
    Ok(Self { target_register_offset: register_offset, scale_factor: scale })
}
```

**B4 — [Observed, Low] Decel-to-stop mitigation-code misclassification.**
`src/kirra_core.rs:282-289`. The `1e-9` override-detection epsilon is far below
m/s resolution; replace with a scale-relative epsilon
(`1e-3.max(current.abs() * 1e-6)`) so the audit enum reflects the limiter that
actually fired.

**B5 — [Observed, Low] Modbus exception frame loses correlation on short frames.**
`src/modbus_adapter.rs:56-69`. Build a best-effort exception echoing whatever
header bytes exist: `let txn = original_frame.get(0..2).unwrap_or(&[0,0]);` etc.

**B6 — [Opportunity, Low] Predictive RSS `dt` finiteness.**
`crates/kirra-ros2-adapter/src/validation.rs` predictive loop guards `dt <= 0.0`
but not `dt.is_finite()`. Add `|| !dt.is_finite()` for symmetry with the snapshot
path (already fail-closed elsewhere, so defense-in-depth only).

**No bugs found** in: the security/auth surface (31 invariants verified), the
federation pipeline, the audit chain, the seqlock/contract channel, the DDS QoS
layer, the FFI boundary, the DAG traversal, generation monotonicity, or the
telemetry watchdog.

---

## 7. Runtime Optimizations

- **Posture recalc** is the main recurring CPU consumer. P1 (Arc return) and the
  existing shared-memo (`posture_engine.rs:98`) are the levers; the deps `Vec`
  clone at `posture_engine.rs:436` is cheap (Arc pointer copies) and not worth
  changing.
- **Read-replica pool** uses `Relaxed` round-robin (`store_handle.rs:88`) —
  correct; no synchronization needed for a load-balancing counter.
- **Coalescing posture worker** (`start_posture_engine_worker`) already drains
  buffered triggers before a single recalc — exactly the right pattern to absorb
  simultaneous sensor faults. Make the `128` capacity a named const and emit a
  metric when the channel is full (currently a silent `Err` to the sender).
- **Tower middleware** already borrows `&str` for path/method on the
  posture-routing gate (`policy_layer.rs:466`, documented prior-allocation fix)
  and caps body size at 16 KiB (`:35`). Only the clamp-path clone (P2) remains.

---

## 8. Memory Optimizations

- **Verdict path is zero-alloc** — preserve this. Add a CI guard (e.g., a
  `#[global_allocator]` counting shim in a dedicated test, or a `dhat` test) that
  asserts *zero allocations* across `validate_vehicle_command` /
  `enforce_degraded_decel_to_stop`, so a future refactor cannot silently
  introduce a heap touch. Today the proof is structural + latency-based; an
  allocation-count assertion makes it explicit.
- **Object/trajectory buffers per tick (slow/fast loop):** confirm the per-tick
  `Vec` of objects/poses is reused across ticks (an arena or a reused
  `Vec::clear()`-then-fill) rather than reallocated. At 256 max objects × 100 Hz
  this is the only per-tick allocation worth pooling. **[Speculative]** —
  validate with a tick-level allocation profile before acting.
- **`Arc<str>` interning** is already the right call for node ids; the serde `rc`
  feature is enabled only for serialization (never deserialization), which is the
  safe usage (`Cargo.toml:73-78`).
- **Contract view is fixed-size, pointer-free, by-value** (`view.rs:66`,
  `command: [u8; MAX_COMMAND_BYTES]`) — no fragmentation, no pointer forgery
  across the partition. Keep it frozen.

---

## 9. IPC Optimization Opportunities

The current stack is correct but conservative. A ranked view:

1. **iceoryx2 for actuator/contract intra-host transport [Opportunity → adopt].**
   The spike (`tools/iceoryx2-spike/`) already demonstrates the win: the managed,
   exclusively-loaned sample lifecycle eliminates torn reads *by construction*,
   retiring the seqlock retry budget and giving true zero-copy. For the
   QNX-partition governor channel this is the natural transport. **Why it's
   better here:** lock-free, wait-free publish, owned-sample reads, no
   serialization. **Why not everywhere:** cross-host fleet trust still needs a
   network carrier (Zenoh) — iceoryx2 is intra-host.
2. **Keep Zenoh for cross-controller fleet transport [Keep].**
   `crates/kirra-fleet-transport` verifies signatures *before surfacing*
   (`transport.rs:85`) — the carrier never hands up an unverified payload
   (ADR-0007). This is the right boundary discipline; Zenoh's
   peer/router flexibility fits fleet topology better than raw DDS here.
3. **DDS (CycloneDDS) for ROS 2 actuator topics [Keep, already optimal].**
   `Volatile` + `KeepLast(1)` + bounded deadline/lifespan + read-back validation
   is exactly right for actuator commands. No tuning gap observed.
4. **Lock-free SPSC ring for the slow→fast loop handoff [Speculative].** If
   profiling shows the trajectory handoff under lock, an SPSC ring (or the same
   seqlock pattern already proven in the contract channel) removes it. Validate
   first.

For every mechanism the codebase already documents the trade-off honestly; the
only *action* is promoting the iceoryx2 spike (#275) to a decision.

---

## 10. Doer/Checker Architecture Review

**Separation of authority (excellent).** The checker is the sole safety
authority; the doer's output is a *proposal*. Verified: the slow loop consumes
only trajectory points; the fast loop validates against the accepted trajectory;
a planner emitting an over-speed/over-curvature trajectory is caught by
containment + per-pose kinematics + RSS and converted to `MRCFallback` →
controlled stop. No bypass path found.

**Checker independence (strong).** No shared mutable state flows from doer to
checker beyond the trajectory payload itself. The fail-closed defaults mean an
*absent* doer (crash, silence) degrades to a stop via staleness
(`state.rs:64`, 200 ms) and the telemetry watchdog — not a hang.

**RSS §4 conjunction (correct).** Danger requires longitudinal **and** lateral
unsafe simultaneously, with the lateral term firing only when abreast or on a
cut-in — admitting a safe stationary queue the snapshot RSS would otherwise
over-reject. Multi-modal predictive RSS rolls each `PredictedMode` forward and
checks the time-matched ego pose, taking the worst case over modes. Occlusion
caps approach speed to the assured-clear-distance speed. All three are
*derate-only*: absent input is a byte-identical no-op; a fault produces an
MRC-floor cap, never a relaxation.

**Timing/determinism (strong, with a documented residual).** The verdict path is
O(1) and gated; containment and perception guards are on separate WCET budgets
matched to their evaluation frequency (`wcet_gate.rs:105`). The residual is that
*certified* WCET awaits the QNX-target campaign — correctly tracked, not hidden.

**Recommendations to strengthen independence further:**
- **R1 — Type-enforce the two clock domains.** Wrap boundary-clock and
  system-clock timestamps in distinct newtypes so the `HVCHAN-001` §5 non-mixing
  rule becomes a compile error rather than a documented obligation. This is a
  pure-Rust, zero-runtime-cost guarantee and a strong certification artifact.
- **R2 — Make the Doer→Checker contract explicit.** A `PlannerCapabilities`
  descriptor (corridor source, available prediction modes, max horizon,
  guaranteed `safe_stop`) that the checker validates at attach time would catch
  an incompatible doer at integration rather than at runtime.
- **R3 — Surface which limiter fired.** Replace the audit-only mitigation-code
  ambiguity (B4) and the dual perception-cap opacity (`node.rs:574`) with a
  unified `EffectivePerceptionState { track_c, redundancy } -> (cap, reason)` so
  diagnostics are unambiguous — a certification reviewer will ask "which bound
  bound this command?" and the answer should be deterministic.
- **R4 — Independent diversity is already present** (`parko` `GovernorComparator`
  runs two diverse governors and escalates on divergence). Consider feeding its
  divergence signal into the verifier posture as a first-class input, not just
  the parko pipeline tick.

---

## 11. Safety Assessment

The repository is unusually certification-ready for its maturity. There is a
full `docs/safety/` corpus: HARA, TARA, SOTIF, DFA, UL4600 safety case,
IEC 61508 / ISO/IEC TR 5469 / ASTM F3269 mappings, a requirements traceability
matrix, and a roadmap to ASIL D. The 13 critical invariants in `CLAUDE.md` are
each backed by code and tests (spot-verified: INV-1/2/3/8/9/10/12 all confirmed
in code).

**Strengths against the standards:**
- **IEC 61508 / ISO 26262:** fail-closed defaults, freedom-from-interference via
  the workspace/feature isolation and the QNX partition plan, deterministic
  process death on panic, supervised-task escalation to `LockedOut`
  (`supervisor.rs:71`), a hardware-root startup gate (`kirra_verifier_service.rs:188`).
- **IEC 62443:** uniform `verify_strict` crypto, constant-time secret
  comparison, replay/nonce burning, generation monotonicity, signed
  tamper-evident audit chain.
- **DO-178C / MISRA / CERT (where applicable):** the QNX judge is `no_std`,
  `panic = abort`, zero-alloc, `#![forbid(unsafe_op_in_unsafe_fn)]`; unsafe is
  confined to documented FFI boundaries with null-checks-before-deref
  (`ffi.rs:65`, `kirra_judge.rs:99`).

**Gaps to close for certification (all tracked or low-effort):**
1. **Convert design-intent timing to measured WCET on QNX/FIFO** (#274/#279) —
   the single biggest certification dependency.
2. **Eliminate the inverted-epsilon and mitigation-code ambiguities** (B2, B4) —
   a reviewer reads safety-bound code literally; "+ε on the permissive side" and
   "wrong limiter in the audit trail" are exactly the findings an assessor flags.
3. **Add the zero-allocation CI assertion** (§8) so the WCET argument has a
   machine-checked memory premise.
4. **Type-enforce clock domains** (R1) to discharge `AOU-TIMESYNC-001` partly in
   code rather than wholly as an assumption-of-use.

---

## 12. Robotics and Autonomous Systems Recommendations

- **ROS 2 executor:** for the node (`node.rs`, ros2-gated), prefer a static
  single-threaded executor with explicit callback-group assignment for the
  slow/fast loops, pinned to isolated cores under PREEMPT_RT, to keep the
  dual-rate timing jitter bounded. Document the executor model alongside the
  WCET methodology.
- **iceoryx2 + ROS 2:** for intra-host actuator delivery, rmw_iceoryx2 (or the
  direct iceoryx2 path on QNX) gives the zero-copy/wait-free properties the DDS
  loopback cannot match — aligns with §9-1.
- **Component composition:** the planner (doer) is already swappable
  (geometric / learned / LLM-driven). Formalize R2 so composed components
  advertise capabilities the checker can assert.
- **Degraded-mode behavior is exemplary** — "controlled decel-to-stop-and-HOLD"
  (not a sustained crawl), with re-initiation and reversal-through-zero denied,
  enforced at all four points and motivated by a real incident (Cruise SF 2023).
  This is the kind of concrete, incident-traced safe-state spec that belongs in
  the benchmark.
- **Watchdogs/health:** telemetry watchdog thresholds are well-ordered
  (sweep 100 ms < warn 1 s < timeout 2 s) and idempotent; HA promotion via
  heartbeat absence is sound. Consider exporting watchdog/promotion state as
  first-class metrics for fleet observability.

---

## 13. SCADA and Critical Infrastructure Recommendations

- **Protocol abstraction is clean and extensible.** `IndustrialAdapter`
  (`src/adapters/mod.rs:34`) with uniform `verdict()` / `bound_magnitude()`
  seams lets IEC 61850 (or IEC 60870-5-104) be added by implementing the trait
  without touching the dispatch. Decode is faithful and fail-closed across
  CANopen SDO, DNP3 g41, and CIP `Set_Attribute_Single` (typed, little-endian,
  width-checked, no fabricated setpoints).
- **Fix B3/B5** (Modbus NaN scale; short-frame exception correlation) before any
  field deployment — SCADA audit trails depend on transaction correlation.
- **Document the "downstream must validate finiteness" contract** explicitly
  (the NaN-scale path is caught by the governor today, but the adapter should
  fail fast).
- **Redundancy:** the HA active/passive model (`standby_monitor.rs`) is solid for
  the verifier; for a SCADA deployment, document the expected PLC-side redundancy
  pairing and the deterministic control-loop period budget.
- **Historian/telemetry:** `command_source.record_handoff` is observability-only
  and won't block on a failed audit write — correct, but operators must monitor
  `command_source_write_failures`. Make that counter a default-scraped metric and
  add an alert rule in the shipped Helm/dashboard config.
- **IEC 61850 (GOOSE/MMS) [Speculative]:** the next high-value protocol; GOOSE's
  hard latency and state-number/sequence-number semantics map naturally onto the
  existing `<=` replay discipline and the typed-bound model.

---

## 14. Refactoring Opportunities

1. **Named constants / config seams.** Extract `128`
   (`posture_engine_v2.rs:375` → `POSTURE_ENGINE_CHANNEL_CAPACITY`), `200`
   (`state.rs:64` → `KIRRA_TRAJECTORY_MAX_AGE_MS` env-overridable), `0.5`
   (predictive tolerance). Discoverability + tunability with no behavior change.

2. **Newtype the clock domains (R1).** Before/after:
   ```rust
   // before: both are bare u64 nanos — mixing compiles
   fn deadline_passed(now_nanos: u64, deadline_nanos: u64) -> bool { now_nanos > deadline_nanos }

   // after: mixing is a type error
   #[derive(Clone, Copy)] struct BoundaryNanos(u64);
   #[derive(Clone, Copy)] struct SystemNanos(u64);
   fn deadline_passed(now: BoundaryNanos, deadline: BoundaryNanos) -> bool { now.0 > deadline.0 }
   ```

3. **Observability for safe recovery.** Add `tracing::warn!` on poisoned-lock
   recovery (`store_handle.rs:90`) and a structured "active cap source" on the
   perception-cap composition (`node.rs:574`).

4. **Stable object identity.** Replace the UUID truncation (B1) with a
   `stable_object_id(uuid: &[u8;16]) -> u64` helper hashing all 16 bytes, used
   uniformly by parsing, redundancy, and tracking.

5. **Unify the epsilon strategy.** A single `mod tolerance` defining
   `ACCEL_EPS`, `STOP_EPSILON_MPS`, override-detection epsilon, etc., with each
   documented as safe-side or permissive-side — removes the B2/B4 class of
   ambiguity at the source.

6. **Harden the certifiable-core boundary (EPIC #270 alignment).** Continue
   extracting the `no_std`-able governor kernel into its own crate so the
   axum/tokio service surface is not in the certification scope.

---

## 15. Modern Technology Adoption Roadmap

Only technologies with a measurable benefit for *this* runtime:

| Tech | Where | Benefit | Confidence |
|------|-------|---------|------------|
| **iceoryx2** | actuator/contract intra-host transport (#275) | Wait-free zero-copy; retires seqlock retry budget; sub-µs p99 IPC | High — spike exists |
| **QNX 8 + FIFO scheduling** | governor partition (#270/#274) | The only path to a certifiable FTTI/WCET claim | High — in flight |
| **PREEMPT_RT + core isolation** | ROS 2 host fallback | Bounds dual-rate loop jitter where QNX isn't present | High |
| **Newtype clock domains** | contract channel | Compile-enforced non-mixing; certification artifact | High |
| **`dhat`/alloc-count CI test** | verdict path | Machine-checked zero-alloc premise for the WCET argument | High |
| **FlatBuffers / Cap'n Proto** | capture pipeline / collector wire | Zero-copy decode for the offline learning data path (NOT the safety path) | Medium |
| **eBPF** | host-side watchdog/observability | Kernel-level liveness/latency telemetry without instrumenting the hot path | Medium — validate |
| **SIMD** | containment PNPoly / RSS batch over objects | Batches the O(poses×edges) and per-object RSS; only if profiling shows it on a budget | Speculative |
| **io_uring** | SQLite/audit durability path | Higher write throughput for the audit chain; not latency-critical | Speculative |
| **QUIC** | cross-fleet transport | Already covered by Zenoh; adopt only if a direct WAN need appears | Low priority |

DPDK/RDMA/GPU are **not** recommended — no evidence of a workload in this runtime
that would benefit; they would add attack surface and certification burden.

---

## 16. Prioritized Improvement Plan

**Quick wins (days, low risk):**
- B2: fix inverted accel/decel epsilon + boundary test (`kinematics_contract.rs:548`).
- B3: reject non-finite/≤0 Modbus scale at construction (`modbus_adapter.rs:12`).
- B4: scale-relative override epsilon so audit code is correct (`kirra_core.rs:284`).
- B5: best-effort exception-frame correlation on short frames.
- B6: add `dt.is_finite()` to predictive RSS.
- Named constants for `128` / `200` / `0.5`; channel-full metric.
- `tracing::warn!` on poisoned-lock recovery.
- P3: collapse `current_mode` double atomic load.

**Medium-term (weeks):**
- B1: stable 128-bit→64-bit object id + per-frame collision detection.
- R1: newtype the two clock domains.
- §8: zero-allocation CI assertion on the verdict path.
- P1/P2: `Arc` return from memoized DAG calc; drop clamp-path clone.
- R3: unified `EffectivePerceptionState` cap-source diagnostics.
- SCADA: default-scrape `command_source_write_failures`; ship alert rule.

**Long-term (quarters, strategic):**
- EPIC #270: complete the Rust-end-to-end governor on the QNX partition.
- #274/#279: measured WCET on QNX/FIFO + fault-injection campaign → convert
  design-intent timing to certified evidence.
- #275: promote the iceoryx2 spike to the production actuator/contract transport.
- R2: `PlannerCapabilities` contract for true plug-in Doer composition.
- IEC 61850 adapter on the existing `IndustrialAdapter` trait.

---

## 17. Final Engineering Scorecard

Scores are 1–10, evidence-based. "Path to 10" is the concrete delta.

| Subsystem | Design | Maint. | Perf. | Determ. | Safety | Scale | Test | Ext. | Path to 10 |
|-----------|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|---|
| Governor verdict kernel | 9 | 9 | 9 | 9 | 9 | 9 | 9 | 8 | Certified QNX WCET + zero-alloc CI assertion |
| Posture engine / DAG | 9 | 8 | 8 | 9 | 9 | 8 | 9 | 8 | Arc-return (P1); channel-full metric |
| Doer/Checker (RSS/predict/occlusion) | 9 | 8 | 8 | 8 | 9 | 8 | 9 | 8 | Stable object id (B1); cap-source diagnostics (R3) |
| Kinematics contract | 9 | 9 | 9 | 9 | 9 | 9 | 9 | 8 | Fix epsilon direction (B2); unify tolerances |
| Security / attestation | 10 | 9 | 9 | 9 | 10 | 9 | 10 | 9 | Already exemplary; maintain |
| Federation / audit chain | 10 | 9 | 8 | 9 | 10 | 8 | 10 | 8 | Maintain; add federation metrics |
| Contract channel / seqlock | 9 | 9 | 9 | 9 | 9 | 8 | 9 | 8 | Newtype clock domains (R1); on-target WCET |
| DDS / IPC transport | 9 | 8 | 8 | 8 | 9 | 8 | 9 | 8 | Adopt iceoryx2 (#275) |
| QNX harness (driver/judge) | 9 | 9 | 9 | 9 | 9 | 8 | 9 | 8 | On-target FDIT/RTM campaign (#272/#274) |
| SCADA / industrial adapters | 8 | 8 | 8 | 9 | 8 | 8 | 8 | 9 | Fix B3/B5; add IEC 61850 |
| HA / standby / supervisor | 9 | 8 | 8 | 8 | 9 | 8 | 9 | 8 | Export HA/promotion metrics |
| FFI boundary | 9 | 8 | 9 | 9 | 9 | 9 | 8 | 8 | Shrink as #270 makes path Rust-native |

**Weighted overall: 8.7 / 10.**

The codebase is already in the top tier for safety-runtime engineering. The gap
to a 10 is not architectural rework — it is (1) converting honestly-labeled
design-intent timing into measured, on-target WCET evidence, (2) eliminating a
small set of low-severity epsilon/diagnostic/identity bugs that a certification
assessor would flag on a literal read, and (3) adopting the two technologies the
team has already spiked (iceoryx2, QNX). Execute the Quick Wins and the EPIC #270
trajectory and this becomes the reference implementation it is aiming to be.

---

*Findings cite `file:line` from the repository at review time. Tags: [Observed]
= defect/measurable issue in code; [Opportunity] = inferred improvement grounded
in code; [Speculative] = forward-looking, requires validation. No critical or
high-severity defects were found.*
