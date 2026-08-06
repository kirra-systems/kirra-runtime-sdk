# ADR-0039 (WM-6): Preserve bidirectional independence between the World Model and the Governor

| Field | Value |
|---|---|
| Status | **Proposed — NOT ratified on merge.** See *Ratification criteria*. Merging records the proposal; it does not ratify it and authorizes no implementation. |
| Date | 2026-08-02 |
| Blueprint | `KIRRA-WM-ARCH-001` §18 (WM-6) — [`docs/design/WORLD_MODEL_ARCHITECTURE.md`](../design/WORLD_MODEL_ARCHITECTURE.md) |
| Deciders | Governor / safety-case owner · World Model owner · architecture owner · safety-assurance owner |
| **Clarified by** | **[ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md)** — canonical terminology, semantic-map vs safety-corridor boundary, transitive Fence B, language independence, and the PENDING assurance ruling. **Read 0042 alongside this ADR; it supersedes the scope claim below.** |
| Safety goals | **SG7** (the safety check is invariant to the command's origin) · **SG1** (the checker's bound is what holds) |
| Cross-refs | [`ADR-0020`](0020-doer-invariant-safety-case.md) (doer-invariant safety case) · [`ADR-0031`](0031-release-token-on-the-actuation-path.md) · [`ADR-0033`](0033-actuation-authority-ros-r2-topology.md) · [`ci/check_mick_actuation_fence.py`](../../ci/check_mick_actuation_fence.py) · [`crates/kirra-trajectory/src/validation.rs`](../../crates/kirra-trajectory/src/validation.rs) · [`robot/world_model.py`](../../robot/world_model.py) §5.1 |

> **Deviation from repository convention, stated deliberately.** Most ADRs here
> carry *"Proposed (design note) — ratified on merge."* These three World Model
> ADRs do **not**. Their ratification criteria include things a merge cannot
> satisfy — named owner reviews, a measured Jetson prototype, a replay
> benchmark. Marking them ratified-on-merge would claim an approval that had
> not happened. See §*Ratification criteria*.

---

## Terminology (canonical — [ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md) Decision 1)

**Kirra World** is the semantic evidence subsystem this ADR fences. It is
distinct from an **independent perception channel** — the redundant perception
inputs that `perception_redundancy.rs` unfortunately also calls "world models".
Bare *"world model"* is not canonical; this ADR's uses refer to **Kirra World**.

## Context

The blueprint's central safety property is that the World Model is **not** a
safety dependency. This ADR ratifies that as two independent rules, in both
directions, before any `kirra-world` code exists.

The order matters. Dependency direction is cheap to establish on a green field
and expensive to reverse afterwards: once the checker reads semantic knowledge,
removing the edge means re-opening the safety argument, and the knowledge layer
inherits whatever assurance burden the checker carries.

**Investigation finding — the codebase already conforms, and that is the
argument for acting now.** The checker's entry point is:

```rust
// crates/kirra-trajectory/src/validation.rs:235
pub fn validate_trajectory_slow(
    trajectory: &[TrajectoryPoint],
    corridor:   &dyn CorridorSource,
    objects:    &[PerceivedObject],
    config:     &VehicleConfig,
    latest_odom: Option<&EgoOdom>,
    posture:    FleetPosture,
) -> TrajectoryVerdict
```

There is **no goal parameter, no destination, no semantic reference, and no
knowledge handle**. `crates/kirra-trajectory` depends on exactly `kirra-core`
and `parko-core`. The semantic destination resolver already influences only the
*proposal*: `handle_plan_with_destinations` resolves a destination into the
planner's `target`, and the checker then bounds the resulting trajectory
against inputs it receives directly.

This ADR therefore **codifies existing practice** rather than imposing a new
constraint. That is the cheapest moment to make it structural.

---

## Decision drivers

1. **Assurance scope containment.** A knowledge layer inside the safety
   envelope inherits the full qualification treatment. Outside it, with a
   *proven* absence of influence, it can evolve at product speed.
2. **Independent checking.** ADR-0020 established that the verdict is a pure
   function of `(trajectory, world)` with no doer parameter. Admitting a
   knowledge input would reintroduce exactly the coupling that ADR removed.
3. **Common-cause failure.** If the planner's knowledge and the checker's
   evidence share a source, a defect in that source corrupts proposal and check
   together — and the check exists precisely to be independent of the proposal.
4. **Staleness semantics differ.** The World Model answers `Unknown` past a
   TTL and is content to be uncertain. The safety path must fail closed to a
   minimum-risk response. Blending the two makes "stale" ambiguous at the one
   boundary where it must not be.
5. **Reversal cost asymmetry.** Fence A can be added later at the cost of a
   refactor. Fence B added later may cost a safety-case revision.

---

## Options considered

| # | Option | Assessment |
|---|---|---|
| A | **Both fences, CI-enforced** | **Proposed.** Codifies current practice; cheapest now |
| B | Fence A only (knowledge cannot act) | Rejected — leaves the expensive direction unguarded |
| C | Convention only, no CI | Rejected — the repo's own precedent is that a convention without a gate erodes |
| D | Allow the checker to read World Model under review | Rejected — "under review" is not a structural property; see *Exceptions* |
| E | No fence; rely on architecture documents | Rejected — the actuation fence exists because this failed before |

---

## Proposed decision

Ratify **two independent rules**.

### Fence A — knowledge cannot act

The World Model MUST NOT:

- publish actuator commands, or reference `cmd_vel`
- import ROS/DDS actuation APIs
- mint or validate release tokens
- call the verifying consumer
- approve, clamp, or veto trajectories
- issue serial / CAN / DDS actuator commands
- bypass Occy or any other doer
- become a planner
- become a safety checker

Its **only** permitted output is typed knowledge: entities, observations,
relationships, queries, resolution outcomes, grounded semantic references,
provenance, freshness, and uncertainty.

### Fence B — safety cannot depend on knowledge

The Governor, verifier, checker, release-token implementation, and verifying
consumer MUST NOT import or query the World Model **when making the safety
decision**.

The safety path reads its own authoritative inputs directly — as applicable:
odometry, trajectory, corridor / map safety inputs, perceived objects, watchdog
state, kinematic configuration, release-token state, and posture state.

**Fence B is TRANSITIVE** ([ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md)
Decision 3). It applies to the full dependency closure of the safety decision
path — **12 workspace crates**, measured, not the 7 roots listed below — and
`kirra-core` is inside that closure under a strict no-dependency rule. A
direct-import-only check is insufficient.

**Fence B is LANGUAGE-INDEPENDENT** (Decision 4). It applies to Python, shell,
systemd configuration, ROS topic wiring and service URLs, not only Rust. The
verifying consumer is Python; configuration alone can create a dependency that
no dependency graph would see.

**Trait implementations and hidden adapters are in scope** (Decision 2). See
the corollary below.

> **World Model → may inform proposal generation.**
> **World Model ↛ safety authorization.**

Information does not become an authoritative checker input by virtue of having
been stored, resolved, or semantically named.

### Dependency diagram

```
World Model
    │ read-only semantic input
    ▼
Mick / destination resolver / Occy
    │ proposal
    ▼
typed boundary
    │
    ▼
Governor / checker
    │ reads independent authoritative inputs
    ▼
release token
    ▼
verifying consumer

FORBIDDEN EDGES
    World Model  ✕→  actuator
    Governor     ✕→  World Model
```

### The trait-seam corollary (investigation-derived)

`CorridorSource` is a **trait** in `kirra-core`; the checker receives
`&dyn CorridorSource` and never fetches a corridor. Fence B therefore extends
to trait implementation, not just crate dependency:

> **The World Model MUST NOT implement `CorridorSource`, or any other
> authoritative-input trait the safety path consumes.**

This matters because `impl CorridorSource for WorldModelCorridor` would satisfy
a naive crate-name dependency scan on the checker while inverting the fence —
the checker would be reading knowledge through an abstract seam. The enforcement
plan below names the traits for this reason.

---

## Structural enforcement — IMPLEMENTED

> **Status update.** The enforcement described in this section is no longer only
> planned: it is implemented by
> [`ci/check_kirra_world_bidirectional_fence.py`](../../ci/check_kirra_world_bidirectional_fence.py),
> with reviewed exceptions in
> [`ci/kirra_world_fence_allowlist.toml`](../../ci/kirra_world_fence_allowlist.toml)
> and 29 fixture cases in
> [`ci/test_kirra_world_bidirectional_fence.py`](../../ci/test_kirra_world_bidirectional_fence.py).
> It runs in the `static-analysis` CI lane alongside — **not instead of** —
> `check_mick_actuation_fence.py`. Implementing the fence does **not** ratify
> this ADR, and authorizes no Kirra World implementation.
>
> **What the checker proves.** Within this repository, at merge time: no package
> in the measured safety closure depends on `kirra-world*` (normal + build
> edges, transitively, plus a direct dev-edge rule); no `kirra-world*` package
> depends on an actuation or authorization crate; no safety-path public item
> names a Kirra World type; every production implementation of a
> safety-authoritative trait has been reviewed and none is Kirra World-derived;
> no safety-path Python module imports Kirra World or reads its reserved
> resources, and no Kirra World Python module imports an actuation module,
> publishes `cmd_vel`, calls a release-token API, or opens an actuator device;
> no safety systemd unit is wired to a reserved Kirra World endpoint, database,
> or unit, and no Kirra World unit is wired to actuation; every allowlist entry
> carries a rationale and an owning ADR.
>
> **What it does NOT prove.** It is static analysis of one repository, so it
> cannot establish what deployed processes actually talk to, what a
> runtime-loaded plugin or a dynamically generated ROS graph connects, whether
> units or environment files were edited on the device, or whether an external
> database is shared out of band. **Runtime topology verification on a real
> deployment remains required**, and the checker's own module docstring says so.
> It is a ratchet against the accidental and convenient edge — not a proof of
> isolation, and not evidence of certification of any kind.
>
> **Closure size — reconciled with ADR-0042.** The checker measures **19
> workspace crates** from **10** configured roots;
> [ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md)
> §*Measured closure* records **12** from **7**. Both are correct: the
> difference is the root set, not an error in either. The checker additionally
> roots at `kirra-verifier`, `kirra-persistence`, `kirra-policy-types` and
> `kirra-ros2-adapter` — the governor control plane, the store behind posture and
> attestation decisions, command classification, and the adapter that runs the
> checker loops. Each is listed with its justification in the checker's
> `SAFETY_ROOTS` block, because an unexplained root is one nobody will dare
> remove or dare trust. The wider set is the one enforced; it was cross-checked
> against `cargo metadata` and agrees exactly.

Modelled on [`ci/check_mick_actuation_fence.py`](../../ci/check_mick_actuation_fence.py),
whose two-part mechanism (transitive `[dependencies]` closure over workspace
path deps + comment-stripped symbol scan) is the working precedent.

**Outbound (Fence A)** — over future `kirra-world*` crates:

| Check | Content |
|---|---|
| Dependency closure | Forbidden workspace: `kirra-release-token`, `kirra-actuation-consumer`, `kirra-inline-governor`, `kirra-ros2-adapter`, `kirra-hv-carrier`, `kirra-consumer-ffi`. Forbidden external: `r2r`, `rclrs`, `rustdds`, `cyclonedds-*`, `zenoh`, `iceoryx2`, `serialport`, `socketcan`, … |
| Symbol scan | `cmd_vel`, `ReleaseToken`, `write_twist`, `MotorSerial`, `issue_ros_release`, `RosReleaseGate`, `kirra_ros_release` |

**Inbound (Fence B)** — over the safety-path crates:

| Check | Content |
|---|---|
| Dependency closure | `kirra-release-token`, `kirra-actuation-consumer`, `kirra-inline-governor`, `kirra-trajectory`, `kirra-safety-authority`, `kirra-hv-carrier`, `kirra-consumer-ffi` must have **no** edge to `kirra-world*` |
| Symbol scan | no `kirra_world`, no world-model query API names, no world-model database path constants |
| **Trait-impl scan** | no `impl CorridorSource for` (or other authoritative-input trait) inside `kirra-world*` |

**Baseline today (measured, this PR):**

```
kirra-release-token        → kirra-contract-channel
kirra-actuation-consumer   → kirra-release-token
kirra-inline-governor      → kirra-contract-channel, kirra-core, kirra-release-token, kirra-hv-carrier
kirra-trajectory           → kirra-core, parko-core
kirra-safety-authority     → kirra-core, kirra-audit-hash
kirra-hv-carrier           → kirra-contract-channel
kirra-consumer-ffi         → kirra-actuation-consumer, kirra-release-token, kirra-r2cp
```

No safety-path crate has any semantic-layer edge. Fence B starts green.

**Honest limits, inherited from the precedent.** The manifest walk is textual
and the symbol scan is a token match; a determined evasion beats both. These
gates are a ratchet against the accidental and convenient edge, not a proof.
Prefer dependency-graph and AST checks over naive substring scans, and strip
comments so documentation and test prose do not trip them.

**This ADR defines the rules. It does not implement the fences.** The scripts
land with the first `kirra-world` crate — and per the blueprint §23.1, the
fence should land *with* that crate, not after it.

---

## Exceptions

**No silent exceptions.** Any proposed exception requires all of:

1. a new ADR that supersedes or amends this one;
2. a safety impact analysis;
3. updated requirements traceability;
4. a dependency review of the resulting graph;
5. an explicit rationale for **why the independent authoritative inputs are
   insufficient** — the burden is on the exception to show the direct input
   cannot serve, not on the fence to justify itself.

A code comment, a review approval, or a temporary feature flag is not an
exception mechanism.

---

## Consequences

**Positive.** The knowledge layer can iterate without destabilizing the safety
case. Independent checking is preserved structurally. Common-cause failure
between planning knowledge and checking evidence is prevented by construction.
The assurance boundary stays where the evidence already is.

**Negative / accepted costs.** Some information will be resolved twice — the
planner may know a corridor semantically while the checker receives it from
perception. That duplication is the price of independence and should not be
"optimized away". Consumers wanting a single source of truth will find this
inconvenient; that inconvenience is the fence working.

**Neutral.** No runtime behaviour changes. The rules describe what the code
already does.

---

## Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | Gradual erosion via convenient imports | CI fence, both directions, landed with the first crate |
| R2 | Trait-seam bypass (`impl CorridorSource`) | Trait-impl scan named explicitly above |
| R3 | Fence gives false confidence (textual checks) | Stated as a ratchet, not a proof; e2e drills remain the evidence |
| R4 | Duplication is refactored away by a well-meaning contributor | Rationale recorded here and in the blueprint |
| R5 | An exception is granted informally under delivery pressure | Exception process requires a superseding ADR |

---

## Alternatives rejected

- **Fence A only.** Guards the cheap direction and leaves the expensive one open.
- **Runtime assertion instead of build-time fence.** A runtime check cannot fail
  a build, and the dependency would already exist.
- **Allowlist of "safe" World Model queries for the checker.** Every such
  allowlist grows. The property being protected is *absence of the edge*, which
  an allowlist by definition destroys.

---

## Assurance impact

**Positive and bounded.** This ADR does not add a safety claim; it protects an
existing one. Specifically it preserves the ADR-0020 doer-invariance property
by ensuring the verdict remains a pure function of inputs the checker reads
itself.

**Scope determination — PENDING, corrected by [ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md) Decision 5.**

An earlier revision of this ADR stated that Kirra World is out of scope for the
ASIL-D evidence set. **That was asserted, not ruled, and the claim is
withdrawn.** The correct formulation is the *proposed argument*:

> If Fence A and Fence B hold, Kirra World is **intended** to remain outside
> the safety decision and authorization scope.

This requires an explicit ruling from the **safety-assurance owner**, addressing
the eight questions in ADR-0042 Decision 5 — including whether absence of a
runtime dependency is sufficient, whether semantic goal selection can influence
a safety goal indirectly, and whether common-source artifacts create
dependent-failure concerns. Until that ruling is recorded, **no document may
state the scope determination as settled**, and this ADR cannot be accepted.

If Fence B were later relaxed, Kirra World would enter the assurance scope and
require the corresponding traceability, coverage, and qualification treatment.

No existing safety claim, ASIL rating, or standards mapping changes.
Kirra is designed in alignment with ISO 26262 ASIL-D requirements and
IEC 61508 SIL 3 requirements. Independent third-party assessment has not yet
been performed.

---

## Migration impact

**None.** No code changes. The baseline measured above already satisfies both
fences. The first migration event is the arrival of `kirra-world`, at which
point the fence scripts must land in the same change.

---

## Open questions

1. Should the inbound fence cover `kirra-core`? It is shared by checker and
   would-be knowledge consumers; a `kirra-core → kirra-world` edge would
   transitively breach Fence B. **Recommendation:** yes, include it.
2. Does the fence extend to the Python consumer (`robot/`)? The verifying
   consumer is Python via FFI; the current fence is Rust-only.
3. Should `robot/world_model.py` be renamed? It is a read projection with no
   authority, but it shares a name with the proposed subsystem.
4. Terminology collision — see *Contradictions* below.

---

## Contradictions found during investigation

Reported, not silently resolved.

**C1 — "world model" already means something else in safety code.**
`crates/kirra-trajectory/src/perception_redundancy.rs` uses *"world model"* to
mean an **independent perception channel** ("two INDEPENDENT world models
(camera-only vs. …)"), which is the True-Redundancy sense — unrelated to the
semantic World Model this ADR fences. Both usages are legitimate and they are
about to collide in the same repository. Needs a naming decision: qualify one
("semantic World Model" / "perception channel"), or rename.

**C2 — the blueprint lists `Map` as a World Model category (§4.1) while the
checker consumes a map-derived corridor.** These are reconcilable but only if
stated: the World Model may hold map *semantics* (names, containment,
relationships); the checker's corridor must come from the corridor source
directly. The trait-seam corollary above is the operative rule. Left unstated,
this is the most likely route to an accidental Fence B breach.

**C3 — `robot/world_model.py` already exists** as an opt-in read projection.
Its §5.1 ruling is the origin of Fence B and is consistent with it, but the
name is now overloaded. Disposition belongs to WM-1 (ADR-0040).

**C4 — repository ADR convention says "ratified on merge."** These three ADRs
deliberately depart from it; see the note at the head of this document.

**C5 — duplicate ADR number.** `0035-shim-removal-v2-plan.md` and
`0035-verifier-crate-decomposition.md` both exist. Pre-existing, unrelated to
this work, reported for the record.

---

## Ratification criteria

This ADR is **Proposed**. It becomes Accepted only when **all** of the
following are recorded:

- [ ] **Governor / safety-case owner** review and sign-off
- [ ] **World Model owner** review and sign-off
- [ ] **Architecture owner** review and sign-off
- [x] **Safety-assurance owner** ruling recorded per
      [ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md)
      Decision 5, using its decision-record template. The ruling must be
      *recorded*; it need not be favourable.
      **RECORDED — 2026-08-05, using that ADR's decision-record template;
      ticked 2026-08-06 by Justin Looney.** The recorded classification is
      *safety-related, non-authoritative*, an **owner self-assessment supported
      by structural evidence only**, with Q2/Q4/Q5 open.
      **This box does not by itself accept ADR-0039.** The separate item
      *"ADR-0042 itself accepted"* below remains open, and ADR-0042's own four
      criteria gate it — so the scope determination is no longer PENDING, but
      acceptance of this ADR still is.
- [x] C1 (terminology collision) — canonical set decided in ADR-0042 Decision 1
- [x] C2 (map/corridor boundary) — stated in ADR-0042 Decision 2
- [ ] ADR-0042 itself accepted

Merging this PR satisfies none of the above.
