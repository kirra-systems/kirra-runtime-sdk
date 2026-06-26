# Kirra — Industry Gap Analysis (Code-Grounded)

| Field | Value |
|---|---|
| Date | 2026-06-26 |
| Status | Independent benchmarking analysis — not a safety-case artifact |
| Method | A **file:line engineering audit** of the codebase (see `ENGINEERING_REVIEW.md`) fused with **verified 2026 competitor research** (fan-out search → source fetch → adversarial verification). |
| Relationship to existing docs | **Builds on, does not replace,** `docs/COMPETITIVE_STACK_GAP_ANALYSIS.md` (vs the AV majors) and `docs/analysis/KIRRA_GAP_ANALYSIS.md` (vs the RTA/middleware/RTOS peer set). Both are strong *market* analyses written from a codebase **profile**. This doc adds the layer neither has: where the **code reality** diverges from the **competitive self-rating** — i.e., the gaps a technical-due-diligence reviewer or TÜV assessor will actually surface. |

> **One-line finding.** Kirra's strategic positioning is sound and genuinely differentiated. The gap to leading industry software is **three-layered**: (1) **certification paper** — universally acknowledged and dominant; (2) **real-time on a certified partition** — acknowledged and on the roadmap; and (3) a **production-hardening / engineering-maturity gap that the team's own competitive docs understate**, because those docs rate several dimensions (HA, WCET, IPC determinism, RSS completeness) as "strong (●●●)" where the file:line audit shows they are not yet 24/7-grade. Layer 3 is the new, actionable content here, and it is the cheapest of the three to close.

---

## 1. Category — reconciling the two framings

The two existing docs each pick a different (correct) lens. Reconciled, Kirra competes in **two overlapping races**:

- **Race A — the assurance bound around an AV/robotics planner.** Peers: **Mobileye RSS**, **NVIDIA Safety Force Field / Halos**, the emerging **Autoware Safety Island**. Kirra's doer/checker thesis is *exactly* this pattern, and the 2026 evidence confirms all three majors are independently converging on it (the team's stack doc is right about this).
- **Race B — the certified safety substrate + governance layer.** Peers: **Apex.AI Apex.Grace**, **QNX OS for Safety**, **Green Hills INTEGRITY**, **RTI Connext Cert / Drive**, **eProsima Safe DDS**, **Eclipse iceoryx2**. These are the layer Kirra wants to *run on* and the bar it is measured against on *certification and determinism*.

A third, orthogonal category — **offline V&V tooling** (**Foretellix Foretify**, **Applied Intuition**) — is *not* a competitor: it builds the evidence/coverage for a safety case before deployment, not a runtime governor. Don't position against it.

**The honest framing:** Kirra is a **runtime-assurance (RTA/Simplex-lineage) + cryptographic fleet-trust governor**. In Race A it is conceptually peer-level and *more portable* than any major. In Race B it is an application-layer governor that is **uncertified and not yet production-hardened**, sitting above substrates that are.

---

## 2. The peer landscape — verified, 2026

| Player | What it is | Cert / RT status (verified 2026) | Relationship to Kirra |
|---|---|---|---|
| **Mobileye RSS** | Formal, technology-neutral safe-distance envelope; **separate runtime checker**, not baked into the planner | `intel/ad-rss-lib` open-source (LGPL-2.1) but **archived ~Aug 2025, unmaintained** (last v5.0.0). Basis for **IEEE 2846-2022**. | **Direct conceptual peer.** The canonical open RSS impl is now orphaned → an **opening** for a maintained, productized bound. |
| **NVIDIA SFF / Halos** | SFF = independent runtime supervisor that vetoes unsafe controls; Halos = full-stack AV-safety **framework** | **DriveOS 6.0 + Thor-X SoC: ASIL-D *conformant* (TÜV SÜD)**; ISO/SAE 21434 process cert; DRIVE AI Inspection Lab ISO 17020-accredited. **SFF's current productization is UNVERIFIED** (latest authoritative material 2019–2022). | Peer bound, **bundled to NVIDIA silicon**. Kirra's vendor-neutrality is the wedge. |
| **Autoware Safety Island** | Isolated real-time supervisor (fallback / MRM) | **Reference blueprint / PoC** (separate **Zephyr-RTOS** supervisor over ROS 2/DDS); not a shipping certified product. Core Autoware: **no ISO 26262 cert, no RSS-equivalent formal bound**. | The industry **reinventing, in the open, what Kirra already is.** Kirra is ahead on the *concept*, behind on the *certified RTOS underneath it*. |
| **Apex.AI Apex.Grace** | ROS 2-API-compatible deterministic **middleware/SDK** (not an OS) | **ISO 26262 ASIL-D SEooC (TÜV NORD)**; static pools, no runtime alloc. | Layer **below** Kirra; the certification bar. Shows the SEooC strategy Kirra should mirror for its governor. |
| **QNX OS for Safety 8.0** | Certified RTOS (+ separate certified Hypervisor) | **ASIL-D, IEC 61508 SIL 3, IEC 62304 Class C, ISO/SAE 21434** (SEooC). (Note: **QOS 8.0**, not general SDP 8.0.) | The partition Kirra's `#270` lane targets. Foundation, not competitor. |
| **RTI Connext Cert / Drive** | Safety-certified DDS | **First DDS to DO-178C/ED-12C DAL A**; **ISO 26262 ASIL-D (TÜV SÜD)**; IEC 61508 SIL 3 pathway. Connext Drive = automotive bundle. | The certified transport Kirra's hand-rolled DDS model (`dds_bridge.rs`) is *modeling* but not realizing. |
| **eProsima Safe DDS 3.0** | Safe API subset of Fast DDS (MISRA C++ 2023) | **ISO 26262 ASIL-D certified**; ROS 2-interoperable; ESA/GMV adoption. | Same as RTI — certified DDS Kirra lacks. |
| **Eclipse iceoryx2** | Rust zero-copy lock-free SHM IPC, decentralized | **Pre-1.0 (v0.9.2, Jun 2026)**; QNX 8.0 + `no_std` support added Dec 2025; **"targeting ASIL-D," not certified**. | The exact in-partition transport Kirra's ADR-0006 selects — **and a pre-1.0 dependency** (see §6). |
| **RTA / Simplex (NASA R2U2, DARPA AA, Black-Box Simplex)** | The runtime-assurance research lineage | Open-source tooling / reference architectures; **no dominant commercial RTA product.** | Kirra's true architectural lineage and its **whitespace** — the productization gap is the opportunity. |
| **AgentSpec / RoboGuard (+ ProbGuard, RoboSafe)** | Runtime guardrails for LLM/embodied agents | Research prototypes (ICSE 2026 / RA-L 2025), ms-overhead, strong jailbreak-suppression results. | Peers on the **LLM-governance frontier** — Kirra is *not alone* here, but is the only one fusing it with a fielded fail-closed kinematic governor. |

---

## 3. Unified capability matrix — claimed vs code-substantiated

Legend: ●●● strong / ●●○ partial / ●○○ nascent-absent. The **"Code-substantiated?"** column is the audit's verdict on Kirra's own self-rating — this is the column that doesn't exist in the current docs.

| Dimension | Kirra **self-rating** | **Code-substantiated?** (this audit) | Best-in-class peer | Real gap |
|---|---|---|---|---|
| Doer/checker separation | ●●● | **Yes** — checker never sees doer intent; `safe_stop` always constructible | Mobileye RSS, NVIDIA SFF | **None conceptually.** Kirra leads on portability. |
| RSS / formal bound present | ●●● | **Partial** — RSS §4 + occlusion + multi-modal are implemented, but the audit found a **predictive-RSS silent fail-open** and a **snapshot-vs-predictive motion-source split** (`validation.rs`) | ad-rss-lib (orphaned), IEEE 2846 | A Mobileye/ad-rss-literate reviewer **will** probe these. Close them to make the bound *correct*, not just present. |
| Fail-closed enforcement | ●●● | **Yes** — envelope-first clamp, per-field NaN/Inf codes, decel-to-stop MRC, `Unknown`-deny all verified | RSS, SOTER | Genuine strength. |
| Deterministic / WCET-bounded | ●●● "O(1) WCET" | **Overstated** — the per-pose kernel *is* O(1)/zero-alloc, but the **WCET gate asserts p99.9 (not max) of the kernel fn, not the deployed serde+Tower path**; host numbers indicative only | RTI Connext Cert (DAL A, target-validated), QNX QOS | Layer 2 gap. The *measured* thing is not the *deployed* thing (`wcet_gate.rs`). |
| HA / anti-split-brain | ●●● "epoch-fenced" | **Overstated** — durable epoch CAS is real, but the audit found an **actuator-path ≤2s two-writer window** (cached-epoch fence, no in-tx re-check) and a **disk-wedge non-demotion** path (`standby_monitor.rs`, `policy_layer.rs`) | QNX/INTEGRITY certified deterministic HA | Layer 3 gap — the highest-consequence one, on the actuator path specifically. |
| Runtime IPC determinism | ●●○ | **Overstated for ingress** — actuator *output* QoS is rigorous, but **all safety-critical ROS 2 *ingress* runs RMW-default `Reliable+KeepLast(10)`**, and the DDS path is a **model with no real writer + hand-rolled CDR** (`node.rs`, `dds_bridge.rs`) | RTI/eProsima certified DDS | Layer 3 gap. Config + realization, not architecture. |
| 24/7 availability hardening | (implied) | **Gap** — **65 blocking `store.with()` calls on tokio workers** + no SQLite `busy_timeout` + a DashMap iterate-then-`get` **deadlock hazard** on the safety engine | Certified middleware built for determinism | Layer 3 gap. Pure runtime hygiene. |
| Cryptographic fleet federation | ●●● | **Yes** — Ed25519 signed reports, atomic nonce burn (UNIQUE-constraint TOCTOU close), generation reconciliation all verified sound | **None** in the peer set | **Uncontested differentiator.** |
| Tamper-evident audit | ●●● | **Yes** — SHA-256 hash chain, tail-truncation detection via anchor head; minor: overload drops aren't self-recorded | None at this fidelity | Genuine strength. |
| Attestation → posture coupling | ●●● | **Yes** — per-node Ed25519 challenge-response gating the posture DAG; no trust mock | Secure-boot/HSM (different layer) | Differentiator. |
| LLM-output governance | ●●● | **Yes** — typed action filter as the front door of a kinematic governor | AgentSpec/RoboGuard (research-only) | Differentiator (fusion is novel). |
| Cross-domain breadth | ●●● | **Yes** — ROS2/DDS/Modbus/OPC-UA/CANopen/DNP3/CIP; minor: two protocol-abstraction seams | Automotive-first peers | Differentiator. |
| Functional-safety **certification** | ●○○ (acknowledged) | **Confirmed absent** | Apex/QNX/RTI/eProsima all ASIL-D | Layer 1 — dominant, acknowledged. |
| Certified RTOS partition + target WCET | ●●○ roadmap | **Confirmed roadmap** (`#270`) | QNX/INTEGRITY ASIL-D | Layer 2 — acknowledged. |

---

## 4. Where Kirra genuinely leads (audit-validated)

These survive the file:line audit and are defensible against the *verified* peer set:

1. **Vendor-neutral, portable, fail-closed bound.** RSS is orphaned-open-source; SFF/Halos is silicon-bundled; Autoware's island is a PoC needing a certified RTOS partner. Kirra is the only one designed from day one as a planner-agnostic, silicon-agnostic governor — and the two-box UDP governor makes the separation physical. **The orphaning of `intel/ad-rss-lib` (Aug 2025) is a concrete opening.**
2. **Cryptographic fleet federation — uncontested.** No certified-middleware vendor, no RTA framework, and no LLM-guardrail project offers Ed25519-signed cross-controller trust with durable single-use nonce burn and generation-ordered reconciliation. This is the real moat and the audit confirms the mechanism is sound, not aspirational.
3. **Attestation wired into runtime authority-to-act.** A live, revocable, posture-linked trust state is a stronger identity→authority coupling than the middleware peers expose at the application layer.
4. **LLM-governance ⋈ Simplex fusion.** AgentSpec/RoboGuard prove the LLM-guardrail idea but as research on the *reasoning* layer; Kirra's action filter is the front door of a *fail-closed kinematic* governor — even a "compliant" LLM action is still envelope-clamped. The fusion is the novel claim.
5. **Tiny + deterministic ⇒ certifiable.** NVIDIA's bound rides a 2,000-TFLOPS SoC; Kirra's checker is `no_std`-able, `panic=abort`, structurally O(1). A small deterministic governor is the *only* part of an AI-driven stack you can realistically certify — which is precisely why every major keeps a rule-based bound around the learned core.

---

## 5. The gap, in three layers

### Layer 1 — Certification paper (dominant; acknowledged)
Against safety-critical buyers, **uncertified ≈ unusable for the safety function regardless of code quality.** Every Race-B peer holds ASIL-D (Apex SEooC/TÜV NORD, QNX QOS, RTI/TÜV SÜD, eProsima). Kirra has mapping docs and a UL-4600-style case *index* but no certificate, no qualified toolchain, no independent assessor, no SOTIF (ISO 21448) story. This is correctly the team's headline gap and the `KIRRA_GAP_ANALYSIS.md`/roadmap already scope it. Nothing to add except: **the orphaned RSS lib + the SEooC precedents (Apex) make a narrowly-scoped governor SEooC the right, tractable first certificate.**

### Layer 2 — Real-time on a certified partition (acknowledged)
The `#270` QNX/iceoryx2/seqlock lane is spec'd but **PENDING-HARDWARE**; host WCET is honestly labeled indicative. The audit adds two specifics the safety case must absorb:
- **The WCET gate measures the wrong thing.** `wcet_gate.rs` benches `validate_vehicle_command` directly and gates on **p99.9, not max** — but the deployed actuator path adds `serde_json` deser + Tower `to_bytes().await` + re-serialize. The thing that feeds the FTTI claim is unmeasured. *Re-scope the gate to max-with-multiplier and add an end-to-end actuator-path bench before citing it as timing evidence.*
- **The QNX shim's torn-read mechanism is unsound on the target.** `tools/qnx-rtm-harness/kirra_shim.cpp` uses a compiler-only `atomic_signal_fence` double-read, while the spec it traces to (`HYPERVISOR_CONTRACT_CHANNEL.md`) mandates an odd/even generation seqlock — so the harness doesn't exercise the protocol it's evidence for (an RTM-traceability gap as well as an aarch64 memory-model one).

### Layer 3 — Production-hardening / engineering maturity (NEW — the value-add)
This is the layer the competitive docs miss, because it's invisible to a profile-level read. **"Leading industry software" in Race B is, above all, 24/7-deterministic and hardened.** The audit found that several dimensions Kirra self-rates ●●● are not yet there:

| Gap (from `ENGINEERING_REVIEW.md`) | Why it's a *competitive* gap | Fix effort |
|---|---|---|
| **Actuator-path ≤2s two-writer window** in HA failover (`policy_layer.rs`: cached-epoch fence, no in-tx `assert_epoch_held` on the actuator path) | Certified middleware HA is deterministic and fenced; a 2s two-writer window *on the actuator path* is exactly what a safety assessor probes first | Days |
| **Disk-wedge non-demotion** — a primary that can't read its epoch never self-demotes (`standby_monitor.rs`) | Split-brain under I/O fault is a classic HA review question | Days |
| **65 blocking SQLite calls on tokio workers**; no `busy_timeout`; DashMap iterate-then-`get` **deadlock hazard** | "Survives 24/7 / survives load" is table-stakes for the substrate tier; these are the highest-probability production stalls | Days |
| **Default QoS on safety ingress** (`Reliable+KeepLast(10)`) | RTI/eProsima sell *certified* QoS determinism; Kirra's ingress can drain 10 stale samples after a stall — the exact hazard its own output side forbids | Hours (config) |
| **DDS path is a model, not a writer; hand-rolled CDR lacks alignment padding** | Interop break with any spec-compliant CycloneDDS/Autoware reader; the QoS rigor is enforced on a struct that never reaches a wire | Weeks (real codec/writer) |
| **Predictive-RSS silent fail-open + motion-source split** | A correctness gap in the *bound itself* — the most credibility-sensitive thing for a formal-safety reviewer | Days |

**None of Layer 3 is certification work.** It is the difference between "passes its own tests and demos" and "behaves like leading-industry software under fault and load." It is also the **cheapest layer to close** — mostly days of well-scoped work the audit already specifies with `file:line` fixes — and closing it materially de-risks Layers 1–2 (you can't certify or WCET-validate code that stalls or two-writes).

### Layer 4 — Domain completeness (lower priority, mostly acknowledged)
Perception has no shipped detector/redundancy cross-check (vs Mobileye True Redundancy); maps are Lanelet2-*lite* (vs Autoware Lanelet2 routing); no SOTIF; no formal/machine-checked proof of the core invariants; no at-scale sim (vs Cosmos/ACI). The team's docs cover these well; the audit doesn't change the picture except to note the perception-divergence monitor (`perception_redundancy.rs`) has an O(A×B) no-dedup hole that weakens the True-Redundancy analog.

---

## 6. Substrate risk — the iceoryx2 dependency

Verified: iceoryx2 is **pre-1.0 (v0.9.2, June 2026)**, gained QNX 8.0 + `no_std` only in **Dec 2025**, and is **"targeting ASIL-D," not certified.** Kirra's ADR-0006 correctly makes the *boundary* a frozen `#[repr(C)]` layout that survives even if iceoryx2 is dropped (good hedge), but the in-partition transport bet rides a fast-moving 0.x crate maintained by a single open-core vendor. **Implication:** the in-partition latency story (~1–2µs, the strongest IPC adoption case) is sound, but schedule/cert risk is real — keep the frozen-layout fallback first-class, and treat iceoryx2's own ASIL-D timeline as an external dependency in the cert plan, not an assumption.

---

## 7. Reconciled, prioritized roadmap

This **merges** the engineering fixes (this audit) with the team's existing market roadmap, de-duplicated. Items already in `COMPETITIVE_ROADMAP.md` are marked ⟢.

**Phase 0 — Harden what exists (weeks; cheapest, de-risks everything else).** *New — this is the missing phase.*
- Close the actuator two-writer window + disk-wedge demotion (Layer 3 HA).
- Move SQLite off the async runtime (`call`/`call_read`), add `busy_timeout`, fix the DashMap deadlock hazard.
- Set explicit sensor-data QoS on ROS 2 ingress.
- Make predictive-RSS fail closed; unify the object motion source.
- Re-scope the WCET gate (max + end-to-end actuator bench).

**Phase 1 — Credibility paper (low effort, high impact).** ⟢ partly scoped.
- Publish the UL-4600 GSN case stating exactly what is/isn't certified.
- Map the trust layer to ISO/SAE 21434 + UNECE R155/R156 + Uptane (deltas only). ⟢ M3.1
- Formalize/model-check the envelope + MRC + `should_route_command` state machine.

**Phase 2 — Make the real-time claim true.** ⟢ M1.1 (QNX).
- Governor on **QNX OS for Safety 8.0** with target-validated WCET under FIFO; retire "indicative." ⟢
- Fix the QNX shim to the spec-mandated odd/even seqlock; correct the RTM mapping.
- SOTIF (ISO 21448) analysis for the perception derate + containment.

**Phase 3 — Earn the certificate.** ⟢ M3.
- ISO 26262 **SEooC of the governor core** (mirror Apex's strategy; the orphaned RSS lib makes a maintained, certifiable bound a market opening). ⟢ M3.4
- Tool qualification for the Rust subset; engage an independent assessor; add safe-controller diversity. ⟢

**Press the moat (continuous).**
- Productize **fleet federation** as the headline, uncontested feature — publish a protocol spec.
- Bound a **real learned doer** (Hydra-MDP/Alpamayo-class via Mick) to make the doer-swap literal. ⟢ (stack doc P0)
- Publish the **LLM-governance ⋈ Simplex** fusion before AgentSpec/RoboGuard/ProbGuard crowd the frontier.

---

## 8. Bottom line

- **Strategically, the team is right:** Kirra is the portable, vendor-neutral, fleet-scale fail-closed bound that the whole industry is independently reinventing (RSS, SFF/Halos, Autoware Safety Island), plus a fleet-trust plane **none of the peers have**. The market analysis in the existing docs holds up against verified 2026 facts, with one bonus: **the canonical open RSS implementation went unmaintained in 2025**, opening room for a maintained, certifiable bound.
- **The gap to leading industry software is three real layers**, in increasing cost: **production-hardening (cheap, days–weeks, and currently overstated as already-strong)** → **certified-partition WCET (months)** → **the ASIL-D certificate itself (years)**.
- **The single most useful correction this audit makes:** close **Layer 3 first.** It is the cheapest layer, it is where the competitive self-ratings currently outrun the code, and it is a prerequisite for Layers 1–2 — you cannot certify or WCET-validate a governor that can stall on a blocking DB call, deadlock its posture engine, or two-write the actuator path during failover. Fix the engineering maturity, and the certification story becomes about *paperwork on solid code* rather than *paper over gaps*.

---

### Sources
Competitor/standards facts verified 2026 (search + fetch + adversarial check): `intel/ad-rss-lib` (archived Aug 2025); IEEE 2846-2022; NVIDIA DRIVE AV SFF / Halos / DRIVE AI Inspection Lab; Autoware Safety Island blueprint; Apex.AI Apex.Grace (TÜV NORD ASIL-D SEooC); BlackBerry QNX OS for Safety 8.0; RTI Connext Cert / Drive (DO-178C DAL A + ASIL-D); eProsima Safe DDS 3.0 (ASIL-D); Eclipse iceoryx2 (v0.9.2, Jun 2026); NASA R2U2 / DARPA Assured Autonomy; AgentSpec (arXiv 2503.18666) / RoboGuard (arXiv 2503.07885); UL 4600 (Ed. 3, 2023); Foretellix / Applied Intuition (V&V category). Code facts: `ENGINEERING_REVIEW.md` (this branch) and the cited `file:line` references therein. Cross-cutting caveat: several vendor domains 403'd direct fetch and were corroborated via indexed snippets + secondaries; SFF's *current* productization status and two cert-body identities (QNX QOS, eProsima) remain MED-confidence.
