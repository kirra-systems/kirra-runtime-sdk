# Occy vs. industry planners — competitive & gap analysis

| Field | Value |
|---|---|
| Status | Analysis / living document |
| Date | 2026-06-22 |
| Lane | Robotics-build (Occy / `kirra-planner`). **Not** the Phase-I proposal lane. |
| Scope | Position Occy against Autoware, Mobileye, and NVIDIA; identify gaps + a roadmap. |
| Refs | #90 (Occy), ADR-0015 (Taj), the merged Occy capability set (#447–#457). |

> Compares the **current** Occy planner (geometric reference proposer behind the
> KIRRA runtime-assurance checker) with industry-leading planning stacks.

## 0. The framing that changes everything

Occy is **not** a production motion planner and is not trying to be one. It is a
deliberately-simple, **verifiable proposer behind a separate formal safety layer
(KIRRA)**. That separation is not a quirk — it is the same insight **Mobileye
productized as RSS** (Responsibility-Sensitive Safety): a formal, transparent,
*verifiable* safety model that sits **apart** from the (complex, possibly-learned)
driving policy. KIRRA literally *is* an RSS-style checker — its
`longitudinal_safe_distance` / `lateral_safe_distance` are RSS / IEEE-2846
formulas. And NVIDIA's **Hydra-MDP** independently confirms the direction: a
learned planner *bounded / taught by rule-based planners*.

**The industry is converging on "complex planner + formal safety bound," and
Occy + KIRRA sits squarely in that camp.** Therefore most of Occy's "gaps" versus
Autoware / NVIDIA are real *as a planner* but **largely orthogonal to the Kirra
thesis** — which is that the planner is *swappable* (Occy, Autoware, or a learned
net) because KIRRA owns the safety guarantee. The single most strategic
"improvement" is not making Occy match NVIDIA; it is that **Kirra can safely adopt
an NVIDIA-style planner as the doer.**

## 1. Where each one sits

| Dimension | **Occy (today)** | **Autoware** | **Mobileye** | **NVIDIA (Hydra-MDP / E2E)** |
|---|---|---|---|---|
| Paradigm | Geometric, rule-based proposer | Modular classical (hierarchical) | RL driving policy + **RSS** formal safety | **Learned end-to-end**, rule-distilled |
| Routing / map | Single free-space corridor; no map | **Lanelet2** HD lane graph + mission router | **REM** crowdsourced maps | BEV features, map-lite |
| Behavior planning | Longitudinal rules + lateral lane-line rules; no lane-change | Behavior path+velocity plugins (lane change, avoid, intersections) | Negotiation, "unwritten rules" in RSS | Learned, naturalistic |
| Motion planning | Centerline + trapezoid speed; smoothstep lateral bump | **Jerk-limited trajectory optimization** + velocity smoother | Optimization under RSS | Single net, BEV → trajectory |
| Prediction | Constant-velocity (tracker → RSS) | Multi-modal agent prediction | Prediction + worst-case RSS | Joint detection → prediction → planning |
| Safety model | **External RSS checker (KIRRA)** | Built into planner | **RSS (external, formal)** | Rule-based teacher + checks |
| Learning | None (deterministic) | None (classical) | RL policy | Core (distillation, world models) |

## 2. Occy's real gaps (prioritized, honest)

1. **No prediction beyond constant-velocity.** The Taj tracker supplies object
   velocity; RSS extrapolates linearly. No intent / multi-modal agent prediction
   (cut-ins, turns) — the #1 functional gap vs. everyone.
2. **No lane graph / routing.** Occy follows one corridor centerline. No lane
   selection, intersections, or full lane-change behavior. Autoware's Lanelet2 /
   Mobileye's REM are the missing substrate — and the adapter's
   `Lanelet2CorridorSource` seam is the intended home (also the right substrate
   for *typed* lane-line positions, which the lane-line rules currently take as
   centerline-relative inputs).
3. **No trajectory optimization / comfort.** Geometric centerline + trapezoid vs.
   Autoware's jerk-limited, dynamically-feasible optimization. No MPC.
4. **Thin lateral behavior.** Route-around + lead-follow only; no lane-change,
   merge, overtaking *decision*, or unprotected-turn negotiation.
5. **No interaction / game-theoretic reasoning.** Plans against a snapshot; does
   not reason about how ego's actions change other agents.
6. **No learned / naturalistic policy** for the long tail — where Tesla / Waymo /
   NVIDIA now compete.
7. **Single-trajectory, no contingency / multi-modal** plans under uncertainty.

## 3. What Occy already does well (for its role)

- **Verifiable, deterministic, tiny** — the whole point (and what an end-to-end
  net cannot give you).
- **Explicit safety-state machine** (posture-gated Nominal → Degraded → LockedOut
  → MRC) — a structured degradation layer most monolithic planners lack as a
  first-class concept.
- **Every proposal RSS-checked** by KIRRA — the runtime-assurance discipline,
  done cleanly.
- **Rule-correct behavior layer** — the amber dilemma-zone, flashing-signal
  semantics, and solid/broken lane-line crossing rules are genuinely lawful, not
  toy stand-ins.

## 4. The honest caveat the build itself surfaced

A *too-strict checker + too-simple planner* is **over-conservative**. We hit it
repeatedly: a dead-ahead lead → MRC (the lateral-RSS floor); an abreast pass →
reject. In dense traffic, Occy + KIRRA-as-tuned-today would stop too often.
Mobileye avoids this by pairing RSS with a *sophisticated* policy **and**
carefully-tuned RSS parameters. So a real improvement axis is **both** smartening
Occy *and* tuning KIRRA's RSS conservatism — not just one.

## 5. Recommended roadmap (in Kirra's grain)

1. **Lane graph + routing** (Lanelet2 seam) — unlocks lane-lines with real
   positions, lane changes, intersections, routing. Highest leverage; the hook
   already exists in the adapter.
2. **Prediction** — even constant-turn-rate / intention priors beat
   constant-velocity.
3. **Trajectory optimization** — jerk-limited / comfort, replacing the trapezoid.
4. **Lateral behaviors** — lane-change / merge / overtake decisions.
5. **The strategic one** — prove KIRRA bounds a **learned planner**: swap a
   NVIDIA / Hydra-MDP-style net in as the doer and show the safety case is
   *unchanged*. That is the Kirra thesis's killer demo, and it is why Occy's
   planner-gaps do not threaten the architecture.

**Net:** Occy is a strong *reference proposer* and a deliberately-thin one; its
planner-capability gaps vs. Autoware / NVIDIA are large but mostly orthogonal to
Kirra's value, which mirrors Mobileye's RSS. The work that matters most is the
lane-graph substrate (next) and, eventually, demonstrating the checker bounding a
learned planner.

## Sources

- Autoware Universe — Planning components: <https://autowarefoundation.github.io/autoware_universe/main/planning/>
- Autoware — Behavior Path Planner: <https://autowarefoundation.github.io/autoware.universe_planning/main/planning/behavior_path_planner/>
- Mobileye — Responsibility-Sensitive Safety (RSS): <https://www.mobileye.com/technology/responsibility-sensitive-safety/>
- Mobileye — Road Experience Management (REM): <https://www.mobileye.com/technology/rem/>
- NVIDIA — End-to-End Driving at Scale with Hydra-MDP: <https://developer.nvidia.com/blog/end-to-end-driving-at-scale-with-hydra-mdp/>
- Motion Planning for Autonomous Driving: State of the Art (arXiv 2303.09824): <https://arxiv.org/pdf/2303.09824>
