# Kirra Runtime SDK — Mobileye-Class Production-Readiness Gap Analysis

**Date:** 2026-07-06
**Scope:** Full repository (root workspace ~24 crates, `parko/` sub-workspace, `tools/`, `ros2_ws/`, `console/`, docs, CI) — ~381 Rust files / ~145K LOC, 36 ADRs, ~60 safety-case documents.
**Method:** Phased subsystem review (inventory → per-subsystem capability assessment → benchmark → gap classification → roadmap). Seven parallel deep reviews: verifier core, governor/actuation path, doer-checker AV stack, parko ML workspace, fleet/OTA/security, real-time/QNX layer, safety case & process.
**Question answered:** *"If Mobileye engineers evaluated kirra-runtime-sdk today, what capabilities would they conclude are missing before this could be considered production-grade?"*

**Evidence labeling** (used throughout):
- **[REPO]** — finding based directly on this repository (code, docs, CI), with `file:line` where load-bearing.
- **[INDUSTRY]** — conclusion inferred from publicly documented production ADAS practice (ISO 26262/21448/21434, UNECE R155/R156, Uptane, AUTOSAR Adaptive, QNX Hypervisor for Safety, published RSS papers).
- **[EST]** — reasoned estimate about what a Mobileye-class evaluator would expect, deliberately avoiding claims about proprietary Mobileye implementations.

> This is an architectural/product/safety capability assessment, not a code review and not a certification determination.

---

## 1. Executive Summary

Kirra is a **fail-closed runtime safety governor** (doer-checker / simplex architecture): untrusted planners — geometric, learned, or LLM-driven — *propose*; a deterministic checker *bounds* every command against posture, kinematic envelopes, and RSS before anything reaches an actuator. Around that kernel sits a fleet-legitimacy control plane (attestation, RBAC, hash-chained audit, federation, HA), an OTA campaign engine with A/B installers, an ML-inference substrate with a diverse-governor comparator, and an early QNX/partition real-time lane.

**What a Mobileye evaluation team would conclude, in one paragraph:** the *safety-concept engineering* is unusually strong for a pre-production codebase — the simplex decomposition is the textbook-correct pattern, the fail-closed discipline is pervasive and tested, the RSS longitudinal/occlusion math is faithful to the published model, and the documentation is exceptionally honest about its own gaps (a trait certification bodies value and rarely see). But the system is a **credible reference implementation / pilot platform, not a production runtime**. Every load-bearing production property — hard real-time execution, certified WCET, hypervisor partitioning and freedom from interference, target-hardware evidence, a real perception stack, Uptane-grade OTA security, HSM key custody, fleet-scale storage, independent safety assessment — is either explicitly deferred (Phase-II), implemented as an honest stand-in, or absent. [REPO]

**The five headline gaps** (full table in §4):

1. **The deployed enforcement path is a POSIX/HTTP/tokio proxy, not a certified in-line real-time enforcer.** The verdict kernel is pure and bounded, but the shipping transport is axum HTTP + serde JSON body-rewrite and UDP/bincode; the QNX/no_std/seqlock partition lane exists only as a feasibility harness with QEMU-VM "INDICATIVE" numbers, no hypervisor, and no aarch64 target data. [REPO]
2. **No independently assessable safety case yet.** Every safety artifact is `Status: Draft`; there is no ISO 26262 Part-2 safety management evidence, no Part-8 tool qualification (rustc, not Ferrocene), MC/DC is a branch-coverage proxy, mutation testing is named but absent, and WCET/FTTI is unproven on target. [REPO]
3. **Perception and world model are stubs.** The checker's guarantees are conditional on inputs that today come from a 2D LaserScan harness with nearest-neighbor tracking, a mock semantic detector, no localization (frame integrity defaults to `Trusted`), no camera/radar/fusion, and an armed-but-unfed pedestrian-RSS channel. The safety case correctly identifies the shared world model as the residual common-cause hole — and the D1 independent detection channel that would close it is a design document only. [REPO]
4. **OTA/security is R156-shaped but not R155/R156-deep.** Single-key signing (no Uptane role separation, no key-compromise recovery), installer verifies a hash not a signature, secure boot/dm-verity/measured-boot are documented-unimplemented, no HSM/KMS, no delta updates, and the fleet store is single-writer SQLite — a hard ceiling around single-site/pilot fleet sizes. [REPO][INDUSTRY]
5. **Scale and observability are pilot-grade.** No request rate-limiting/backpressure on the control plane, no tracing spans/correlation IDs, metrics without latency histograms, a designed-but-unwired fleet ingress rate limiter, and no multi-tenancy. [REPO]

**Counterweight — where Kirra is genuinely differentiated** (§10): the doer-invariant safety case ("KIRRA bounds *any* doer, including an LLM"), machine-checked safety-case gates in CI (SPI registry, SOTIF trigger-coverage manifest, auto-generated traceability matrix), evidence-class honesty encoded in types (`INDICATIVE` vs `QNX-TARGET-MEASURED`), the negative-control mutation rows in the KPI gate, and the LLM/agentic-AI action-filter seam. Several of these are ahead of typical industry practice, including — in process terms — what most production teams demonstrate publicly. [REPO][EST]

**Verdict preview** (§13): **not production-grade today; a strong foundation with an unusually low-risk path to pilot deployments in constrained ODDs** (sidewalk courier, industrial sites) and a long, capital-intensive path (18+ months, hardware + certification partners) to Mobileye-class consumer-vehicle ADAS. Production Readiness Score: **4.4/10 overall** — with individual axes ranging from 8/10 (safety concept, supply chain) to 2/10 (perception, real-time platform).

---

## 2. Current Architecture

### 2.1 Layered view [REPO]

```
┌─────────────────────────────────────────────────────────────────────┐
│ Fleet console (Next.js) · website · examples (Rust/C/Python)        │  UX/DX
├─────────────────────────────────────────────────────────────────────┤
│ kirra-verifier (root crate): axum HTTP control plane                │
│   posture DAG engine · attestation (Ed25519+PCR16+TPM quote) ·      │  Fleet
│   RBAC principals · audit hash-chain + WORM shipper · federation ·  │  legitimacy
│   HA epoch fencing · OTA campaign engine · SQLite (WAL) store       │  (QM plane)
├─────────────────────────────────────────────────────────────────────┤
│ Doer side (untrusted)          │ Checker side (safety authority)    │
│  kirra-planner (Occy geometric │  kirra-trajectory: containment +   │
│   + learned + Mick LLM seam)   │   per-pose kinematics + RSS +      │  Doer-
│  kirra-taj (perception Phase-A)│   predictive RSS + redundancy      │  checker
│  kirra-map (Lanelet2-lite)     │  kirra-core kinematics contract    │  kernel
│  parko (ONNX/OpenVINO/TRT)     │  parko-kirra diverse comparator    │
├─────────────────────────────────────────────────────────────────────┤
│ kirra-ros2-adapter dual-rate node · DDS bridge (CycloneDDS gated) · │  Integration
│ industrial adapters (Modbus/DNP3/CANopen/CIP) · C FFI · action      │
│ filter (LLM JSON → typed intent)                                    │
├─────────────────────────────────────────────────────────────────────┤
│ RT/partition lane (Phase-I): kirra-contract-channel (repr(C)        │
│ seqlock) · kirra-hv-carrier (POSIX SHM) · kirra-l3-e2e ·            │  Real-time
│ kirra-release-token · no_std QNX judge · iceoryx2 spike ·           │  (prototype)
│ kirra-timing / kirra-wcet-bench                                     │
├─────────────────────────────────────────────────────────────────────┤
│ OTA: kirra-ota-installer (A/B, health-gated rollback, nvbootctrl    │  Fleet ops
│ seam) · Zenoh fleet transport (Ed25519 verify-before-use) ·         │
│ capture→collector learning loop (JSONL→Parquet)                     │
└─────────────────────────────────────────────────────────────────────┘
```

### 2.2 Execution and threading model [REPO]

- **Control plane:** `#[tokio::main]` multithreaded async; axum 0.8 routers; one SQLite writer behind `Arc<Mutex<VerifierStore>>` + 4 read-only WAL replicas, all access offloaded via `spawn_blocking` (`src/store_handle.rs:40,127,180`). Mutex poison deliberately *recovered* (`into_inner()`) — fail-operational. Background loops (telemetry watchdog 100 ms, standby heartbeat, campaign monitor 1 s, audit shipper 5 s) run as **supervised** tokio tasks with restart budgets; exhaustion of a critical task forces fleet `LockedOut` (`src/supervisor.rs:125-138`).
- **Verdict kernel:** `validate_vehicle_command` (`crates/kirra-core/src/kinematics_contract.rs:462`) is a stateless pure P0→P6 pipeline — NaN/Inf Priority-0 rejection, envelope-before-rate clamping, direction-aware accel/brake, bicycle-model lateral bound. A second, stateful scalar kernel (`KirraKernelGovernor`, `src/kirra_core.rs:216`) serves the C FFI and Modbus proxy — **two kernels maintaining the same Degraded decel-to-stop invariant** (a flagged maintenance risk).
- **Lifecycle:** explicit fail-closed startup ordering (store → key admission truth-table → generation anti-regression init → HA epoch arbitration → `sd_notify(READY=1)` + systemd watchdog); graceful shutdown with WAL checkpoint; release builds `panic = "abort"` with a documented fail-closed process-death rationale.
- **IPC/serialization inventory:** HTTP/JSON (control plane), UDP+bincode (two-box governor), POSIX-SHM seqlock over a frozen `#[repr(C)]` contract (partition lane), Zenoh (fleet lane, signed payloads), hand-rolled CDR-LE for DDS (feature-gated CycloneDDS writer with QoS read-back), r2r ROS 2 (feature-gated). No zero-copy transport in any default build.

### 2.3 Where the safety authority actually lives [REPO]

Four mirrored Degraded/MRC enforcement points (gateway envelope middleware, fabric `AssetGovernor`, ros2-adapter slow loop, parko-kirra MRC profile) with the HTTP actuator route reachable under Degraded by design (ADR-0011 Option A). The dual-rate ROS 2 node is the strongest runtime engineering in the repo: 200 µs fast-loop WCET budget, MRC published every cycle, monotonic freshness clock (immune to NTP jumps), stale/poisoned/absent inputs → MRC floor, `KeepLast(1)+BestEffort` QoS to prevent stale-command backlog (`crates/kirra-ros2-adapter/src/node.rs:108-160,592-631`).

---

## 3. Mobileye Capability Matrix

Rating: ● present/strong · ◐ partial/prototype · ○ absent. "Mobileye-class expectation" rows are [EST] anchored in [INDUSTRY] norms; "Kirra today" is [REPO].

| Capability | Kirra today | Notes |
|---|:-:|---|
| **Runtime platform** | | |
| Component lifecycle mgmt | ◐ | Explicit fail-closed startup ordering, supervised tasks; no execution-manager abstraction, no dependency-ordered multi-process orchestration (AUTOSAR EM-equivalent absent) |
| Execution management / scheduling | ○ | tokio best-effort; `SCHED_FIFO` only in benches; QNX runmask pinning is a TODO comment |
| Process supervision & restart policies | ◐ | In-process task supervisor with restart budget + LockedOut escalation; nothing supervises the *process* beyond systemd watchdog |
| Fault containment / partitioning | ○ | QNX Hypervisor absent (license-blocked); POSIX `PROT_READ` + type-level read-only is the only boundary |
| Health monitoring | ◐ | Telemetry watchdog (2 s dead-man), inference deadline+jitter monitor, HA heartbeat; no unified health tree/aggregation service |
| Watchdogs | ● | Layered: systemd, telemetry watchdog, inference straggler watchdog, posture-cache TTL |
| Safety state machine | ● | Nominal/Degraded/LockedOut with hysteresis, sticky lockout (loom-verified), typed `LockoutReason` |
| Safety supervisor | ● | The verifier + posture engine *is* one; supervisor-trip and campaign auto-halt wired |
| Safe-state management | ● | SS-002 decel-to-stop-and-HOLD spec, MRC per asset (ADR-0012), 4 enforcement points |
| Redundant/diverse execution | ◐ | Real implementation-diverse governor + comparator (parko-kirra); shared spec/config/toolchain residual; N-version deferred; divergence→FleetPosture wiring PROPOSED only |
| Multi-compute / multi-SoC | ○ | Single-box; two-box UDP prototype; no lockstep, no cross-SoC state sync |
| **IPC & middleware** | | |
| Zero-copy transport | ○ | iceoryx2 is an isolated spike, never in the SDK dependency tree |
| Shared memory | ◐ | POSIX-SHM seqlock carrier proven functionally on QNX-in-QEMU; ordering gap on weak memory unmodeled (see §4 G-13) |
| Deterministic messaging / QoS | ◐ | DDS QoS model + negotiated-QoS read-back is genuinely good; CycloneDDS writer never built in CI; default build is a byte-framer |
| Latency guarantees | ○ | Fail-closed timeouts substitute for deadlines; no end-to-end latency budget enforced at runtime |
| WCET support | ◐ | Structural O(1) gate + host p99.9 regression bench + honest methodology doc; no target data, no static analysis, no MBPTA/EVT |
| **Memory & real-time** | | |
| Heap avoidance / static allocation | ◐ | no_std zero-alloc judge and kirra-timing exist; deployed paths allocate per request/clamp/inference-tensor |
| Memory pools / arenas / fragmentation control | ○ | None |
| Lock-free structures | ◐ | DashMap, atomics, seqlock, lock-free verdict arms; Mutex on FFI and inference hot paths |
| Priority inheritance / scheduling analysis / CPU affinity / NUMA | ○ | Not present in code (Linux `CPU_SET` in one bench only) |
| **Boot & recovery** | | |
| Fast startup / dependency ordering | ◐ | Ordered and fail-closed, but unmeasured; SQLite recovery + generation anti-regression are solid |
| Crash recovery / warm restart / state persistence | ◐ | WAL + power-loss-drill-proven audit chain; posture generation persists; no warm-standby state handoff beyond heartbeat promotion (~12 s) |
| **Diagnostics** | | |
| Structured logging | ◐ | `tracing` events everywhere; zero spans/`#[instrument]`, no correlation IDs |
| Metrics | ◐ | Prometheus counters/gauges incl. OTA + safety series; **no histograms/latency percentiles** |
| Trace collection / performance counters | ○ | None (QNX tracelogger noted as not-established) |
| Fault reporting | ● | Hash-chained audit ledger + typed deny codes + SPI registry is stronger than typical |
| **Safety** (detail §5, §9) | ◐ | Exemplary concept + traceability honesty; no independent assessment, tool qual, target evidence |
| **Security** (detail §4) | ◐ | Strong attestation/RBAC/audit/supply-chain; no Uptane, no HSM, secure boot unimplemented |
| **OTA** | ◐ | Campaign engine + A/B + health-gated rollback shape is right; depth gaps per §4 G-7 |
| **Configuration** | ◐ | Gateway config versioned+digested+validated; the verifier itself is ~30 unvalidated env vars |
| **Developer experience** (detail §7) | ● | Genuinely good: examples, C header, quickstarts, honest docs, 21-job CI |
| **Simulation / HIL / integration testing** | ◐ | CARLA bridge + scenario runner + virtual clock; HIL is a Python stub; no statistical validation campaign |

---

## 4. Gap Analysis Table

Severity: **Critical** = blocks any production/certification claim; **High** = blocks pilot-at-scale or a major R155/R156/26262 clause; **Medium** = engineering debt with bounded risk; **Low** = polish. Effort: S < 2 pw · M = 2–8 pw · L = 2–6 pm · XL = 6+ pm (senior-engineer months, [EST]).

| # | Gap | Sev | Why Mobileye would expect it | Current state [REPO] | Desired state | Complexity / Effort | Dependencies | Risk if omitted |
|---|---|---|---|---|---|---|---|---|
| G-1 | **Hard-real-time in-line enforcement path** | Critical | [EST] Production ADAS enforces in the control loop with bounded latency, not via an HTTP proxy that rewrites JSON bodies | Verdict kernel pure/bounded, but deployed transport = axum/serde + UDP/bincode; per-clamp `serde_json::to_vec` allocation (`src/gateway/policy_layer.rs:389-430`) | Governor on RTOS partition consuming the frozen `repr(C)` contract over SHM; HTTP plane demoted to management only | XL | G-2, G-13, QNX license, target HW | Latency/jitter unbounded → FTTI claim impossible; a stalled proxy is "only" fail-closed availability loss, but availability *is* a product property |
| G-2 | **Hypervisor partitioning + freedom-from-interference evidence** | Critical | [INDUSTRY] ISO 26262-6 FFI (PO-2 of Kirra's own decomposition) requires enforced spatial/temporal isolation | QNX Hypervisor absent (no `qvm`, license-blocked; `docs/safety/HV_FAULT_CAMPAIGN.md:22-28`); campaign specified but unexecuted | QNX Hypervisor for Safety (or equivalent) with executed interference fault campaign on target | XL (mostly procurement + campaign) | Commercial license, aarch64 target | The ASIL decomposition is void without PO-2 — the entire ASIL-D(D) claim collapses |
| G-3 | **Certified WCET on target hardware** | Critical | [INDUSTRY] FTTI budgets need target-measured (and for ASIL-D, often statically analyzed) WCET | All numbers QEMU-VM `INDICATIVE`; no aarch64 data; no aiT/OTAWA; no cache/interference analysis; no EVT/MBPTA | Target-measured HWM + margin + static-analysis corroboration on the deployment SoC, cache-cold discipline executed | L–XL | Target HW, G-2, tracing toolchain | Timing claims remain marketing; assessor rejects FTTI |
| G-4 | **Real perception stack (or certified perception contract)** | Critical | [EST] Mobileye's entire value is perception; even as a checker-SEooC, the AoU burden must be dischargeable by *someone* | 2D LaserScan + NN tracker + mock semantic detector; no camera/radar/3D/fusion; no localization (frame integrity defaults `Trusted`, `node.rs:714-720`); pedestrian-RSS unfed | Either a qualified perception partner integration discharging the AoUs, or the D1 independent detection channel built | XL | Sensors, D1 design (exists on paper) | Checker guarantees are conditional on inputs nobody currently produces; "you cannot be conservative about something you cannot see" (repo's own words) |
| G-5 | **Independent safety assessment + Part-2 safety management** | Critical | [INDUSTRY] 26262 requires confirmation measures with independence; every Kirra artifact is self-authored Draft | HARA/decomposition/SEooC docs real but `Draft`, "Pending TÜV pre-assessment"; no safety plan, no confirmation reviews | Baselined safety case, safety plan, confirmation measures, TÜV/exida pre-assessment executed | L (process + external) | Budget, G-6 partially | No customer OEM can accept the SEooC; claims stay unverifiable |
| G-6 | **Tool qualification (compiler) + true MC/DC + mutation testing** | High | [INDUSTRY] Part-8 TCL analysis; MC/DC for ASIL-D; mutation testing already *named* in SAFETY_GOALS.md:99 | rustc unqualified (Ferrocene "pending adoption"); MC/DC = branch-coverage proxy (flag regressed); mutation testing absent; fuzzing build-only in CI | Ferrocene (or qualified toolchain) for the judge/kernel; measured MC/DC on check path; cargo-mutants gate; continuous fuzzing | M–L | Ferrocene licensing; judge already rustc-direct | Verification evidence rejected at assessment; silent test-rot risk |
| G-7 | **Uptane-grade OTA security + key lifecycle** | High | [INDUSTRY] Uptane is the automotive OTA norm; R156 expects key-compromise recovery and secure update chains | Single Ed25519 signer; installer verifies hash not signature; no role separation/expiry/rollback-attack metadata; no delta/resume/bandwidth mgmt; governor key non-rotatable; no HSM/KMS; TPM-unseal a stub | Uptane roles (root/targets/snapshot/timestamp), signature-verified staging, key rotation + HSM custody, delta updates | L | G-8 for on-device roots | One key compromise = fleet-wide arbitrary governor artifact; unrecoverable without re-flash |
| G-8 | **Secure boot / dm-verity / measured boot on device** | High | [INDUSTRY] R155 + any attestation story needs a hardware root of trust; PCR16 today is self-reported | Designed in `ROOTFS_AB_DESIGN.md:84-102`, unimplemented; TPM quote parser exists and is careful, but provisioning is "a deployment concern" | Orin Secure Boot + dm-verity rootfs + TPM AK provisioning + quote-required policy default-on | L | Device fleet, per-SoC work | Attestation is a signed self-assertion; measured-boot claims in docs overstate deployment reality |
| G-9 | **Fleet-scale backend** | High | [EST] Mobileye operates at continental scale; single-writer SQLite + 12 s heartbeat failover is a pilot topology | `Arc<Mutex<VerifierStore>>` SQLite WAL + epoch fence; active/passive HA sharing one file; no quorum/lease service | Pluggable store (Postgres/distributed) behind the existing store seam; real leader lease; horizontal read path | L | Store trait extraction (seam exists) | Hard ceiling ~single site / hundreds of nodes; enterprise sales blocked |
| G-10 | **Control-plane hardening: rate limiting, backpressure, tracing** | High | [INDUSTRY] Any internet-adjacent control plane needs DoS controls; the repo's own TARA lists this | No tower rate-limit/429; fleet `IngressRateLimiter` built+tested but **unwired** (zero call sites); no spans/correlation IDs; no latency histograms | Wire the existing limiter; add tower concurrency/rate layers; `#[instrument]` spans; histogram metrics | S–M | None — mostly assembling existing parts | Signature-verify DoS rides the carrier (the exact attack the module documents); no runtime latency observability despite timing claims |
| G-11 | **Execution manager / deterministic scheduling integration** | High | [EST] AUTOSAR-Adaptive-EM-like ordered startup, deadline monitoring, CPU affinity, priority config | tokio defaults; FIFO only in benches; runmask TODO | Declarative process/task manifest: priorities, affinity, deadlines, startup dependencies, deadline-miss telemetry | M–L | G-1 platform decision | Nominal-path jitter unbounded; multi-node bring-up remains artisanal |
| G-12 | **RSS completeness: lateral μ, split accel/brake, proper response, curved geometry** | High | [EST] RSS is Mobileye's own formal model; reviewers will check §4 fidelity first | Longitudinal/opposite/occlusion faithful; lateral primitive knowingly under-specified (single `lat_accel_max`, μ=0, `rss.rs:111-141` tagged #408); conjunction approximated by 2.5 m footprint band; straight-lane Frenet only; fixed 0.5 s reaction | Resolve #408 with safety-case-backed params; curved-lane Frenet; formal proper-response obligation; footprint from vehicle class | M | Safety engineer sign-off | Lateral unsafe-by-μ scenarios admitted; credibility hit with the one audience that knows RSS best |
| G-13 | **Seqlock memory-ordering proof on weak memory + loom coverage** | High | [INDUSTRY] An ASIL-D cross-partition primitive on aarch64 needs more than a 20k-iteration stress test | Reader `g2` Acquire but body loads Relaxed — needs acquire *fence* after body copy for aarch64; loom models cover posture protocols only, **not the seqlock** | Add fence; loom-model the seqlock; consider herd7/litmus on the aarch64 model | S (fix) + M (proof) | None | Torn-read admitted as valid on the actual deployment ISA — a silent safety-integrity hole in the flagship primitive |
| G-14 | **Diverse-governor claim closure** | Medium | [INDUSTRY] 1oo2D credit requires signed-off diversity analysis + divergence driving the durable safe state | Real implementation diversity, honest shared-spec residual; COMPARATOR_DIVERSITY.md unsigned DRAFT; divergence→FleetPosture PROPOSED only | Sign-off; wire divergence to durable LockedOut latch; decide N-version scope | M | Safety engineer | Comparator credit unusable in the safety case; divergence stays advisory |
| G-15 | **ML runtime production depth: OOD, model lineage, HW CI** | Medium | [EST] Production ML runtimes version/rollback models, detect distribution shift, and gate on real silicon | Integrity allow-list opt-in (logs-only default); no OOD detection; no accuracy telemetry; all CI ubuntu-latest, GPU/TRT self-skipping | Signed model manifests + rollback; OOD monitor feeding posture; a Jetson/Orin CI runner | M–L | Hardware runner | Silent model swap/drift; TRT behavior unproven until a customer finds it |
| G-16 | **Statistical validation campaign** | Medium | [INDUSTRY] SOTIF Area-3 arguments need scenario volume with statistical treatment (per e.g. PEGASUS-style practice) | Deterministic sweeps "in the low hundreds"; no Monte Carlo/CIs; CARLA bridge exists but no campaign; HIL = Python stub | 10⁴–10⁵ scenario corpus, sampled + CI-reported KPIs, CARLA campaign runner, real HIL rig | L | Sim infra | Residual-risk quantification impossible; SOTIF case stays structural-only |
| G-17 | **Unified validated configuration** | Medium | [INDUSTRY] Type-safe validated config with effective-config digest across the product, not just the gateway | ~30 ad-hoc `KIRRA_*` env vars; excellent versioned/digested config exists for the industrial gateway only | Extend the gateway config pattern to the verifier + node; schema + startup validation + digest in audit log | M | None | Misconfiguration class errors (wrong envelope, silent default) — the exact class the vehicle-class fail-closed check was added for |
| G-18 | **Zero-copy transport adoption** | Medium | [EST] High-rate sensor/trajectory paths at production scale avoid per-frame copies | iceoryx2 spike isolated; DDS path copies `Vec<u8>` frames; hand-rolled CDR | Adopt iceoryx2 (per ADR-0006) inside partitions; loaned-buffer DDS or SHM for trajectories | L | G-1/G-2 platform | Throughput/jitter ceiling; Orin tail already ~47–51 ms unbounded in the spike |
| G-19 | **Cert/PKI lifecycle** | Medium | [INDUSTRY] mTLS at fleet scale needs issuance/renewal/revocation | Fingerprint pinning per cert; no CA issuance/renewal/CRL/OCSP | SPIFFE-like or EST-based identity lifecycle | M | — | Cert expiry = manual fleet outage; revocation impossible |
| G-20 | **Schema migration framework** | Low | [INDUSTRY] Versioned migrations with `user_version`, tested up/down paths | `CREATE IF NOT EXISTS` + additive `ALTER` (`src/verifier_store/mod.rs:527-630`) | Versioned migration table + CI upgrade tests from released versions | S | — | Upgrade-in-place risk grows with every release |
| G-21 | **Warm-standby state handoff / faster failover** | Low | [EST] 12 s promotion is long for a safety supervisor with a 5 s posture TTL | Heartbeat promotion 10 s + poll | Lease-based failover ≤ posture TTL; replicated posture cache | M | G-9 | Availability gap window; posture staleness denial storm during failover |
| G-22 | **AUTOSAR Adaptive / middleware interop** | Low | [EST] OEM integration often demands ara::com / SOME/IP or DDS-native presence | None (HTTP/DDS-lite/ROS 2 only) | SOME/IP or DDS-native service mapping for the verdict/posture APIs | L | Customer-driven | Slower OEM integration; not a safety gap |

---

## 5. Safety Assessment

**Concept: strong. Evidence: draft. Independence: absent.** [REPO]

- **Architecture.** The simplex/safety-monitor decomposition (`ASIL D = ASIL D(D) governor + QM(D) planner`, ISO 26262-9 Cl.5) is the correct and defensible pattern, and the repo states its own proof obligations (PO-1 diagnostic coverage, PO-2 FFI) and admits the decomposition is void without them. The residual common-cause analysis (shared world model, coupling factors C1–C12) is intellectually honest — the C5/C7 "you cannot be conservative about something you cannot see" finding is exactly what an assessor would write. [REPO]
- **HARA** is real (17 hazards, S/E/C per 26262-3, traced goals) but Draft, with two parallel goal schemes (16-goal kernel + 9-goal Occy) that must be unified before baselining. [REPO]
- **Safe-state design** is a standout: Degraded = decel-to-stop-and-HOLD (motivated explicitly by the Cruise SF Oct-2023 pullover-drag incident), LockedOut = human-reset, MRC disambiguation documented, four enforcement points, automatic-vs-manual recovery split, sticky lockout loom-verified. [REPO]
- **Verification** is broad but pre-certification: 51-property kinematics proptest, 4 fail-closed fuzz targets (build-gated only), loom on two protocols, MC/DC pair tests (proxy measurement), power-loss audit drill, HA split-brain drill, fault injection tied to safety goals. Missing: mutation testing, run-to-fuzz, gating coverage thresholds, target-hardware anything. [REPO]
- **SOTIF** is a proper ISO 21448 treatment with a machine-gated trigger-coverage manifest (`ci/sotif_trigger_coverage.json`) — prose promises are CI-enforced, which is genuinely rare. Known gaps (occlusion G1, localization G2) surfaced honestly. [REPO]
- **The single biggest safety-case risk** is not any one artifact — it is that the *conditionality* of the whole case rests on AoUs (1,334 lines of them) that no current integration discharges: synchronized clocks, perception quality, localization integrity, dead-man base controllers. A Mobileye reviewer would say: *the checker is real; the system it checks is hypothetical.* [REPO][EST]

## 6. Runtime Assessment

- **Control plane** (QM): well-engineered fail-closed supervisory service; error-handling discipline near-exemplary (no unwrap/panic on core paths, typed errors, constant-time compares, zeroize). Its weaknesses are scale-shaped, not correctness-shaped: SQLite single-writer, no rate limiting, no spans, no latency histograms, env-var config sprawl, partial clock injectability. [REPO]
- **Real-time claim vs reality:** the honest summary the repo itself encodes — *host timing is INDICATIVE, never WCET* — is correct, and the discipline of encoding evidence classes in types (`MeasurementEnv::is_certified_wcet`, a KVM VM structurally cannot mint a certified row) is better than most production teams manage. But nothing hard-real-time is deployed: no RTOS in production, no partitioning, no affinity/priority configuration, no memory pools, allocation on every deployed hot path. The 91 µs Ed25519 finding (release token ≈ the whole 100 µs verdict budget) shows the team measures the right things. [REPO]
- **Fail-closed as a substitute for determinism** is the load-bearing runtime idea: no/late/stale answer ⇒ deny ⇒ MRC. This is legitimate for a governor (availability degrades, never safety) — but it converts every performance deficiency into an availability incident, and availability is a product requirement too. A fleet that MRC-stops on every GC-of-the-day is not shippable. [REPO][EST]
- **Concurrency:** DashMap/atomic/broadcast architecture is sound; loom coverage is narrow (two protocols) relative to the concurrency surface, and the flagship cross-partition seqlock is not loom-modeled and has a real weak-memory ordering question (G-13). [REPO]

## 7. SDK Assessment

Better than the industry median for safety-adjacent SDKs. [REPO][EST]

- **Surface:** Rust lib + C FFI (`include/kirra.h`, runnable C demo, CERT-mapped pointer contracts), Python action-filter examples (LangChain/OpenAI), Rust quickstart, HTTP+SSE APIs with a documented authorization matrix, Prometheus metrics, a Next.js fleet console, helm/systemd/docker deployment.
- **Docs:** 36 ADRs of unusually high decision-record quality; honest per-feature IMPLEMENTED/PARTIAL/GAP tagging; versioning policy with a "safety asymmetry" rule; migration and runbook docs. Weaknesses: incomplete AEGIS→KIRRA rebrand, stale test-count figures propagating, everything Draft.
- **API stability:** semver policy exists and is enforced by process, but the repo is pre-1.0 in spirit (1.1.x with a documented v1.5.0 tag wart); the C ABI and the frozen `repr(C)` contract are the only formally frozen surfaces.
- **DX gaps:** no generated OpenAPI spec found for the HTTP surface, no client SDKs beyond examples, single process-global FFI governor (not per-actuator), no cargo-doc-published site guarantee, no `cargo fmt --check` lane.

## 8. Production Readiness Score

Scores 0–10, [REPO]-grounded, [EST]-calibrated against "Mobileye-class production" = 10 and "typical funded AV startup pilot" ≈ 5.

| Axis | Score | One-line justification |
|---|:-:|---|
| Safety concept & architecture | **8** | Correct simplex pattern, pervasive fail-closed, honest residuals |
| Safety evidence & certification readiness | **3** | All Draft, no independence, no tool qual, no target data |
| Real-time platform | **2** | Prototype lane only; deployed = tokio/HTTP/UDP |
| Perception & world model | **2** | LaserScan harness + mocks; no localization |
| Planning (as a product) | **3** | Deliberate: doers are demonstrators of checker generality |
| Checker/RSS kernel | **7** | Faithful longitudinal/occlusion math, strong numerics; lateral μ & geometry gaps |
| Fleet control plane | **6** | Excellent discipline, pilot-scale ceiling |
| OTA | **5** | Right shape (campaigns, A/B, health rollback); Uptane/secure-boot depth missing |
| Security | **5** | Strong attestation/RBAC/audit/supply-chain; key custody & device root of trust missing |
| Diagnostics & observability | **4** | Audit/SPI strong; tracing/histograms absent |
| Testing & CI | **7** | 21 lanes, loom, fuzz, proptest, KPI gates; no mutation/coverage-gating/HW lanes |
| SDK & DX | **7** | Multi-language examples, ADR quality, console |
| Scalability & HA | **3** | Single-writer SQLite, 12 s failover |
| **Overall (weighted toward safety+runtime)** | **4.4** | Reference implementation / pilot grade |

## 9. ISO 26262 Readiness

| Part | Status | Evidence [REPO] |
|---|---|---|
| Part 2 (management) | **Missing** | No safety plan, no confirmation measures, no independence; all artifacts self-authored Draft |
| Part 3 (concept) | **Substantially drafted** | HARA (17 hazards), safety goals, ASIL decomposition, SEooC AoU register — pending qualified-assessor confirmation |
| Part 4 (system) | **Partial** | Architecture + safe-state spec + technical safety requirements traceable; no system-level V&V on target |
| Part 5 (hardware) | **Out of scope / failing** | PMHF 17.7 FIT single-supply fails ASIL-D target; correctly flagged as integrator-shared |
| Part 6 (software) | **Partial** | Coding guidelines, unit tests, branch coverage, proptest, MC/DC-proxy; no qualified toolchain, no measured MC/DC, no unit-verification sign-off |
| Part 7 (production/operation) | **Early** | OTA + field-monitoring SPI design; SPI loop not operational (some SPIs tagged GAP) |
| Part 8 (supporting) | **Missing** | No tool qualification (rustc/cargo/SQLite explicitly unqualified); config mgmt of the safety case not baselined |
| Part 9 (ASIL analysis) | **Drafted** | Decomposition + DFA + coupling analysis are real; PO-1/PO-2 open |
| SOTIF (21448) | **Ahead of 26262 evidence** | Machine-gated trigger coverage; structural default-deny argument; G1/G2 open |
| Cybersecurity (21434/R155) | **Skeleton** | TARA provisional; CSMS process artifacts absent |

**Honest framing:** roughly at the entry gate of a TÜV *pre-assessment* (which the docs themselves target), i.e., concept-phase-complete, implementation-evidence-partial, process-evidence-absent. That is 18–30 months of focused work from an ASIL-D SEooC certificate under normal conditions. [EST]

## 10. Competitive Differentiators

Where Kirra **exceeds** typical industry approaches (and would earn genuine respect in a Mobileye review): [REPO][EST]

1. **Doer-invariant safety case.** "The checker bounds *any* doer — geometric, learned, or LLM" is architecturally cleaner than monitor designs coupled to one planner, and ADR-0020 argues it explicitly. The learned-planner and LLM seams exist precisely to *prove* checker generality. This is a differentiated product thesis: a vendor-neutral safety SEooC for the agentic-AI-in-the-physical-world era.
2. **LLM/agentic action filtering as a first-class safety surface.** The typed-intent parse + posture gate + envelope backstop + fuzzed JSON decoder, marketed as "drop between your AI agent and your robot fleet," addresses a market (LLM-driven robotics) that classical ADAS stacks ignore. No incumbent ADAS runtime has this seam.
3. **Machine-checked safety case.** SPI registry gated by a test that verifies every claimed telemetry event is actually emitted in non-test code; SOTIF trigger coverage CI-gated against the doc catalog; traceability matrix auto-generated from `// SAFETY:` tags; an RTM *gap report* that publicly documented its own aspirational claims and tracked honest closure. This is safety-case-as-code — ahead of common industry practice, where the RTM is a spreadsheet that drifts.
4. **Evidence-class honesty encoded in types.** `INDICATIVE-KVM` vs `QNX-TARGET-MEASURED`, a cert gate that structurally cannot mint a certified row in a VM, "HWM is a lower bound on true WCET" in the methodology. Assessors reward this; most vendors do the opposite.
5. **Negative-control mutation rows in the KPI gate** — the CI metric must *detect* an injected regression or the gate fails. Testing the test is rare.
6. **Fail-closed micro-engineering depth**: width-faithful industrial decoders that refuse to fabricate magnitudes, DDS negotiated-QoS read-back refusing silent downgrade, anti-smuggling CANopen guards, constant-time compare with length-independent floor, power-loss audit drill under SIGKILL. This texture is hard to fake and signals a real safety culture.
7. **Supply-chain posture** (SHA-pinned actions, cargo-deny on both workspaces, CycloneDX SBOM, keyless cosign by digest, double-build reproducibility on the judge) is above most startups and many production teams.
8. **Radical honesty as a moat.** Nearly every reviewer note in this analysis was *already written down somewhere in the repo*. For a certification-driven sale, a company that documents its own gaps is a lower-diligence-risk partner. [EST]

Where Mobileye could reasonably view the architecture as **novel**: the combination of (2) + (3) — an LLM-era actuation firewall with a CI-enforced safety case — and the release-token-on-actuation-path design (ADR-0031) informed by an actual measured crypto-vs-WCET conflict. [EST]

## 11. Prioritized Roadmap

Effort = senior-engineer time [EST]. Impact keys: Safety (S), Performance (P), Commercial (C), Certification (Cert).

### Immediate (≤ 2 weeks)
| Item | Impact | Effort |
|---|---|---|
| Fix seqlock ordering for weak memory (acquire fence after body copy) + add a loom model of the seqlock (G-13) | S/Cert: closes a silent integrity hole in the flagship primitive | ~1 pw |
| Wire the existing `IngressRateLimiter` into the Zenoh ingest path; add tower rate/concurrency limits + 429s on axum (G-10) | S (DoS)/C | ~1 pw |
| Turn fuzz lanes from build-only to short run-to-fuzz in CI; add `cargo fmt --check`; make cargo-deny advisories gating with a triage allowlist | Cert/S | ~0.5 pw |
| Add latency histograms (verdict path, store ops, HTTP) + `#[instrument]` spans with correlation IDs (G-10) | P/C: makes timing claims observable | ~1 pw |
| Unify the two safety-goal schemes; refresh stale test counts in the ~5 affected docs | Cert (assessor first impressions) | ~0.5 pw |

### Near-term (1–2 months)
| Item | Impact | Effort |
|---|---|---|
| Resolve RSS #408: split lateral accel/brake params, add μ margin, safety-case-backed constants; footprint from vehicle class (G-12) | S/Cert/C: credibility with RSS-literate buyers | 3–4 pw |
| cargo-mutants gate on the checker crates + gating coverage threshold (G-6 partial) | Cert | 2 pw |
| Signature-verified OTA staging (verify cosign/Ed25519 sig, not just hash) + governor-key rotation procedure (G-7 start) | S/Cert (R156) | 3 pw |
| Extend versioned/digested config pattern from the gateway to verifier + node; startup validation of all `KIRRA_*` (G-17) | S/C | 3 pw |
| Wire comparator divergence → durable FleetPosture latch; sign off COMPARATOR_DIVERSITY.md (G-14) | S/Cert | 2–3 pw |
| Stand up one Jetson Orin self-hosted CI runner (parko TRT + iceoryx2 + adapter lanes) (G-15 partial) | S/P/Cert: first real-silicon evidence loop | 2 pw + HW |
| SQLite migration framework with `user_version` + upgrade tests (G-20) | C | 1–2 pw |

### Medium-term (3–6 months)
| Item | Impact | Effort |
|---|---|---|
| Uptane role model for OTA (root/targets/snapshot/timestamp, expiry, rollback protection) + HSM/KMS key custody (G-7) | S/Cert (R155/R156)/C: table stakes for automotive buyers | 2–3 pm |
| Secure boot + dm-verity + TPM AK provisioning on Orin; flip `require_tpm_quote` default for provisioned fleets (G-8) | S/Cert | 2–3 pm |
| Procure QNX Hypervisor for Safety; execute the already-specified HV fault campaign (interference: CPU flood/starve/clock skew) on aarch64 target (G-2, unblocks G-3) | Cert: PO-2 evidence begins | 2–3 pm + license |
| Target-hardware WCET campaign (Orin + QNX): tracelogger, cache-cold discipline, publish first `QNX-TARGET-MEASURED` rows; add EVT/MBPTA analysis to kirra-timing (G-3) | Cert/P | 2–3 pm |
| Statistical scenario campaign: scale KPI corpus 10⁴+, Monte-Carlo sampling + confidence intervals, CARLA campaign runner replacing the HIL stub (G-16) | S/Cert (SOTIF)/C | 2–3 pm |
| Pluggable store backend (extract trait from `VerifierStore`, add Postgres impl) + lease-based failover ≤ posture TTL (G-9, G-21) | C (scale)/S | 2–3 pm |
| TÜV/exida pre-assessment engagement; baseline the safety case out of Draft; write the Part-2 safety plan (G-5 start) | Cert: converts docs into evidence | 1–2 pm + external |
| OOD/input-shift monitor feeding posture; signed model manifests with rollback (G-15) | S/C | 1.5 pm |

### Long-term (6–18 months)
| Item | Impact | Effort |
|---|---|---|
| Production RT governor: no_std verdict kernel + seqlock contract + release token deployed on a QNX partition consuming SHM in-line; HTTP demoted to management plane; iceoryx2 adoption per ADR-0006 (G-1, G-18) | S/P/Cert: the architecture the docs already promise | 6–9 pm |
| Ferrocene (or equivalent) qualified toolchain for judge + checker kernel; Part-8 tool qualification dossier (G-6) | Cert | 2–3 pm + license |
| Discharge the perception AoUs: either a qualified perception-partner integration or build the specified D1 independent detection channel (radar+thermal, veto-only) (G-4) | S/Cert/C: the biggest single credibility unlock | 9–15 pm + sensors |
| Execution manager: declarative task/process manifest (priority, affinity, deadlines, startup DAG), deadline-miss telemetry (G-11) | P/S | 3–4 pm |
| ISO 26262 ASIL-D(D) SEooC certification campaign for the Governor (independent assessment, confirmation measures, measured MC/DC, target V&V) (G-5/G-6 completion) | Cert/C: the product's entire premium | 12+ pm, external |
| Curved-geometry RSS + formal proper-response + pedestrian-RSS fed by a real VRU detector (G-12 completion) | S/C | 3–4 pm |
| CSMS (R155) process build-out: vulnerability mgmt, incident response, OTA-specific TARA, monitoring SLAs | Cert/C | 2–3 pm, ongoing |

## 12. Top 10 Highest-ROI Improvements

1. **Seqlock fence + loom model (G-13)** — days of work; removes a latent ASIL-D-primitive integrity hole on the actual target ISA.
2. **Wire the ingress rate limiter + HTTP backpressure (G-10)** — the code already exists; closes the repo's own documented DoS gap.
3. **RSS lateral parameters (#408) resolution (G-12)** — modest effort, outsized credibility with exactly the RSS-literate evaluators this analysis assumes.
4. **One Orin CI runner (G-15)** — converts an entire class of "self-skipping" tests into real evidence; unblocks TRT, iceoryx2, and WCET workstreams.
5. **Signature-verified OTA staging + key rotation (G-7 first slice)** — small change to the installer seam; removes the single-hash trust cliff.
6. **cargo-mutants + run-to-fuzz + gating coverage (G-6 slice)** — cheap, mechanical, and directly answers the "can a test be silently deleted?" assessor question.
7. **Latency histograms + tracing spans (G-10)** — makes the fail-closed timing story observable in production; near-zero risk.
8. **Divergence → durable posture wiring + diversity sign-off (G-14)** — turns already-built comparator machinery into claimable 1oo2D credit.
9. **TÜV pre-assessment engagement (G-5)** — external, schedulable now; converts the Draft corpus into a prioritized authority-backed gap list (and is the docs' own stated next step).
10. **Config unification on the existing gateway pattern (G-17)** — eliminates the misconfiguration class that the vehicle-class abort was invented to catch, product-wide.

## 13. Overall Verdict

**kirra-runtime-sdk is not production-grade by Mobileye standards today, and it is one of the more credible foundations toward that bar that a small team could have built.**

A Mobileye evaluation would likely conclude: [EST]

- The **safety concept** (simplex doer-checker, fail-closed everywhere, RSS-bounded actuation, honest AoUs) is correct, well-argued, and aligned with how the industry's most safety-literate organizations think. The checker kernel and the dual-rate fail-closed node would survive expert scrutiny with findings, not rejection.
- The **platform** underneath it is a pilot: POSIX/tokio/HTTP/SQLite where production demands RTOS partitions, bounded latency, zero-copy transport, and fleet-scale storage. The QNX/no_std/seqlock lane shows the team knows precisely where it must land — but knows is not has: no hypervisor, no target silicon, no certified WCET, no independent assessment.
- The **inputs are the moat they don't own**: with perception, localization, and the D1 channel unbuilt or mocked, Kirra today is a governor of hypothetical integrations. As an SEooC that is a *legitimate* position — but it makes the commercial case wholly dependent on partners willing to discharge 1,334 lines of assumptions.
- The **process maturity is inverted from the usual failure mode**: most startups have code ahead of safety story; Kirra has a safety story (and safety-case-as-code CI machinery) ahead of platform evidence. That inversion is the cheaper one to fix with money and time, because retrofitting honesty is much harder than procuring hardware.

**Recommendation to a CTO/CSO:** treat this as a Series-stage safety-kernel asset, not a deployable runtime. Fund the 2-week and 1–2-month items immediately (they are cheap and close real holes), commit to target hardware + QNX Hypervisor + a pre-assessment within two quarters, and make a strategic decision on the perception question — partner vs. build D1 — because that, not the code, is the long pole to anything Mobileye would call production.

---

*Findings marked [REPO] cite this repository as of 2026-07-06 (commit lineage: main @ 0a2a8f1). [INDUSTRY] items reference publicly documented standards and practices (ISO 26262/21448/21434, UNECE R155/R156, Uptane, published RSS literature, AUTOSAR Adaptive, QNX product documentation). [EST] items are the author's calibrated estimates of Mobileye-class expectations and deliberately avoid claims about Mobileye's proprietary implementations.*
