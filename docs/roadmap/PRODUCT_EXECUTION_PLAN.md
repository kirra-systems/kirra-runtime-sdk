# Kirra — Product Execution Plan: Gap Closure → Three Shippable Products

**Date:** 2026-07-02 · **Basis:** `docs/analysis/INDUSTRY_BENCHMARK_GAP_ANALYSIS.md` (G1–G20)
**Mandate:** full scope, no cuts, staged sequence — LLM/agentic + industrial governor first, fleet legitimacy plane second, certified AV governor third. Capacity-agnostic: the plan holds for solo-with-Claude-Code; only calendar stretches, never scope.

---

## 1. Product Ladder

| | Product | Buyer | Cert burden | Ships at |
|---|---|---|---|---|
| **P1** | **Kirra Governor** — fail-closed runtime governor + action filter for LLM/agentic and industrial robots (typed-intent seam, kinematic envelope, RSS checker, release tokens) | Robotics teams putting LLMs/learned planners on hardware; industrial integrators (Modbus/DNP3/CANopen/CIP fleets) | QM→SIL2-adjacent; no 26262 assessor required to sell | Gate B |
| **P2** | **Kirra Fleet** — legitimacy control plane (attestation, posture DAG, tamper-evident audit, federation, OTA campaign engine, console) | Fleet operators of P1-governed robots; industrial OT security buyers | Security-grade (21434-shaped), not ASIL | Gate C |
| **P3** | **Kirra Certified** — the QNX/Ferrocene ASIL-D governor partition + contract channel + release-token chain, sold to AV programs that own their perception | AV programs, Tier-1s, delivery-AV/robotaxi | ISO 26262 ASIL-D(D), UL 4600, R155/156 | Gate E |

Each product is a packaging of the same spine; P1 revenue funds and field-hardens the P2/P3 tracks. The doer-checker thesis is the through-line: **P1 sells the checker, P2 sells trust in the fleet of checkers, P3 sells the certified checker.**

---

## 2. Stage Map and Gates

```
Stage 0  "Make the code tell the truth"        ──► GATE A
Stage 1  P1 productization                     ──► GATE B  (P1 GA)
Stage 2  P2 fleet plane GA                     ──► GATE C  (P2 GA)
Stage 3  Certification leg   (parallel from Stage 1) ──► GATE D  (certified artifact + audit-ready evidence)
Stage 4  World model build-out (parallel from Stage 2) ──► GATE E′ (AoUs demonstrated in-tree)
Stage 5  P3 pilot + assessor sign-off          ──► GATE E  (P3 sellable)
```

Rough calendar (multiply ~1.5–2× if solo): Stage 0 ≈ 2 wks · Stage 1 ≈ 2–3 mo · Stage 2 ≈ +2–3 mo · Stage 3 ≈ 6–12 mo overlapped · Stage 4 ≈ 6–12 mo overlapped · Stage 5 ≈ 3–6 mo after D.

**Gate definitions (hard, checkable):**
- **GATE A** — no documented safety claim is false in code; supply-chain basics (SBOM, deny, signing) green in CI; every fail-open default eliminated.
- **GATE B** — a stranger installs P1 from release artifacts to a governed robot in <1 day using only shipped docs; every public API semver'd; security review passes (no shared-token-only surface, TLS everywhere); scenario-KPI gate enforcing in CI.
- **GATE C** — two-site fleet with OTA campaign, staged rollout + rollback demonstrated end-to-end; audit chain survives power-loss test; secure-boot-rooted attestation on at least one reference platform.
- **GATE D** — the QNX/Ferrocene governor artifact is produced by CI, WCET-certified numbers exist from target hardware under FIFO, MC/DC achieved on kernel crates, RTM unified ≥90% test-level on ASIL-D goals, external assessor engaged with evidence pack delivered.
- **GATE E** — design-partner vehicle running the certified partition; AoUs either demonstrated by in-tree perception (Stage 4) or contractually owned by partner perception and enforced by the divergence monitor; UL 4600-shaped case submitted.

---

## 3. Workstreams

### WS-0 · Truth & Wiring (Stage 0 — closes G2, G10, parts of G8/G17/G20, G11)
The gap analysis found a fail-closed codebase with a handful of fail-open or claim-vs-code defects. These go first because every later evidence claim inherits them.

| PR | Task | Done when |
|---|---|---|
| 0.1 | Wire parko `evaluate_scene`/occlusion/water/commit-zone into `run_pipeline_tick`; remove `rss_state` `safe:true` default (fail-closed default + explicit enable) | Injected unsafe scene MRCs the live tick in a test; default posture without RSS input is not-safe |
| 0.2 | Restore posture-generation persistence from dead code (`posture_engine_v2.rs:139-161`) incl. restart test; or excise the monotonicity claim from all docs | Generation survives restart in test; federation ordering claim true |
| 0.3 | Incident-class audit writes go through the `synchronous=FULL` connection (or explicit WAL checkpoint on incident); power-loss simulation test | Kill -9 after incident write → row present on reopen |
| 0.4 | Parko per-tick inference deadline (timeout → MRC, watchdog on hung backend); replace angular reconcile cap `f64::INFINITY` with `MRC_ANGULAR_CEILING` | Hung-backend test MRCs within budget; angular divergence bounded |
| 0.5 | Mount `/metrics` (existing `format_prometheus_metrics`) on the verifier binary; counters for posture transitions, lockout reasons, failover, drops | Prometheus scrape returns fleet-safety series |
| 0.6 | CI: cargo-cyclonedx SBOM per release, cargo-deny (license/ban/source), cosign signing of release artifacts + container images | Release carries SBOM + signatures; deny gate red on violation |
| 0.7 | CHANGELOG.md (backfilled from release docs), SEMVER + MSRV + deprecation policy doc, release.yml reads real changelog | Policy exists; release notes no longer fall back to "Release vX" |

### WS-1 · Identity & Key Lifecycle (Stage 1 — closes G7; feeds G6)
- Per-principal API tokens with scopes replacing single `KIRRA_ADMIN_TOKEN` (keep it as break-glass); RBAC matrix (operator / integrator / auditor / admin) enforced in `require_admin_token` successor; audit every authz decision.
- TLS on the verifier (rustls server) with an enforced no-cleartext mode default; mTLS option for fleet ingress replacing header identity.
- TPM-bind the governor release-token signing key (tpm.rs machinery exists at fleet layer); key rotation + epoch story for governor and federation keys; build the ADR-0031 Clause-D session-MAC escalation.
- **DoD:** compromise of any single credential no longer grants the full admin+actuator surface; keys never live as raw env seeds in production mode.

### WS-2 · P1 Product Surface (Stage 1 — closes G20, G16, G18, G12, parts of G15/G17)
- **SDK:** Rust quickstart + examples (governed cmd_vel loop, action-filter integration, typed-intent planner); C example against an expanded `kirra.h` (verdict struct + posture query + envelope config LANDED — `KirraVerdict` + `kirra_check_move_velocity` returns the bounded scalar + a stable `KIRRA_VERDICT_*` reason code, `kirra_posture()` returns a `KIRRA_POSTURE_*` operating-posture code, and `kirra_envelope()` returns the compiled `KirraEnvelope` (linear/angular/accel bounds), all exercised by the CI C example; still to add: release-token verify); Python client polish; published rustdoc + `deny(missing_docs)` on public crates.
- **Pedestrian RSS** primitive (VRU longitudinal/lateral bounds + crossing model) wired into `validate_trajectory_slow` — the courier/sidewalk persona is P1's flagship, this is a launch blocker, not an AV-track item.
- **Model integrity:** SHA allow-list verification at backend load (fingerprint exists, verify it); accelerator health watchdog; measured (not hardcoded) HW capabilities.
- **Config governance:** one versioned, JSON-Schema'd config surface (`KirraRuntimeConfig` absorbs the env sprawl; env vars become overrides); migration story; startup prints effective config digest to audit chain.
- **Learning loop capture:** ✅ §3 capture-point decision CONFIRMED (hybrid, 2026-07-04); the
  verdict emit already captures all arms (not just Deny) at both seams (#191/#192), default-OFF;
  the isolated collector exists (`COLLECTOR_DESIGN.md` D1–D6). Remaining: a live P1 bench
  install feeding the collector, then the train/sim/release stages (WS-6).
- **Observability pack:** Grafana dashboards + alert rules shipped in helm; console wired to live telemetry (retire demo-seed as the default path).
- **DoD = GATE B checklist** plus: examples CI-built; a fresh-machine install script test in CI.

### WS-3 · Scenario & Evidence Engine (starts Stage 1, runs forever — closes G9, G19; feeds G5)
- Phase 1 (Stage 1): CI gate thresholding `unsafe_miss_rate` / admissibility / `hazard_recall` on the existing metric harnesses; corpus to low hundreds via parameterized scenario generators (the `EvalScenario` machinery scales, the corpus doesn't).
- Phase 2 (Stage 2–3): CARLA + AWSIM automated pipelines (docs exist → jobs); nightly scenario sweep with KPI trend dashboards; corpus to thousands; scenario-DB schema with ODD/trigger tagging.
- Phase 3 (Stage 3–4): systematic ISO 21448 trigger catalog (weather, blooming, spray, low-sun, degraded markings, VRU edge cases, adversarial patches) with per-trigger scenario coverage and residual-risk ledger — trigger→evidence coverage is now machine-gated (`ci/sotif_trigger_coverage.json` via `scenario_kpi_gate`), and the UL 4600 SPI / residual-risk ledger is machine-gated + evaluable (`ci/spi_registry.json` via `kirra_verifier::spi_ledger`: every SPI's audit `event_type` must be really emitted, plus a tested rollup evaluator); adversarial-prompt eval suite for the Mick LLM seam LANDED (`crates/kirra-planner/tests/mick_adversarial_prompt.rs`: hostile-completion corpus proving the intent-vocabulary airgap + the checker backstop).
- **DoD:** no safety-relevant PR merges without the KPI gate; every SOTIF trigger row maps to scenarios or a documented AoU.

### WS-4 · Fleet Plane GA (Stage 2 — closes G3 phase 1, G6, G10 rest, G14, G17 rest)
- **OTA/A-B for the governor artifact first** (narrow, high-value): signed artifact (WS-0.6 cosign roots it), dual-slot install with health-gated automatic rollback, campaign engine v1 in the verifier (cohorts, staged rollout %, halt-on-regression using posture telemetry), R156-shaped update audit trail. Then generalize to node software.
- **Secure/measured boot** on a reference platform (Jetson Orin: UEFI Secure Boot + dm-verity rootfs) feeding the existing PCR16 attestation so the chain starts at boot, not runtime; default build no longer returns `Trusted` unconditionally — TPM feature on by default for the fleet product, explicit dev-mode opt-out.
- **Persistence past the SQLite ceiling:** WORM/append-only off-box audit shipping (the chain already hash-links; add a shipper + remote verifier); documented + tested HA topology (active/passive stays, but the shared-file SPOF gets an alternative: replicated store or litestream-class WAL shipping with failover drill in CI); DR drill automated.
- **Transport security executed:** Zenoh TLS enabled opt-in (`fleet_peer_config`/`FleetTlsConfig`; the toolchain fight is cleared — `time` 0.3.48→0.3.53, `ring` provider), per-controller key registry (#314), rate limiting on ingest; two-box lane retired or hardened.
- **DoD = GATE C checklist.**

### WS-5 · Certification Leg (Stage 3, parallel from Stage 1 — closes G4, G5; consumes WS-3)
1. **QNX cross-build CI job** producing the governor artifact per release (recipe → pipeline; `-Zbuild-std` job exists in session history, promote it). Artifact enters the cosign/SBOM chain.
2. **Ferrocene toolchain build** of the kernel crates (kirra-core/trajectory/contract-channel/hv-carrier/release-token); qualify the delta from QM Rust; pin the certified profile.
3. **Certified-hardware WCET campaign:** target board procurement, QNX-on-target FIFO runs, `KIRRA_WCET_CERTIFIED` flipped only there; hypervisor fault campaign (HV_FAULT_CAMPAIGN.md rows HV-R1/S1/S2/C1/T1) when the QNX Hypervisor license lands — chase the license as a named dependency with a deadline.
4. **Evidence unification:** merge manual RTM with the `// SAFETY:` tag gate into one generated RTM; drive ASIL-D goal test-level coverage 20%→60%→90%; achieve MC/DC on kernel crates (nightly `--mcdc` graduation) and threshold-gate coverage in CI.
5. **Assessor engagement:** select (TÜV/exida-class), deliver the evidence pack (safety case index is already GSN-shaped), move safety docs Draft→Reviewed on a burn-down; confirmation measures + safety manager role named.
- **DoD = GATE D checklist.**

### WS-6 · World Model (Stage 4, parallel from Stage 2 — closes G1, G13; no scope cut per mandate)
Build the perception→fusion→tracking→localization→map chain to the checker's rigor, behind the seams that already exist:
1. **Real detector behind the taj seam:** RGB→TensorRT detector replacing `MockSemanticDetector`; scored by the existing safety-weighted eval (`unsafe_miss_rate` first) — the eval bar predates the model by design, use it as the acceptance gate.
2. **Multi-sensor fusion:** camera+lidar (radar next) into the corridor/objects contract; occupancy/BEV world model as a second independent channel so `cross_check` finally has two *real* channels — True Redundancy demonstrated, not just implemented.
3. **Tracking lifecycle:** birth/confirm/coast/delete, IMM/Kalman, clutter-robust association, cross-channel identity for the redundancy monitor.
4. **Localization:** GNSS/INS/wheel-odom fusion + map-matching producing the ε that `frame_integrity` today consumes on faith; relocalization; the AoU (pos err <0.5 m) measured continuously and posture-fed.
5. **Map lifecycle:** map versioning/epochs, change-sets, UTM/MGRS projector (ADR-0023 follow-up), dynamic layers (closures/construction); localization-map binding.
6. **AoU closure:** recall >90% / pos <0.5 m demonstrated in-tree on the scenario corpus; until then, P3 pilots run on partner perception with AoUs contractually owned and enforced by the divergence monitor — the interim path, not a scope cut.
- **DoD = GATE E′:** every AoU in the formal spec §2 has a measuring mechanism and passing evidence.

### WS-7 · P3 Pilot (Stage 5 — consumes D + E′/partner path)
Design-partner AV program: certified partition on partner vehicle, contract channel to their planner, release tokens to their actuation, fleet plane above; R155/156 compliance for the OTA path; UL 4600 case assembled from the evidence engine; per-partner ODD scenario packs.

---

## 4. Full Gap Coverage Matrix (nothing dropped)

| Gap | Workstream · Stage | Gap | Workstream · Stage |
|---|---|---|---|
| G1 perception AoU | WS-6 · S4 (interim: partner+divergence, WS-7) | G11 supply chain | WS-0.6 · S0 |
| G2 parko wiring | WS-0.1 · S0 | G12 pedestrian RSS | WS-2 · S1 |
| G3 fleet lifecycle | WS-4 · S2 | G13 map lifecycle | WS-6 item 5 · S4 |
| G4 certified artifact | WS-5 items 1–3 · S1→S3 | G14 transport security | WS-4 · S2 |
| G5 evidence execution | WS-5.4–5 · S3 | G15 learning loop | WS-2 · S1 (capture) → WS-6 (train/sim) |
| G6 secure boot | WS-4 · S2 | G16 model integrity | WS-2 · S1 |
| G7 key lifecycle | WS-1 · S1 | G17 observability | WS-0.5 · S0 → WS-2/4 |
| G8 RT scheduling | WS-0.4 · S0 (deadline) → WS-5 (partition) | G18 config governance | WS-2 · S1 |
| G9 scenario scale | WS-3 · S1→S3 | G19 SOTIF catalog | WS-3 phase 3 · S3 |
| G10 persistence integrity | WS-0.2/0.3 · S0 → WS-4 (WORM/HA) | G20 SDK/DX | WS-0.7 · S0 → WS-2 · S1 |

---

## 5. Dependency Spine

```
WS-0 (truth) ─► everything
WS-0.6 signing ─► WS-4 OTA (signed artifacts are the root of update trust)
WS-1 keys/TLS ─► GATE B, and WS-4 fleet PKI
WS-3 KPI gate ─► GATE B, and all WS-5/WS-6 acceptance evidence
WS-5.1 QNX pipeline ─► WS-5.3 WCET campaign ─► GATE D
WS-4 secure boot ─► attestation claim in P2 marketing (don't claim before it's rooted)
WS-6 channels ─► True-Redundancy demonstration ─► GATE E′
QNX Hypervisor license = named external dependency (WS-5.3); chase now, don't block P1/P2 on it
```

---

## 6. Working Model (how this actually gets executed)

- **Cadence stays what it is:** one item at a time, new branch + PR per item, Copilot review addressed, merge, next. WS-0 is seven PRs; Stage 1 items decompose the same way at session time.
- **Evidence rules carry over unchanged:** INDICATIVE vs CERTIFIED labeling; `KIRRA_WCET_CERTIFIED` only on certified hardware; no safety claim lands in docs before the code+test lands (WS-0 exists because of two violations of exactly this).
- **Definition of done is always a gate row,** not "code merged": each PR names which gate checklist line it advances.
- **No scope cuts — but explicit interim paths:** where full scope takes quarters (perception, hypervisor), the plan names the honest interim (partner AoU + divergence monitor; KVM-indicative) and the trigger that retires it. Interim ≠ cut; every interim has a retirement condition in this document.
- **Sequencing rule when solo:** never run more than one *safety-evidence* workstream concurrently with product work; WS-3's CI gates are the mechanism that lets product PRs proceed without eroding the safety posture.

## 7. First Ten PRs (start today)

1. WS-0.1 parko live RSS wiring (kills the `safe:true` default) — the single highest-ROI change in the repo
2. WS-0.2 generation persistence restore + restart test
3. WS-0.3 incident-durable audit writes + power-loss test
4. WS-0.4 parko inference deadline + angular MRC ceiling
5. WS-0.5 verifier `/metrics`
6. WS-0.6 SBOM + cargo-deny + cosign in CI/release
7. WS-0.7 CHANGELOG + semver/MSRV/deprecation policy
8. WS-3.1 scenario-KPI CI gate on existing metrics (unsafe_miss_rate / admissibility thresholds)
9. WS-5.1 QNX cross-build CI job producing the governor artifact
10. WS-2 pedestrian RSS primitive (design doc + first implementation PR)

GATE A is expected closed at PR 7; PRs 8–10 open Stage 1 and the cert leg in parallel.
