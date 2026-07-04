# Kirra Runtime SDK — Industry-Benchmark Gap Analysis

**Date:** 2026-07-02 · **Audience:** CTO / CSO / Principal Engineering
**Method:** measured repository inventory (Phase 1), five independent subsystem deep-reads with file:line citations (Phase 2), benchmark against the publicly known capabilities of top-tier production ADAS platform vendors (Phases 3–4), classified gaps and roadmap (Phases 5–7).

**Provenance discipline.** Every claim is tagged:
- **[REPO]** — verified in this repository (cited)
- **[INDUSTRY]** — general production-ADAS practice
- **[VENDOR-PUBLIC]** — public statements, papers, and filings of top-tier production ADAS vendors
- **[VENDOR-EST]** — reasoned inference about those vendors; not verified; no proprietary speculation

---

## 1. Executive Summary

**The question:** *If a top-tier production-ADAS engineering team evaluated kirra-runtime-sdk today, what would they conclude is missing before it could be considered production-grade?*

**The one-paragraph answer.** They would find a genuinely rigorous, fail-closed **runtime safety governor** — an RSS-enforcing checker with MC/DC-tested primitives, a frozen-layout cross-partition contract channel measured at sub-microsecond latency on a QNX target, an implementation-diverse dual governor, and a documentation/governance culture (36 ADRs, 59 safety docs, candid gap reports) that exceeds most pre-production programs. They would then conclude it is **not an ADAS stack and not yet a certifiable product**: the entire world-model half (sensor fusion, tracking lifecycle, localization, map lifecycle) is absent or demo-grade, the checker's guarantees are conditional on perception Assumptions of Use (recall >90%, position error <0.5 m) that **no in-tree component demonstrates**, the certified safety artifact (QNX + Ferrocene governor) is **produced by no pipeline**, safety evidence execution lags documentation (~20% test-level RTM coverage, MC/DC attempted-not-achieved, all safety docs Draft, no assessor), and there is **no fleet lifecycle at all** — no OTA, no A/B, no rollback, no secure boot. [REPO throughout; see §4]

**But the comparison is partly a category error — and that is the strategic finding.** Kirra is not competing with the incumbents' full stacks; it is the **runtime-assurance layer those vendors build internally and do not sell**. Its differentiators (vendor-neutral external checker, LLM/agentic doers as first-class untrusted proposers, heterogeneous-fleet legitimacy plane spanning industrial protocols, candid in-repo safety case) have no public equivalent from the incumbent vendors. The honest self-assessment: **TRL 4–5; a credible safety-kernel company, not yet a shippable product.** The roadmap (§11) sequences the path: close the live wiring gaps in days, the evidence-execution gaps in months, and the fleet/perception gaps over 6–18 months — most of the highest-ROI items are wiring and pipeline work on machinery that already exists.

---

## 2. Current Architecture Overview [REPO]

~125,144 LOC Rust across 3 workspaces; 2,214 `#[test]` functions; 36 ADRs; 59 safety documents. Six layers:

| Layer | What it is | Maturity |
|---|---|---|
| **Fleet control plane** (root `kirra-verifier`) | axum HTTP service: Ed25519 challenge-response attestation, gray/black posture DAG, tamper-evident audit chain, epoch-fenced HA over shared SQLite, industrial adapters (Modbus/DNP3/CANopen/CIP) | Mature beta |
| **Vehicle governor kernel** (`kirra-core`, `kirra-trajectory`) | The CHECKER: SG2 containment + P0–P6 kinematics + IEEE-2846 structured RSS (lon∧lat conjunction, multi-modal predictive, occlusion Rule 4) + True-Redundancy cross-check; all fail-closed | **Production-grade design**; strongest asset |
| **Doer layer** (`kirra-planner`, `kirra-map`, `kirra-taj`) | Geometric planner + Mick typed-intent seam (LLM emits one JSON intent; every failure → HOLD); learned planner = ES-trained toy proving the bounding thesis; lanelet-lite map; single-lidar Phase-A perception with mock semantic detector | Geometric: medium; learned/LLM/perception: **demo** |
| **ML / diverse governor** (`parko/`) | Dual implementation-diverse governors, divergence escalator, ONNX/OpenVINO/TensorRT backends with determinism honesty; RSS math excellent — **but not wired into the live tick** | Design high; integration gap |
| **Transport / partition** | Frozen 176-byte `#[repr(C)]` seqlock contract channel (`no_std`, forbid-unsafe), POSIX-SHM carrier with RO governor mapping, Ed25519 release tokens over enforced bytes; zenoh fleet spike; DDS Volatile discipline | Channel: production-grade; carriers: host-proven, target=spec |
| **Target evidence** | QNX 8.0 E2E harness — full doer→checker→release chain 5/5 PASS across two QNX processes; decide_cycle 0.69 µs p50 / 1.1 µs p99.9 FIFO; WCET CI gate; INDICATIVE-vs-CERTIFIED labeling discipline | Methodology production-grade; evidence Phase-I (VM) only |

**Execution model:** tokio async everywhere except the QNX judge path; no deterministic executor; the RT story is deliberately extracted into the partition kernel, not retrofitted onto the services. [REPO]

---

## 3. Industry-Leader Capability Matrix

| Capability | Industry leader (top-tier production ADAS) | Kirra | Verdict |
|---|---|---|---|
| **Formal safety model (RSS)** | Originated the published formal model and drove IEEE 2846 standardization [VENDOR-PUBLIC] | Implements the structured-road subset with MC/DC tests, documented deviations (#408), fail-closed NaN discipline [REPO] | **Comparable in kind** for structured roads; missing pedestrian/unstructured/mutual-blame RSS |
| **Enforcement point** | Safety model embedded as a monitor inside their own closed stack [VENDOR-PUBLIC] | RSS as an *external* governor bounding any doer, with signed release tokens [REPO] | **Kirra differentiator** (different product) |
| **Perception** | Two independent full sensing channels (camera-only + radar/lidar), each independently drive-capable [VENDOR-PUBLIC]; validated at 100M+-unit fleet scale [VENDOR-PUBLIC] | Single lidar, straight-cone corridor, mock semantic detector; True-Redundancy *cross-check logic* exists but only implementation-level, and no real channel to feed it [REPO] | **Critical gap** — 2–3 generations behind |
| **Tracking / prediction** | Production multi-target trackers; learned interaction-aware prediction [VENDOR-EST] | Single-frame NN associator; CV/CTRV/lane-follow kinematic modes with honest plausibility gating [REPO] | **Critical gap** |
| **Localization + maps** | Crowdsourced HD maps harvested from consumer fleets, continuously refreshed, auto-generated [VENDOR-PUBLIC] | Lanelet-lite local map, no localization, no versioning, sub-km projection [REPO] | **Critical gap**; also not Kirra's product to win |
| **Certified runtime/silicon** | Custom ASIL-rated SoCs, deterministic RTOS on-chip [VENDOR-PUBLIC] | QNX partition design + measured sub-µs kernel + WCET methodology; certified artifact not yet built; hypervisor track license-blocked [REPO] | Gap, but the **architecture is aimed correctly** |
| **Data engine** | Petabyte fleet harvesting, mining, sim farms [VENDOR-PUBLIC/EST] | Learning-loop architecture + real isolated collector (Parquet, lineage); capture point CONFIRMED + BUILT (hybrid, all-arms, default-OFF); train/sim/release unbuilt; ~7-scenario eval corpus [REPO] | **Major gap** (2–3 orders of magnitude on scenarios) |
| **Fleet ops (OTA/monitoring/keys)** | Consumer fleets OTA-updated; R155/R156 compliance [VENDOR-PUBLIC] | No OTA/A-B/rollback anywhere in code; single admin token; no PKI at scale [REPO] | **Critical gap** |
| **Certification evidence** | TÜV-assessed, ASIL-B/D certified components [VENDOR-PUBLIC] | GSN-structured safety case, HARA, RTM — all Draft, ~20% test-level, no assessor [REPO] | **Major gap** in execution, not structure |
| **Runtime assurance for third-party/AI planners** | Not offered publicly [VENDOR-PUBLIC] | The core product: fail-closed bounding of geometric/learned/LLM doers, proven end-to-end on QNX [REPO] | **Kirra's category** |

---

## 4. Gap Analysis Table

Severity = blocking effect on a production-grade claim. Effort: S <2wk · M <2mo · L <6mo · XL 6–18mo.

| # | Gap | Sev. | Evidence [REPO] | Complexity | Effort | Risk if unaddressed |
|---|---|---|---|---|---|---|
| G1 | **Perception AoU undemonstrated** — checker guarantees conditional on recall >90% / pos err <0.5 m; no fusion, no tracking lifecycle, no localization, detector is a mock | Critical | `kirra-taj` Phase-A only; `frame_integrity` consumes ε it doesn't compute; formal spec §2 AoU | Very high | XL | Safety case is structurally sound but *vacuous at the boundary*; any deployment claim is unsupportable |
| G2 | **Parko live RSS wiring gap** — `evaluate_scene`/occlusion/water/commit-zone built+tested but NOT invoked by `run_pipeline_tick`; `rss_state` defaults `safe:true` | Critical | parko trait exposes only `evaluate()` | Low | S | The best RSS math in the repo is dead code on the running path — silent fail-open by default |
| G3 | **No fleet lifecycle** — OTA, A/B partitions, rollback, staged rollout, campaign engine all absent from code | Critical | grep hits only roadmap/TARA docs | High | XL | Cannot answer "how does a governor fix reach N vehicles and roll back" |
| G4 | **Certified artifact produced by no pipeline** — QNX/Ferrocene governor is a documented recipe; release ships only the Linux verifier; all timing evidence VM-indicative | Critical | `release.yml`; `crates/kirra-governor-service/README.md`; INDICATIVE-KVM labels | Medium | L | The thing being certified does not exist as a build product |
| G5 | **Safety evidence execution lag** — RTM ~20% test-level (11/16 goal-level), MC/DC attempted→branch fallback, coverage not threshold-gated, all docs Draft, dual unreconciled traceability systems, no assessor | Critical | `RTM_GAP_REPORT.md`; `ci.yml:89-107`; `traceability_gate.rs` | Medium | L | Documentation outruns evidence — the classic audit failure mode |
| G6 | **No secure/measured boot** — trust chain starts at runtime PCR16; TPM feature off by default (default build returns Trusted unconditionally); no binary measurement | High | `startup_sentinel.rs:30`; `tpm.rs` feature-gated | High | L | Attestation attests an unmeasured platform |
| G7 | **Key lifecycle prototype** — governor signing key in-process (no HSM/TPM binding), no rotation; single shared admin bearer token, no RBAC, no mTLS (plaintext bind, header identity) | High | `kirra-release-token`; `require_admin_token`; `verifier.rs:170` | Medium | L | Single credential compromises the whole admin+actuator surface |
| G8 | **Non-RT inference/service scheduling** — parko loop has no deadline/WCET budget/preemption/per-tick timeout (hang stalls drain); verifier safety loops share default work-stealing tokio pool | High | parko scheduler advisory 150 ms flag; `#[tokio::main]` default | Medium | M | Unbounded latency on paths that feed posture |
| G9 | **Scenario program 2–3 orders under-scale** — ~7 doer-eval scenarios; no KPI-gated regression (unsafe_miss_rate/admissibility thresholds) despite excellent metric design | High | `kirra-doer-eval`; `semantic_eval.rs` | Medium | L→XL | Validation claim rests on unit tests |
| G10 | **Persistence integrity gaps** — audit chain `synchronous=NORMAL` (incident rows not fsync'd); posture generation persistence is commented-out dead code while monotonicity is claimed; SQLite is simultaneously safety store, audit store, key store, and HA arbiter | High | `verifier_store/mod.rs:884`; `posture_engine_v2.rs:139-161` | Low–Med | S–M | Least-durable write is the incident record; federation ordering claim is false |
| G11 | **Supply chain attestation absent** — no SBOM, no cargo-deny/vet, no artifact signing (SHA256SUMS only), no SLSA | High | `ci.yml`; `release.yml:158` | Low | S | R155/21434 non-starter; procurement blocker |
| G12 | **Pedestrian RSS absent while the courier persona operates in pedestrian space** | High | `validation.rs` structured-road subset; mick sidewalk intents exist | Medium | M | ODD/safety-model mismatch for the flagship persona |
| G13 | Map lifecycle (versioning/updates/localization integration/UTM) | Medium | ADR-0023 consequences | High | XL | Blocks geographic scale; possibly partner-supplied |
| G14 | Transport security not executed (Zenoh TLS/QUIC disabled; UDP prototype; app-layer Ed25519 only, no rate limiting) | Medium | fleet-transport README | Medium | M | Honest spike, but fleet plane unhardened |
| G15 | Learning loop — capture decision CONFIRMED (hybrid §3, 2026-07-04) and BUILT: fire-and-forget emit at both seams captures ALL arms (not Deny-only), default-OFF; the isolated collector exists. Remaining open-circuit is the downstream train/sim/release stages (WS-6) | Medium | `LEARNING_LOOP_ARCHITECTURE.md` §3, §9; `COLLECTOR_DESIGN.md` | Medium | L | Capture + collector done; the flywheel's training/validation/release half is unbuilt |
| G16 | Model integrity — SHA fingerprint computed but never verified against an allow-list; no accelerator watchdog; capabilities hardcoded | Medium | parko backends | Low | S–M | ML artifact substitution undetected |
| G17 | Observability stack — no /metrics on the verifier binary (aggregator exists, unmounted), no Prometheus/Grafana/OTel | Medium | `metrics.rs:34` vs `build_app` | Low | S | Fleet safety events invisible to standard ops tooling |
| G18 | Config governance — two paradigms (typed JSON vs env sprawl), no schema, no versioning | Medium | `config.rs` vs `ENVIRONMENT.md` | Low | M | Misconfiguration surface in a fail-closed system |
| G19 | SOTIF trigger catalog unsystematic (occlusion/water/divergence modeled; weather, blooming, spray, low-sun, degraded markings, adversarial patches not) | Medium | subsystem-3 review | Medium | L | 21448 argument incomplete |
| G20 | SDK/DX — 2 Python examples only, 4-function FFI stub header, no semver/CHANGELOG/deprecation/MSRV policy, angular reconcile cap = `f64::INFINITY` | Low | `examples/`, `include/kirra.h` | Low | S–M | Adoption friction; one real bound gap (angular cap) |

---

## 5. Safety Assessment

**What is genuinely strong [REPO]:**
- **Fail-closed is a culture, not a feature.** Every reviewed layer defends the NaN/absent/stale/overflow paths: RSS primitives fail to 1e6 m distance / 0 speed with the `NaN.max(0.0)==0.0` trap explicitly documented; `AgentScene::Absent` (no data = UNSAFE) is distinguished from `KnownEmpty`; the B3 guard derates when a mode-set yields zero evaluable windows; staleness fails closed on backward clock steps; startup invariants gate before `bind()`.
- **The checker is the sole authority and the architecture enforces it** — collector mechanically cannot reach the verdict path (dependency-graph-enforced); the LLM cannot reach the actuator (typed intent + HOLD on every failure); release tokens sign *enforced* bytes, not proposals.
- **Diversity is real** — the dual governor re-derives the envelope via different algebra with zero shared enforcement code, 10k-case no-false-divergence fuzz, and proven fault detection.
- **Honesty artifacts** — deviations from IEEE 2846 are documented as safety-case decisions (#408); RTM_GAP_REPORT admits prior claims were "aspirational"; WCET results carry INDICATIVE-KVM provenance and a cert gate that refuses host numbers.

**What a safety assessor would flag:**
1. The guarantees are **conditional on Assumptions of Use no component meets** (G1) — the strongest possible checker bound is only as good as the perception feeding it, and the formal spec says so itself.
2. **The live path defaults are not uniformly safe**: parko's `rss_state` defaulting `safe:true` while the real evaluator is unwired (G2) is exactly the class of gap the project's own culture exists to prevent.
3. Evidence execution lags: MC/DC not achieved, ~20% test-level RTM, everything Draft (G5).
4. Two enforcement-relevant claims in the docs are currently false in code: cross-restart generation monotonicity (dead code) and per-commit audit durability (NORMAL sync) (G10).
5. Pedestrian-space ODD without a pedestrian RSS bound (G12).

---

## 6. Runtime Assessment

- **Kernel:** decide_cycle 0.69 µs p50 / 1.1 µs p99.9 under FIFO on QNX 8.0 (KVM) — ~1% of the 100 µs verdict budget; seqlock read 60 ns host / 97 ns QNX; full two-process doer→checker→release chain 5/5 PASS. Crypto dominates the actuation leg (Ed25519 ≈ 91 µs p50) — correctly split off by ADR-0031 with a designed MAC escalation. [REPO, measured this program]
- **Honest caveats:** all numbers are VM/host-indicative — zero certified-hardware evidence; deployment ISA never measured; hypervisor fault campaign specified but blocked on QNX Hypervisor licensing. [REPO]
- **Services:** everything off the kernel path is tokio-async, soft-real-time, heap-allocating, work-stealing — acceptable *because* the RT claim is confined to the extracted kernel, but the parko inference loop lacks even soft deadlines (G8).
- **Verdict:** the runtime architecture is aimed at the right target (partitioned kernel + measured evidence + clock-domain discipline); what's missing is the *certified* leg: Ferrocene build, target hardware, hypervisor campaign, and a pipeline that produces the artifact (G4).

---

## 7. SDK Assessment

The repo is named an SDK; today it is a **reference implementation, not an SDK**. Integration surfaces that exist: HTTP API (well-gated), C FFI (4-function stub), ROS 2 adapter (real, feature-gated), typed-intent seam (well-designed). Missing: Rust API examples, C example, semver/deprecation/MSRV policy, CHANGELOG, published rustdoc, versioned config schema, and a supported-integration matrix (G20). The strongest "SDK" property is the one that matters most and is hardest to copy: the **integration contract is safety-shaped** (frozen ABI layout, fail-closed decode, release tokens) rather than convenience-shaped.

---

## 8. Production Readiness Score

Scored against a production ADAS bar (10 = shippable at fleet scale). [REPO-derived]

| Dimension | Score | One-line justification |
|---|---|---|
| Safety architecture & design | **8/10** | Fail-closed spine, diversity, RTA structure — genuinely strong |
| Safety evidence execution | **3/10** | ~20% test-level RTM, MC/DC unachieved, all Draft, no assessor |
| Runtime determinism (kernel) | **6/10** | Sub-µs measured, methodology rigorous; evidence VM-only, no certified toolchain |
| Perception / world model | **2/10** | Single lidar, mock detector, no fusion/tracking/localization |
| Planning | **3/10** | Geometric works; learned/LLM are (deliberate) demonstrations |
| Fleet control plane | **6/10** | Mature beta; SQLite ceiling, single-token authz, no TLS in-crate |
| Deployment & fleet lifecycle | **2/10** | No OTA/A-B/rollback/secure boot; single-replica Helm |
| CI/CD & supply chain | **6/10** | 13 disciplined jobs, WCET gate, gating audit; no SBOM/deny/vet/signing |
| Security & identity | **5/10** | Excellent crypto primitives; thin authz, key lifecycle prototype |
| SDK / developer experience | **3/10** | 2 examples, stub FFI, no stability policy |
| Documentation & governance | **9/10** | 36 ADRs, 59 safety docs, candid gap reports — best-in-class for size |
| **Overall** | **≈4.5/10 · TRL 4–5** | Advanced prototype with production-grade core and pilot-grade periphery |

---

## 9. ISO 26262 Readiness

| Part | State [REPO] |
|---|---|
| Part 2 (management) | Governance artifacts unusually strong (ADR/AoU registers, reopening conditions); no safety manager role, no assessor engagement, no confirmation measures |
| Part 3 (concept) | HARA, safety goals, ASIL decomposition (D(D) checker + QM(D) doer), safe-state spec — **structurally complete, all Draft** |
| Part 4/6 (system/software) | Design evidence good (SAFETY/REQ/TEST tags, MC/DC pair tests on primitives); *achieved* MC/DC absent; dual traceability unreconciled; test-level RTM ~20% |
| Part 5 (hardware) | Out of scope / absent (no target hardware program yet) |
| Part 8 (supporting) | Config mgmt partial (no schema/versioning); tool qualification absent — Ferrocene is targeted, not used; the prototype runs QM Rust on QNX 8.0 while the cert target says QNX 7.1 + Ferrocene |
| 21448 (SOTIF) | Several triggering conditions modeled with real mechanisms (occlusion, water, divergence); catalog unsystematic; quantitative numbers DRAFT/VALIDATION-PENDING |
| 21434 / R155-156 | TARA exists; residual gaps (RBAC, rate limiting) self-identified; no SBOM/OTA compliance story yet |

**Readiness statement:** concept phase substantially complete; product development in progress; **not audit-ready**. The distinguishing (and bankable) trait is that the project *already tracks its own conformance gaps honestly* — an assessor's first-pass findings largely already exist as `RTM_GAP_REPORT.md` and the AoU register.

---

## 10. Competitive Differentiators (vs top-tier production ADAS vendors)

1. **Category:** vendor-neutral *external* runtime-assurance governor (ASTM F3269-shaped) that bounds ANY doer. The incumbents embed their monitors in their own stacks and do not sell this layer. [REPO / VENDOR-PUBLIC]
2. **LLM/agentic planners as first-class untrusted doers** — typed-intent seam, constrained decode, every failure → HOLD, model mechanically cannot reach actuation. No public equivalent from the incumbent vendors. [REPO]
3. **Heterogeneous-fleet legitimacy plane** — attestation, posture DAG, tamper-evident audit, and industrial protocol enforcement (Modbus/DNP3/CANopen/CIP) spanning robots/AGVs/edge, not just cars. Outside the incumbents’ addressed market. [REPO]
4. **Implementation-diverse dual governor** with divergence escalation and proven fault detection — diversity at the enforcement layer, cheaper than sensor-level True Redundancy and composable with it. [REPO]
5. **Radical evidence transparency** — the safety case, its gaps, and its assumptions live in-repo with candid status. The incumbents’ evidence is closed; for customers who must *own* their safety case (UL 4600 operators, industrial integrators), inspectability is the product. [REPO]
6. **Frozen-ABI cross-partition contract channel** with measured ns-class reads and a WCET-honesty discipline (INDICATIVE vs CERTIFIED gating) — a certifiable integration seam offered as a component. [REPO]
7. **Doer-eval admissibility metric** — scoring planners by what the *real* checker admits is a novel, safety-native ML evaluation primitive. [REPO]

---

## 11. Prioritized Roadmap

**Next 2 weeks (wiring + truth-telling; all S-effort):**
1. Wire parko `evaluate_scene`/occlusion/water into `run_pipeline_tick`; kill the `safe:true` default (G2).
2. Restore generation persistence from dead code or retract the monotonicity claim from docs (G10).
3. Fsync (synchronous=FULL or explicit checkpoint) on incident-class audit writes (G10).
4. Add SBOM (cargo-cyclonedx) + cargo-deny + cosign signing to release (G11).
5. Per-tick inference deadline/timeout in parko; angular MRC ceiling to replace `f64::INFINITY` (G8/G20).
6. Mount `/metrics` on the verifier binary — the aggregator already exists (G17).
7. CHANGELOG + semver policy stub (G20).

**1–2 months (evidence + identity):**
8. QNX cross-build CI job producing the governor artifact per release — recipe → pipeline (G4).
9. Scenario-KPI regression gate: threshold `unsafe_miss_rate` / admissibility / `hazard_recall` in CI; grow corpus to low hundreds (G9).
10. Pedestrian RSS primitive for the courier ODD (G12).
11. TPM-bind the governor release-token key (tpm.rs exists at fleet layer); admin-token → per-principal tokens; TLS on the verifier or mandated mesh with enforcement check (G7).
12. ~~Close the learning-loop capture decision; extend verdict emit beyond the Deny arm~~
    **DONE** (G15): §3 confirmed hybrid (2026-07-04); the emit already captures all arms and
    the collector exists. Remaining G15 is the downstream train/sim/release loop (WS-6).
13. Model allow-list verification + accelerator watchdog in parko (G16).

**3–6 months (certification leg):**
14. Certified-hardware WCET campaign + hypervisor fault campaign (HV_FAULT_CAMPAIGN.md is executable) once the QNX Hypervisor license lands; Ferrocene toolchain build of the kernel (G4).
15. Secure/measured boot integration (dm-verity / verified boot) feeding the existing PCR16 attestation (G6).
16. Unify the dual traceability systems; drive test-level RTM to >60%; achieve MC/DC on the kernel crates and threshold-gate coverage (G5).
17. OTA/A-B design + minimal implementation for the governor partition specifically (narrow scope: one signed artifact, dual-slot, health-gated rollback) (G3).
18. Scenario corpus to thousands via CARLA/AWSIM pipelines (docs exist; automate) (G9).

**6–18 months (product scale):**
19. Perception productization behind the existing taj seam: real detector, fusion, tracking lifecycle, localization integration — OR a formal partner strategy that makes the AoU someone's contractual deliverable, verified by the divergence monitor (G1). *This is the fork-in-the-road decision: build perception or certify the checker against partner perception.*
20. Fleet update campaign engine + key management at scale (R155/R156 alignment) (G3/G7).
21. External assessor engagement (26262 + UL 4600); move safety docs Draft → Reviewed (G5).
22. Map lifecycle or map-partner integration; systematic SOTIF trigger catalog + validation program (G13/G19).

---

## 12. Top 10 ROI Improvements

| # | Improvement | Effort | Why the ratio is exceptional |
|---|---|---|---|
| 1 | Wire parko RSS into the live tick | Days | The best safety math in the repo is currently dead on the running path — a default fail-open, in a fail-closed codebase |
| 2 | Scenario-KPI CI gate | Days–2wk | The metrics (`unsafe_miss_rate`, admissibility) are already designed and safety-weighted; only the gate is missing |
| 3 | QNX artifact pipeline | 1–2wk | Converts "documented recipe" into "the certified thing exists"; unblocks all downstream cert work |
| 4 | SBOM + cargo-deny + artifact signing | Days | Cheapest possible procurement/21434 credibility; pure CI work |
| 5 | Audit fsync on incident writes | Days | The forensic record of the incident preceding a crash is currently the least durable write in the system |
| 6 | Generation persistence: fix or retract | Days | A documented safety claim that is false in code is worse than the absent feature |
| 7 | Parko per-tick deadline/timeout | Days | A hung inference call currently stalls the drain forever |
| 8 | Pedestrian RSS bound | 2–6wk | Closes the flagship-persona/safety-model mismatch |
| 9 | TPM-bind the governor signing key | 2–4wk | Machinery exists at the fleet layer; upgrades the release token from prototype to defensible |
| 10 | Mount /metrics + basic alert rules | Days | Fleet safety events (lockouts, failovers, drops) become operable with standard tooling |

---

## 13. Overall Verdict

**If a top-tier production-ADAS engineering team reviewed this repo, their conclusion would be:** *"This team has built the part we consider the hardest to get culturally right — a genuinely fail-closed, honestly-evidenced enforcement kernel — and has not built most of what we consider table stakes: perception at scale, fleet lifecycle, and executed certification evidence."* Both halves of that sentence are correct, and the second half is largely **deliberate sequencing, not blindness** — the repo's own gap reports already name most of what this analysis found, which is itself the strongest maturity signal here.

**Strategic read:** Kirra should not try to out-build the full-stack incumbents at their own game. The defensible position is the **certifiable runtime-assurance layer for everyone who is not a full-stack incumbent** — AV programs with their own perception, industrial robot fleets, and LLM/agentic robotics, all of whom need an external checker with an inspectable safety case. On that path, the critical-path gaps are not perception (partner-supplied, AoU-contracted, divergence-monitored) but: the live wiring gap (fix this week), the certified artifact pipeline, evidence execution to audit grade, key/boot trust chain, and a minimal OTA story for the governor partition itself.

**Bottom line: TRL 4–5. Production-grade core, pilot-grade periphery, best-in-class honesty. The fastest route to "production-grade" runs through pipelines and evidence, not new invention — roughly half of the top-ROI items are days of work on machinery that already exists.**
