# ADAS-Benchmark Gap Closure & Competitive Due-Diligence Review

**Date:** 2026-07-08 (post PR #877)
**Baseline:** `docs/analysis/ADAS_BENCHMARK_GAP_ANALYSIS.md` (MGA G-1…G-22, 2026-07-06) + `ADAS_BENCHMARK_GAP_CLOSURE_PLAN.md` (WP-01…WP-24)
**Method:** four independent code-verification passes (baseline extraction; runtime-wiring verification; CI/test/formal-methods audit; safety-architecture depth audit). **No conclusion rests on status documents alone** — every verdict is anchored to code, tests, or CI config. Where the status doc and the code disagreed, the code won; this review **downgrades several of the plan's own "DONE" claims** (see G-9, G-11, G-15, G-21).

---

## 1. Executive Summary

**Overall implementation maturity: 6.5 / 10** — a genuinely engineered fail-closed safety governor with production-grade *cores* and an unusually rigorous verification culture, but with a recurring, honest pattern: **pure cores are excellent; live wiring, target silicon, real perception, and certification are deferred.**

**Overall competitive position:** Kirra is **not a Tier-1 ADAS-benchmark competitor today and should not be evaluated as one** — the benchmark vendor ships a certified perception-to-actuation stack on its own silicon with a fielded fleet. Kirra is a different *kind* of asset: an open, auditable, cryptographically-anchored **runtime safety-checker layer** (doer-checker/simplex + RSS + fleet trust) that in three specific dimensions already exceeds anything the benchmark vendor exposes: (1) open, testable, traceable RSS; (2) cryptographic runtime/fleet trust as a first-class safety property; (3) CI-gated statistical safety regression bounds. The winning strategy is to be the *checker the benchmark vendor doesn't sell*, not to chase the perception stack it already won.

**Confidence:** **High** on all in-repo conclusions (everything was code-verified; nothing had to be marked ⚪ Cannot Verify — a notable finding in itself). **Medium** on benchmark-vendor comparisons (public information only).

**Major findings:**

1. **The status docs are honest but optimistic at the margins.** Every claimed artifact exists in code — no stub masquerades as an implementation anywhere (verified). But six "DONE" items are **pure cores with zero live consumers**: the WP-19 lease (the live promotion loop still runs the legacy 10 s heartbeat path — the ≤5 s failover *product property* is undelivered), WP-22 EVT/pWCET (zero callers; the `evt` feature is enabled by no dependent), WP-24's `model_lineage`/`ood`/`model_targets` (defined + tested, never invoked), WP-18's `EpochFence`/`NodeStore` traits (never consumed generically) and the Postgres migration backend (mock executor only — **no real PG driver exists**).
2. **Weighted gap closure ≈ 45 % of the full benchmark gap set, ≈ 70 % of the in-repo software scope** (method in §4). The plan's "17 of 22 closable" framing is fair; its per-WP "DONE" framing overstates ~5 rows.
3. **Three work packages were never started**: WP-11 (curved-geometry RSS), **WP-21 (the in-line SHM enforcement path — the software half of headline Critical gap G-1)**, WP-21b (zero-copy production adoption).
4. **The verification culture is the strongest asset**: ~2,980 tests; 22 CI jobs incl. mutation testing scoped to the checker diff, 4 loom concurrency models, 4 fuzz targets, 14 proptest files, per-PR + nightly statistical KPI gates with Wilson/Clopper-Pearson bounds, supply-chain + action-pinning gates, and integration drills using *real* state machines (SIGKILL power-loss, two-store split-brain fence, two-node OTA rollout). The negative-control methodology (`governor_closes_loop_proof` runs governor-off controls) is assessor-grade thinking.
5. **The certification story is structurally prepared and materially incomplete**: MC/DC is aspirational (the CI job name says "MC/DC pending — issue #65"; it falls back to branch coverage); all WCET evidence is self-labeled INDICATIVE; safety constants (`RSS_LAT_BRAKE_FRACTION`, lateral μ, redundancy tolerances) are `VALIDATION-PENDING`; no independent assessment (explicit carve-out).
6. **No deductive formal methods** (no Kani/Creusot/Prusti/Miri) — conspicuous *because* the codebase is unusually well-shaped for them (pure, `no_std`, panic-free cores). This is the highest-leverage hidden opportunity (§8).

---

## 2. Original Gaps — Verified Status

Statuses: ✅ Fully Closed · 🟡 Mostly Closed · 🟠 Partially Closed · 🔴 Open · ⚪ Cannot Verify. Priority = MGA severity.

| Gap | Description | Priority | Verified Status | Key evidence | Conf. |
|---|---|---|---|---|---|
| **G-1** | Hard-real-time in-line enforcement path | Critical | 🟠 Partial | QNX judge harness real (`tools/qnx-rtm-harness/`, on-QNX-VM results, `no_std` panic=abort judge); frozen `#[repr(C)]` contract + seqlock real (`crates/kirra-contract-channel`). **WP-21 (the in-line SHM path) has no completion record and no code**; target validation external | High |
| **G-2** | Hypervisor partitioning + FFI evidence | Critical | 🔴 Open | External by plan. Software readiness exists (HVCHAN contract, `kirra-hv-carrier` POSIX-SHM stand-in with cross-process tests) | High |
| **G-3** | Certified WCET on target | Critical | 🟠 Partial | `src/wcet_gate.rs` CI gate real (p99.9, host-INDICATIVE rule enforced in code); EVT/MBPTA core exists (`crates/kirra-timing/src/evt.rs`) but has **zero consumers**; target-silicon data external | High |
| **G-4** | Real perception / certified perception contract | Critical | 🟠 Partial | Checker contract + taj Phase-A/B pipeline + pedestrian-RSS "armed but unfed"; all perception inputs are mock/seam-fed; sensors external | High |
| **G-5** | Independent assessment + Part-2 mgmt | Critical | 🔴 Open | Explicit carve-out; WP-06 doc-consistency only | High |
| **G-6** | Tool qual + MC/DC + mutation | High | 🟡 Mostly | Mutation gate real & CI-wired (`cargo-mutants --in-diff` on `kirra-trajectory`); fuzz 4 targets @60 s; **MC/DC pending** (job name says so; falls back to `--branch`); Ferrocene external | High |
| **G-7** | Uptane OTA + key lifecycle | High | 🟡 Mostly | Genuine four-role Uptane w/ rotation + rollback/freeze/chain checks (`kirra-release-token/src/uptane.rs`); installer `SignatureRequired` fail-closed wired; durable metadata store / on-device Uptane client / HSM custody deferred | High |
| **G-8** | Secure/measured boot | High | 🟠 Partial | Real bounds-checked TPMS_ATTEST parser + PCR16 policy live in `/attestation/verify`; `kirra-ota-ctl enroll` real. Quote signature is an Ed25519 stand-in (disclosed in-source); device secure-boot external | High |
| **G-9** | Fleet-scale store | High | 🟠 Partial | Migration framework live in `VerifierStore::new` (fail-closed future refusal). **But**: the PG backend is engine-over-mock (only impl is `#[cfg(test)] MockPg`); `EpochFence`/`NodeStore` traits never consumed generically. **No fleet-scale backend exists**; the single-site SQLite ceiling stands | High |
| **G-10** | Rate limit / backpressure / tracing / histograms | High | ✅ Fully (sw) | Two-pool concurrency shed (429+Retry-After) live in `build_app`; request-ID + latency-histogram middleware live; ingress token bucket in fleet-transport; `/metrics` posture-exempt | High |
| **G-11** | Execution manager / deterministic scheduling | High | 🟠 Partial | Manifest boot gate live (aborts on cycle/dup); **3 of 7** loops registry-dispatched; deadline metrics wired but **1 of 7** tasks records; `SchedulingClass` is **inert data** — zero `sched_*` syscalls (correctly flagged blocked-by-tokio in-source) | High |
| **G-12** | RSS completeness | High | 🟡 Mostly | Formula-correct RSS core (`parko-core/src/rss.rs`); §4 conjunction + cut-in refinement; occlusion Rule 4; multi-modal predictive; VRU reachable-set. **Curved geometry (WP-11) never built; key constants `VALIDATION-PENDING`** | High |
| **G-13** | Seqlock ordering proof + loom | High | ✅ Fully | 4 real loom models incl. seqlock torn-read + sticky-lockout race (CI job); 20k-iteration torn-read test | High |
| **G-14** | Diverse-governor divergence→posture | Medium | 🟡 Mostly | Genuinely diverse second governor, leaky-bucket accumulator + hysteresis, 10k-case never-falsely-diverge proptest; live node wiring remaining; honest common-mode disclosure | High |
| **G-15** | ML lineage / OOD / HW CI | Medium | 🟠 Partial | `model_integrity` allow-list **live on the real ORT load path** (`parko-onnx/session_core.rs:56`); `model_lineage`, `ood` (PSI), `model_targets` (signed manifest) are **pure cores with zero live consumers**; Jetson runner external | High |
| **G-16** | Statistical validation campaign | Medium | 🟡 Mostly | Per-PR MC campaign + nightly 10⁴–10⁵ corpus **actually run in CI**; Wilson/CP interval gating; negative-control fault families. Corpus synthetic/seam-fed (seam-pinned rows labeled tautological); CARLA external | High |
| **G-17** | Unified validated configuration | Medium | 🟡 Mostly | `KIRRA_ENV_KEYS` registry + unknown-var WARN + `EffectiveConfigDigest` audit **live at boot**; per-module reads not yet routed through `EffectiveConfig` (Slice B) | High |
| **G-18** | Zero-copy transport adoption | Medium | 🟠 Partial | iceoryx2 spike real with real Orin NX results under isolcpus/FIFO; **WP-21b never started** | High |
| **G-19** | Cert/PKI lifecycle | Medium | 🟡 Mostly | Expiry fail-close live in the auth path; hourly census monitor spawned; Prometheus census; re-pin renewal; revocation routes. CRL-at-TLS-callback deferred | High |
| **G-20** | Schema migration framework | Low | ✅ Fully | Live in `VerifierStore::new`; fail-closed future-DB refusal; atomic step+stamp; dialect-agnostic engine; integration drill | High |
| **G-21** | Faster failover / warm standby | Low | 🟠 Partial | Lease timing model + durable lease ops + deterministic drill all real; **live promotion loop still uses the legacy 10 s heartbeat** — the ≤5 s property is not delivered | High |
| **G-22** | AUTOSAR Adaptive interop | Low | 🔴 Open | Deferred by plan, customer-driven | High |

⚪ Cannot Verify: **none** — every claimed artifact was findable and inspectable.

---

## 3. Implementation Quality Grades

| Gap / area | Grade | Rationale |
|---|---|---|
| G-13 seqlock + loom | **A** | Real memory-ordering models, documented non-vacuity, CI-run |
| G-10 control-plane hardening | **A−** | Live, isolated pools, correct 429-vs-503 semantics, probe exemption |
| G-20 migrations | **A−** | Fail-closed both directions, atomic step+stamp; docked for the unconsumed PG seam |
| G-19 cert lifecycle | **A−** | Fail-close at auth is the right place; monitor + metrics + renewal-in-place |
| G-7 Uptane | **B+** | Crypto core A-grade (role separation, rotation, `verify_strict`, sealed `VerifiedUpdate`); orchestration missing |
| G-12 RSS | **B+** | The math is the best part of the codebase; docked hard for `VALIDATION-PENDING` constants and missing curved geometry — the *numbers* aren't certified even though the *algebra* is right |
| G-16 statistical gate | **B+** | Statistically honest (CI bounds, negative controls, seed-deterministic); synthetic corpus honestly labeled |
| G-17 config spine | **B+** | Registry + digest + drift audit live; read-path unification still Slice B |
| G-14 diverse governor | **B** | Genuinely diverse implementation + accumulator; live wiring pending |
| G-6 verification tooling | **B** | Mutation real but narrowly scoped; fuzz smoke-depth; MC/DC pending |
| G-11 execution manager | **B−** | Boot gate + drift-refusing dispatch well built; 3/7 adoption, 1/7 deadline recording, inert SchedulingClass |
| G-15 ML integrity | **B−** | The live piece (allow-list on ORT load) is correct; the rest is shelf inventory |
| G-8 measured boot (sw) | **B−** | Real quote *verification* engine; Ed25519 stand-in honestly disclosed |
| G-3 EVT/pWCET | **C+** | Good statistics; zero consumers — quality without integration |
| G-21 lease | **C+** | Invariant work excellent; product property undelivered |
| G-9 fleet store | **C+** | Seams clean; "fleet-scale backend: Yes" was the plan's biggest overstatement |
| G-1 / G-18 platform lane | **C** | Real harnesses with real on-target results; the in-line path and production zero-copy don't exist |

---

## 4. Gap Closure Statistics

| Status | Count | Gaps |
|---|---|---|
| ✅ Fully closed | **3** | G-10, G-13, G-20 |
| 🟡 Mostly closed | **7** | G-6, G-7, G-12, G-14, G-16, G-17, G-19 |
| 🟠 Partially closed | **9** | G-1, G-3, G-4, G-8, G-9, G-11, G-15, G-18, G-21 |
| 🔴 Open | **3** | G-2, G-5, G-22 |
| ⚪ Cannot verify | **0** | — |

**Weighted completion** (Critical = 5, High = 3, Medium = 2, Low = 1; per-gap fractional completion from the evidence above):

- **Full benchmark gap set: ≈ 45 %** (28.6 / 64 weighted points). The Critical tier drags it down: the missing halves are perception, silicon, hypervisor, and assessment.
- **In-repo software scope only: ≈ 70 %.** The delta from the plan's own ~95 % claim is the unstarted WP-11/21/21b and the unwired pure cores.

By item count the plan looks nearly finished; by engineering weight the *hard half* — the half that makes it a fieldable competitor — is what remains. The sequencing (cores-before-wiring) is correct engineering; the status vocabulary should read "CORE DONE," not "DONE."

---

## 5. Remaining High-Value Work — Ranked by ROI

| # | Work | Category | Effort | Risk | Rationale | Rec. |
|---|---|---|---|---|---|---|
| 1 | **WP-21: in-line SHM enforcement path** | Critical | L | Med | The single biggest gap between "governor proxy" and "governor". Every prerequisite core exists and is tested — integration, not invention. Unblocks G-1's software half and gives G-18 a consumer | **Do next** |
| 2 | **Live lease flip (WP-19 final)** behind an env gate + a scripted 2-process failover drill | Important | M | Med-High | Converts a proven model into the actual ≤5 s failover property; epoch fence stays as backstop | **Do** — only with the live drill |
| 3 | **Wire the three orphan ML cores** (lineage → ORT `load_model` rollback; OOD fed per-tick confidences; node sets `KIRRA_MODEL_ALLOWLIST` from verified `model_targets`) | Important | M | Low | Three finished, tested cores currently produce zero runtime value; highest value-per-line in the backlog | **Do** |
| 4 | **True MC/DC** (resolve issue #65; make `--mcdc` real and gating) | Important (cert) | M | Low | The coverage job's own name admits the gap; non-negotiable for the ASIL story | **Do** |
| 5 | **RSS constant validation + curved geometry (WP-11)** | Important | M–L | Med | The algebra is done; the *numbers* are placeholders. Needs a human safety engineer in the loop | **Do** (partially external) |
| 6 | **One real Postgres driver adapter + PG service container in CI** | Important | M | Low | Converts G-9 from "seams" to "second backend, proven" | **Do, then stop** (see §6) |
| 7 | **Consume EVT in `kirra-wcet-bench`** (emit pWCET + diagnostics in the bench report) | Nice | S | Low | Closes WP-22's loop for a day's work | **Do** (small) |
| 8 | **G-11 completion**: registry-drive remaining loops; deadline-record them; supervisor escalation on sustained miss | Nice | M | Med | The deadline→escalation link has real safety semantics | Defer behind 1–6 |
| 9 | **WP-21b: feature-gated iceoryx2 production path** | Nice | M–L | Med | Needs WP-21 as its consumer; sequence after #1 | Defer until #1 |
| 10 | **Config Slice B** (route module env reads through `EffectiveConfig`) | Nice | M | Low | Real misconfiguration-prevention value, no urgency | Defer |

External-track work stays external (correctly): hypervisor procurement + interference campaign (G-2), target-silicon WCET (G-3), sensors/D1 (G-4), assessment (G-5), Ferrocene (G-6), device secure-boot enrollment (G-8), Jetson CI runner (G-15), CARLA host (G-16).

---

## 6. Work That Should Be Removed or Retired

| Item | Recommendation | Rationale |
|---|---|---|
| **`SchedulingClass` syscalls under tokio** | **Remove from roadmap; re-home** | Correctly diagnosed in-source as the wrong layer: tokio tasks multiplex over shared workers. The RT intent belongs to dedicated OS threads in the QNX/EPIC-#270 lane. Keep the enum as declarative metadata |
| **Whole-store `VerifierStorage` trait over all ~130 methods** | **Cancel as a goal; extract on demand** | Two domains proved the pattern. Rule: a new domain trait may only be extracted together with a consumer or the live-PG milestone |
| **G-22 AUTOSAR interop** | **Keep deferred; remove from active plan** | Zero pull signal; revisit only with a signed OEM requirement |
| **CRL-file at the TLS callback** | **Defer indefinitely** | Revocation already works; a second revocation path is maintenance for marginal value |
| **The ~3,900-file `fmt` lane debt** | **Do once mechanically, or delete the ambition** | A permanently-deferred formatting gate is worse than none |
| **Duplicate-named power-loss drills** | Keep both, **rename** | They test different things (SIGKILL-prefix vs abort-durability) but the names invite deletion-by-confusion |
| **Status vocabulary** | Amend | "DONE" → "CORE DONE / WIRED DONE"; the code is honest — the status language is the only inflation risk |

---

## 7. Competitive Analysis

*(Confidence: Medium — benchmark-vendor side from public material.)*

### Already ahead of what the benchmark vendor exposes

| Area | Evidence | Value |
|---|---|---|
| **Open, auditable, testable RSS** | Formula-correct math with per-check `SAFETY:/REQ:/TEST:` tags, §4 conjunction + cut-in refinement, occlusion Rule 4, multi-modal predictive pass, VRU reachable sets; property-tested, mutation-gated, 10k-scenario adversarial sim | High — the benchmark vendor's RSS is a paper + a black box; Kirra's is inspectable and regression-gated |
| **Cryptographic runtime & fleet trust as a safety property** | Ed25519 `verify_strict` attestation, real TPM-quote verifier, hash-chained tamper-evident audit ledger with key rotation, four-role Uptane, signed model manifests, epoch-fenced actuation | High — nothing comparable is exposed at the vendor's governor layer; Kirra's most defensible moat |
| **CI-gated statistical safety** | Per-PR Wilson/CP-bounded `unsafe_miss_rate`/`hazard_recall` gates + nightly 10⁴–10⁵ corpus; negative-control fault families | Med-High — "no safety-relevant PR merges without a statistical bound" is auditable process capability |
| **Fail-closed-by-construction discipline** | NaN-before-arithmetic, envelope-cap-first with re-clamp, decel-to-stop-and-HOLD, fenced actuation, deny-by-default; enforced by tests + panic-budget ratchet | Med — cultural + architectural, hard to retrofit |
| **Verification culture** | mutation + loom + fuzz + proptest + negative-control drills + honest self-labeling of what tests cannot prove | Med — compounding |

### The benchmark vendor remains significantly ahead

| Area | Reality gap |
|---|---|
| **Perception** | Kirra has *zero* real perception; every checker input is seam-fed. The vendor's camera stack + crowd-sourced maps embody a decade of fielded data. **Do not compete here** — consume perception via the certified-contract seam (D1) |
| **Silicon + certified timing** | Certified SoCs with real WCET vs. all-INDICATIVE timing and an unbuilt RT path (WP-21) |
| **Certification** | Certified shipping product vs. assessor-shaped scaffolding with self-flagged holes and no assessment engagement |
| **Fleet scale & field data** | Millions of vehicles vs. a single-file SQLite ceiling and no fielded deployment |
| **Maps / geometry** | Crowd-sourced HD maps vs. a Lanelet2-lite lane graph; curved-geometry RSS unbuilt |

### Leapfrog opportunities

| Opportunity | Why it's open | Difficulty | Differentiation | P(success) |
|---|---|---|---|---|
| **Formally-verified checker core** — Kani proofs on `validate_vehicle_command`, RSS distance functions, lease/fence invariants | The cores are already pure, `no_std`, panic-free — unusually Kani-tractable. A closed stack cannot expose equivalent proofs; "the checker is *proved*, not just tested" is a category jump | M | Very high | High |
| **Explainable safety verdicts** | Per-field `DenyCode` forensics + hash-chained audit + traceability tags exist; productize as "every denial is a signed, replayable, human-readable artifact" | S–M | High | High |
| **Safety-case-as-code** | RTM matrices, SOTIF trigger-coverage gate, quality ratchets already run in CI; extend to a generated, versioned safety-case bundle per release | M | High | Med-High |
| **Deterministic full-system replay** | `VirtualClock` + `ScenarioRunner` + capture schema exist; extend to record/replay of full governor sessions | M | Med-High | Med |
| **The open checker for *other people's* doers (incl. LLM planners)** | The LLM-intent seam + doer-checker type system already admit an untrusted LLM doer; "certifiable guardrail for AI drivers" is a market an integrated vendor structurally can't serve | M–L | Very high | Med |

---

## 8. Hidden Opportunities (not in the original analysis)

1. **Kani/Miri CI lanes** — Kani on the pure fail-closed cores; **Miri on the two enumerated-`unsafe` boundaries** (`kirra-hv-carrier` SHM, contract-channel seqlock). Tiny, fenced unsafe surface = perfect Miri targets.
2. **Wire-or-delete rule for pure cores** — a merged "core" module must gain a non-test consumer within N PRs. This review found six violations; the pattern recurs without a forcing function.
3. **Bin decomposition** — `kirra_verifier_service.rs` is 5,263 ratcheted lines and rising ~50/PR; extract route groups. Target < 2,000.
4. **Fuzz depth** — 60 s/target is bitrot protection, not discovery; add a weekly scheduled 4–8 h fuzz workflow.
5. **Differential perception testing** — turn the True-Redundancy `cross_check` machinery around in CI to differential-test taj Phase-A vs Phase-B on shared synthetic ground truth.
6. **`EffectiveConfig` as the single config authority** (Slice B) — the actual misconfiguration-prevention story G-17 was about.
7. **Publish the checker crates** (`kirra-trajectory`, contract types) source-available — the RSS auditability advantage only converts to value if outsiders can read it.

---

## 9. Final Engineering Assessment

| Dimension | Score /10 | Justification |
|---|---|---|
| Architecture | **8** | Doer-checker enforced in the type system; clean crate boundaries; honest seams. Docked for the monolithic service bin and two-workspace friction |
| Code Quality | **8** | Idiomatic, heavily documented, panic-budgeted, ratcheted; forbid(unsafe) in safety crates |
| Safety (design) | **8** | Fail-closed everywhere, formula-correct RSS, MRC semantics informed by real incidents. Not 9+ until constants validated and the in-line path exists |
| Performance | **6** | Zero-copy proven in a spike but unadopted; 91 µs Ed25519 verdict path measured; all timing INDICATIVE |
| Scalability | **4** | Single-file SQLite ceiling is real; PG is a mock-backed seam |
| Reliability | **7** | Real HA fence, power-loss drills, supervised loops; live lease undone; no field hours |
| Observability | **7** | Metrics/histograms/request-IDs/deadline counters live and posture-exempt |
| Test Coverage | **8** | ~2,980 tests + mutation/loom/fuzz/proptest + statistical gates; MC/DC pending keeps it off 9 |
| Maintainability | **7** | Ratchets and docs excellent; 5,263-line bin, ~3,900 unformatted files, doc-drift risk (this review found "DONE" drift) |
| Developer Experience | **7** | CLAUDE.md is a model of its kind; fast CI lanes; two-workspace split costs onboarding |
| Certification Readiness | **4** | Assessor-shaped scaffolding with honest holes: MC/DC pending, WCET indicative, constants VALIDATION-PENDING, no assessment engaged |
| Competitive Differentiation | **7** | Crypto fleet trust + open RSS + verification culture are real moats; perception absence caps it |
| Innovation | **8** | LLM-doer-under-certifiable-checker, per-PR statistical safety gates, signed model manifests |
| Production Readiness | **5** | The governor *service* (industrial/fleet supervision) is deployable today; the AV stack is not fieldable without perception + silicon |
| **Overall Technical Maturity** | **6.5** | Excellent cores, honest engineering, rigorous verification — one integration wave and one external campaign from formidable |

---

## 10. Bottom Line

The plan's software scope is ~70 % genuinely done, and — more importantly — *done well* where it's done: the review found **zero fabricated implementations** and a verification culture stronger than most production ADAS suppliers'. The remaining 30 % is disproportionately the value-bearing part: **wire the six orphan cores, build WP-21, flip the lease behind a drill, make MC/DC real, validate the RSS constants** — roughly one focused engineering wave.

Strategically: stop measuring against the benchmark vendor's perception stack — that race is lost and irrelevant. The winnable position is **the independent, cryptographically-anchored, formally-checkable safety layer that sits above anyone's doer, including LLM-driven ones** — a product an integrated Tier-1 structurally cannot offer because its value *is* the closed integration. Kirra's three real moats (open RSS, runtime crypto-trust, CI-gated statistical safety) all point at that position, and the highest-leverage new investment (Kani proofs on the checker core) would make it a category of one.
