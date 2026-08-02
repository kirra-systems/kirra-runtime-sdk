# Kirra World — Architecture Blueprint

| Field | Value |
|---|---|
| Document ID | KIRRA-WM-ARCH-001 |
| Status | **Draft — design proposal.** Decides nothing until the ADRs in §23 are ratified. |
| Scope | The trusted knowledge subsystem of Kirra OS. Design only; no implementation. |
| Cross-refs | [`docs/ARCHITECTURE_STACK.md`](../ARCHITECTURE_STACK.md) · [`robot/world_model.py`](../../robot/world_model.py) · [`crates/kirra-sidecars/src/destination.rs`](../../crates/kirra-sidecars/src/destination.rs) · [`src/audit_chain.rs`](../../src/audit_chain.rs) · [`docs/safety/SAFETY_CASE_INDEX.md`](../safety/SAFETY_CASE_INDEX.md) |
| Forward refs | `ARCHITECTURE.md`, `CONSTITUTION.md` and `SAFETY.md` are introduced by the Kirra OS foundation PR (#1302) and are referenced here by name, without links, until it lands. |

> **This document decides nothing.** It proposes. Where a rule already exists in
> the repository, it cites the owning artifact rather than restating it. Where it
> proposes something new, §23 names the ADR that would have to ratify it.

---

## 1. Executive Summary

Kirra OS separates intelligence, knowledge, planning, verification, and
actuation. **Kirra World is the knowledge layer** — the trusted semantic
representation of what the robot currently knows, what it observed, how certain
it is, and why it believes it.

The design rests on one inversion that most robotics knowledge systems get
wrong:

> **The World Model does not store truth. It stores evidence.
> Truth is a derived, revisable view over evidence — never a stored fact.**

Consequences that follow directly, and that the rest of this document
elaborates:

1. **Observations are immutable and append-only.** Nothing is ever overwritten,
   including a mistake. Corrections are new evidence that supersede, not edits
   that erase.
2. **Entity identity is itself a judgement**, not a primary key. Two
   observations later judged to be the same object are *resolved*, and that
   resolution is a recorded, reversible event. Most systems mutate the entity
   and lose the fact that they ever thought otherwise.
3. **Trust is not a scalar or a single enum.** It decomposes into four
   orthogonal axes — origin, corroboration, adjudication, temporal validity —
   which are stored separately and collapsed to a grade only at the query
   boundary, for consumers that ask for one.
4. **Time is bitemporal.** *Valid time* (when the fact held in the world) is
   distinct from *transaction time* (when the system learned it). Without both,
   "what did you believe at 14:03?" is unanswerable, and incident
   reconstruction degrades to guesswork.
5. **The World Model is never a safety dependency.** The checker reads its own
   inputs directly. This is already the ruling behind
   [`robot/world_model.py`](../../robot/world_model.py) §5.1, and this design
   hardens it into a bidirectional CI dependency fence (§18).

**Recommended persistence:** an append-only event log plus materialized
projections, both in **SQLite**, reusing the existing
[`kirra-persistence`](../../crates/kirra-persistence/) migration framework, WAL
mode, and crash-consistency drill. Not a graph database. Justification in §13.

**Recommended crate:** `crates/kirra-world`, with a `no_std`-friendly pure core
and I/O confined to a storage seam. Layout in §14.

---

## 2. Vision

Robots today know a great deal and can explain almost none of it.

A modern stack holds a costmap, a TF tree, a detection buffer, a behaviour-tree
blackboard, a navigation goal, and a pile of ROS parameters. Each is a
different shape, with a different lifetime, a different notion of staleness, and
no shared account of where anything came from. Ask "why did you stop?" and the
honest answer is assembled by a human reading four logs.

That is tolerable when the operator is an engineer. It stops being tolerable the
moment a robot works alongside someone who is not, or the moment an incident
needs reconstructing for an assessor.

**Kirra World's ambition is that every fact the robot acts on can be traced,
timed, scored, and explained — by construction rather than by forensics.**

Three properties define success:

- **An operator can ask "why do you believe that?" and get a real answer** — the
  observation, the sensor, the time, the frame, the corroboration, and the
  chain of resolutions in between.
- **An assessor can reconstruct the robot's belief state at any past instant**,
  because belief is a projection over an immutable log rather than a mutable
  snapshot.
- **A new subsystem can be added without inventing another private world.**
  Mick, Occy, navigation, skills, fleet, and future predictive models consume
  the same knowledge through the same typed queries.

What this is *not*: a planner, a language model, a safety checker, a controller,
a simulator, or LLM memory. Each of those has an owner elsewhere in Kirra OS,
and §18 makes the boundaries enforceable rather than aspirational.

---

## 3. Guiding Principles

Twelve rules. The first is the axis the rest turn on.

**P1 — Evidence, not truth.** The store holds observations. Every "fact" a
consumer sees is a derived view with an explicit derivation. There is no table
of true things.

**P2 — Append-only, forever.** No observation is mutated or deleted. Retention
is by *compaction into summaries that cite what they compacted* (§11), never by
erasure.

**P3 — Provenance is not optional metadata.** A datum without a source chain is
not admissible. Provenance loss is a corruption, not a degradation.

**P4 — Absence and staleness are answers.** A field past its freshness budget
reads `Unknown`, never its last value. This already exists in
[`robot/world_model.py`](../../robot/world_model.py) and is generalized here.

**P5 — Identity is a revisable judgement.** Entities are resolutions over
observations. Merge and split are recorded events, not destructive edits.

**P6 — Uncertainty is structured, not scalar.** Four orthogonal axes (§9);
scalars are derived at the boundary for consumers who want one.

**P7 — Bitemporality.** Valid time and transaction time are both first-class.

**P8 — Determinism.** The same log replayed yields the same projections, bit for
bit. Projections are pure functions of the log plus a clock reading supplied as
an argument — never read ambiently.

**P9 — The World Model has no authority.** It cannot command, approve, sign, or
gate. It informs. §18 fences this in CI.

**P10 — Language never supplies geometry.** An LLM may select *which* entity is
meant; coordinates come from perception or a trusted registry. This is the rule
already enforced in
[`destination.rs`](../../crates/kirra-sidecars/src/destination.rs), extended to
every spatial datum.

**P11 — Frames are explicit and carried.** Every spatial datum carries its frame
and map identity. The lesson is already paid for: the grounded-destination latch
needed a separate channel precisely because a frame-ambiguous pose is a
correctness hazard.

**P12 — Schema evolution is a first-class requirement.** A decade-long store
outlives every consumer that reads it. Versioned, migrated, fail-closed on a
future schema — the discipline already in
[`kirra-persistence/src/migrations.rs`](../../crates/kirra-persistence/src/migrations.rs).

---

## 4. Domain Model

### 4.1 The nine categories

Everything is exactly one of:

| Category | Definition | Mutable? |
|---|---|---|
| **Observation** | A timestamped, sourced, framed measurement or assertion about the world | **Never** |
| **Entity** | A resolved, persistent thing with stable identity | Identity revisable via events |
| **Relationship** | A typed, directed, time-bounded edge between entities | Superseded, never edited |
| **Event** | Something that happened, including system events (merge, split, correction) | **Never** |
| **Task** | Intent with a lifecycle — requested, active, complete, abandoned | State transitions are events |
| **Map** | A spatial reference frame set plus its semantic layers | Versioned, immutable per version |
| **Frame** | A named coordinate frame and its transform lineage | Transforms are Observations |
| **Capability** | What an agent can do, and how that was established | Verified, expires |
| **Derived State** | A projection computed from the above | Recomputable; never authoritative alone |

### 4.2 Justifying the boundary cases

Three of these deserve challenge, and honest design means stating why they
survive it.

**Frame vs Map.** A Frame is a coordinate system; a Map is a semantic artifact
that *contains* frames and layers. Collapsing them fails as soon as one map is
expressed in several frames, or one frame outlives the map that introduced it
(`base_link` survives a map reload; `map` does not). They stay separate.

**Capability could be a Relationship** (`robot --has_capability--> X`) or an
entity property. It stays a category because it is *established differently*:
capability is asserted by the agent and verified by self-test or configuration,
not observed in the world. Modelling it as a relationship would put
self-knowledge and world-knowledge in the same evidence pipeline, where a
sensor-freshness rule would be applied to a fact that no sensor produced. The
separation keeps the freshness semantics honest.

**Task looks like workflow, not knowledge.** It stays because the robot's
current intent is something an operator asks about ("what are you doing?"), and
because relationships bind to it (`package --assigned_to--> task`). What is
excluded is *execution* — the Task category records intent and outcome, never
drives a step.

**Rejected as a category: Belief / Hypothesis / Prediction.** These are not
stored as world knowledge at all (§20). Admitting them would let a prediction
become indistinguishable from an observation after one schema migration — the
exact failure this architecture exists to prevent.

**Rejected as a category: Policy / Rule / Constraint.** Owned by the Governor
and the contract profiles. A world model that stores constraints will eventually
be asked to enforce them.

---

## 5. Layered Architecture

The suggested layering is close to right. Two changes are proposed and justified
below.

```
                      Physical World
                            │
                       ┌────┴────┐
                       │ Sensors │  camera · lidar · depth · odom · IMU
                       └────┬────┘
                            │  raw, unowned
                  ┌─────────▼─────────┐
                  │ Observation Layer │  normalize · stamp · frame · quality
                  └─────────┬─────────┘
                            │  typed, immutable, sourced
   ╔════════════════════════▼════════════════════════╗
   ║              EVIDENCE STORE (append-only)       ║   ← the only writable
   ║   observations · events · assertions            ║      thing in the system
   ╚════════════════════════┬════════════════════════╝
                            │  replay / fold
        ┌───────────────────▼───────────────────┐
        │        Projection Engine (pure)       │
        │  entity resolution · relationships ·  │
        │  trust · freshness · derived state    │
        └───────────────────┬───────────────────┘
                            │  materialized views
                  ┌─────────▼─────────┐
                  │   Query Engine    │   ← the ONLY read path
                  └─────────┬─────────┘
                            │  typed answers + provenance
   ┌──────────┬─────────────┼─────────────┬──────────────┐
   ▼          ▼             ▼             ▼              ▼
 Mick       Occy         Skills        Fleet      Predictive WM
(explain) (context)    (preconds)   (federate)    (hypotheses)
                            │
                            │  proposals only — never facts
                            ▼
                    ┌───────────────┐
                    │   Governor    │  ✗ does NOT read the World Model
                    └───────┬───────┘
                            ▼
                          Robot

  ┌──────────────────────────────────────────────────────────┐
  │ Cross-cutting: Frame & Time Service · Provenance Chain    │
  └──────────────────────────────────────────────────────────┘
```

### 5.1 Change one — Frame & Time is cross-cutting, not a layer

Frames and clocks are needed at *every* level: to stamp an observation, to
resolve an entity across frames, to answer a historical query, to express a
freshness budget. Placing them as a layer would force either duplication or an
upward dependency. They are a service consumed by all layers, with one
normative rule inherited from
[`HYPERVISOR_CONTRACT_CHANNEL.md`](../safety/HYPERVISOR_CONTRACT_CHANNEL.md) §5:
**clock domains do not mix**, and conversion happens at the producing edge.

### 5.2 Change two — the Governor is not downstream of the World Model

The suggested chain ended `Query Engine → Planning → Governor → Robot`, which
reads as though knowledge flows into the safety decision. It must not. The
Governor validates against its **own** directly-read inputs; the World Model
informs the *proposer*, not the *checker*.

This is not a stylistic preference. If the Governor read the World Model, then
a knowledge-layer defect — a bad entity merge, a stale projection, a poisoned
operator assertion — would become a safety defect, and the World Model would
inherit the entire ASIL-D evidence burden. Keeping it out is what allows the
knowledge layer to iterate quickly and the safety layer to stay small.

§18 makes this a CI-enforced fence in **both** directions.

### 5.3 Why "Projection Engine" replaces "Entity Resolution + Semantic Model"

Entity resolution, relationship inference, trust computation, and freshness are
all the same kind of operation: *a pure fold over the evidence log producing a
view*. Naming them one layer makes the determinism requirement (P8) enforceable
at one boundary instead of four.

---

## 6. Entity Taxonomy

### 6.1 Structure

Every entity carries:

| Field | Notes |
|---|---|
| `entity_id` | Stable, opaque, monotonic. Never reused, never encodes semantics |
| `kind` | Semantic type (below) |
| `lifecycle` | `Provisional → Established → Dormant → Retired`, plus `Merged(into)` / `Split(from)` |
| `aliases` | Operator names, detector labels, imported identifiers — each with its own provenance |
| `first_observed` / `last_observed` | Transaction and valid time both |
| `resolution_confidence` | How sure we are this is *one* thing (distinct from attribute confidence) |
| `provenance_head` | Hash-chain head over the observations that constitute it |

Attributes are **not** stored on the entity. They are projections over the
observations bound to it, so an attribute always retains its own source, time,
and confidence. An entity is a spine; observations hang from it.

### 6.2 Kinds

Grouped, extensible, closed at the root:

| Group | Kinds |
|---|---|
| **Agents** | `Robot`, `Person`, `Animal` |
| **Physical objects** | `Object`, `Tool`, `Package`, `Vehicle`, `Door`, `ChargingDock` |
| **Places** | `Room`, `Zone`, `Waypoint`, `Landmark`, `Surface` |
| **Abstract** | `Mission`, `Task`, `Route`, `Skill`, `Capability` |

Two rules keep this from sprawling:

- **New kinds are additive and versioned**, never repurposed. A consumer that
  does not know a kind must degrade to `Unknown`, not guess a supertype.
- **Kind is a *classification observation*, not an intrinsic property.** A
  detector saying "package" is evidence. The kind shown in a projection is the
  adjudicated result, and it can change without the entity changing identity —
  the box you thought was a package turning out to be a tool is a
  reclassification, not a new object.

### 6.3 Identity: the hard part, done properly

The failure mode in every system that gets this wrong: an entity is created
with a primary key, later evidence shows two entities were one, the rows are
merged, and the fact that the system ever believed otherwise is gone. Incident
reconstruction is then impossible precisely where it matters.

Kirra World instead treats identity as an **adjudication**:

```
observations ──► candidate clustering ──► identity assertion ──► entity
                       (pure)              (recorded Event)
```

- Merging two entities emits an `EntityMerged { from, into, evidence, at }`
  event. Both original IDs remain resolvable forever and answer with a
  redirect.
- Splitting emits `EntitySplit { from, into[], evidence, at }`.
- A query at a past instant resolves identity **as it was adjudicated then** —
  because identity is a projection like everything else.

Cost, stated honestly: identity queries need an indirection through the merge
graph, and the merge graph grows. §21 carries the risk.

---

## 7. Observation Model

### 7.1 The immutable record

```
Observation {
  observation_id     : Ulid              // monotonic, sortable, globally unique
  kind               : ObservationKind
  subject            : SubjectRef         // entity, candidate, frame, or unbound
  payload            : TypedPayload       // per-kind, versioned schema

  // WHO
  source             : SourceRef          // sensor / operator / import / derivation
  source_class       : Sensor | Operator | Configuration | Import | Derivation | Network
  producer_version   : semver             // the code that produced it

  // WHEN
  valid_time         : Interval           // when it held in the world
  transaction_time   : Instant            // when we learned it (monotonic)
  clock_domain       : ClockDomain        // boundary vs system — never mixed

  // WHERE / UNDER WHAT FRAME
  frame              : FrameRef           // required for spatial payloads
  map                : Option<MapVersion>

  // HOW CONFIDENT / HOW VERIFIED
  confidence         : Confidence         // structured, not a bare float
  quality            : QualitySignals     // per-modality; e.g. scan_confidence
  validation         : ValidationRecord   // which checks ran, and their outcome

  // FRESHNESS / REPRODUCIBILITY
  ttl                : Option<Duration>   // producer-declared freshness budget
  evidence_digest    : Hash               // content hash — reproducibility anchor
  prev_hash          : Hash               // chain link (audit_chain.rs pattern)
}
```

The eight mandatory questions from the brief map exactly: `source` +
`source_class` (who), `valid_time` + `transaction_time` (when), `frame` + `map`
(where/frame), `confidence` (how confident), `validation` (how verified), `ttl`
(still fresh), `evidence_digest` + `producer_version` (reproducible).

### 7.2 Per-source schemas

| Source class | Kinds | Notes |
|---|---|---|
| **Camera** | `Detection`, `Classification`, `Pose2D/3D`, `Appearance` | Carries detector identity + model digest, so a model swap is visible in provenance |
| **Lidar** | `PointCluster`, `FreeSpace`, `Occupancy` | Reuses Taj's `scan_confidence` and rejection tallies |
| **Depth** | `RangeImage`, `SurfaceEstimate` | Validity fraction is a first-class quality signal |
| **Odometry / IMU** | `TransformObservation` | This is how TF enters — transforms are *observed*, not ambient |
| **Operator** | `Assertion`, `Naming`, `Correction`, `Confirmation`, `Retraction` | Highest adjudication weight, still evidence |
| **Configuration** | `RegistryEntry` | The place/route registries become observations with `source_class = Configuration` |
| **Import** | `MapLayer`, `ExternalEntity` | Lanelet2, floor plans, BIM |
| **Network / Fleet** | `PeerObservation` | Signed; §19 |
| **Derivation** | `DerivedFact` | Must cite its inputs; never a leaf |

Two consequences worth naming:

- **The existing registries become evidence, not a parallel truth store.**
  `places.json` loads as a set of `Configuration` observations carrying
  operator provenance and map identity — so "why do you think the kitchen is
  there?" answers "because it was calibrated on 2026-04-11 against map
  r2-lab-01", rather than "because it is in the file".
- **Transforms as observations** is what makes historical spatial queries
  correct. Asking where something was at 14:03 requires the transform tree *as
  it was at 14:03*, which a mutable TF buffer cannot provide.

### 7.3 Confidence, structured

```
Confidence {
  score        : Option<f32>      // producer's own, [0,1], meaning is per-kind
  basis        : ConfidenceBasis  // ModelScore | GeometricResidual | OperatorCertainty
                                  // | Corroboration | Assumed | Unspecified
  calibration  : Option<CalibrationRef>   // was the score calibrated, and how
}
```

A bare float is nearly useless across modalities: a detector's 0.9 and a
geometric residual's 0.9 are not comparable, and treating them as such is how
fusion systems silently over-trust. Carrying the basis makes cross-modal
comparison an explicit decision rather than an accident. **`Unspecified` is a
valid and common value** — the design must not force producers to invent
precision they do not have.

---

## 8. Relationship Model

Relationships are first-class, directed, typed, and time-bounded.

```
Relationship {
  relationship_id : Ulid
  subject         : EntityRef
  predicate       : Predicate
  object          : EntityRef | LiteralRef
  valid_time      : Interval          // closed when superseded
  transaction_time: Instant
  source          : SourceRef
  confidence      : Confidence
  derivation      : Option<DerivationRef>   // if inferred, from what
  superseded_by   : Option<RelationshipId>
}
```

| Group | Predicates |
|---|---|
| **Topological** | `inside`, `contains`, `connected_to`, `adjacent_to`, `part_of` |
| **Spatial** | `near`, `supports`, `on_top_of`, `last_seen_at` |
| **Assignment** | `belongs_to`, `assigned_to`, `charging_for`, `operated_by` |
| **Temporal** | `preceded`, `caused_by` (weak — see below) |

Design notes:

- **Supersession, not update.** Moving a toolbox closes the old
  `last_seen_at` interval and opens a new relationship. The history is the
  point: "where was it yesterday?" is a query, not a lost fact.
- **Inferred relationships must cite their derivation.** `inside` derived from
  a pose and a room polygon carries the polygon version and the pose
  observation. When the map changes, dependent inferences are invalidated by
  provenance rather than left silently wrong.
- **`caused_by` is deliberately weak.** Causal claims from a robot are
  hypotheses; the predicate is admitted for operator-asserted causality and for
  system events (this merge was caused by that assertion), not for inferred
  physical causation.
- **Symmetry and inverses are derived, not stored twice.** `contains` implies
  `inside`; storing both invites divergence.

---

## 9. Trust Model

### 9.1 The core proposal: four orthogonal axes

The requested states — Observed, Confirmed, Derived, Predicted,
OperatorConfirmed, Imported, Expired, Ambiguous, Rejected, Unknown — are real
and useful, but they are **not one dimension**. `Imported` describes origin;
`Expired` describes time; `Ambiguous` describes adjudication; `Confirmed`
conflates corroboration with adjudication. Collapsing them into one enum is
exactly why trust states in most systems become mush after eighteen months —
every new case forces either a wrong assignment or a new variant, and consumers
end up pattern-matching on a taxonomy nobody can explain.

Kirra World stores four axes and derives the familiar labels:

```
Origin        : Observed | Derived | Imported | Asserted | Predicted*
Corroboration : Uncorroborated | Corroborated(n) | Contradicted(n)
Adjudication  : Pending | Confirmed | Rejected | Ambiguous
Validity      : Fresh | Stale | Expired | Timeless
```

\* `Predicted` never appears in the evidence store (§20); it is listed because
the *axis* is shared with the predictive subsystem, which reuses this vocabulary
for its own records.

The requested states map cleanly:

| Requested | Axes |
|---|---|
| Observed | `Observed` · `Uncorroborated` · `Pending` · `Fresh` |
| Confirmed | any origin · `Corroborated(≥k)` · `Confirmed` · `Fresh` |
| OperatorConfirmed | `Asserted` · — · `Confirmed` · `Timeless` or `Fresh` |
| Derived | `Derived` · — · inherits weakest input · inherits weakest input |
| Imported | `Imported` · — · `Pending` · `Timeless` until contradicted |
| Expired | any · any · any · `Expired` |
| Ambiguous | any · `Contradicted(n)` · `Ambiguous` · any |
| Rejected | any · any · `Rejected` · any |
| Unknown | no admissible evidence |

### 9.2 Transition rules

Normative, and deliberately few:

1. **Origin never changes.** It is a property of the record, fixed at write.
2. **Corroboration is monotonic in evidence, not in time.** New agreeing
   evidence increments; new disagreeing evidence moves to `Contradicted`. It
   never decays on its own — decay is the Validity axis's job.
3. **`Contradicted` ⇒ `Ambiguous`** unless an adjudication rule resolves it.
   Ambiguity is a *stable, reportable state*, not an error. Mick must be able
   to say "I have conflicting information about that."
4. **Operator assertion outranks sensor evidence for adjudication, and never
   for geometry.** An operator may confirm *which* entity is meant; an operator
   assertion cannot silently rewrite a measured pose (P10). An operator
   correcting a pose creates a `Correction` observation whose payload is
   itself an operator-sourced measurement, visibly distinct from a sensed one.
5. **Derived inherits the weakest input** on every axis. No derivation
   launders confidence upward. This single rule prevents the most common
   knowledge-graph pathology: a chain of plausible inferences producing a
   high-confidence conclusion from low-confidence roots.
6. **Validity is computed at read time**, never stored. `Fresh` is not a state
   the system enters; it is a question asked at the instant of the query, with
   the clock passed in. This is the existing
   [`world_model.py`](../../robot/world_model.py) rule generalized, and it is
   what makes projections pure.
7. **`Rejected` is terminal for the record, not for the subject.** Rejecting an
   observation never deletes it; it marks it inadmissible to projections and
   records who rejected it and why.

### 9.3 The derived grade

Consumers that want one number get `TrustGrade` at the query boundary, with the
axes always available alongside. **The grade is advisory and is never an input
to a safety decision** — §18.

---

## 10. Provenance Model

Provenance is a **directed acyclic graph over records**, hash-chained per
source, and it is never summarized away.

```
Sensor reading ──► Observation ──► Validation ──► Entity binding
                        │                              │
                   Operator                        Derivation
                  correction                            │
                        └──────────► Adjudication ◄─────┘
```

Every record carries `prev_hash`, mirroring
[`src/audit_chain.rs`](../../src/audit_chain.rs), so tampering is detectable
rather than merely discouraged. Reusing that mechanism is deliberate: the
audit-ledger philosophy is already implemented, drilled for crash consistency,
and understood by the safety case.

**Mandatory answers.** For any datum, the query engine can produce:

- the originating observation(s) and their sources
- the producer's code version and, for ML sources, the model digest
- the validation checks that ran and their outcomes
- every operator interaction — assertion, confirmation, correction, retraction
- every derivation step, with the rule that produced it
- the calibration record for any transform in the spatial chain

**Never permitted:** compaction that drops provenance, a derivation that cannot
name its inputs, or an import without an external-source record. A record
failing any of these is inadmissible — it is not stored with a warning, it is
refused (P3).

---

## 11. Time Model

### 11.1 Bitemporal, plus generation

| Axis | Meaning | Answers |
|---|---|---|
| **Valid time** | When the fact held in the world | "Where was the toolbox at noon?" |
| **Transaction time** | When the system learned it | "What did you believe at 14:03?" |
| **Generation** | Monotonic counter over the log | Ordering, replay, federation merge |

The two time axes answer genuinely different questions, and incident
reconstruction needs the second. A robot that stopped at 14:03 stopped because
of what it *believed* then — including a belief later corrected. A store with
only valid time will confidently reconstruct a decision the robot never made.

Generation reuses the epoch-fenced monotonic ordering already established in
[`ADR-0037`](../adr/0037-epoch-fenced-generation-ordering.md).

### 11.2 Capabilities

- **Latest view** — projection at `transaction_time = now`, with freshness
  evaluated against a supplied clock.
- **Time travel** — projection at any `(valid_time, transaction_time)` pair.
- **Diff** — "what changed since yesterday?" is a projection difference, which
  is what makes that Mick flow (§16) answerable at all.
- **Staleness / TTL** — producer-declared budgets, evaluated at read (P4).
- **Supersession** — closing an interval, never deleting a row.
- **Event replay** — deterministic re-fold; the property that makes the whole
  design testable.

### 11.3 Retention

The honest tension: append-only forever meets a robot with finite disk.

Proposal — **compaction with citation**. A compaction window is replaced by a
`Summary` record that (a) states what it summarizes, (b) carries the digest of
the compacted range, and (c) preserves adjudicated conclusions and all operator
interactions verbatim. Raw high-rate observations may be dropped; the *account*
of them may not. A time-travel query into a compacted window returns the
summary and says so — degraded resolution, never silent fabrication.

Ratifying the retention policy is an ADR (§23), because it is the one place
where P2 is knowingly bounded.

---

## 12. Query Architecture

The Query Engine is the **only** read path. No consumer touches storage.

Four families:

| Family | Example | Returns |
|---|---|---|
| **Point** | `where_is(entity)` | Value + provenance + validity |
| **Set** | `entities_in(room, kind)` | Ranked set + per-item validity |
| **Graph** | `path_between(a, b, predicates)` | Path + per-edge confidence |
| **Temporal** | `changes_since(t)` | Diff with cause per change |

Every answer carries: the value, the trust axes, the validity at the supplied
clock, and a `ProvenanceHandle` that can be expanded on demand. **There is no
API that returns a bare value.** That is a deliberate ergonomic cost: it makes
"I got a number and lost where it came from" impossible to write.

Three rules:

- **Queries are pure and clock-parameterized.** `now` is an argument.
- **Queries are bounded.** Every query declares a result and traversal bound;
  unbounded graph traversal is not offered, because an unbounded query in a
  robot is an availability hazard.
- **A query may return `Unknown` and that is a success.** The error channel is
  for malformed queries and storage faults, never for absence of knowledge.

---

## 13. Persistence Recommendation

### 13.1 Comparison

| Option | Embedded | Offline | Auditable | Deterministic | Migration | Simplicity |
|---|---|---|---|---|---|---|
| **SQLite** | ✅ | ✅ | ✅ WAL + chain | ✅ | ✅ framework exists | ✅ |
| RocksDB | ✅ | ✅ | ⚠️ manual | ✅ | ⚠️ hand-rolled | ⚠️ no query layer |
| PostgreSQL | ❌ | ❌ | ✅ | ✅ | ✅ | ⚠️ operational weight |
| Graph DB (Neo4j etc.) | ❌ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ❌ |
| Event sourcing | — approach, not a store — |
| Hybrid | depends on the pieces |

### 13.2 Recommendation

> **Event-sourced append-only log + materialized projections, both in SQLite.
> An in-memory graph index is built from projections at startup and maintained
> incrementally. SQLite is the single durable substrate.**

Justification, weighted for this project specifically:

- **Embedded robotics.** The robot is a Jetson that may be offline for hours.
  A server-based store is disqualified on availability grounds, not preference.
- **Auditability.** The hash-chained ledger pattern already exists, is
  crash-consistency drilled, and is understood by the safety case. Reusing it
  is worth more than any query-language advantage a graph database offers.
- **Determinism.** Replay-to-projection is testable, and the deterministic
  virtual-clock harness ([`src/clock.rs`](../../src/clock.rs)) already exists to
  test it.
- **Migration.** A ten-year store *will* migrate.
  [`kirra-persistence/src/migrations.rs`](../../crates/kirra-persistence/src/migrations.rs)
  already provides versioned, fail-closed-on-future-schema migration with two
  backends.
- **Simplicity.** One substrate, one backup story, one corruption story.

**Why not a graph database**, despite the domain being a graph: the graph here
is small (thousands of entities, not billions), the query patterns are known
and bounded, and the operational and determinism costs are severe on an
embedded target. The graph shape belongs in an **index**, not in the durable
substrate. This is the decision most likely to be questioned, and it should be
— §23 gives it its own ADR.

**Fleet tier.** [`ADR-0038`](../adr/0038-postgres-shared-state-hybrid.md)
already establishes the hybrid pattern: shared control-plane state may live in
Postgres while the hash-chained ledger stays local. Kirra World inherits it
exactly — **the local evidence log never leaves the robot**; federation exchanges
signed observations (§19).

---

## 14. API Concepts

Contracts only. CQRS: commands mutate the log, queries never do.

### 14.1 Commands

```
RecordObservation(ObservationDraft)      -> ObservationId | Refused
AssertEntity(AssertionDraft)             -> EntityId      | Refused   // operator teaching
ConfirmEntity(EntityId, Evidence)        -> Ack           | Refused
CorrectObservation(ObservationId, Draft) -> ObservationId | Refused   // new record, not an edit
RetractAssertion(ObservationId, Reason)  -> Ack           | Refused
MergeEntities(from[], into, Evidence)    -> EventId       | Refused
SplitEntity(from, into[], Evidence)      -> EventId       | Refused
ForgetEntity(EntityId, Reason)           -> EventId       | Refused   // retire; never erase
ImportMapLayer(MapLayerDraft)            -> MapVersion    | Refused
```

`ForgetEntity` deserves a note: "forget this place" is an operator-facing
lifecycle transition to `Retired` plus suppression from default projections. It
is **not** deletion. If genuine erasure is ever required (a privacy regime, a
person entity), that is a distinct, audited `Redact` operation with its own ADR
— and it must leave a tombstone proving something was redacted, or the chain
breaks.

### 14.2 Queries

```
WhereIs(EntityRef, At)                    -> Located | Unknown | Ambiguous
WhatIsAt(FrameRef, Region, At)            -> RankedSet
Resolve(NameOrAlias, Context)             -> Resolution   // reuses the destination-resolver contract
Related(EntityRef, Predicate, Depth, At)  -> Graph
ChangesSince(Instant, Filter)             -> Diff
Explain(FactHandle)                       -> ProvenanceTree
Capabilities(AgentRef, At)                -> CapabilitySet
Freshness(FieldRef, At)                   -> Fresh | Stale | Expired | Unknown
```

### 14.3 Events (emitted)

```
ObservationRecorded · EntityEstablished · EntityMerged · EntitySplit
RelationshipOpened · RelationshipClosed · AdjudicationChanged
ContradictionDetected · EntityRetired · MapVersionActivated
```

`ContradictionDetected` is deliberately an event: an operator should be able to
learn that the robot's knowledge disagrees with itself *when it happens*, not
when a query surfaces it.

### 14.4 Errors versus outcomes

Sharply separated:

- **Outcomes** — `Unknown`, `Ambiguous`, `Stale`, `Refused(reason)`. Normal.
  Every consumer must handle them.
- **Errors** — malformed request, storage fault, schema mismatch, chain
  integrity failure. Abnormal, fail-closed, audited.

Conflating the two is how "I don't know" becomes an exception that somebody
catches and turns into a default value.

---

## 15. Integration Points

| Consumer | Reads | Writes | Constraint |
|---|---|---|---|
| **Perception (Taj)** | frames, map | observations | Producer only; never reads its own output back as truth |
| **Mick** | queries | operator assertions | Never invents; never states stale as current |
| **Occy** | semantic context | task events | Context only — never the geometry the checker validates |
| **Skills** | preconditions | task + outcome events | May be refused; refusal is an outcome |
| **Fleet** | export | signed peer observations | §19 |
| **Predictive WM** | full read | nothing | One-way (§20) |
| **Governor** | **nothing** | nothing | Fenced (§18) |
| **Operator console** | queries + provenance | assertions, corrections | The teaching surface |

---

## 16. Mick Integration

Mick is the explanation surface. Each requested flow maps to a query and, more
importantly, to a **defined refusal**.

| Operator says | Query | Honest failure |
|---|---|---|
| "Where is my toolbox?" | `Resolve` → `WhereIs` | "I last saw it in the workshop at 09:14. That is four hours old." |
| "Have you seen the package?" | `WhereIs(valid_time window)` | "No package observation today." — never "I don't think so" |
| "What changed since yesterday?" | `ChangesSince` | Enumerated diff with cause per change |
| "Teach this place." | `AssertEntity` (§17) | Refuses without trusted localization |
| "Forget this place." | `ForgetEntity` | Retires; states that history is retained |
| "Explain why you believe that." | `Explain` | Renders the provenance tree in prose |

Three non-negotiables, each already precedented in the repository:

1. **Never invent.** The chat surface has no telemetry; a question about state
   it was not given is declined. This is enforced today by
   [`robot/mick_chat_contract.py`](../../robot/mick_chat_contract.py) and
   extends unchanged to World Model answers.
2. **Never state stale as current.** Every spoken fact carries its age when the
   age matters. "In the workshop" and "in the workshop, four hours ago" are
   different claims, and only the second is true.
3. **Never supply geometry.** Mick resolves *which* entity; coordinates come
   from the store (P10).

**"Explain why you believe that" is the flagship capability.** It is the reason
provenance is mandatory rather than nice-to-have, and it is the single feature
most likely to distinguish Kirra from every other robotics knowledge layer. It
should be treated as a product requirement, not a debugging tool.

---

## 17. Learning — how an operator teaches Kirra

```
"This is the coffee station."
        │
        ▼
1. Resolve the utterance to a TYPE, never coordinates      (P10)
        │
        ▼
2. Require trusted localization — pose + frame + map,
   with quality above threshold                            ← refuses if not met
        │
        ▼
3. Confirm with the operator: name, type, and the pose
   the ROBOT measured (read back, not asserted)
        │
        ▼
4. Record an Assertion observation
   (source_class = Operator, origin = Asserted)
        │
        ▼
5. Establish or bind the entity; open relationships
        │
        ▼
6. Append to the hash-chained audit ledger
```

Load-bearing details:

- **The robot supplies the pose; the operator supplies the meaning.** The
  operator is naming *where the robot currently is*, not dictating a location.
  This is the same rule that makes voice-to-destination safe today.
- **Refusal is a designed outcome.** Poor localization → "I can't record this
  here; I'm not confident enough about where I am." That refusal is more
  valuable than a recorded place that is quietly two metres off.
- **Confirmation is a distinct observation**, so "did I confirm that?" is
  answerable.
- **No LLM-generated coordinates, ever.** Non-negotiable.

---

## 18. Governor Boundary — the safety fence

### 18.1 Prohibitions

The World Model must never: command motion, publish `cmd_vel`, mint or verify
release tokens, approve or clamp trajectories, or substitute for the Governor,
Occy, or the verifier.

### 18.2 Enforcement — bidirectional, in CI

The repository already proves this class of property structurally with
[`ci/check_mick_actuation_fence.py`](../../ci/check_mick_actuation_fence.py),
which shows no dependency route from the Mick binaries to actuation. The same
technique applies, in **both** directions:

```
FENCE A (outbound):  kirra-world  ─✗─►  release-token · actuation · ROS/DDS command
FENCE B (inbound):   checker crates ─✗─►  kirra-world
```

**Fence B is the unusual one and the more important.** Fence A stops the
knowledge layer from acting. Fence B stops the safety layer from *depending* on
knowledge — which is what would otherwise happen gradually, one convenient
import at a time, until a bad entity merge could influence a verdict.

Fence B is also what keeps the ASIL-D evidence burden off the World Model. A
knowledge layer inside the safety boundary would need the full traceability,
MC/DC, and qualification treatment; outside it, it can evolve at product speed.
That is a deliberate architectural trade, and it should be stated in the safety
case rather than assumed.

### 18.3 The subtle leak to watch

Occy legitimately reads World Model context and produces proposals the checker
validates. The invariant that keeps this safe: **the checker re-derives
everything it needs from its own inputs.** A World Model error can therefore
cause a *bad proposal* — which the checker refuses — but never a *bad
verdict*. Any change that would let checker behaviour vary with World Model
content is a fence violation regardless of what the dependency graph says, and
belongs in a review checklist as well as in CI.

---

## 19. Fleet Evolution

Interfaces now; consensus later. Deliberately not solved here.

- **Observations are the unit of exchange**, signed with the existing Ed25519
  federation machinery. A peer's observation enters as
  `source_class = Network` with the peer's identity intact — it never becomes a
  local sensor reading.
- **Merge is append, never overwrite.** Two robots observing the same room
  produce two observations; adjudication decides what to believe, and both
  survive.
- **Generation ordering** reuses [`ADR-0037`](../adr/0037-epoch-fenced-generation-ordering.md).
- **The local evidence log never leaves the robot.** Federation exchanges
  observations, not ledgers — mirroring [`ADR-0038`](../adr/0038-postgres-shared-state-hybrid.md).
- **Trust is per-peer and per-kind.** A peer trusted for "door state" is not
  thereby trusted for "person identity".

Explicitly deferred: conflict resolution across partitions, global entity
identity, and eventual-consistency guarantees. Naming them as deferred is the
point — a design that quietly assumed them would be wrong in a way that is
expensive to discover later.

---

## 20. AI Prediction Integration

```
   World Model  ──read──►  Predictive World Model  ──►  Planner
        ▲                          │
        └────────── ✗ ─────────────┘
              never writes back
```

- **Predictions live in a separate store** and reference World Model entity IDs.
  They are never observations.
- **A prediction may not be promoted to an observation.** If a predicted event
  is later observed, the *observation* is recorded from the sensor that saw it.
  The prediction's accuracy is a separate, valuable record.
- **The World Model stays deterministic.** Its projections are a pure fold over
  evidence; a learned model in that path would destroy replay.
- **Predictions carry their own model identity and version**, so a model swap is
  visible in the same way a detector swap is.

This boundary is what allows aggressive experimentation with world-model-style
learned systems — V-JEPA-class predictors, occupancy forecasters — without any
of it touching the account of what was actually observed.

---

## 21. Risks

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R1 | **Becoming a de-facto safety dependency** by gradual convenience imports | **Critical** | Fence B (§18.2), plus a review checklist item |
| R2 | Unbounded storage growth | High | Compaction-with-citation (§11.3); measure before choosing thresholds |
| R3 | Entity resolution errors are sticky and propagate | High | Merges are reversible events; `resolution_confidence` surfaced in answers |
| R4 | Operator teaching poisons the store | Medium | Assertions are evidence, not overrides; retraction is first-class; audited |
| R5 | Query latency enters a control loop | Medium | Bounded queries; the checker never queries; Occy's use is slow-loop only |
| R6 | Provenance volume dwarfs data | Medium | Chain by reference; measure early |
| R7 | Bitemporal complexity leaks into every consumer | Medium | Sensible defaults — "now/now" is one call; time travel is opt-in |
| R8 | Schema ossification across a decade | Medium | Versioned payloads; unknown kinds degrade to `Unknown`, never guessed |
| R9 | Trust axes are conceptually right but unusable in practice | **Medium-High** | Derived `TrustGrade` for the common path; validate with real operator flows before committing |
| R10 | The store becomes a second source of truth for geometry | High | P10; the checker re-derives (§18.3) |

R9 deserves candour: four orthogonal axes are more correct than one enum and
also more to learn. If operator-facing flows show the axes confuse more than
they clarify, the right response is a better derived grade — not collapsing the
axes, which would reintroduce exactly the mush this design avoids.

---

## 22. Deferred Work

Named so they are not mistaken for solved:

- Distributed consensus and partition-tolerant merge (§19)
- Erasure/redaction under a privacy regime, and its chain implications (§14.1)
- Compaction thresholds — needs measurement, not a guess (§11.3)
- Semantic similarity search over entities (embeddings) — attractive and
  non-deterministic; belongs in the predictive tier if anywhere
- Multi-map topology and map-to-map transforms
- Person entities and the entire privacy question they open
- Cross-robot entity identity
- Formal verification of projection determinism

---

## 23. Suggested ADRs

| ADR | Decision | Why it needs ratification |
|---|---|---|
| **WM-1** | Evidence-store-over-truth-store; append-only observations | The foundational inversion; everything else follows |
| **WM-2** | SQLite event log + projections; **not** a graph database | Most likely to be challenged (§13.2) |
| **WM-3** | Four-axis trust model rather than a single enum | Novel; needs sign-off before consumers depend on it |
| **WM-4** | Bitemporal time model | Real complexity cost, taken deliberately |
| **WM-5** | Entity identity as revisable adjudication (merge/split as events) | Rejects the primary-key model everyone expects |
| **WM-6** | **Bidirectional Governor fence**, CI-enforced | Safety-case relevant; the most important one |
| **WM-7** | Predictions never write to the evidence store | Guards determinism |
| **WM-8** | Compaction-with-citation retention policy | The one place P2 is knowingly bounded |
| **WM-9** | Operator teaching protocol — robot supplies pose, operator supplies meaning | Product-defining |
| **WM-10** | Federation exchanges signed observations; ledgers stay local | Sets the fleet trajectory |

---

## 24. Suggested GitHub Epics

| Epic | Contents | Depends on |
|---|---|---|
| **E1 — Evidence core** | Observation record, hash chain, SQLite log, replay harness | WM-1, WM-2 |
| **E2 — Projection engine** | Pure fold, freshness, determinism tests under virtual clock | E1, WM-4 |
| **E3 — Entity resolution** | Candidate clustering, merge/split events, redirects | E2, WM-5 |
| **E4 — Relationship graph** | Predicates, supersession, in-memory index | E3 |
| **E5 — Trust & provenance** | Four axes, derived grade, `Explain` query | E2, WM-3 |
| **E6 — Query engine** | Typed API, bounded traversal, outcome/error split | E4, E5 |
| **E7 — Safety fences** | Fence A + Fence B CI checks, review checklist | WM-6 |
| **E8 — Mick integration** | Six conversation flows, refusal behaviours | E6 |
| **E9 — Operator teaching** | Teach/forget/correct, localization gate, console surface | E3, WM-9 |
| **E10 — Registry migration** | Places/routes become Configuration observations | E1, E3 |
| **E11 — Occy context** | Semantic goal resolution; checker independence preserved | E6, WM-6 |
| **E12 — Fleet interfaces** | Signed observation exchange; no consensus | E1, WM-10 |
| **E13 — Predictive seam** | Read-only interface, hypothesis store | E6, WM-7 |
| **E14 — Retention** | Compaction with citation, measured thresholds | E1, WM-8 |

**E7 should land early**, alongside E1 — before there is anything to be tempted
by. A fence added after the first convenient import is a refactor; a fence added
first is a constraint.

---

## 25. Five-Year Roadmap

| Horizon | Theme | Outcome |
|---|---|---|
| **Year 1** | Evidence foundation | Append-only log, projections, entity resolution, `Explain`, fences, Mick's six flows, registries migrated. *A robot that can justify every fact it states.* |
| **Year 2** | Semantic depth | Rich relationships, map layers, capability model, operator console, task integration. *A robot that understands its environment structurally.* |
| **Year 3** | Fleet knowledge | Signed observation exchange, per-peer trust, cross-robot queries under explicit partition semantics. *Robots that learn from each other without trusting each other.* |
| **Year 4** | Predictive integration | Hypothesis store, learned predictors reading the model, accuracy tracking, prediction-informed planning — evidence store still deterministic. *Anticipation without contamination.* |
| **Year 5** | Standard cognitive substrate | Stable public schema, third-party producers and consumers, portable evidence bundles, assessor-usable reconstruction. *An interoperable knowledge layer.* |

---

## 26. The closing question

> **"If Kirra World became the standard cognitive architecture for robotics,
> what decisions today would make that possible?"**

Standards are not adopted because they are elegant. They are adopted because
they solve a problem the alternatives structurally cannot, and because
committing to them is cheap and reversible. Seven decisions, in the order they
matter:

**1. Make provenance mandatory now, not optional-with-a-migration-path.**
Every system that made provenance optional has provenance on roughly nothing.
It is the one property that cannot be retrofitted, because the data that would
have carried it is already stored. If Kirra World has one durable advantage in
ten years, this is it.

**2. Keep the evidence store out of the safety boundary — and prove it in CI.**
This is counter-intuitive: the instinct is that the safety-critical system
should own the knowledge. The opposite is true. A knowledge layer inside the
safety envelope cannot evolve, because every change re-opens qualification. A
knowledge layer outside it, with a *proven* absence of influence, can iterate
for a decade while the safety argument stays stable. Fence B is what makes the
architecture survivable.

**3. Publish the schema and the contracts before the implementation.**
Standards form around interfaces, not codebases. If the observation record,
the trust axes, and the query contracts are stable, versioned, and documented,
others can produce and consume without adopting Kirra's runtime. If they are
discovered by reading Rust, nobody will.

**4. Make "explain why you believe that" work on day one.**
It is the demonstration that no competing architecture can imitate without
having made decision 1 years earlier. It is also the feature operators will
actually ask for, which is what turns an architecture into a product.

**5. Refuse the shortcuts that feel harmless.** Letting a language model
supply a coordinate. Letting a prediction be stored as an observation. Letting a
merge overwrite. Letting a stale value be returned because the fresh one is
missing. Each is locally reasonable and each destroys a global property that
cannot be rebuilt. The value of this architecture is entirely in what it
refuses.

**6. Design for the assessor as a first-class user.** Robotics is heading
toward a regime where autonomous behaviour must be reconstructable after the
fact. A knowledge layer that can answer "what did it believe, when, and on what
basis" is not a nice property then — it is the price of deployment. Building it
before it is mandatory is what makes it cheap.

**7. Stay boring in the substrate and ambitious in the model.** SQLite, hash
chains, pure folds. The innovation belongs in the *semantics* — evidence over
truth, four-axis trust, revisable identity, bitemporality — not in the storage
engine. Every robotics knowledge system that chose an exotic substrate spent its
innovation budget on operations.

---

The through-line: **Kirra World's contribution is not that it knows more. It is
that it can account for everything it claims to know.** Every decision above
protects that one property, and it is the property that would make it worth
standardizing.
