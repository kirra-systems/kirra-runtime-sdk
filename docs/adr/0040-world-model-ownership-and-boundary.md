# ADR-0040 (WM-1): Establish `kirra-world` as the owner of semantic world evidence

| Field | Value |
|---|---|
| Status | **Proposed — NOT ratified on merge.** See *Ratification criteria*. Merging records the proposal; it does not ratify it and authorizes no implementation. |
| Date | 2026-08-02 |
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
| `robot/testdata/places.json`, `routes.json` | Named places + saved routes with `map_id` | Operator-authored JSON |
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

## Open questions

1. Does `kirra-world-store` earn its separation? Prototype graph decides.
2. Should the destination resolver migrate to a consumer in the first
   implementation phase, or later once the store is proven?
3. Who owns `Capability` — the agent or the World Model? The blueprint keeps it
   a category (§4.2); ownership across a fleet is unresolved.
4. Naming collision disposition (ADR-0039 C1/C3).
5. Does the dashboard's "administrative write" need a distinct writer class, or
   is it an operator-calibration adapter with a different transport?

---

## Ratification criteria

**Proposed.** Accepted only when all are recorded:

- [ ] **Repository dependency review** — the proposed crate graph reviewed
      against the existing workspace
- [ ] **Prototype crate graph** — `kirra-world` compiling as a leaf with no
      ROS, no actuation, and no checker edge (ADR-0039 baseline preserved)
- [ ] **Compatibility inventory** — each row of the compatibility table
      confirmed by its current owner
- [ ] **Deployment ownership decision** — who runs it, where it stores, who
      backs it up
- [ ] Open questions 1 and 4 dispositioned

Merging this PR satisfies none of the above.
