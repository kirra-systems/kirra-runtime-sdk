# Kirra stack vs. Tier-1 ADAS benchmark / Autoware / NVIDIA — stack-wide gap analysis

| Field | Value |
|---|---|
| Status | Analysis / living document |
| Date | 2026-06-23 |
| Scope | The **whole Kirra stack** (KIRRA governor + verifier/federation/audit + Parko perception + Occy planner + Mick LLM brain + QNX/WCET lane) vs. the Tier-1 ADAS benchmark vendor, Autoware, and NVIDIA — current (2025–2026) state. Identifies where we lead, where we lag, and a prioritized improvement roadmap. |
| Companion | `docs/COMPETITIVE_PLANNER_ANALYSIS.md` (Occy-only, planner-scoped). This doc is the stack-wide superset. |

> Sources are current as of CES/GTC 2026; key URLs are listed in §7. Where a vendor's
> own domain 403'd direct fetch, claims are corroborated from secondary sources.

## 0. The framing — and the finding that validates it

Kirra is a **runtime-legitimacy engine + fail-closed safety governor**: a separable
*checker* (KIRRA) that bounds a swappable *doer* (perception + planner + LLM), plus a
fleet-scale trust/governance plane. The question "how do we compare to the AV majors"
is really two questions, because **we are not primarily an AV-planning company** — we
are an *assurance and governance* layer that can sit under any planner.

**The headline finding of this round: all three majors are independently converging on
our architecture — a separate formal/rule-based safety bound wrapped around an
increasingly-learned planner.**

- **the Tier-1 ADAS benchmark vendor** explicitly rejects monolithic end-to-end in favor of **"Compound AI"**
  (specialized models) with **RSS** — a formal, technology-neutral safety envelope —
  sitting *independently on top* and able to override the RL driving policy. RSS is the
  doer/checker split, productized, and is the seed of **IEEE 2846**.
- **NVIDIA's** own SOTA learned planner **Hydra-MDP** does *not* trust the net to be
  safe: it **distills a rule-based simulator's verdict** (NAVSIM/PDM: no-collision,
  drivable-area, TTC, comfort) into the student, then re-applies a **separate runtime
  guardrail** (Safety Force Field / the **Halos** ASIL-D stack) around it. They ship the
  largest learned AV models (Alpamayo, a 32B reasoning VLA) *and* keep a certifiable
  rule-based bound — explicitly because the net "lacks an internal mechanism to evaluate
  and correct unsafe actions before execution."
- **Autoware** has **no** formal runtime safety bound today (safety is baked into the
  rule-based planner + an MRM fail-safe selector), but is now building a **"Safety
  Island"**: an isolated Zephyr-RTOS supervisor separate from the Linux compute. That is
  the industry reinventing, in the open, **what KIRRA already is**.

So the strategic posture writes itself: **don't race them on planning, perception, or
model scale — those gaps are real but orthogonal. Press the assurance + governance moat,
and make KIRRA the portable, vendor-neutral bound that can sit under benchmark-, Autoware-,
or NVIDIA-class doers.** The adversarial-doer demo (#482) already proved KIRRA's safety
case is invariant to the doer; that is the whole game.

## 1. The competitors at a glance (2026)

- **Tier-1 ADAS benchmark vendor** — vertically integrated on its certified ADAS SoC family. An eyes-on
  camera-led 11-cam product, an eyes-off product (adds a redundant SoC + radar/lidar),
  and an L4 robotaxi line. **True Redundancy**: two *independent* world models
  (camera-only vs radar+lidar), each filtered through RSS. Crowdsourced HD maps
  (Roadbook). RL driving policy + RSS. New: **ACI** self-play sim, **VLSA** "slow-thinking"
  vision-language model. MRC fallback via redundancy.
- **Autoware** — leading open-source stack (Autoware Foundation), ROS 2 / DDS,
  **Core/Universe** split. **Lanelet2** HD maps → routing graph. Plugin planning
  (`behavior_path_planner` lane-change/avoid/pull-over; `behavior_velocity_planner`
  intersection/crosswalk/traffic-light; `motion_velocity_smoother`). `map_based_prediction`
  (multi-modal). Safety = planner heuristics + **MRM** fail-safe; **no RSS-equivalent**,
  no ISO-26262 cert. Moving to E2E: **AutoSeg** vision FM, **Diffusion Planner**, AMD
  partnership, Open AD Kit v2 (CES 2026).
- **NVIDIA** — **DRIVE Thor** (Blackwell, ~2,000 TFLOPS, 2025) / AGX Orin; **DriveOS**;
  **Hyperion** (14 cam / 9 radar / 1 lidar). **"AV 2.0"** single-network E2E (won CVPR
  E2E challenge 2024 & 2025). **Hydra-MDP** (rule-distilled learned planner). **Cosmos**
  world foundation models + **Omniverse** neural sim; **Alpamayo** open reasoning-VLA
  (10B→32B) with closed-loop AlpaSim/AlpaGym. Safety = **Halos** (TÜV SÜD ASIL-D) +
  **SFF** guardrail around the learned core.

## 2. Stack-wide comparison matrix

| Dimension | **Kirra stack** | **Tier-1 ADAS benchmark** | **Autoware** | **NVIDIA** |
|---|---|---|---|---|
| Primary identity | **Assurance + fleet-legitimacy layer** (planner-agnostic) | Vertically-integrated AV product | Open-source AV platform | AV compute + models platform |
| Planner paradigm | Geometric reference proposer (Occy), **swappable** | RL driving policy | Modular rule-based plugins | Learned E2E (Hydra-MDP, Alpamayo) |
| Perception | Parko (vendor-neutral inference; **no shipped detector yet**) | certified SoC + **True Redundancy** (2 indep. world models) | Modular perception | Hyperion + BEV E2E |
| Maps / routing | `lanemap`-lite (Lanelet2-*lite*, no parse/router yet) | crowdsourced HD maps | **Lanelet2** + routing graph | Map-lite / BEV |
| Prediction | CV/CTRV + lane-intent priors | Prediction + worst-case RSS | **Multi-modal** map-based | Joint detect→predict→plan |
| Safety / assurance model | **External, formal, fail-closed runtime checker (KIRRA): RSS distances + containment + per-pose kinematics + posture** | **RSS** (external, formal, → IEEE 2846) | Planner-embedded + MRM; **no formal bound** (Safety Island emerging) | **SFF / Halos** guardrail + rule-distillation |
| Fleet trust / governance | **Verifier + Ed25519 federation + hash-chained audit + attestation + posture engine + HA** | Cloud (crowdsourced maps); not a fail-closed governor-of-governors | None (single-vehicle) | Fleet tooling; not a trust/legitimacy engine |
| LLM / foundation / world models | **Mick** — bounded LLM intent brain (proven bounded) | VLSA slow-thinking VLM | AutoSeg FM, Diffusion Planner | Cosmos WFM, Alpamayo 32B VLA |
| Determinism / WCET / cert | **O(1) WCET checker, fail-closed, audited; QNX lane (#270) early** | certified-SoC ASIL | **No FuSa cert** | **ASIL-D (Halos/DriveOS)** |
| Learning | None (deterministic checker; doer may learn) | RL policy + self-play | Moving to E2E | Core (distillation, world models) |

## 3. Where the Kirra stack leads

1. **The assurance layer is first-class, separable, and portable.** The benchmark vendor's RSS is a
   peer idea but **bundled inside their vertical stack**; NVIDIA's SFF/Halos is bundled
   and proprietary to their silicon; Autoware has none (building one). KIRRA is the only
   one designed as a **vendor-neutral, planner-agnostic, fail-closed governor** that can
   bound *any* doer — including an NVIDIA- or benchmark-class one. The two-box UDP governor
   (`kirra-governor-service`, ADR-0014) makes that separation physical.
2. **A fleet-scale trust/legitimacy plane that none of them have.** The verifier
   (`verifier.rs`), Ed25519 cross-controller **federation** (`federation.rs` /
   `federation_reconciliation.rs`), SHA-256 hash-chained **audit chain**
   (`audit_chain.rs`), per-node **attestation** (Ed25519 over `(node_id, nonce)` vs
   registered AK, #73), the **posture engine** with generation persistence, and HA
   standby/promotion — this is a *distributed runtime-legitimacy engine*, not AV planning.
   No competitor has a fail-closed governor-of-governors at this fidelity. **This is the
   real moat** and it is orthogonal to who wins on planning.
3. **The bounded-LLM thesis is proven, not promised.** As the Tier-1 ADAS benchmark vendor (its vision-language safety agent) and NVIDIA
   (Alpamayo, a 32B reasoning VLA) ship LLM/VLA "brains," they are *still arguing* the
   safety case for them. We have it: Mick proposes typed intent, Occy grounds it, KIRRA
   bounds it, and `tests/adversarial_doer_bounded_by_kirra.rs` shows the bound holds for a
   reckless/black-box doer exactly as for the careful one.
4. **Explicit posture state machine** (Nominal → Degraded → LockedOut → MRC) as a
   first-class, structured degradation layer — most monolithic planners lack this as a
   concept; ours is the spine the whole governor hangs on.
5. **Tiny, deterministic, WCET-bounded, auditable.** The checker is `no_std`-able,
   `panic=abort`, O(1)-structural-WCET (`wcet_gate.rs`), fail-closed, and every verdict is
   audit-chained. NVIDIA's bound rides a 2,000-TFLOPS SoC; ours can ride a Raspberry Pi or
   a QNX partition.

## 4. Where it lags — prioritized gaps (with codebase-tied fixes)

1. **Occlusion / limited-visibility reasoning (RSS Rule 4) — missing.** KIRRA checks
   against a perception *snapshot*; it does not reason about *occluded* space (a
   pedestrian who could emerge from behind a parked car). The benchmark vendor's RSS Rule 4 mandates
   exactly this. **Fix:** a visibility-aware speed bound — treat occluded regions as
   potential worst-case agents and cap admissible speed accordingly. This is a
   *checker-shaped* improvement (lives in the RSS/containment layer, `validation.rs`), and
   it *reduces* false-safe over-permissiveness rather than adding over-conservatism.
2. **Perception has no shipped detector + no redundancy cross-check.** Parko is a
   vendor-neutral inference seam with no model wired; meanwhile the Tier-1 ADAS benchmark vendor ships **True
   Redundancy** (two independent world models). **Fix (two steps):** (a) wire a TensorRT
   detector behind Parko; (b) — the KIRRA-flavored one — add a **perception-divergence
   monitor**: run two *independent* perception channels and have KIRRA fail-closed when
   they disagree beyond tolerance (a True-Redundancy analog promoted to an *assurance*
   check, feeding the `perception_monitor` derate / posture).
3. **Prediction is CV/CTRV + lane-intent; everyone else is multi-modal.** The checker only
   needs worst-case bounds, so this is less acute than it looks — but a cut-in or
   unprotected turn the snapshot can't see is a real hole. **Fix:** a **multi-modal-aware
   RSS** — bound over the object's *several* predicted modes (Autoware-style
   `map_based_prediction` hypotheses), not just the instantaneous-velocity tangent;
   degrade to CV as fallback.
4. **HD maps / routing still thin.** `lanemap` is Lanelet2-*lite* with no file parse, no
   router, no intersections — vs Autoware's Lanelet2 routing graph and the benchmark vendor's crowdsourced HD maps.
   **Fix:** the feature-gated `Lanelet2CorridorSource` (C++ `lanelet2_core`) to *populate*
   the model, then a lane-selection router and intersection controls (which also unlocks
   deriving right-of-way upstream, closing the `cedes_to_ego_ids` integrator-supplied gap).
5. **Cert / RTOS maturity is early.** NVIDIA has TÜV SÜD **ASIL-D** (Halos/DriveOS);
   The Tier-1 ADAS benchmark vendor's SoC is ASIL; Autoware has none; we have the **QNX/partition lane (#270)** and
   an O(1) WCET argument but it is a prototype/demonstrator (Pi/QNX stepping stones, per
   ADR-0014/0001). **Fix:** mature the WCET-measurement methodology (#274/#279) and the
   QNX RTM matrix (#271/#272) toward an actual ASIL-style argument for the governor core —
   our determinism/tininess makes this *more* tractable than certifying a 32B net.
6. **No sim / synthetic-data engine at scale.** NVIDIA has Cosmos/Omniverse/AlpaSim;
   the Tier-1 ADAS benchmark vendor has ACI self-play; Autoware has sim. We have the deterministic
   `ScenarioRunner` + `VirtualClock` temporal harness — excellent for *formal-bound*
   regression, but not large-scale scenario coverage. **Fix (framed correctly):** lean on
   the formal bound (we don't need their data scale to *certify the checker*), but build a
   scenario-coverage harness on `ScenarioRunner` for the *doer's* validation, and consider
   consuming their open assets (NAVSIM-style metrics, Cosmos-generated scenes) as inputs.
7. **Trajectory optimization / comfort — largely closed this session.** S-curve
   jerk-limited speed, Chaikin path smoothing, curvature-aware speed, and a
   forward–backward velocity profile already landed (see companion doc §4). Remaining:
   explicit steering-rate cap + joint path+speed optimization. Lower priority.

## 5. Prioritized improvement roadmap

**P0 — close the strategic loop (highest leverage, harness already proven):**
- Swap a **real learned doer** behind `plan_for_intent` (Hydra-MDP-style, or an Alpamayo-
  class VLA via Mick) and show KIRRA's safety case is *unchanged* from the geometric Occy.
  This is the killer demo; #482 proved the harness, this makes the claim literal.

**P1 — checker completeness (make the bound less conservative *and* more complete):**
- **Occlusion-aware speed bound** (RSS Rule 4) — gap #1.
- **Multi-modal-prediction-aware RSS** — gap #3.
- **Perception-divergence assurance monitor** (True-Redundancy analog) — gap #2b.

**P2 — substrate the doer needs anyway:**
- **Lanelet2 parse + router + intersections** — gap #4 (also unlocks RoW derivation).
- **Ship a Parko detector** — gap #2a.

**P2 — the certifiability story:**
- Mature the **QNX/WCET/partition** lane toward an ASIL-style argument for the governor —
  gap #5. This is a *differentiator* (small + deterministic ⇒ certifiable), not catch-up.

**Press the moat (cross-cutting, ongoing):**
- Productize the **fleet-legitimacy plane** (federation, audit, attestation, posture,
  console) as the portable governor that sits *under* any stack. This is where we are
  uniquely ahead; the AV majors are single-stack-vertical and have no analog.

## 6. Strategic recommendation

The gap analysis points one way: **we are the layer the whole industry is independently
reinventing** (Autoware Safety Island, NVIDIA Halos/SFF, benchmark-vendor RSS) — but ours is the
only one built as a *portable, vendor-neutral, fleet-scale, fail-closed* bound from day
one. The right plays, in order:

1. **Make the doer-swap literal** (P0) — bound a real learned/foundation planner; that is
   the demo that sells the thesis to anyone shipping Hydra-MDP/Alpamayo/Diffusion-Planner.
2. **Close the RSS completeness gaps** (occlusion, multi-modal) so the bound is *correct*,
   not just present — these are the credibility gaps a benchmark-literate reviewer will probe.
3. **Lean into the governance moat** — it is genuinely unique and orthogonal to the
   planning race we should not try to win.
4. **Mature certifiability** as the long-pole differentiator — a tiny deterministic
   governor is the *only* part of an AI-driven stack you can actually certify, which is
   precisely why everyone keeps a rule-based bound around the learned core.

Do **not** invest in matching NVIDIA on model scale or Autoware on planner breadth — those
are the doer's job, and the doer is swappable by design.

## 7. Sources

**Tier-1 ADAS benchmark:** RSS — Shalev-Shwartz, Shammah, Shashua, *On a Formal Model of Safe and Scalable Self-driving Cars* ([arXiv:1708.06374](https://arxiv.org/abs/1708.06374)) ·
[IEEE 2846 (Business Wire)](https://www.businesswire.com/news/home/20191219005720/en/) ·
*Safety First for Automated Driving* (SaFAD) industry whitepaper

**Autoware:** [Core/Universe concepts](https://autowarefoundation.github.io/autoware-documentation/main/design/autoware-concepts/difference-from-ai-and-auto/) ·
[Behavior Velocity Planner](https://index.ros.org/p/autoware_behavior_velocity_planner/) ·
[Fail-safe / MRM API](https://autowarefoundation.github.io/autoware-documentation/galactic/design/autoware-interfaces/ad-api/list/api/fail_safe/) ·
[Safety Island blueprint](https://autoware.org/collaborative-blueprint-for-safe-and-secure-mixed-critical-orchestration-in-autonomous-vehicles/) ·
[Autoware E2E](https://www.thinkautonomous.ai/blog/autoware-end-to-end/) ·
[Autoware.Flex (arXiv 2412.16265)](https://arxiv.org/html/2412.16265v1)

**NVIDIA:** [Hydra-MDP (arXiv 2406.06978)](https://arxiv.org/abs/2406.06978) ·
[Hydra-MDP++ (arXiv 2503.12820)](https://arxiv.org/html/2503.12820) ·
[Hydra-MDP tech blog](https://developer.nvidia.com/blog/end-to-end-driving-at-scale-with-hydra-mdp/) ·
[Halos safety system](https://blogs.nvidia.com/blog/halos-safety-system-autonomous-vehicles/) ·
[Safety Force Field](https://nvidianews.nvidia.com/news/nvidia-introduces-drive-av-safety-force-field-computational-defensive-driving-policy-to-shield-autonomous-vehicles-from-collisions) ·
[Cosmos WFM](https://developer.nvidia.com/blog/simplify-end-to-end-autonomous-vehicle-development-with-new-nvidia-cosmos-world-foundation-models/) ·
[Alpamayo 2](https://nvidianews.nvidia.com/news/nvidia-alpamayo-2-super-robotaxis) ·
[DRIVE Thor](https://nvidianews.nvidia.com/news/nvidia-unveils-drive-thor-centralized-car-computer-unifying-cluster-infotainment-automated-driving-and-parking-in-a-single-cost-saving-system)
