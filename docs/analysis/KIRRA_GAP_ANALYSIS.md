# Kirra — Competitive Gap Analysis

*Where Kirra leads, matches, and lags the best-in-class — and what to build to get there.*

**Date:** 2026-06-24
**Status:** Strategy / external-benchmarking document (not a safety-case artifact)
**Method:** Multi-source web research (fan-out search → source fetch → adversarial 3-vote verification of every external claim) fused with the Kirra codebase profile. Every external factual claim below carries a citation that survived independent verification. Claims that failed verification were dropped.

> **Read this first — what kind of document this is.** This is a candid market/technical positioning analysis, not a marketing sheet and not a safety case. Where Kirra is aspirational rather than certified, it says so plainly. The single most important conclusion is in §1: **Kirra is not competing with full-stack AV systems or with certified RTOSes — it is a runtime-assurance + fleet-trust layer, and its true peers are a mix of one commercial niche, an academic research field, and a set of standards.**

---

## 1. Kirra's category and its true peers

Kirra is a **distributed runtime-assurance (RTA) + fleet-trust/governance layer**. It is explicitly *not* a perception→planning→control stack; it wraps or sits beside one and treats the planner (e.g. Autoware/ROS 2) as an untrusted guest. That single architectural choice — an untrusted "advanced controller" supervised by a trusted fail-closed "safe controller" — places Kirra squarely in the **Simplex / Run-Time Assurance** lineage.

This matters because it means Kirra's competitors are **not** Waymo, NVIDIA DRIVE, or Apex.Grace *as systems*. Its real peer set splits into four groups:

| Peer group | Who | Relationship to Kirra |
|---|---|---|
| **A. Certified safety middleware / RTOS islands** | Apex.AI (Apex.Grace / Apex.OS Cert), Green Hills (INTEGRITY, µ-velOSity, µ-visor), BlackBerry QNX + TTTech Auto MotionWise, ETAS/EB/Vector AUTOSAR Adaptive, Wind River | **Not direct competitors — they are the layer *below* Kirra.** They provide the certified RTOS/partition Kirra wants to run *on*. Kirra is an application-layer governor; they are the foundation. They are also the bar Kirra is measured against on *certification*. |
| **B. Runtime-assurance / Simplex frameworks** | SOTER on ROS, DARPA Assured Autonomy RTA work, NASA RTA, classic Simplex | **Kirra's true architectural peers.** Mostly academic/research, not productized. This is the category Kirra most directly *is*. |
| **C. AI/LLM-output-to-actuator governance** | AgentSpec, RoboGuard | **Kirra's most novel and most contested frontier.** A live and fast-moving research area as of 2025 — Kirra is *not* alone here, contrary to an early hypothesis. |
| **D. Fleet trust / attestation / OTA / cyber standards** | Uptane, TPM/DICE, ISO/SAE 21434, UNECE WP.29 R155/R156 | **The standards Kirra's trust layer should map to.** Kirra has the mechanisms; it lacks the standards alignment. |

The headline framing for the owner: **Kirra is a credible, unusually complete *open-source RTA + fleet-trust governor*. Its differentiation is real (fail-closed-by-construction breadth, cryptographic fleet federation, LLM-output governance). Its gap is equally real and entirely predictable: none of it is independently certified, and the certified middleware vendors own the "trust" word in the automotive buyer's mind via paper Kirra doesn't have.**

---

## 2. Comparison matrix: Kirra vs the field

Legend: ●●● strong / ●●○ partial / ●○○ nascent or absent. "Cert" = independent functional-safety certification.

| Dimension | **Kirra** | Apex.Grace / Apex.OS (A) | Green Hills INTEGRITY (A) | QNX + MotionWise (A) | SOTER / academic RTA (B) | AgentSpec / RoboGuard (C) |
|---|---|---|---|---|---|---|
| Runtime safety model (Simplex/RTA) | ●●● governor + posture engine | ●○○ (substrate, not a monitor) | ●○○ (separation only) | ●●○ (deterministic sched.) | ●●● canonical AC/SC/decision | ●●○ (LLM-specific monitor) |
| Fail-closed enforcement in code | ●●● (envelope-first clamp, NaN/Inf reject, decel-to-stop MRC, Unknown-deny) | ●●○ | ●●○ (freedom-from-interference) | ●●○ | ●●● | ●●○ |
| Attestation / node trust | ●●● per-node Ed25519 challenge-response | ●○○ | ●●○ (secure boot ecosystem) | ●●○ (HSM/secure boot) | ○○○ | ○○○ |
| Cryptographic fleet federation | ●●● signed cross-controller reports, nonce burn, generation reconciliation | ●○○ | ○○○ | ○○○ | ○○○ | ○○○ |
| HA / redundancy / anti-split-brain | ●●● epoch-fenced promotion, watchdog escalation | ●●○ | ●●● | ●●● | ●○○ | ○○○ |
| Real-time / WCET + certified RTOS partition | ●●○ **roadmap** (QNX partition, seqlock contract, WCET *methodology* + CI gate; host numbers indicative only) | ●●● ASIL-D runtime | ●●● ASIL-D separation kernel | ●●● ASIL-D RTOS + det. sched. | ●○○ | ○○○ |
| Functional-safety **certification** | ●○○ **aspirational only — uncertified** | ●●● ASIL-D (SEooC) [1][6] | ●●● ASIL-D + SIL3 + EN50128/50657 SIL4 [4][5][7] | ●●● ASIL-D RTOS [3] | ○○○ | ○○○ |
| AI/LLM-output governance | ●●● action filter + typed envelope gate, integrated into a fielded governor | ○○○ | ○○○ | ○○○ | ○○○ | ●●● (but research-only, narrow) [11][14] |
| Cross-domain breadth (AV + robots + edge + industrial) | ●●● ROS2/DDS/Modbus/OPC-UA/CANopen | ●●○ (auto + some robotics) | ●●○ | ●○○ (auto SDV) | ●○○ (robotics) | ●●○ |
| Openness | ●●● open source | ●○○ commercial | ○○○ proprietary | ●●● papers/open | ●●● papers/open |
| Maturity / adoption | ●●○ pre-production, no field deployments cited | ●●● shipping in production programs | ●●● decades, safety-critical | ●●● production SDV | ●○○ research | ●○○ research |

---

## 3. Where Kirra LEADS / is genuinely differentiated

These are specific and defensible — not marketing.

1. **Breadth of fail-closed-by-construction enforcement in one open governor.** The academic RTA peers (SOTER) prove the *pattern* — an untrusted advanced controller defaulting to a verified safe controller on anomaly [8][9][10] — but as research artifacts. Kirra ships that pattern *plus* envelope-first clamping, NaN/Inf rejection at every entry, the "Degraded = controlled decel-to-stop-and-HOLD" MRC envelope, and an `Unknown`-command default-deny, as one coherent, testable codebase. The combination of RTA discipline + production-style invariants in open source is rare.

2. **Cryptographic fleet *federation* is essentially uncontested in this peer set.** None of the certified-middleware vendors, none of the RTA frameworks, and none of the LLM-guardrail projects offer Ed25519-signed cross-controller trust reports with durable single-use nonce burn and generation-ordered reconciliation. This is Kirra's most distinctive systems-level capability. The closest *standards* (Uptane for OTA, TPM/DICE for attestation) solve adjacent problems, not live runtime fleet-posture trust.

3. **Runtime attestation wired directly into the safety posture.** Per-node Ed25519 challenge-response gating a fleet posture engine (Nominal/Degraded/LockedOut over a DAG) is a stronger coupling of *identity* to *authority-to-act* than the middleware peers expose at the application layer. They give you secure boot and an HSM; Kirra gives you a live, revocable, posture-linked trust state.

4. **LLM-output-to-actuator governance integrated into a real governor.** This frontier is *not* unique to Kirra — AgentSpec (a DSL of triggers/predicates/enforcement for LLM agents, spanning embodied agents and autonomous driving, ~ms overhead, 100% AV-compliance in eval) [11][12][13] and RoboGuard (two-stage guardrail cutting unsafe-plan execution from >92% to <3% under jailbreak) [14][15][16] are direct conceptual peers. **But** both are research prototypes focused on the LLM-reasoning layer. Kirra's differentiation is that its action filter is the *front door of a fail-closed kinematic governor*: even a "compliant" LLM action is still clamped by the envelope. Kirra fuses the AgentSpec/RoboGuard idea with the SOTER idea — that fusion is the novel part, not the LLM gating alone.

---

## 4. Where Kirra MATCHES the field

- **The RTA architecture itself.** Kirra's untrusted-planner-as-guest + fail-closed-governor design is the textbook Simplex AC/SC/decision-module structure [8][9]. This is good company — it's the accepted academic approach — but it is *table stakes* within group B, not a differentiator there. Kirra matches; it doesn't exceed the conceptual state of the art.
- **HA / anti-split-brain.** Epoch-fenced promotion via conditional-CAS is solid and on par with what mature middleware provides. Comparable, not ahead.
- **Cross-domain transport.** ROS2 + DDS + Modbus/OPC-UA + CANopen is a genuinely broad integration surface, matching or exceeding the breadth of the commercial peers (most of which are automotive-first).

---

## 5. Where Kirra LAGS / has gaps vs best-in-class

Candid and prioritized by how much they block real-world adoption.

1. **No independent functional-safety certification — this is the dominant gap.** Apex.Grace is ISO 26262 **ASIL-D** certified as a SEooC [1]; Apex.OS Cert is **TÜV Nord** ASIL-D [6]; Green Hills INTEGRITY's separation kernel is pre-certified ASIL-D [7] and µ-velOSity carries ASIL-D + IEC 61508 SIL 3 + EN 50128/50657 SIL 4 [4]; QNX SDP 8 is a certified RTOS foundation [3]. Kirra's ISO 26262 / IEC 61508 mapping is **aspirational documentation, not a certificate.** Against safety-critical buyers, uncertified ≈ unusable for the actual safety function, regardless of code quality.

2. **No certified RTOS/partition under the governor yet.** The QNX-partition + seqlock hypervisor contract + WCET methodology is a *roadmap*, and Kirra correctly marks its host WCET numbers as indicative (not target-validated). The peers already *run on* ASIL-D separation kernels [7] with deterministic time-triggered scheduling [3]. Until Kirra's governor executes on a certified partition with target-validated WCET, its real-time safety claims are unproven where it counts.

3. **No tool qualification.** Certified offerings come with qualified toolchains (compilers, debuggers, libraries). Kirra's Rust toolchain is not qualified for safety use — a hard requirement for any ASIL claim.

4. **No SOTIF (ISO 21448) story.** Kirra governs *commands*; it does not address performance limitations / insufficiencies of the nominal function (sensor/perception limitations in the absence of faults). Best-in-class AV safety cases treat SOTIF as co-equal with 26262.

5. **No formal verification of the invariants.** Kirra's invariants are enforced and tested in code, but not machine-checked. Best-in-class RTA research increasingly pairs the safe controller with formal proofs (reachability, temporal-logic guarantees — cf. RoboGuard's temporal-logic specs [15]). Kirra's "formal-ish" should become "formal" for the core envelope/MRC logic.

6. **No redundant/diverse sensing or independent assessment.** Kirra is single-channel logic. ASIL-D typically demands redundancy/diversity and an independent safety assessor. Neither is present.

7. **No standards alignment for the trust layer.** Kirra's federation/attestation mechanisms are strong but not mapped to **ISO/SAE 21434** (cyber engineering) or **UNECE WP.29 R155/R156** (CSMS/SUMS), and its OTA/update trust is not aligned to **Uptane**. The mechanisms exist; the *conformance* doesn't.

8. **Maturity/adoption.** No cited field deployments. The commercial peers are in production programs. This is expected for an open project but is a real go-to-market gap.

---

## 6. Standards landscape — what best-in-class compliance looks like

| Standard | Scope | Best-in-class bar | Kirra today |
|---|---|---|---|
| **ISO 26262** | Automotive functional safety (E/E) | ASIL-D cert via accredited assessor (TÜV) [4][6][7] | Mapping docs only |
| **ISO 21448 (SOTIF)** | Safety of the intended function (no-fault insufficiencies) | Documented SOTIF analysis + validation | Absent |
| **IEC 61508** | Cross-industry functional safety (SIL) | SIL 3/4 cert [4] | Mapping docs only |
| **UL 4600** | Safety case for autonomous products | Goal-based safety case w/ independent review | Not started |
| **IEEE 2846 / RSS** | Formal assumptions for AV decision safety | RSS-style minimum-safe-distance model adopted/published | RSS-style checks present, not conformance-stated |
| **ISO/SAE 21434** | Automotive cybersecurity engineering | CSMS + TARA + assessment | Strong mechanisms, no conformance |
| **UNECE WP.29 R155/R156** | Type-approval CSMS/SUMS (mandatory in many markets) | Certified CSMS + SUMS | Not mapped |

Best-in-class is a *portfolio*: 26262 (faults) **+** 21448 (insufficiencies) **+** 21434/R155 (security) **+** UL 4600 (overall safety case), each independently assessed. Kirra has the *engineering* for several but the *paper* for none.

---

## 7. Prioritized, phased roadmap to best-in-class in the niche

Effort = rough order of magnitude. Impact = how much it moves Kirra toward credible/adoptable.

### Phase 0 — Credibility hardening (0–3 mo, low effort, high impact)
- **Publish the safety case as a UL-4600-style GSN argument**, explicitly stating what *is* and *is not* certified. Turns "aspirational" from a liability into honesty that buyers respect. *(Low / High)*
- **Map the trust layer to ISO/SAE 21434 + Uptane** on paper; identify deltas. Cheap, and it converts existing mechanisms into a conformance narrative. *(Low / High)*
- **Formalize the core envelope + MRC + `should_route_command` logic** (model-check the state machine / reachability of the decel-to-stop). Highest-leverage step toward "formal" vs "formal-ish." *(Med / High)*

### Phase 1 — Make the real-time safety claim true (3–9 mo, high effort, high impact)
- **Stand the governor up on a certified partition** (QNX SDP 8 or INTEGRITY) and produce **target-validated WCET** under FIFO scheduling — retire the "indicative" caveat for the hot path. This is the single biggest technical credibility unlock. *(High / High)*
- **SOTIF (ISO 21448) analysis** for the perception-degradation derate (Track-C) and corridor containment — the natural place Kirra already touches insufficiencies. *(Med / High)*

### Phase 2 — Earn the paper (9–24 mo, very high effort, decisive impact)
- **Pursue ISO 26262 SEooC certification of the governor** (the bounded, O(1), no-alloc command path is the right scope — mirror Apex.Grace's SEooC strategy [1]). *(Very High / Decisive)*
- **Tool qualification** for the Rust toolchain subset used in the certified scope. *(High / Required-for-cert)*
- **Engage an independent safety assessor** and add redundancy/diversity to the safe-controller channel. *(High / Required-for-ASIL-D)*

### Phase 3 — Extend the moat (parallel, opportunistic)
- **Productize fleet federation as the headline feature** — it's the least-contested differentiator. Publish a protocol spec; pursue interop. *(Med / High)*
- **Publish the LLM-governance + RTA fusion** (the AgentSpec/RoboGuard-meets-Simplex story) as a paper/benchmark to claim the frontier before it crowds. *(Med / Med-High)*

**Minimum to be credible end-to-end:** Phase 0 + the QNX-partition/WCET item from Phase 1. **Minimum to win in the niche:** add Phase 2 SEooC certification of the governor and lead with federation.

---

## Sources

All external claims below passed independent 3-vote adversarial verification; uncited statements are reasoning over Kirra's own profile.

- [1] Apex.AI — Apex.Grace ISO 26262 ASIL-D (SEooC). https://www.apex.ai/apexgrace
- [3] BlackBerry QNX + TTTech Auto — MotionWise Schedule for QNX SDP 8.0 (deterministic time-triggered scheduling on certified RTOS). https://www.tttech-auto.com/newsroom/blackberry-qnx-and-tttech-auto-launch-new-motionwise-scheduling-solution-qnx-8
- [4][5] Green Hills Software — µ-velOSity (TÜV Nord: ISO 26262 ASIL-D, IEC 61508 SIL 3, EN 50128/50657 SIL 4) and µ-visor hypervisor. https://www.businesswire.com/news/home/20250311164318/en/Green-Hills-Software-Expands-Its-Safety-Certified-Solutions-for-Microcontrollers-with-a-Focus-on-Automotive-and-Industrial-Markets
- [6][7] Green Hills + Apex.AI — Apex.OS Cert TÜV Nord ASIL-D; INTEGRITY separation kernel pre-certified ASIL-D. https://ghs.com/news/20220105_CES_Integrity_Apex_OS_ASIL_D_driving.html
- [8][9][10] SOTER on ROS — Simplex/RTA framework (AC + safe controller + decision module). https://arxiv.org/pdf/2008.09707
- [11][12][13] AgentSpec — runtime enforcement DSL for LLM agents (embodied + autonomous driving). https://arxiv.org/abs/2503.18666
- [14][15][16] RoboGuard — two-stage runtime guardrail for LLM-enabled robots. https://arxiv.org/abs/2503.07885

*Note on research limits:* several primary-source pages (the Tier-1 ADAS benchmark vendor RSS, IEEE 2846, NVIDIA Halos, Uptane, UNECE R155 explainers) were reached but returned no independently verifiable extracted claims in this run and are treated as background, not citations. RSS/IEEE 2846 and Uptane/21434/WP.29 specifics in §6 reflect established domain knowledge, not freshly verified quotes — re-verify before external publication.
