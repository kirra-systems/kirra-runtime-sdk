# Kirra World safety-scope ruling — assembled input

| | |
|---|---|
| **Identifier** | KIRRA-WM-D5-INPUT-001 |
| **Status** | **Input material. This is NOT the ruling and it fills no field of it.** |
| **For** | ADR-0042 **Decision 5** — the safety-assurance ruling, status `PENDING` |
| **Owner of the ruling** | Justin Looney (assigned 2026-08-03) |
| **Prepared** | 2026-08-04 |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

---

## What this document is, and what it deliberately is not

ADR-0042's decision record has seven `UNASSIGNED` fields. Its
*What completing the ruling would take* section maps the eight questions onto
those fields so that "pending" is not a state with no visible exit. This
document goes one step further: for each question it assembles **what the
repository can already answer, what it cannot, and where the facts live** — so
the ruling is a reading exercise rather than a research exercise.

**It does not:**

- fill, propose or pre-fill any field of the decision record;
- answer **Q7** — the scope classification *is* the ruling, and stating one here
  would be making it;
- claim, imply or prepare a formal safety-scope exclusion;
- soften **Q5**, which ADR-0042 names as the sharpest question and which this
  document leaves exactly as sharp;
- change ADR-0042 in any way. That record is untouched and still reads
  `PENDING`.

**Independence posture carries over unchanged.** Per ADR-0042's recorded
posture, the eventual ruling is an **owner self-assessment, not an independent
assurance review**; the same person may hold the system-owner and assessor
roles; no independent internal or external assessment has occurred. Material
assembled by an assistant does not alter that, and should not be read as
strengthening the ruling's independence — if anything it is one more thing the
owner is reviewing rather than an additional reviewer.

---

## Q1 — Is the absence of a runtime dependency sufficient?

**What is established.** The bidirectional fence is **enforced, not merely
stated**. `ci/check_kirra_world_bidirectional_fence.py` reports `INTACT` over a
**safety closure of 19 workspace packages from 10 roots**, computed transitively
over normal and target dependencies and cross-checked against `cargo metadata`.
ADR-0042 Decision 3 widened Fence B from named crates to the transitive closure
precisely because a direct-import check is insufficient. The three
`kirra-world*` packages contain **shape only**; the domain-logic gate reports
"Nothing to block."

**What this does not settle.** Structural absence of a dependency is a statement
about *linkage*. Q2, Q3 and Q5 all concern influence that does not require
linkage. The honest framing for the ruling is that Fence A and Fence B answer
"can Kirra World code execute inside the safety closure" — a question now
answered by machine — and do not by themselves answer "can Kirra World content
affect a safety outcome", which is Q2/Q5.

**Where to look.** ADR-0042 §*Measured closure*, §*Decision 3*;
`ci/check_kirra_world_bidirectional_fence.py`; ADR-0039 §*Structural
enforcement* for what the checker does and does not prove.

---

## Q2 — Can semantic goal selection influence a safety goal indirectly?

**What is established.** The doer/checker split is real and enforced: the
planner proposes, `validate_trajectory_slow*` bounds, and the checker is the
sole safety authority. `PlanOutput::safe_stop` must always exist. The Mick
sidecars are fenced from actuation by `ci/check_mick_actuation_fence.py` — no
dependency route to release-token, serial consumer or ROS-DDS can compile into
them.

**What is not established, and is the question.** "Systematically steering the
doer toward the envelope's edge" is a *statistical* influence on which admissible
trajectories get proposed. Nothing in the repository measures the distribution
of proposals, only whether each one is admissible. A bounding checker constrains
the set; it does not constrain selection within the set.

**This is genuinely open** and no evidence here closes it.

---

## Q3 — Do common-source artifacts create dependent-failure concerns?

**This one has a concrete, current answer, and it is the most actionable item in
this document.**

**Today: structurally absent.** The checker does not consume the planner's map.
`kirra-ros2-adapter/src/node.rs` states it explicitly:

> *"today `~/input/map` is a placeholder subscription — the slow loop uses the
> injected `CorridorSource`, not this blob."*

`kirra-map` is **not** in the enforced safety closure. So the archetypal
common-source artifact — one map file feeding both doer and checker — does not
exist in the current wiring.

**And the condition under which that changes is already named**, in the same
comment:

> *"When Phase-2 makes the map load-bearing, ADD a 'map received before first
> validation' gate that fails closed (MRC) until the blob lands."*

The TransientLocal QoS is already in place as that gate's precondition.

**What the ruling may want from this.** The answer to Q3 is currently *no*, but
it is *no by present wiring*, not by prohibition. If the ruling relies on it,
"the checker's corridor is not map-derived" belongs in **`Assumptions`**, and
Phase-2 map adoption belongs in **`Conditions that reopen the decision`** —
because the code has already flagged that Phase 2 changes this and specified the
mitigation without implementing it.

**One residual to check that this document has not:** whether any *other*
artifact is common-source — a calibration file, a class list, a frame
definition. The map was checked because it is the obvious candidate.

---

## Q4 — Does Kirra World affect ODD assumptions?

**What the AoU register says.** Several ODD-bearing assumptions are recorded as
**`AoU-GAP`** — assumed, assigned to an integrator or operator, not discharged
by anything in the stack:

| AoU | Assumption | Status |
|---|---|---|
| `AOU-PERCEPTION-RANGE-001` | Reliable detection ≥ 130 m worst-case over the ODD | **AoU-GAP** (base) |
| `AOU-PERCEPTION-CLASS-001` | Reliable detection of worst-case classes (pedestrian / cyclist / child / low-contrast debris) | **AoU-GAP** (base) |
| `AOU-LOCALIZATION-001` | ≤ 0.10 m 95th-pct lateral error over the ODD | **AoU-GAP** (base) |
| `AOU-R2-ENVIRONMENT-001` | R2 operates **indoors only** | **AoU-GAP** |
| `AOU-R2-SURFACE-001` | Flat hard floor, no drop-offs, nothing a 2-D lidar cannot see | Operator |

**The sharpest of these for Q4** is `AOU-R2-ENVIRONMENT-001`, whose own note
records that **nothing in the stack detects that the robot has been carried**
out of its ODD. So ODD membership is presently an *operator* guarantee with no
runtime detection behind it.

**Why that bears on Kirra World.** If semantic goal selection can influence
*where* the platform goes, and ODD exit is undetected, then the question "does
Kirra World affect ODD assumptions" is not answerable by pointing at the fence —
it depends on whether goal selection can route toward an ODD boundary. That is
Q2 and Q4 meeting, and the ruling may want them answered together.

---

## Q5 — Are incorrect semantic goals fully bounded by the checker?

**Left exactly as ADR-0042 states it, deliberately.** The checker bounds
*trajectories*; a semantic error producing a legal trajectory to a
wrong-but-reachable place is bounded kinematically while being operationally
wrong. Whether that is a safety concern or an availability concern is the
owner's call.

**What this document can add is only scope, not an answer.** The checker's
bounds are kinematic and geometric — containment, per-pose kinematics, RSS
(longitudinal ∧ lateral), occlusion, multi-modal predictive RSS. None of them
evaluate *destination semantics*. So the phrase "fully bounded … for every
hazardous outcome" resolves to: bounded for every hazardous outcome expressible
in the checker's geometry, and silent on any hazard that is not.

Whether the set of hazards not expressible in that geometry is empty is a HARA
question, not a code question. `docs/safety/HARA.md` is where that argument
would have to be made or found wanting.

---

## Q6 — Are the checker's independent inputs adequate for all hazardous outcomes?

**What is established.** The checker's independent input channels exist and are
fail-closed when armed-but-silent, which is the property that matters:

- **True-Redundancy divergence** (`KIRRA_PERCEPTION_REDUNDANCY_ENABLED`) —
  requires two independent perception channels to agree; divergence or a silent
  secondary yields an MRC-floor cap.
- **VRU channel** (`KIRRA_VRU_CHANNEL_ENABLED`) — armed-but-silent **stops the
  ego** rather than driving blind.
- **Occlusion channel** (`KIRRA_OCCLUSION_CHANNEL_ENABLED`) — same three-way
  decision; armed-but-silent yields an MRC-floor cap.

**What is not established.** All three are **opt-in and default OFF**. Adequacy
is therefore conditional on deployment configuration, not on the code. And the
producers behind them carry their own assumptions — `AOU-VRU-RATE-001` and
`AOU-OCCLUSION-RATE-001` require the producer to publish at rate; the perception
AoUs above are `AoU-GAP`.

So the accurate statement for the ruling is: *the checker's inputs are
independent by construction and fail closed when armed; whether they are
adequate depends on which are enabled and on assumptions currently carried as
gaps.* Both halves belong in **`Assumptions`**.

---

## Q7 — Which classification?

**Not addressed here, by design.** Q7 is the ruling. Any classification stated
in this document would be the decision, taken by the wrong party and in the
wrong record.

The one procedural note worth carrying: ADR-0042's field description asks for
the classification "stated as a term of art with its source standard named" —
so whichever term is chosen (QM, safety-related but non-authoritative, or
another), it needs its standard attached.

---

## Q8 — What evidence preserves the classification over time?

**Already-running machinery that would qualify**, if the ruling chooses to rely
on it — all of these are enforced per-PR today:

| Mechanism | What it holds |
|---|---|
| `check_kirra_world_bidirectional_fence.py` | Fence A/B over the measured 19-package closure; fails on any new edge |
| `check_world_domain_logic_gate.py` | `kirra-world*` stays declaration-only until the ruling is recorded; **releases itself** when it is |
| `test_world_domain_logic_gate.py` | 38 tests, including t29 (posture cannot be silently deleted) and t30 (ruling fields stay open) |
| `check_mick_actuation_fence.py` | No dependency route from the intent sidecars to actuation |

**The gap to be aware of.** These are all *structural*. Q2, Q4 and Q5 concern
influence and semantics, and **no current gate produces recurring evidence for
any of them.** If the classification depends on those questions, `Required
evidence` will need something that does not exist yet — which is a finding
about scope of work, not an objection.

---

## Summary of what is and is not answerable from the repository today

| Q | State |
|---|---|
| Q1 | **Machine-answered** for linkage; open for influence |
| Q2 | **Open.** No evidence here bears on proposal-distribution effects |
| Q3 | **Currently no** — checker corridor is not map-derived; Phase-2 named as the change condition |
| Q4 | **Open, and coupled to Q2.** ODD membership is an undetected operator guarantee |
| Q5 | **Open.** Scope clarified (geometric bounds only); the call is unchanged |
| Q6 | **Conditionally yes** — independent and fail-closed when armed; all three channels default OFF |
| Q7 | **Not addressed.** This is the ruling |
| Q8 | Structural mechanisms exist and run; nothing recurring covers Q2/Q4/Q5 |

**Three of eight have material answers. Four are genuinely open. One is the
ruling itself.** That is the honest state, and it is worth saying plainly: this
document shortens the ruling's research, it does not shorten its judgement.
