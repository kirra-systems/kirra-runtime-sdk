# Apollo + Aegis Integration — Architecture Sketch and Execution Plan

**Status:** Pre-execution sketch. Drafted while QNX resource manager work is the active priority. Pick this up when QNX ships and TPM is integrated.

**Audience:** You, future you, and any engineer who picks this up cold.

**Honest caveats up front:**
- Drafted from public knowledge of Apollo's architecture. The specific topic names, message types, and module boundaries below may have shifted between Apollo versions. Verify against the current Apollo release when execution starts (Apollo 9.0 is the latest I'm aware of; there may be a 10.x by the time you read this).
- The 2-4 week effort estimate assumes simulation only. Real-vehicle integration is multiples of that.
- "What Apollo does" descriptions are based on the public docs and code; I have not personally run Apollo end-to-end.

---

## 1. What Apollo is (one-paragraph refresher)

Apollo is Baidu's open-source autonomous driving stack, GitHub `ApolloAuto/apollo`. Modules communicate over **Cyber RT**, a pub/sub middleware similar to ROS2 DDS but built for deterministic real-time scheduling. Apollo ships with **Dreamview**, a web-based simulator and visualization tool. It targets Linux (Ubuntu 18.04/20.04 historically) and has QNX support for production deployments. The user base includes Baidu's own ApolloGo robotaxi service and licensees in the Chinese OEM ecosystem (BYD, Geely, Chery at various points).

The module pipeline, simplified:

```
Sensors → Perception → Prediction → Routing/Planning → Control → Canbus → Vehicle
                                                                  │
                                                                  └─ HMI / Monitor / Recording
```

Each arrow above is a Cyber RT channel (their term for a topic). Each module is a separate process (or set of processes) with its own scheduling and isolation.

---

## 2. Where Aegis inserts

Two candidate insertion points. They have different cost/value tradeoffs.

### Option A: Between Control and Canbus (RECOMMENDED for first integration)

```
Planning → Control → [Aegis Governor] → Canbus → Vehicle
                          │
                          └─ Posture / Audit / Federation
```

**Why this is the right first cut:**
- Control's output is a concrete `ControlCommand` (throttle %, brake %, steering angle, gear). Aegis already evaluates exactly this kind of command.
- Insertion is *transparent* to Apollo — Control still publishes its commands on `/apollo/control`, but Canbus subscribes to `/apollo/control/validated` (the Aegis output topic) instead of the original.
- If Aegis goes offline or fails open, you can revert by re-pointing Canbus at the original topic in the Cyber RT config. Operational rollback is one config line.
- The kinematic envelope check is the headline scenario — clamping a high-acceleration command from a hallucinating planner is the demo that closes pitches.

**What Aegis evaluates here:**
- Bounded throttle / brake rates (no instant 0→100% commands)
- Steering angle within physical limits, steering rate within mechanical limits
- Cross-checks: gear position vs. velocity (no reverse at 60 km/h), brake-and-throttle exclusion, etc.
- Lockout state propagation from posture engine

### Option B: Between Planning and Control (LATER)

```
Prediction → Planning → [Aegis Trajectory Filter] → Control → Canbus → Vehicle
```

**Why not first:**
- Planning's output is a trajectory (a sequence of (x, y, t, v) waypoints), not a single command. Aegis would need to evaluate the trajectory as a whole — every waypoint must satisfy the envelope, plus inter-waypoint feasibility.
- This is genuinely useful — it catches unsafe *plans* before they become unsafe *commands* — but it's a deeper integration and a different kind of evaluation than Aegis currently does.
- Save it for v2.

### Option C (mentioned for completeness, do not pursue)

Replacing or modifying Apollo's perception or prediction outputs. This is out of scope. Aegis is not a perception system.

---

## 3. The concrete architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                            Apollo Stack                              │
│                                                                      │
│  Perception → Prediction → Planning → Control                        │
│                                            │                         │
│                                            ▼                         │
│                                  /apollo/control                     │
└────────────────────────────────────────────┼─────────────────────────┘
                                             │
                                             │ Cyber RT subscription
                                             ▼
              ┌─────────────────────────────────────────────────┐
              │              Aegis Cyber RT Bridge              │
              │                                                 │
              │  1. Receive ControlCommand                      │
              │  2. Transform → OperationalCommand::WriteState  │
              │  3. Submit to Aegis governor                    │
              │  4. Receive (ValidatedCommand, StateTransition) │
              │     or SafetyFault                              │
              │  5. On Ok: republish validated ControlCommand   │
              │  6. On Err: publish lockout, hold last-known    │
              │     safe state                                  │
              └─────────────────────────────────────────────────┘
                                             │
                                             ▼
                          /apollo/control/aegis_validated
                                             │
                                             │ Cyber RT subscription
                                             ▼
              ┌─────────────────────────────────────────────────┐
              │             Apollo Canbus Module                │
              │   (config patched to subscribe to validated     │
              │    topic instead of original /apollo/control)   │
              └─────────────────────────────────────────────────┘
                                             │
                                             ▼
                                  Vehicle CAN bus
```

### Sideband: Posture, Audit, HMI

```
              ┌─────────────────────────────────────────────────┐
              │              Aegis Governor State               │
              └──────────────┬───────────────┬──────────────────┘
                             │               │
                             ▼               ▼
              /apollo/aegis/posture   /apollo/aegis/audit
                             │               │
                             ▼               ▼
           ┌────────────────────┐   ┌─────────────────────┐
           │  Apollo HMI /      │   │  Aegis audit chain  │
           │  Dreamview overlay │   │  → flash/disk write │
           │  shows lockout     │   │  → SSE export       │
           └────────────────────┘   └─────────────────────┘
```

The **posture topic** carries `FleetPosture` (Nominal / Degraded / LockedOut) and reason codes. Apollo's monitor module subscribes and exposes the state in Dreamview, so an operator sees *why* Aegis intervened.

The **audit topic** is the tamper-evident chain. It's not consumed by Apollo — it's exported via the existing Aegis audit chain mechanisms to whatever storage backend you're using.

---

## 4. Implementation breakdown

### Phase 0: Environment (1-3 days)

Goals: build Apollo from source, run Dreamview, run a default planning demo, confirm you can see ControlCommands flowing on `/apollo/control`.

Tasks:
1. Clone `ApolloAuto/apollo`, follow their Docker-based build instructions
2. Launch Dreamview with the default Sunnyvale Loop demo map
3. Use `cyber_monitor` to inspect `/apollo/control` messages live
4. Capture a recording (Cyber RT record format) of a normal driving session for use in later test scenarios

**Done when:** you can replay a recording and see ControlCommands at ~100Hz.

### Phase 1: Aegis bridge skeleton (3-5 days)

Goals: a Rust/C++ process that subscribes to `/apollo/control`, deserializes it, prints it, and republishes unchanged to `/apollo/control/aegis_validated`. No actual safety logic yet.

Tasks:
1. Decide language. Apollo's native idiom is C++; Cyber RT has C++ APIs first. Rust bindings exist (third-party, search for `cyber_rs` and similar) but are less mature. **Recommendation: thin C++ shim that talks to Aegis over a local IPC, with Aegis itself remaining in Rust.** This keeps Aegis's core unchanged and isolates Apollo-specific dependencies.
2. Generate the protobuf bindings for Apollo's `ControlCommand` (proto file lives in `apollo/modules/common_msgs/control_msgs`)
3. Write the bridge as a Cyber RT component (Apollo's plugin model)
4. Patch the Apollo Canbus configuration to subscribe to the validated topic
5. Run end-to-end. Verify vehicle behavior in simulation is unchanged when the bridge is a no-op pass-through.

**Done when:** Apollo's simulation runs identically with the bridge in the loop as without.

### Phase 2: Aegis evaluation hook (5-7 days)

Goals: actual safety logic in the loop. Bridge submits each ControlCommand to Aegis, Aegis evaluates, bridge respects the result.

Tasks:
1. Define the mapping from `ControlCommand` (throttle/brake/steering) to Aegis's `OperationalCommand::WriteState`. This is a vehicle-model-specific transform — throttle % → acceleration target, steering angle → angular velocity command, etc. Get this wrong and Aegis evaluates against incorrect physics.
2. Wire the bridge to call Aegis's evaluation API (via IPC or, if you decide on a single-process design, direct Rust FFI)
3. Implement the failure modes:
   - `Ok(validated, transition)` → republish validated command, commit transition
   - `Err(fault)` → republish last-known-safe command (or zero throttle, full brake, neutral steering depending on policy), publish lockout on posture topic, append to audit chain
4. Add the posture publisher on `/apollo/aegis/posture`
5. Add the audit publisher on `/apollo/aegis/audit`

**Done when:** in simulation, an injected over-envelope command (e.g., 100% throttle from a stop while in reverse) gets rejected by Aegis and you see the lockout in Dreamview.

### Phase 3: Demo scenarios (3-5 days)

Goals: a small library of failure-mode scenarios that show what Apollo does versus what Apollo + Aegis does.

Scenario ideas (these are sketches; refine when you have Apollo running):

1. **Hallucinated lane change at speed.** Force Planning to produce a trajectory that demands lateral acceleration above ~4 m/s² at 60 km/h. Apollo alone: vehicle skids or rolls. Apollo + Aegis: command clamped to safe lateral g, gentle lane change, posture stays Nominal.

2. **Post-collision behavior.** Inject a "collision detected" signal (Apollo has crash detection in its monitor module). Apollo alone with default policy: pullover maneuver executes. Apollo + Aegis configured with a conservative post-collision policy: lockout asserted, no movement until supervisor reset. *Direct reference to the Cruise 2023 incident.*

3. **Sensor degradation cascade.** Disable perception's lidar fusion mid-drive. Apollo alone: planner gets degraded inputs and may produce erratic commands. Apollo + Aegis: posture engine detects sensor fault propagation via DAG, transitions to Degraded, restricts to ReadTelemetry, vehicle comes to a controlled stop.

4. **AI command flooding.** Apollo's planner stuck in a loop producing 1000 commands/sec. Apollo alone: Canbus gets overwhelmed or commands get out of phase. Apollo + Aegis: bounded ring buffer, rate limiting at the governor boundary.

Each scenario gets:
- A scripted Cyber RT recording or Dreamview replay
- Comparative video (Apollo alone vs. Apollo + Aegis)
- A short analysis: what Apollo did, what Aegis allowed, why

**Done when:** you have 3-4 scenarios with side-by-side recordings and a one-page analysis each.

### Phase 4: Writeup (2-3 days)

Goals: the public artifact. See section 6 below for the outline.

---

## 5. Risks and known gotchas

**Cyber RT version drift.** Apollo's middleware ABI has changed between major releases. The bridge code is the most version-sensitive piece. Pin to a specific Apollo release tag during development and document it.

**Protobuf coupling.** Apollo's message types are protobufs that evolve. Generate them at build time from the Apollo source tree rather than vendoring stale copies.

**Determinism in simulation.** Apollo's simulator is mostly deterministic but not perfectly. Comparative demos (Apollo alone vs. Apollo + Aegis) should use Cyber RT *recordings* as inputs, not live planning, to ensure the input stream is identical across runs. Otherwise you can't tell whether differences are from Aegis or from planner non-determinism.

**Kinematic model mismatch.** Aegis's existing kinematic envelope is bicycle-model-based with parameters for some asset types. Apollo's default vehicle is the Lincoln MKZ; its physical limits may differ from Aegis's defaults. Calibrate the envelope to the Apollo vehicle before running scenarios, or scenarios will fire incorrectly.

**Apollo's own safety modules.** Apollo has a `guardian` module that does some safety filtering already. Two-layer safety filtering is fine in principle but creates ambiguity in scenarios — which layer rejected what? Either disable guardian for the demo (cleaner story) or explicitly characterize Aegis as complementary to guardian (more nuanced story). Pick one and be consistent.

**Latency budget.** The bridge adds at least one IPC hop on the control path. Apollo's control loop targets ~100Hz (10ms cycle). The Aegis evaluation must complete in well under 10ms or you've broken Apollo's control loop. Benchmark this before scenario work. If latency is a problem, consider co-locating Aegis evaluation inside the bridge process (Rust → C++ FFI) rather than IPC.

**The post-collision scenario specifically.** Referencing the Cruise incident directly is powerful but politically charged. Some readers will see "Aegis would have saved her" as overclaiming. Frame it as: "Independent runtime safety governance with a conservative post-collision policy would have prevented the dragging maneuver. The policy is the operator's choice; Aegis is the mechanism for enforcing it independently of the AV stack." Don't claim Aegis prevents collisions. Claim Aegis enforces policy independently.

---

## 6. Writeup outline

Title (working): **Independent Runtime Safety Governance for End-to-End AV Stacks: An Apollo + Aegis Integration**

Target venue: blog post on your own site first, then republish on Medium / LinkedIn. If it lands well, expand into a workshop paper for IV (Intelligent Vehicles), ITSC (Intelligent Transportation Systems), or SafeAI workshop at AAAI.

**Length target:** 2500-4000 words plus diagrams and demo videos.

### Section outline

1. **Why this exists** (300 words)
   - The shift to end-to-end neural planning makes safety analysis harder
   - The 2023 Cruise incident as a case study in policy-versus-mechanism separation
   - The thesis: independent runtime safety governance becomes more important, not less, as AI stacks get less interpretable

2. **What Aegis is** (300 words)
   - One-paragraph product description
   - The architectural role: between AI planner and physical actuator
   - What Aegis does *not* do (perception, planning, prediction — defer to the AV stack)
   - Existing integrations (ROS2, industrial protocols) as evidence of the pattern

3. **What Apollo is** (200 words)
   - Brief overview of the Apollo stack and its user base
   - The module pipeline and Cyber RT
   - Why Apollo specifically — open source, large user base, real production deployments

4. **The integration architecture** (600 words plus diagrams)
   - Insertion point: between Control and Canbus
   - The bridge component and its responsibilities
   - Posture and audit sideband
   - Configuration changes to Apollo (the one-line Canbus subscriber patch)
   - What's preserved (Apollo's perception, planning, control) and what's added (envelope enforcement, lockout, audit)

5. **Demo scenarios** (1000-1500 words, the meat of the post)
   - Each of the 3-4 scenarios from Phase 3
   - For each: setup, what Apollo alone does, what Apollo + Aegis does, why the difference matters
   - Embedded videos or animated GIFs
   - The post-collision scenario carries the Cruise reference; handle carefully per the framing note above

6. **Honest limitations** (400 words)
   - What this integration does *not* prove
   - Aegis cannot prevent collisions, only enforce policy after a collision is detected
   - Simulation results do not equal real-vehicle safety
   - Production deployment requires per-vehicle calibration, certification, integration with the OEM's safety case
   - This is a *demonstration*, not a *certification*

7. **What's next** (200 words)
   - QNX deployment (referencing your existing QNX work)
   - Trajectory-level filtering (Option B)
   - Real-vehicle pilot with a willing partner
   - Standards conformance (link to your standards matrix)

8. **Call to action** (100 words)
   - Open-source the bridge code on GitHub
   - Invite Apollo and AV teams to evaluate Aegis on their stack
   - Direct contact for partnership / pilot inquiries

### What the writeup is NOT

- Not a Cruise post-mortem. Cruise gets one paragraph as motivation, not a chapter.
- Not a marketing piece. Honest limitations section is non-negotiable; cutting it kills the credibility that makes the rest land.
- Not an academic paper. Save formal evaluation for a workshop submission later.
- Not a claim of certification or production-readiness.

---

## 7. Estimated timeline

Assumes one engineer (you), focused, with QNX work already done.

| Phase | Calendar |
|-------|----------|
| Phase 0: Environment | Week 1 |
| Phase 1: Bridge skeleton | Week 1-2 |
| Phase 2: Aegis evaluation hook | Week 2-3 |
| Phase 3: Demo scenarios | Week 3-4 |
| Phase 4: Writeup | Week 4 |
| Buffer for inevitable surprises | Week 5 |

Total: 4-5 weeks elapsed for a publishable demo with writeup.

If 4-5 weeks looks long, the scope cut is to drop scenarios 3 and 4, ship with just the kinematic envelope clamp and the post-collision lockout. Two scenarios is enough for a credible writeup.

---

## 8. Deliverable inventory

When this is done, you have:

1. A public GitHub repo with the Apollo + Aegis bridge code (Apache 2 license to match Apollo)
2. A configuration patch document showing how to integrate Aegis into an Apollo deployment
3. 3-4 demo scenarios with recorded comparative videos
4. A 2500-4000 word writeup on your site
5. A submission-ready workshop paper outline if you want to take it to academia
6. Concrete reference to point at in WeRide / partnership conversations: "here's Aegis integrated with Apollo, here's what it caught, here's what it didn't"

That last item is the actual business value. The code and writeup are evidence; the *conversation enabler* is the artifact.

---

## 9. When to pick this up

Do not start this while QNX is on the 30-day clock. The order is:

1. Finish QNX resource manager
2. TPM integration
3. Tag v1.0.5
4. Robot arrives, ROS2 demo
5. WeRide architecture doc
6. *Then* this Apollo work

If something is going wrong in (1)-(5) and you find yourself wanting to context-switch into Apollo because Apollo is more fun than QNX, **that is a signal, not a plan.** Apollo is the right next move *after* the acquisition checklist is closed, not as an escape from the checklist.
