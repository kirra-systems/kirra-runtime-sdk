# ADR-0040 (WM-1): Establish `kirra-world` as the owner of semantic world evidence

| Field | Value |
|---|---|
| Status | **Accepted** — 2026-08-06. All five ratification criteria recorded. Acceptance settles ownership, boundary and deployment; it **authorizes no implementation** and carries two conditions — the `PerceivedObject` import rule and the Tier 1 retention driver. See *Acceptance record*. |
| Date | 2026-08-02 (proposed) · 2026-08-06 (accepted) |
| Accepted by | **Justin Looney**, holding the World Model owner, architecture owner and deployment owner roles. One approver across all three — recorded plainly rather than as three sign-offs, following [`ADR-0041`](0041-world-model-persistence-architecture.md)'s precedent. |
| Blueprint | `KIRRA-WM-ARCH-001` §4, §5, §14, §15 (WM-1) — [`docs/design/WORLD_MODEL_ARCHITECTURE.md`](../design/WORLD_MODEL_ARCHITECTURE.md) |
| Deciders | World Model owner · architecture owner · deployment owner |
| Depends on | [`ADR-0039`](0039-world-model-bidirectional-governor-fence.md) (WM-6) — the fence constrains every option below |
| **Clarified by** | **[ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md)** — canonical terminology (Decision 1) and the semantic-map boundary (Decision 2). |
| Cross-refs | [`ADR-0035`](0035-verifier-crate-decomposition.md) (crate decomposition precedent) · [`crates/kirra-sidecars/src/destination.rs`](../../crates/kirra-sidecars/src/destination.rs) · [`robot/world_model.py`](../../robot/world_model.py) · [`robot/location_registry.py`](../../robot/location_registry.py) |

> **Convention deviation** — as ADR-0039: *not* ratified on merge. Ratification
> requires a dependency review and a prototype crate graph, which a merge
> cannot supply.

---

## Terminology and what the name does NOT imply

The subsystem's canonical name is **Kirra World**
([ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md)
Decision 1). The crate is `kirra-world`; the generic phrase is *semantic world
model*; the unrelated perception concept is an *independent perception channel*.

> **Naming confers no authority.** "Kirra World" names the subsystem that owns
> semantic evidence. It does **not** make that evidence authoritative for any
> safety decision, and owning a category (including `Map`) does **not** make
> Kirra World the source of the checker's corresponding safety input. Ownership
> is about who curates and answers for the data — never about who the checker
> may believe.

## Context

The blueprint proposes that one subsystem own semantic world evidence. Today
that knowledge is scattered:

| Where | What it holds | Nature |
|---|---|---|
| [`robot/world_model.py`](../../robot/world_model.py) | Posture, perception, stop reason, operator — TTL'd, `UNKNOWN` on stale | Read projection, explicitly non-authoritative, opt-in |
| [`crates/kirra-sidecars/src/destination.rs`](../../crates/kirra-sidecars/src/destination.rs) | `PlaceRegistry`, `RouteRegistry` — operator-calibrated coordinates | Trusted config, fail-closed resolver |
| [`robot/testdata/places.example.json`](../../robot/testdata/places.example.json), [`routes.example.json`](../../robot/testdata/routes.example.json) | Named places + saved routes with `map_id` | Operator-authored JSON; the deployed paths are set by `KIRRA_DEST_PLACES_PATH` / `KIRRA_DEST_ROUTES_PATH`, and these are the shipped examples of their shape |
| [`crates/kirra-sidecars/src/destination_service.rs`](../../crates/kirra-sidecars/src/destination_service.rs) | Grounded destination latch, frame-explicit | Apply-once, seq-ordered |
| Perception (`kirra-taj`) | Objects, corridor, health | Live, per-tick |

None is wrong. Each was built for its purpose. But there is no shared account
of provenance, no history, and no single place to ask "why do you believe
that?".

---

## Decision drivers

1. **ADR-0039 compliance.** Whatever owns knowledge must be structurally
   fenceable in both directions. This eliminates any option that co-locates
   knowledge with actuation or with the checker.
2. **Determinism.** The blueprint requires projections to be a pure fold over
   evidence, testable under the existing virtual clock.
3. **ROS independence.** Knowledge must not require a ROS runtime; the
   reference robot has one, future targets may not.
4. **Embedded deployment.** Single Jetson, offline-capable, modest resources.
5. **Language and runtime consistency.** The safety spine is Rust; persistence,
   migrations and hash-chaining are Rust.
6. **Testability without hardware.** The repository's precedent is pure cores
   with I/O behind seams.
7. **Migration cost** from the four existing knowledge locations.

---

## Options considered

| # | Option | Ownership clarity | Dependency direction | Determinism | ROS independence | Fenceable (ADR-0039) | Verdict |
|---|---|---|---|---|---|---|---|
| **A** | **Pure Rust crate `crates/kirra-world`**, optional service/adapter crates | ✅ explicit | ✅ leaf, depends on lean types only | ✅ pure core | ✅ none | ✅ crate-level, both directions | **Proposed** |
| B | Module inside an existing sidecar (`kirra-sidecars`) | ⚠️ blurred with planner/chat | ⚠️ inherits sidecar deps | ✅ | ✅ | ⚠️ fences the whole sidecar, not the module | Rejected |
| C | Standalone service with domain logic **and** persistence fused | ⚠️ ok | ❌ consumers depend on a process | ⚠️ needs a running service to test | ✅ | ✅ | Rejected |
| D | Python-first, extending `robot/world_model.py` | ⚠️ | ⚠️ | ❌ no type system for the domain | ✅ | ❌ Rust fence tooling does not cover it | Rejected |

**Why A.** It is the repository's established shape — ADR-0035 decomposed the
verifier into lean leaf crates precisely so heavy concerns stop leaking into
lean ones, and the actuation fence operates at crate granularity. Option B
would force the fence to cover `kirra-sidecars` wholesale, which already houses
the planner and chat surfaces. Option C makes the domain untestable without a
process. Option D puts the domain model outside the type system and outside
every structural gate the repository has.

---

## Proposed decision

### Crate layout

```
crates/kirra-world/          PURE domain core — entities, observations,
                             relationships, provenance, trust axes,
                             bitemporal time, projections, query contracts.
                             No I/O. No ROS. Storage behind a trait seam.

crates/kirra-world-store/    Persistence adapter implementing the storage
                             seam (see ADR-0041). SEPARATE because the core
                             must be testable without a database, and because
                             ADR-0038's hybrid precedent may later want a
                             second backend.

crates/kirra-world-service/  Local API adapter. Thin: it adapts the core's
                             typed contracts to a wire form and owns no
                             domain logic.

robot/ + ROS adapters        Ingestion and compatibility only. They produce
                             observations; they never define the domain.
```

**The three-crate split is justified, not assumed.** `kirra-world-store` is
separate because ADR-0041 defers the persistence decision to measurement — a
core that compiles without a storage backend is what keeps that decision
reversible. `kirra-world-service` is separate because ADR-0039 Fence A is
easier to state over a crate that has no transport dependencies at all.

If the prototype crate graph shows the store seam adds cost without buying
reversibility, collapsing `kirra-world-store` into `kirra-world` behind a
feature is an acceptable simplification — that is a ratification input, not a
decision to make on paper.

### Ownership

`kirra-world` **owns**:

| Concern | Note |
|---|---|
| Entity identity | Including merge/split adjudication |
| Immutable observations | The evidence log |
| Relationships | Typed, directed, time-bounded |
| Provenance | Source chains and derivation records |
| Trust axes | Origin · Corroboration · Adjudication · Validity |
| Bitemporal timestamps | Valid time and transaction time |
| Projections | Derived views, deterministic |
| Semantic queries | The only read path |
| Schema versions | Domain-level |
| Persistence migrations | Delegated to the store adapter, owned here |

`kirra-world` **does not own** — and these subsystems do not own knowledge:

| Subsystem | Its role |
|---|---|
| **Mick** | Read-only consumer; produces operator assertions as *evidence* |
| **Occy** | Read-only consumer of planning context |
| **Destination resolver** | Read-only resolution; may migrate to be a consumer |
| **ROS adapters** | Ingestion only |
| **Perception** | Produces observations; does not own the store |
| **Governor** | **No relationship whatsoever** (ADR-0039 Fence B) |

### Write authority

Accepted writer classes:

- operator-calibration adapter
- perception ingestion adapter
- map importer
- mission-state adapter
- system configuration importer
- validated external-service adapter

**A language model MUST NOT write confirmed facts.** LLM output may create only:

```
suggestion · candidate label · candidate relationship · candidate query
```

Each is stored with `origin = Asserted`, `adjudication = Pending`, and remains
Pending until validated by a trusted source or confirmed by an operator.

This extends the rule already enforced in `destination.rs`, where a
coordinate-shaped field in a language-sourced request is refused outright
(`DEST_COORDINATES_FORBIDDEN`). **Language selects; trusted sources supply.**

### Read authority

| Consumer | Access |
|---|---|
| Mick | Read-only query |
| Destination resolver | Read-only resolution |
| Occy | Read-only planning context |
| Skills | Capability-scoped read |
| Dashboard | Read + explicitly authorized administrative writes (audited) |
| Fleet adapter | Future bounded synchronization |
| **Governor / checker** | **None** — ADR-0039 |

### Service boundary

**The typed in-process Rust API is canonical.** Wire APIs adapt it; they never
define the domain.

First implementation: **library form only** (`kirra-world` as a dependency).
A local service (Unix socket preferred over localhost HTTP, since it inherits
filesystem permissions and cannot be reached off-box by accident) is added when
a non-Rust consumer needs it — the Python robot layer being the likely first.

Rationale: every wire format is a second schema to migrate. Deferring it keeps
one.

### Failure behaviour

Explicit outcomes, never `Option` where uncertainty matters:

```
Resolved · NotFound · Ambiguous · Stale · Conflict · Invalid · Unavailable · Unsupported
```

`Option::None` collapses "we looked and it is not there", "we could not look",
and "the answer is contradictory" into one value. The destination resolver's
`ResolveOutcome` already demonstrates the better pattern in this repository.

---

## Compatibility treatment

**Nothing is deleted or rewritten in this PR**, and nothing should be in the
first implementation PR either.

| Existing | Disposition | Rationale |
|---|---|---|
| [`robot/world_model.py`](../../robot/world_model.py) | **Compatibility projection — retain** | It is a correct, tested, opt-in read projection whose freshness rule is the origin of Fence B. It may later read *through* `kirra-world`, but its contract stays |
| `places.json` / `routes.json` | **Import source** | Become `Configuration` observations carrying operator provenance and `map_id` — gaining history without changing the files |
| [`crates/kirra-sidecars/src/destination.rs`](../../crates/kirra-sidecars/src/destination.rs) | **Adapter, then consumer** | Its `ResolveOutcome` contract is the model for the query API; it migrates from owning registries to querying them |
| [`crates/kirra-sidecars/src/destination_service.rs`](../../crates/kirra-sidecars/src/destination_service.rs) | **Retain** | The frame-explicit latch is a delivery mechanism, not knowledge storage |
| Tracked-object inputs | **Import source** | Perception observations; already carry confidence and staleness |
| [`robot/location_registry.py`](../../robot/location_registry.py) | **Retain, read-only** | Deliberately never writes; that discipline should survive |

**Naming collision (from ADR-0039 C1/C3):** `robot/world_model.py` and the
`perception_redundancy.rs` sense of "world model" both predate this subsystem.
A disposition is a ratification input.

---

## Consequences

**Positive.** One owner for semantic evidence. Domain testable without a
database, a service, ROS, or hardware. Fence-able at crate granularity in both
directions. Existing components keep working unchanged.

**Negative / accepted.** Three crates where one might do — justified above but
real. A period where knowledge exists in both the old locations and the new
store; the compatibility table exists so that period is deliberate rather than
accidental. An in-process-only first cut means the Python layer waits.

---

## Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | Crate split proves to be over-engineering | Collapsing `-store` into a feature is pre-authorized above |
| R2 | Dual-source period drifts (registry vs store disagree) | Registries are the import source, not a parallel truth; one direction only |
| R3 | LLM output leaks in as confirmed fact | `Pending` adjudication enforced in the type system, not by convention |
| R4 | Service form is added prematurely, creating a second schema | Library-first is the decision; a service needs its own justification |
| R5 | `kirra-world` accretes planner logic | Fence A (ADR-0039) forbids becoming a planner; review checklist |

---

## Alternatives rejected

- **Option B (sidecar module).** Blurs ownership with the planner and chat
  surfaces and makes the fence coarser than the concern.
- **Option C (fused service).** Domain becomes untestable without a process.
- **Option D (Python-first).** Outside the type system and outside every
  structural gate the repository relies on.
- **Single crate with everything.** Rejected mainly because it forces the
  persistence decision (ADR-0041) to be made before it can be measured.

---

## Assurance impact

**No new safety claim is made here, and no scope determination is asserted.**

The *intent* is that `kirra-world` remains outside the safety decision and
authorization scope if Fence A and Fence B hold. **That determination is PENDING
an explicit safety-assurance ruling**
([ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md)
Decision 5) and must not be stated as settled. This ADR neither claims it nor
depends on it: ownership of semantic evidence is a curation decision, and
**naming a subsystem the owner of a category confers no safety authority**.

No existing safety claim, ASIL rating, or standards mapping changes.
Kirra is designed in alignment with ISO 26262 ASIL-D requirements and
IEC 61508 SIL 3 requirements. Independent third-party assessment has not yet
been performed.

---

## Migration impact

**None in this PR.** The first implementation PR should carry **domain types
only** — no storage, no service, no adapters — so the types can be reviewed
against these ADRs before anything persists (blueprint §23.1, step 5).

Existing consumers are unaffected until they choose to migrate. The registries
remain the operative coordinate source until an import path is proven.

---

## Prototype crate-graph findings

Built as `crates/kirra-world`, `-store`, `-service` — three crates of
**unconstructible placeholder types with no fields, no logic, no storage and no
API**. Recorded here because a prototype's value is what it *rules out*, and
because the boundary between what it settled and what it did not is easy to
overstate.

**Confirmed.**

| Claim | Result |
|---|---|
| `kirra-world` compiles as a leaf | **0 dependencies** — no dependency section at all, so it cannot acquire one silently |
| No ROS / actuation / checker edge | Fence A **INTACT** across all three crates |
| Safety closure unaffected | Still **19 workspace packages**; nothing in it reaches `kirra-world*` |
| The proposed three-node shape compiles | `-service → -store → kirra-world` builds, `cargo fmt` and `clippy -D warnings` clean |

**The fence is now live rather than reserved.** It previously reported *"Kirra
World packages not present yet — reserved fence validated"*; it now reports
**3 packages present** and checks them. Both directions were exercised against
the real crates, not fixtures:

- adding `kirra-release-token` to `kirra-world-service` → **Fence A breach**,
  naming the path `kirra-world-service -> kirra-release-token`;
- adding `kirra-world` to `kirra-trajectory` → **Fence B breach**, naming
  `kirra-ros2-adapter -> kirra-trajectory -> kirra-world`.

**Open question 1 is NOT closed.** The graph shows what the seam is *for*: a
consumer can depend on `kirra-world` alone and get the domain vocabulary with no
storage dependency — a future doer-side adapter that only needs to name entities
would not link a database. What it cannot show is whether that is worth the
maintenance cost, because **an empty crate splits cleanly from anything**. If no
second backend appears and nothing ever depends on the core without the store,
the honest answer will be that the seam did not earn its keep, and the collapse
into a `kirra-world` feature is already pre-authorized above for exactly that
outcome. Deciding it needs the storage implementation that ADR-0041 gates behind
measurement.

**What this does not do.** It ratifies nothing. All four World Model ADRs remain
**Proposed**. The safety-assurance scope ruling (ADR-0042 Decision 5) is still
**PENDING and unassigned**, and the first real domain-types work stays gated
behind it — the placeholders are unconstructible precisely so that gate cannot
be crossed by accident while it is open.

---

## Repository dependency review — findings, 2026-08-05

The first ratification item, performed against the workspace as it stands after
WM-2 shipped. **This is evidence for a ruling, not a ruling.** No checklist box
is ticked by this section; the reviews it feeds belong to the owners named
above.

### The graph as built matches the graph as proposed

| Crate | Direct dependencies |
|---|---|
| `kirra-world` | **0** |
| `kirra-world-store` | 6 — `kirra-world`, `kirra-audit-hash`, `rusqlite`, `serde_json`, `sha2`, `hex` |
| `kirra-world-service` | 2 — `kirra-world`, `kirra-world-store` |

`-service → -store → kirra-world`, with the core a true zero-dependency leaf.
Fence A holds; the safety closure is unchanged at 19 workspace packages from 10
roots.

### Open question 1 named the wrong prerequisite

The question is *"does `kirra-world-store` earn its separation?"*, and the ADR
records that its cost/benefit *"cannot [be shown] until storage exists."*

**Storage now exists** — 2 673 lines across schema, write path, chain,
projection, bitemporal queries and compaction, measured on target (D-20, D-21).
The question is still not decidable, and the reason is instructive:

> The seam carries **one line**. `kirra-world-store` uses exactly
> `pub use kirra_world::{EntityId, ObservationId}` — a re-export of two
> unconstructible placeholders which, by its own comment, exists *"so the
> dependency edge is real rather than declared-and-unused."* Nothing in the
> store's 2 673 lines consumes a domain type, because there are none to consume.

The dependency runs **store → core**. An empty core therefore means an empty
seam *regardless of how much store exists*. The prerequisite for deciding Q1 was
never storage; it is the **domain core** — Tier 1 of
[`WM_SCOPE.md`](../design/WM_SCOPE.md). Building more store cannot answer it.

Cost today is one manifest line and one re-export. Measurable benefit today is
zero. **Neither figure decides anything**, and quoting either as though it did
would be the error this section exists to prevent.

### The seam's original justification has partly been spent

ADR-0040 justifies the store seam as keeping ADR-0041's persistence decision
reversible while that decision was open. **ADR-0041 is now Accepted**
(2026-08-04, R1–R5 adopted), so the decision is made. Reversibility still has
value against ADR-0041's own reopening conditions, but that is the weaker
*"if reopened"* form, not the original *"while open"* form.

If the seam is kept, it should be kept on a justification that survives —
and a stronger one is available: the blueprint's §5 layering requires the domain
core to be **pure**, and collapsing the store into it would put `rusqlite`,
`serde_json`, `sha2` and `hex` in the same crate as the domain types, inverting
the direction §5 specifies. That argument does not depend on any decision
staying open.

### Compatibility inventory — prepared for confirmation, not confirmed

Each row checked against the tree. **Owner confirmation is still required**;
what follows only removes the "does this still exist and mean what it says"
question from the owner's plate.

| Row | State on 2026-08-05 |
|---|---|
| `robot/world_model.py` | **Exists**, 195 lines, still an opt-in TTL'd read projection. Matches "compatibility projection — retain" |
| `crates/kirra-sidecars/src/destination.rs` | **Exists**, 1 734 lines; the `ResolveOutcome` contract the query API is meant to model is present (55 references) |
| `crates/kirra-sidecars/src/destination_service.rs` | **Exists**, 717 lines. Matches "retain" |
| Tracked-object inputs | **Checked, and the row's factual half is only half true — see below.** The *disposition* still needs its owner |
| `places.json` / `routes.json` | **The compatibility row is right; the inventory row's path was wrong — now fixed.** The deployed files really are `places.json` / `routes.json` (set by `KIRRA_DEST_PLACES_PATH` / `KIRRA_DEST_ROUTES_PATH`), so the disposition above stands. What did not exist was `robot/testdata/places.json`: the shipped examples are `places.example.json` / `routes.example.json`, and the inventory now cites those |

#### Tracked-object inputs — "already carry confidence and staleness" is true of the wrong type

The row's rationale reads *"Perception observations; already carry confidence
and staleness."* Checked against the tree, that is true of some perception
types and **false of the one the checker actually consumes**:

| Type | `confidence` | timestamp |
|---|---|---|
| `kirra_taj::CameraVruObservation` | **yes** | **yes** (`stamp_ms`) |
| `kirra_taj` object-goal / intent types | **yes** | — |
| **`kirra_core::trajectory::PerceivedObject`** | **no** | **no** |

`PerceivedObject` — id, position, velocity, heading, velocity vector — is what
the RSS and trajectory checks run on, and it carries neither. **Staleness on
that path is a property of the *channel*, not of the object**: freshness is
enforced by subscription policy (`KIRRA_SUBSCRIPTION_STALENESS_MS`) and by
`AcceptedTrajectory`'s `promoted_at_ms` + `max_age_ms`, never by a field
travelling with the datum.

**Why this matters to the disposition rather than being a nitpick.** The row
says these become import-source *observations*, and an observation in Kirra
World must carry its own provenance and validity. If the datum has neither, the
importer has to attach both from channel context — and "attach a confidence
from context" is precisely how a fabricated confidence enters an evidence
store wearing the same clothes as a measured one.

So the disposition is still plausible, but it is **not free**, and whoever
confirms this row should confirm it knowing that the import needs a stated rule
for where confidence and validity come from on the `PerceivedObject` path.
That rule does not exist yet.

#### Re-verified 2026-08-06, and the gap is sharper than first stated

Re-checked against the tree before drafting the disposition below. The original
finding holds exactly:

| Type | `confidence` | timestamp | Consumed by the checker |
|---|---|---|---|
| `kirra_core::trajectory::PerceivedObject` | **no** | **no** | **yes** — `rss_tangent_frame` and the object-slice RSS passes in [`crates/kirra-trajectory/src/validation.rs`](../../crates/kirra-trajectory/src/validation.rs) |
| `kirra_taj::CameraVruObservation` | **yes** | **yes** (`stamp_ms`) | via the VRU channel |

`PerceivedObject` is `{id, pos, velocity_mps, heading_rad, vel}` — five fields,
none of them provenance. Freshness on that path stays a property of the
*channel*: `AcceptedTrajectory` carries `promoted_at_ms` + `max_age_ms`, and the
subscription budget is `KIRRA_SUBSCRIPTION_STALENESS_MS`. Nothing travels with
the datum.

**What the re-check added.** The store has exactly **one** machine-enforced
writer-class rule, and it does not cover this path:

* `WriterClass::LlmCandidate` may never write `ClaimStatus::Confirmed` — rejected
  at [`crates/kirra-world-store/src/lib.rs:523`](../../crates/kirra-world-store/src/lib.rs).
* `WriterClass::Sensor`, documented as *"a sensor or perception producer"*, has
  **no such constraint**. It may write `Confirmed` freely.
* Decoding an unrecognised writer class falls back to `LlmCandidate` — the most
  constrained variant. The store's instinct is fail-closed, which is why the
  uncovered path reads as an oversight rather than a decision.

So a `PerceivedObject` importer — which would plausibly be classed `Sensor`,
though **no importer exists yet and this is an inference, not an observation** —
would enter the store as a **`Confirmed` `Sensor` claim**: the strongest
assertion the store offers, built from a datum carrying neither a confidence nor
a time of its own. Both would be synthesized from channel context, and nothing
in the store would record that they were synthesized. The guard that exists
catches the *obviously* untrustworthy writer; it does not catch the *silently
under-determined* one.

#### Proposed wording — for the owner to accept, amend or reject

> **Tracked-object row — confirmed, split by type, one half conditional.
> Drafted 2026-08-06.**
>
> The disposition *"import-source observations"* is **confirmed unconditionally
> for perception types that carry their own confidence and timestamp**
> (`CameraVruObservation` and the `kirra_taj` object-goal / intent types). For
> those, the row's rationale is accurate as written.
>
> For **`PerceivedObject` it is confirmed conditionally**: the type may become an
> import source only once a **stated rule exists for where its confidence and
> validity come from**, and that rule must make the synthesis visible in the
> store rather than indistinguishable from a measured value. Until then no
> `PerceivedObject` import path may be built.
>
> **Where the rule belongs:** `WM_SCOPE.md` Tier 1 — the observation model and
> the four orthogonal trust axes. This is not new work invented by the
> condition; it is work Tier 1 already owns, and the anti-laundering rule
> (*derived inherits the weakest input on every axis*) is the principle the rule
> must satisfy.

#### This deferral is weaker than ADR-0042's OQ1, and the difference should be stated

ADR-0042's open question 1 was deferred on a **self-announcing** trigger: the
first breach reds Fence B, so no vigilance is required. **This condition has no
equivalent.** Nothing in CI fails if someone writes a `PerceivedObject` importer
that synthesizes a confidence — the store would accept it as a `Confirmed`
`Sensor` claim, exactly as intended for a real sensor.

What would make it self-enforcing is a narrow guard on the import boundary — a
requirement that any import from a type lacking a confidence field must declare
its confidence source, checked where importers are constructed. That guard does
not exist and is **not proposed here**, because designing it belongs with the
Tier 1 observation model rather than ahead of it. Recorded so the owner
confirms this row knowing the condition currently rests on remembering it.

#### RULED — 2026-08-06

> **CONFIRMED — 2026-08-06 by Justin Looney, World Model owner.** The wording
> above is **adopted unamended**: unconditional for perception types carrying
> their own confidence and timestamp, **conditional for `PerceivedObject`** —
> no import path may be built until a stated rule exists for where its
> confidence and validity come from, with the synthesis visible in the store
> rather than indistinguishable from a measured value. The rule belongs to
> `WM_SCOPE.md` Tier 1.
>
> **The import-boundary machine guard was offered and deliberately NOT taken.**
> Adding it now would remove this deferral's known weakness — that nothing reds
> in CI if an importer synthesizes a confidence — but it would mean designing
> the guard ahead of the Tier 1 observation model that defines what it should
> check. The weakness is therefore **accepted and recorded**, not mitigated:
> until Tier 1 lands, this condition rests on being remembered.

**One row of five.** The compatibility-inventory checkbox requires *each row
confirmed by its current owner*; this ruling confirms the `tracked-object
inputs` row only, so **that box stays unticked** pending the remaining four.

### Open question 4 appears already dispositioned

Q4 is *"naming collision disposition (ADR-0039 C1/C3)"*.

* **C1** — terminology collision — is **already ticked in ADR-0039**, decided by
  ADR-0042 Decision 1.
* **C3** — `robot/world_model.py` — has a disposition in **this ADR's own
  compatibility table** ("retain"), and ADR-0042 additionally puts its rename
  behind safety review because the module is imported, installer-staged and
  env-gated.

Both halves therefore have recorded dispositions. **Q4 looks like a bookkeeping
gap rather than an open question** — but closing it is the owner's act, not this
section's.

### Open question 1 is circular as stated, and the checklist already contains the way out

Ratification requires Q1 *dispositioned*. The finding above says Q1 cannot be
**answered** before the domain core exists. But ADR-0040 also states that
merging *"authorizes no implementation"* — so, read strictly, ratification waits
on a question that needs the implementation ratification gates.

Two things dissolve it.

**First, the checklist asks for a disposition, not an answer.** A deferral with
a recorded trigger *is* a disposition, and this repository already uses that
shape — ADR-0041's WM-2 milestone defers retention enforcement against a named
precondition rather than leaving it open-ended.

**Second, the outcome is already pre-authorized.** This ADR states that
collapsing `kirra-world-store` into `kirra-world` behind a feature is *"an
acceptable simplification"* if the seam does not earn its keep. So a deferral
concedes nothing that has not already been conceded.

#### Proposed wording — for the owner to accept, amend or reject

> **Q1 — dispositioned by deferral, 2026-08-05.** The seam is **retained** for
> now. Its original justification (keeping ADR-0041's persistence decision
> reversible *while open*) is spent, since ADR-0041 is Accepted; it is retained
> instead on the blueprint §5 layering argument — the domain core must stay
> pure, and collapsing the store into it would place `rusqlite`, `serde_json`,
> `sha2` and `hex` beside the domain types.
>
> **Revisit trigger:** when the domain core carries real types and the store
> consumes them — i.e. on completion of `WM_SCOPE.md` Tier 1 — measure what the
> seam actually carries. **If it is still near-empty, collapse it**, under the
> authorization this ADR already gives. Not before: until then the measurement
> has no content, because the dependency runs store → core and an empty core
> means an empty seam.

> **RULED — 2026-08-06 by Justin Looney, World Model owner and architecture
> owner.** The wording above is **adopted unamended**. The alternative ordering
> (build Tier 1 first, ratify with a real answer) was considered and declined:
> it leaves this ADR Proposed while its own subject matter is built, which is
> the circularity the deferral exists to dissolve.
>
> **The box this feeds is `Open questions 1 and 4 dispositioned` — a
> conjunction, and it stays UNTICKED.** Q1 is now dispositioned; Q4 is
> *"appears already dispositioned"*, which is a finding, not the owner's act.
> Ticking on a half-satisfied conjunction is exactly the drift these rulings
> exist to prevent.

The section below was the draft this ruling adopted, kept for the record.

This was **drafted, not decided** when written. Nothing in it ticked the box,
and the owner might have preferred the alternative ordering — build Tier 1 first
and ratify with a real answer — at the cost of leaving this ADR Proposed while
its own subject matter
is implemented, which is the situation ADR-0042 was created to correct.

**A note on the gate, so the deadlock is not overstated.** The domain-logic gate
is self-releasing and released; running
`ci/check_world_domain_logic_gate.py` reports *"Domain implementation is
unblocked. This gate no longer constrains the `kirra-world*` crates."*
ADR-0040's Proposed status is a governance position, not an enforced gate — so
Tier 1 is not mechanically blocked either way.

### Open question 6 — predictive containment

Raised 2026-08-05, because a layering diagram and the blueprint currently
disagree and nobody had noticed.

> **Should predictive state remain in a separate store referenced by Kirra
> World entity IDs, or may Kirra World host a separately fenced predictive
> namespace?**

**This does not gate ratification.** The checklist names open questions **1 and
4** specifically; raising a sixth must not silently enlarge that gate, and this
sentence exists so it cannot.

#### What the blueprint currently says

`KIRRA-WM-ARCH-001` §20 — *not* ADR-0041, which is where these rules are most
often mis-attributed because the persistence decisions live there:

> *"Predictions live in a **separate store** and reference World Model entity
> IDs. They are never observations... A prediction may not be promoted to an
> observation... The World Model stays deterministic. **A learned model in that
> path would destroy replay.**"*

And §9.1: `Predicted` **never appears in the evidence store**.

#### The distinction the question exists to protect

Two things are both "the LLM's opinion", and only one is admitted today:

| | Example | Status |
|---|---|---|
| **LLM-originated candidate** — proposes something confirmable | *"I think that is the toolbox"* | **Already inside, already fenced** — `writer_class = llm_candidate`, refused `confirmed` by the schema (SD-2), excluded from the confirmed-only fold, reachable only by naming `candidates()` |
| **Predictive belief** — a probability over unobserved state | *"The keys are probably still near the door"* | **Outside.** §9.1 |

A diagram that nests "the cognitive layer" inside Kirra World collapses these,
and the collapse is invisible because both are the same thing to a reader.

#### The default, unless and until a ruling changes it

* the prediction store is **separate**;
* predictions **never become observations**;
* predictions **cannot enter confirmed-only projections**;
* predictive consumers get a **read seam, not write authority** over confirmed
  knowledge.

#### Why it is worth asking rather than assuming

The "separately fenced namespace" option is not obviously wrong — the store
already demonstrates that hostile content can live inside a structure that
refuses to fold it, which is exactly what SD-2 and the confirmed-only fold do
for `llm_candidate`. The argument against is §20's: a learned model *in the
projection path* destroys replay, and replay is what the whole evidence-first
inversion buys.

Answering it means deciding whether "separately fenced" can be made as
structural as SD-2 is — a schema-level refusal, not a convention. That is a
ruling, not a diagram.

### One further checkbox may be tickable

ADR-0039's safety-assurance item says the ruling *"must be **recorded**; it need
not be favourable."* **Decision 5 was recorded on 2026-08-05.** That box is
still unticked. Flagged, not ticked.

### Deployment ownership cannot be prepared — only framed

The one item with no technical component. Recorded here is what the decision
*costs*, since two of its three parts already have measured consequences:

* **Where it stores** is a capacity decision, not a path decision. D-20/D-21
  measured **15.79 days** to fill 8 GiB at 10 Hz on the ratified schema, and no
  retention driver exists yet — so "where" implies "how big" and "who empties
  it".
* **Who backs it up** interacts with the ledger's tamper-evidence. Per ADR-0038
  the hash-chained audit ledger is **per-instance and local**; a backup regime
  that copies or merges instances has to preserve chain semantics, or the
  property the ledger exists for does not survive the backup.
* **Who runs it** is unconstrained by anything measured, and is a pure
  ownership question.

---

## Open questions

1. Does `kirra-world-store` earn its separation? Prototype graph decides.
   **Partially answered — see *Prototype crate-graph findings*. The seam's
   purpose is demonstrated; its cost/benefit is not, and cannot be until storage
   exists. Remains open.**
2. Should the destination resolver migrate to a consumer in the first
   implementation phase, or later once the store is proven?
3. Who owns `Capability` — the agent or the World Model? The blueprint keeps it
   a category (§4.2); ownership across a fleet is unresolved.
4. Naming collision disposition (ADR-0039 C1/C3).
   **DISPOSITIONED — Justin Looney, 2026-08-06.** Both halves already carried
   dispositions recorded elsewhere; this records that fact rather than making a
   new decision.
   * **C1** (terminology collision) — disposed by ADR-0042 Decision 1, and
     already ticked in ADR-0039's own checklist. Executed in code: the two
     safety-closure uses now read *independent perception channel*.
   * **C3** (`robot/world_model.py`) — disposed twice over: this ADR's
     compatibility table says **retain**, and ADR-0042 puts any rename behind
     safety review because the module is imported by `rabbit_converse.py`,
     staged by the installer, and gated by `KIRRA_WORLD_MODEL_ENABLED` —
     renaming it changes robot deployment, not prose.
   No new decision was required. What was missing was the record.
   **The ratification box stays unticked**: it reads *"open questions 1 **and**
   4 dispositioned"*, and Q1 remains drafted-not-decided.
5. Does the dashboard's "administrative write" need a distinct writer class, or
   is it an operator-calibration adapter with a different transport?
6. **Predictive containment.** Should predictive state remain in a **separate
   store** referenced by Kirra World entity IDs, or may Kirra World host a
   **separately fenced predictive namespace**? See *Open question 6 —
   predictive containment* above, in the dependency-review findings. **Does not gate ratification**: the checklist
   names open questions 1 and 4 specifically, and raising this must not
   silently enlarge that gate.

---

## Ratification criteria

**Proposed.** Accepted only when all are recorded:

- [x] **Repository dependency review** — the proposed crate graph reviewed
      against the existing workspace

      **SIGNED OFF — 2026-08-06 by Justin Looney.** On the findings recorded in
      *Repository dependency review — findings, 2026-08-05* above: the graph as
      built matches the graph as proposed. The review also surfaced four things
      that were acted on rather than filed — Q1's wrong prerequisite, the seam's
      partly-spent justification, the tracked-object row's half-true rationale,
      and a wrong inventory citation — so this sign-off covers a review that
      changed the ADR, not one that merely confirmed it.
- [x] **Prototype crate graph** — `kirra-world` compiling as a leaf with no
      ROS, no actuation, and no checker edge (ADR-0039 baseline preserved).
      **Built; findings below. Ratifies nothing else in this ADR.**
- [x] **Compatibility inventory** — each row of the compatibility table
      confirmed by its current owner

      **CONFIRMED — all five rows, by Justin Looney.** Four rows confirmed
      2026-08-06 (`robot/world_model.py`, `destination.rs`,
      `destination_service.rs`, `places.json`/`routes.json` — each verified
      present against the tree and matching its stated disposition, with the
      `places`/`routes` citation corrected to the shipped `*.example.json`
      files). The fifth, **tracked-object inputs**, was confirmed separately
      with a **condition** — see the ruling above: unconditional for perception
      types carrying their own confidence and timestamp, conditional for
      `PerceivedObject`, whose import path may not be built until a stated rule
      exists for where its confidence and validity come from.

      **This box therefore closes over one conditional row.** That is
      deliberate: the row is confirmed, and its condition is carried in the
      inventory rather than left as an unticked box, because the condition binds
      Tier 1 work rather than this ADR's ratification.
- [x] **Deployment ownership decision** — who runs it, where it stores, who
      backs it up

      **DECIDED — 2026-08-06 by Justin Looney, deployment owner.**

      | Part | Decision |
      |---|---|
      | **Who runs it** | The **verifier's operator**. Kirra World is co-located with the verifier rather than given its own operator. |
      | **Where it stores** | **Local SQLite**, alongside the verifier's own database — not the Postgres shared tier. A semantic, non-authoritative store does not belong in the tier the control plane depends on. |
      | **Who backs it up** | The **verifier's existing backup regime**, which already respects [`ADR-0038`](0038-postgres-shared-state-hybrid.md)'s per-instance local audit ledger. Inherited rather than designed, so the chain semantics that regime already preserves are not re-derived for a second system. |

      **The capacity consequence is bound, not accepted silently.** D-20/D-21
      measured **15.79 days** to fill 8 GiB at 10 Hz on the ratified schema, and
      no retention driver exists. This decision makes a **retention driver an
      explicit exit criterion for `WM_SCOPE.md` Tier 1** — so the fill date
      cannot arrive unowned, and Tier 1 cannot be called done without it.

      **What co-location costs, stated rather than glossed.** A knowledge store
      now shares a host with the safety verifier, so its disk pressure is the
      verifier's disk pressure — which is precisely why the retention criterion
      is attached rather than deferred. The blast-radius separation that a
      dedicated host would have given is traded for a backup regime that already
      exists and already handles the ledger correctly.
- [x] Open questions 1 and 4 dispositioned

      **BOTH HALVES NOW RULED — the conjunction is satisfied, 2026-08-06.**

      **Q1 (the `kirra-world` / `kirra-world-store` seam)** — dispositioned by
      **deferral**: the seam is retained on the blueprint §5 layering argument,
      with a revisit trigger at Tier 1 completion. Full ruling above.

      **Q4 (naming-collision disposition, ADR-0039 C1/C3)** — **dispositioned as
      already settled.** C1 is ticked in ADR-0039, decided by ADR-0042
      Decision 1. C3 (`robot/world_model.py`) carries "retain" in this ADR's own
      compatibility table, with any rename behind safety review because the
      module is imported by `rabbit_converse.py`, installer-staged, and gated by
      the live `KIRRA_WORLD_MODEL_ENABLED`. Q4 was a **bookkeeping gap, not a
      live question** — but closing it was the owner's act, which is what this
      records. The rename is **not** decided here; it stays at "retain".

      **This box stayed unticked while only Q1 was ruled** — earlier the same
      day — because a conjunction is not half-satisfiable. It ticks now because
      both halves are ruled, not because the second was assumed to follow.

Merging the PR that **introduced this ADR** satisfied none of the above.
Boxes above are ticked only by a separately recorded owner ruling, each of
which names its owner and date inline — never by the act of merging a
document change.

---

## Acceptance record

**Accepted 2026-08-06 by Justin Looney**, holding the World Model owner,
architecture owner and deployment owner roles — one approver across all three,
recorded plainly rather than as three sign-offs.

### Two conditions ride on this acceptance

Neither gated it, and both bind Tier 1 rather than this ADR:

| Condition | Source | Enforced by |
|---|---|---|
| **No `PerceivedObject` import path** until a stated rule exists for where its confidence and validity come from, with the synthesis visible in the store | Compatibility inventory, tracked-object row | **Partly** — the *prohibition* half is checked (fence check 9, 2026-08-07); the *requirement* half is still unenforced. See below |
| **A retention driver** is an exit criterion for `WM_SCOPE.md` Tier 1 | Deployment-ownership decision | The Tier 1 checklist |

### PARTLY machine-checked since 2026-08-07 — and only one half of it

Recorded as a later fact, **not an amendment**: the ruling below stands
unchanged, and nothing here revisits it.

The condition has two halves, and they need different things:

* **The prohibition** — *"no `PerceivedObject` import path may be built"* —
  needs no rule to exist, because it forbids rather than requires. It is now
  checked by **check 9** of `ci/check_kirra_world_bidirectional_fence.py`: any
  reference to the type in a `kirra-world*` package's code reds CI. Comments and
  string literals are stripped, so the quotation of this condition in
  `observation.rs` is a description of the rule rather than a breach of it.
  Three tests hold it: that it fires on a real import path, that prose does not
  trip it, and that its scope is `kirra-world*` only.

* **The requirement** — *"any import from a type lacking a confidence field must
  declare its confidence source"* — is the guard offered and **deliberately not
  taken** below, and it is **still not built**. It needs the Tier 1 observation
  model to define what it should check, exactly as the ruling says.

So the weakness recorded below is **narrowed, not closed**. What now reds CI is
someone building the import path inside Kirra World. What still rests on being
remembered is an importer that synthesizes a confidence — and one built outside
`kirra-world*`, which check 9 cannot see. That scope limit is asserted by its
own test rather than left for a reader to infer from a green run.

### The first condition is not machine-checked, and that was a choice

An import-boundary guard was offered and **deliberately declined**: designing it
now would mean guessing what it should check, ahead of the Tier 1 observation
model that defines the trust axes. So this condition rests on being remembered —
unlike ADR-0042's open question 1, whose breach reds Fence B and therefore
announces itself. Recorded here rather than in the row alone, so an accepted ADR
does not quietly carry an unenforced condition.

### What acceptance settled, and what it did not

**Settled.** Crate layout and ownership, write and read authority, the service
boundary, failure behaviour, the compatibility treatment of all five existing
surfaces, the seam's retention with a Tier 1 revisit, and deployment ownership
in all three parts.

**Not settled.** No implementation is authorized. Open questions 2, 3, 5 and 6
remain open — including **Q6 (predictive containment)**, which the dependency
review raised and which was explicitly kept out of the ratification gate so that
raising it could not silently enlarge the criteria.

### Deployment ownership, and its cost

Kirra World runs **co-located with the verifier**, on **local SQLite**, under the
**verifier's existing backup regime**. The regime is inherited rather than
designed, so [`ADR-0038`](0038-postgres-shared-state-hybrid.md)'s per-instance
local audit ledger keeps the chain semantics that regime already preserves.

The cost is stated rather than glossed: **a knowledge store now shares a host
with the safety verifier**, so its disk pressure is the verifier's. That is
exactly why the retention driver became a Tier 1 exit criterion instead of a
later concern — the measured 15.79-day fill must not arrive unowned.

### Independence posture

Owner self-assessment throughout; no independent review. Kirra is designed in
alignment with ISO 26262 ASIL-D requirements and IEC 61508 SIL 3 requirements;
independent third-party assessment has not yet been performed.
