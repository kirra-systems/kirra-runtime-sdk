# IEEE 2846 / RSS-Style Safety Extension for Kirra — Architecture Sketch and Execution Plan

**Status:** Pre-execution sketch. This extension is now mapped to **Increment 3 — Behavioral Safety (PARK-013 through PARK-019)** in `/work/roadmap.md`. The phases described below are the detailed specification for those tasks. Current active work: PARK-001/002/003 (Increment 1). Do not start IEEE 2846 implementation until Increment 1 is tagged (PARK-006) and QNX is unblocked (PARK-024).

**Audience:** You, future you, and any engineer who picks this up cold.

**Honest caveats up front:**

- The author of this document has not read IEEE 2846-2022. The standard must be purchased and read before any code is written against it. The technical content below is based on public knowledge of RSS-style safety models and standards-body framing, not on the standard text itself.
- "Improve on RSS" is explicitly NOT the goal. The goal is implementing IEEE 2846-conforming safety invariants in a vendor-neutral, auditable, integrable way that the market does not currently have. Fight on integration and transparency, not on the math.
- Patent landscape around RSS is non-trivial. IEEE 2846 was developed with explicit IEEE patent policy commitments; it is the safe legal foundation. Implementing RSS-the-original-paper directly is not.
- All effort estimates are rough. Multiply by 1.5x if you've never integrated with AV perception output before.

---

## 1. What this extension does

Kirra today evaluates commands against a kinematic envelope: physical limits of the vehicle, bounded by mechanical capability and operator policy. It does not consider what's *around* the vehicle.

This extension adds a second evaluation layer: **given what the AV stack's perception reports, does this command maintain safe distances and bounded responsibility relative to other actors?**

The extension is *not* perception. Kirra still does not have its own sensors. It subscribes to the AV stack's perception output and validates commands against it. If perception is wrong, this layer is wrong. That limitation is fundamental and must be stated honestly in every customer-facing artifact.

What this catches:
- Planner produces a colliding command despite perception correctly identifying the obstacle (planner bug)
- Planner produces a command violating safe-distance invariants given perception state (aggressive tailgating, unsafe cut-ins, etc.)
- Control execution drifts from plan in ways that violate safety envelopes (opportunistic catch)

What this does NOT catch:
- Perception missing an obstacle entirely (perception failure, out of scope)
- Misclassification of obstacle type leading to wrong safety parameters (perception failure, out of scope)
- Failures of physical actuators downstream of Kirra (actuator failure, separate safety layer)

---

## 2. Architectural placement

The existing Kirra evaluation pipeline:

```
Command in → [kinematic envelope check] → ValidatedCommand or SafetyFault
```

The extended pipeline:

```
Command in → [kinematic envelope check] ──┐
                                          ▼
                                 [RSS evaluator] ──→ ValidatedCommand or SafetyFault
                                          ▲
                                          │
                              Perception snapshot
                              (from AV stack subscription)
```

Both checks must pass for a command to be validated. Either failing produces a `SafetyFault` (existing mechanism) which feeds the existing audit chain, lockout state machine, and posture engine.

**Key architectural decision:** keep the RSS evaluator as a separate module, not folded into the existing kinematic governor. Rationale:

- Preserves existing test coverage and pure-function discipline of the kinematic envelope
- Isolates perception coupling to one module
- Allows customers to deploy Kirra with or without the RSS layer depending on whether they expose perception data
- Makes the IEEE 2846 conformance claim auditable in isolation

The RSS evaluator produces new `SafetyFault` variants that map to new `LockoutReason` variants. The existing fault → audit → lockout pipeline absorbs them without changes.

---

## 3. The data contract

The RSS evaluator needs a perception snapshot. Define it as a stable input type independent of any AV stack's specific message format:

```rust
pub struct PerceptionSnapshot {
    pub timestamp_us: u64,
    pub ego: EgoState,
    pub actors: heapless::Vec<TrackedActor, MAX_ACTORS>,
}

pub struct EgoState {
    pub position: Vec2,           // local frame, meters
    pub velocity: Vec2,           // m/s
    pub heading_rad: f32,
    pub speed_mps: f32,
}

pub struct TrackedActor {
    pub id: u32,
    pub kind: ActorKind,          // Vehicle / Pedestrian / Cyclist / Unknown
    pub position: Vec2,           // relative to ego, meters
    pub velocity: Vec2,           // m/s
    pub bbox_extent: Vec2,        // half-extent meters
    pub heading_rad: f32,
    pub confidence: f32,          // [0.0, 1.0]
    pub age_ms: u32,              // how long this track has existed
}

pub enum ActorKind {
    Vehicle,
    Pedestrian,
    Cyclist,
    StaticObject,
    Unknown,
}
```

Then the AV-stack-specific bridge code (Apollo's `PerceptionObstacles`, Autoware's `PredictedObjects`, ROS2's whatever) transforms into this canonical type. The RSS evaluator only ever sees the canonical type. This isolates AV-stack churn from Kirra core.

`MAX_ACTORS` is a compile-time bound for the no_std-friendly path. 64 is a defensible starting number (more than enough for any realistic intersection scene). Customers running on Linux with std can override to use `Vec` instead of `heapless::Vec` via a feature flag.

---

## 4. The evaluation logic (skeleton, not the math)

The RSS evaluator's job, given a `PerceptionSnapshot` and a proposed `OperationalCommand::WriteState`:

```
For each tracked actor:
    Compute the predicted ego trajectory under the proposed command for T seconds
    Compute the predicted actor trajectory under IEEE 2846 worst-case assumptions
    Check the minimum distance between predicted trajectories
    If minimum distance < required safe distance for this actor type:
        Return Err(SafetyFault::RssLongitudinalViolation { actor_id, ... })
            or RssLateralViolation, etc.
Return Ok(())
```

The detailed math — what counts as "required safe distance," what response times and decelerations to assume, how to handle lateral vs longitudinal cases — comes from IEEE 2846. **Do not write the math from memory. Implement directly from the standard text.** Cite the section number in code comments so reviewers can verify.

The output of the evaluator is structural: a `Result<(), RssFault>` where `RssFault` carries enough detail for forensic reconstruction:

```rust
pub enum RssFault {
    LongitudinalViolation {
        actor_id: u32,
        actor_kind: ActorKind,
        required_distance_m: f32,
        actual_distance_m: f32,
        time_to_violation_s: f32,
    },
    LateralViolation { /* similar */ },
    DangerousResponseRequired { actor_id: u32 },
    /* ... */
}
```

Map each variant to a `SafetyFault` enum variant that the existing governor knows how to handle. Audit chain entries should capture the full fault detail; the on-the-wire lockout reason can be coarser.

---

## 5. Implementation breakdown

### Phase 0: Read the standard (1-2 weeks)

Goals: actually understand IEEE 2846 before designing against it. This is non-negotiable. Without this, every later phase is built on guesses.

Tasks:
1. Purchase IEEE 2846-2022 from the IEEE Standards Store
2. Read it. Take notes. Identify the specific equations, parameter sets, and assumptions you'll need to implement.
3. Identify the conformance claims you can make. IEEE 2846 specifies *assumptions* about other actors; your implementation is conformant if it correctly applies those assumptions in safety calculations.
4. Identify the gaps. Where does the standard punt on implementer choice? Document those choices for later defense.

**Done when:** you can summarize IEEE 2846 in your own words in a 1-page doc, and you have a list of specific equations and parameters extracted from the standard.

### Phase 1: Canonical perception types and bridge (1-2 weeks)

Goals: define the `PerceptionSnapshot` data contract and write one AV-stack bridge (Apollo recommended, since the Apollo integration is the prior step).

Tasks:
1. Implement `PerceptionSnapshot`, `EgoState`, `TrackedActor`, `ActorKind` in `parko-core`
2. Implement the Apollo `PerceptionObstacles` → `PerceptionSnapshot` transform in the Apollo bridge component (extending the bridge built during the Apollo integration phase)
3. Add tests for the transform with sample protobuf inputs

**Done when:** Apollo's perception output can be deserialized into a `PerceptionSnapshot` and round-tripped through tests.

### Phase 2: RSS evaluator core (3-4 weeks)

Goals: implement the IEEE 2846 safety distance calculations as pure functions, testable against published reference cases.

Tasks:
1. Implement longitudinal safe-distance function: `safe_long_distance(ego, lead, params) -> f32`
2. Implement lateral safe-distance function
3. Implement multi-actor evaluation loop
4. Implement `RssFault` and its mapping to `SafetyFault` / `LockoutReason`
5. Add new `LockoutReason` variants: `RssLongitudinalViolation`, `RssLateralViolation`, `RssDangerousResponse`
6. Write a comprehensive unit test suite using IEEE 2846 example scenarios if the standard includes them, plus synthetic scenarios

**Done when:** the evaluator passes its unit test suite, and a hand-crafted "ego too close to lead vehicle at high speed" scenario produces the expected `RssLongitudinalViolation` fault.

### Phase 3: Parameter calibration (1-2 weeks)

Goals: calibrate the assumed response times, worst-case decelerations, and other parameters for the target vehicle and operational design domain.

Tasks:
1. Identify the parameter set IEEE 2846 requires
2. Source initial values from published literature, vehicle specifications, or operator policy
3. Implement a parameter pack mechanism (default pack + customer-override pack)
4. Document each parameter and its source/justification

**Done when:** you have a documented default parameter pack for a passenger-vehicle ODD and a separate one for a small mobile-robot ODD, with citations for every value.

### Phase 4: Integration into the governor pipeline (1 week)

Goals: wire the RSS evaluator into the existing `evaluate_command_safety` path.

Tasks:
1. Extend `evaluate_command_safety` to accept an optional `&PerceptionSnapshot` argument
2. After the existing kinematic envelope check passes, run the RSS evaluator
3. Translate RSS faults to `SafetyFault` variants
4. Ensure audit chain captures the full `RssFault` detail
5. Update the Apollo bridge to pass perception snapshots into Kirra evaluation

**Done when:** end-to-end, an Apollo scenario where the planner produces a tailgating command results in an Kirra lockout with an audit entry that names the actor and the distance violation.

### Phase 5: Demo scenarios (2 weeks)

Goals: 4-6 simulation scenarios that demonstrate the RSS layer catching specific failure modes.

Scenario ideas:
1. **High-speed tailgating** — planner closes to unsafe longitudinal distance at 30 m/s. Kirra blocks throttle, clamps to maintain safe distance.
2. **Unsafe lane change** — planner commands a lane change into a gap that's too small at current closing speeds. Kirra rejects.
3. **Pedestrian within RSS exclusion zone** — perception reports a pedestrian within a kinematically reachable zone. Kirra enforces a hard speed cap.
4. **Cyclist in adjacent lane** — different ActorKind triggers different RSS parameters; Kirra enforces wider lateral margin than for a vehicle.
5. **Cut-in scenario** — another vehicle cuts in front of ego; Kirra enforces emergency braking response within IEEE 2846 assumed response time.
6. **Perception confidence threshold** — low-confidence detection (confidence < 0.6); Kirra treats it conservatively but distinguishes from high-confidence detection in audit chain.

Each scenario gets a side-by-side recording (Apollo alone vs. Apollo + Kirra + RSS) and a one-page analysis.

**Done when:** you have 4-6 scenarios with comparative recordings and analyses.

### Phase 6: Writeup (1-2 weeks)

Goals: the public artifact. See section 7 below for the outline.

---

## 6. Risks and known gotchas

**Latency budget.** The RSS evaluator runs on every command in the control loop. With perception of 30+ actors and IEEE 2846 calculations per actor, this is non-trivial work. Benchmark early. Target: well under 5ms on the deployment hardware. If you can't hit it, scope cuts: limit max actors evaluated, prioritize closest actors, downsample evaluation rate.

**Perception staleness.** A perception snapshot is a snapshot. By the time Kirra evaluates against it, the snapshot is 10-50ms old. RSS calculations must account for the time delta between snapshot timestamp and evaluation time, propagating actor positions forward. Get this wrong and you'll have spurious violations near the perception cycle boundary.

**Parameter calibration is harder than the math.** IEEE 2846 specifies what assumptions to make; it doesn't tell you the numbers. Choosing assumed response time 0.5s vs 1.0s changes behavior dramatically. Conservative parameters mean frequent interventions (annoying); aggressive parameters mean missed violations (unsafe). There is no universally correct value. Document your choices and let customers override.

**False positive management.** The RSS layer will produce false positives — situations where the math says unsafe but human drivers would have judged safe. This is acceptable for a safety governor (better to be conservative) but unacceptable if it makes the vehicle undrivable. Have a tunable suppression mechanism: if the same actor has triggered N violations in the last T seconds and none resulted in actual incidents, downgrade the response. Be very careful with this — it's the kind of feature that becomes a vector for shipping unsafe defaults.

**Perception coupling visibility.** When the system intervenes, the customer needs to know *why*. The audit chain entry must name the actor that triggered the violation, with enough detail that a reviewer can trace back to the perception output. Without this, every intervention looks like a black box, which defeats the transparency value proposition.

**The "improve on RSS" temptation.** Phase 0 will tempt you to identify "obvious improvements" to the IEEE 2846 math. Resist this. Your value proposition is *implementation fidelity and transparency*, not novel safety theory. If you genuinely find a gap in the standard, file it as a comment with the IEEE working group rather than implementing your own variant. Conforming-to-standard is your moat. Diverging-from-standard is a liability.

**RSS-originator patent surface.** IEEE 2846 was developed with patent policy commitments; conformant implementations should be safe. But: implementing equations that look very similar to specific RSS paper equations, even via IEEE 2846, may attract attention. Have legal counsel review the implementation before public release. This is genuine advice, not paranoia — the Tier-1 ADAS benchmark vendor is a Goliath and they have lawyers.

**Multi-actor combinatorics.** Evaluating every actor against every other actor (for multi-agent interactions) is O(N²). For 30 actors that's 900 evaluations per cycle. At 100Hz that's 90,000 per second. Cache aggressively, evaluate only actors within reachable zones, profile early.

**Testing without real-world data.** Your initial test scenarios will be hand-crafted. They will reflect your assumptions about what failures look like. Real-world failures look different. Once you have a customer running this in real testing (even simulation), capture *every* intervention as a regression test case. Build the corpus over time.

---

## 7. Writeup outline

Title (working): **Vendor-Neutral IEEE 2846 Safety Governance: Adding Behavioral Safety Invariants to Kirra**

Target venue: blog post first, then a workshop submission to IV (Intelligent Vehicles), ITSC (Intelligent Transportation Systems), or SafeAI workshop. If the Apollo writeup exists, this is its sequel.

**Length:** 3000-5000 words plus diagrams and demo videos.

### Section outline

1. **Why this exists** (300 words)
   - The shift to end-to-end planners makes behavioral safety analysis harder
   - The gap between vehicle-physics safety and traffic-behavior safety
   - The thesis: vendor-neutral, auditable behavioral safety enforcement is a real market gap

2. **Why IEEE 2846 and not RSS directly** (400 words)
   - RSS's contribution: a formal model for assumptions about other actors
   - IEEE 2846's role: standards-body formalization of those assumptions, patent-clean
   - What Kirra implements: IEEE 2846 conformance with transparent enforcement
   - What Kirra explicitly does NOT claim: "improvement on" RSS or novel safety theory

3. **The architectural placement** (500 words plus diagrams)
   - Existing Kirra kinematic envelope check
   - Added RSS evaluator as a separate module
   - Perception data contract (`PerceptionSnapshot`)
   - Bridge from AV-stack-specific perception (Apollo example)
   - Audit chain integration

4. **The data contract** (300 words)
   - Canonical perception types
   - Why a canonical type instead of AV-stack-specific
   - How customers integrate with non-Apollo stacks

5. **The evaluation logic** (600 words)
   - IEEE 2846 safe-distance calculations (cite specific sections)
   - Multi-actor handling
   - Parameter packs and calibration
   - Latency and performance characteristics

6. **Demo scenarios** (1000-1500 words)
   - 4-6 scenarios from Phase 5
   - Side-by-side: Apollo alone vs. Apollo + Kirra + RSS
   - Audit chain entries showing the transparency value

7. **Honest limitations** (500 words)
   - This depends on perception correctness; perception failures are out of scope
   - Parameter calibration is operator responsibility
   - False positive management is non-trivial
   - Simulation results are not real-world safety
   - This is a demonstration, not a certification

8. **What's next** (200 words)
   - Per-ODD parameter packs
   - Real-vehicle pilot
   - Formal verification of the evaluator (longer-term research direction)
   - Certification path against IEC 61508 / ISO 26262 SEooC

9. **Call to action** (100 words)
   - Open-source the RSS evaluator and bridge code
   - Invite AV teams to evaluate against their stacks
   - Direct contact for pilot inquiries

### What the writeup is NOT

- Not a claim of novel safety theory
- Not a critique of the Tier-1 ADAS benchmark vendor or RSS
- Not a certification claim
- Not a marketing piece — the honest limitations section is non-negotiable

---

## 8. Estimated timeline

Assumes one engineer focused, with QNX and the Apollo integration already shipped.

| Phase | Calendar |
|-------|----------|
| Phase 0: Read the standard | Week 1-2 |
| Phase 1: Canonical types and bridge | Week 2-3 |
| Phase 2: RSS evaluator core | Week 3-6 |
| Phase 3: Parameter calibration | Week 6-7 |
| Phase 4: Governor pipeline integration | Week 7-8 |
| Phase 5: Demo scenarios | Week 8-10 |
| Phase 6: Writeup | Week 10-11 |
| Buffer | Week 11-13 |

Total: 11-13 weeks (about one quarter) elapsed for a publishable demo with writeup.

Scope cuts if you need to ship faster:
- Phase 5: ship with 2-3 scenarios instead of 4-6
- Phase 3: ship with one parameter pack instead of two
- Phase 0: cannot cut. Reading the standard is non-negotiable.

---

## 9. Deliverable inventory

When this is done, you have:

1. An RSS evaluator module in `parko-core` with comprehensive unit tests
2. A canonical `PerceptionSnapshot` data contract
3. An Apollo perception bridge (extending the Apollo integration's bridge)
4. Parameter packs for at least one ODD
5. 4-6 simulation demo scenarios with comparative recordings
6. A 3000-5000 word public writeup
7. Updated audit chain documentation showing the new fault variants
8. A defensible IEEE 2846 conformance claim, citable in customer conversations

The eighth item is the business value. Everything else is supporting evidence.

---

## 10. When to pick this up

This maps to **Increment 3** in `/work/roadmap.md` (PARK-013 through PARK-019). Do not start until:

1. **PARK-001–006** — Increment 1 complete (parko-core v0.1.0 tagged)
2. **PARK-007–012** — Increment 2 complete (InferenceBackend finalized, MockBackend in place)
3. **PARK-024** — QNX deployment spike resolved (TIME-SENSITIVE — do not let this block unrelated work, but it must not be abandoned)

PARK-to-phase mapping:
- Phase 0 (read standard) → prerequisite for PARK-013
- Phase 1 (canonical types) → maps to `PerceptionSnapshot` struct in parko-core
- Phase 2 (evaluator core) → PARK-013 (longitudinal) + PARK-014 (lateral)
- Phase 3 (parameter calibration) → part of PARK-013/014 acceptance criteria
- Phase 4 (governor integration) → PARK-015 (posture engine) + PARK-016 (KirraGovernor gate)
- Proptest suite → PARK-017
- Audit chain → PARK-018
- Simulation scenarios → PARK-019

There is no scenario in which RSS extension is more urgent than QNX. If you find yourself wanting to start this before PARK-024 ships, recognize that as a procrastination signal and go back to QNX.

When you do start, the first action is buying IEEE 2846 and reading it. Not coding. Reading. Two weeks of phase 0 prevents three months of phase 2 rework.
