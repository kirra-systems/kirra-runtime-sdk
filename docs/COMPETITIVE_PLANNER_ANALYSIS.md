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
2. **Lane graph / routing — substrate now exists, routing still thin.** Occy now
   carries a parse-free **Lanelet2-lite** lane model (`kirra_planner::lanemap`:
   `LaneGraph` / `Lane` / `LaneEdge`) that derives the drivable corridor over a
   span of lanes *and* the **typed lane-line positions** the lane-line rules used
   to take as hand-fed literals — so a commanded lane change is now gated by the
   *map's* line types (broken-permits / solid-blocks), end-to-end to KIRRA. Still
   missing: lane *selection* / a real router, intersections, and the actual map-file
   parse (Autoware's Lanelet2 / Mobileye's REM). The adapter's feature-gated
   `Lanelet2CorridorSource` (C++ `lanelet2_core`) remains the home for the parse
   that would *populate* this model.
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
reject. The **overtake** build sharpened the lateral-RSS half of it: during the
*angled* ramp of a pass the lateral RSS treated any **fast adjacent-lane** vehicle
as a lateral threat (the path heading projects the other car's speed into a closing
lateral component), MRC-ing oncoming *and* same-direction traffic alike.

**Addressed (lateral RSS, longitudinal gate):** the root cause was that the lateral
safe-distance bound danger *independently* of longitudinal distance, but RSS
(IEEE 2846 §5; Shalev-Shwartz et al.) defines a dangerous state as the
**conjunction** — two vehicles cannot collide laterally unless also longitudinally
close. Both checkers (`validate_trajectory_slow`, `compute_scene_rss`) now **gate**
the lateral defence-in-depth on `RSS_LONGITUDINAL_CONFLICT_M` (8 m): a lateral
shortfall is dangerous only when the object is alongside/imminent, so a lead well
ahead and oncoming traffic safely passing in the next lane no longer trip it (the
overtake demo's direction-isolation control now passes at the trajectory level, not
just the #469 unit level). The dominant longitudinal RSS — car-following / head-on —
is unchanged, and the lateral layer still fails closed for a genuine cut-in.

**Remaining:** the longitudinal/alignment half — a car *centered* in the ego lane
still can't be passed, because clearing the checker's 4 m lateral-alignment band
needs >4 m of side room (a pass is admissible only for a car near the lane edge or
on a wide road). And in dense traffic, smartening Occy's policy is the orthogonal
axis. Mobileye pairs RSS with a *sophisticated* policy **and** carefully-tuned RSS
parameters — both levers, not one.

## 5. Recommended roadmap (in Kirra's grain)

1. **Lane graph + routing** (Lanelet2 seam) — unlocks lane-lines with real
   positions, lane changes, intersections, routing. Highest leverage; the hook
   already exists in the adapter. **Done (substrate):** `kirra_planner::lanemap`
   delivers the parse-free model + the corridor / typed-boundary derivations, with
   the map-file parse left feature-gated. **Remaining:** lane selection / router and
   intersections.
2. **Prediction** — even constant-turn-rate / intention priors beat
   constant-velocity.
3. **Trajectory optimization** — jerk-limited / comfort, replacing the trapezoid.
4. **Lateral behaviors** — lane-change / merge / overtake decisions. **Done
   (overtake):** the `PlanInput` reference-path vs drivable-area split +
   `compute_overtake_bump` let Occy *propose* a cross-centerline pass into the
   oncoming lane (gated by lane-line type + drivable fit); KIRRA's head-on RSS
   governs the oncoming risk. **Remaining:** merge / unprotected-turn negotiation,
   and the §4 RSS-tuning needed for passes to be admitted as routinely as they are
   proposed.
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
