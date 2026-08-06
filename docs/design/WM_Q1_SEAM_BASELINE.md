# Q1 seam — baseline measurement

**KIRRA-WM-Q1-BASELINE-001** · recorded 2026-08-06

## This is NOT the Q1 disposition, and must not be read as one

[ADR-0040](../adr/0040-world-model-ownership-and-boundary.md)'s open question 1
was **dispositioned by deferral on 2026-08-06**, adopted unamended by the World
Model owner. The seam between `kirra-world` and `kirra-world-store` is
**retained**. That ruling carries a revisit trigger, quoted exactly:

> **Revisit trigger:** when the domain core carries real types and the store
> consumes them — i.e. on completion of `WM_SCOPE.md` Tier 1 — measure what the
> seam actually carries. **If it is still near-empty, collapse it**, under the
> authorization this ADR already gives. Not before: until then the measurement
> has no content, because the dependency runs store → core and an empty core
> means an empty seam.

**Tier 1 is not complete**, so the trigger has not fired and no disposition is
proposed here. This document exists for one reason: when the trigger does fire,
the measurement will be compared against *something*, and a baseline taken
before the comparison is worth more than one reconstructed after it.

Reading this as a recommendation to collapse the seam would be the specific
error the ruling's final sentence forecloses.

## What the seam carries today

Counting unit: **one `use`/`pub use` item naming a `kirra_world` path**, in the
`src/` tree of a crate that depends on `kirra-world`. Independence unit: the
consuming crate. Held fixed: workspace at `ea06132d`, default features, no
`cfg` gating on any of the lines below.

| Consumer | Lines referencing `kirra_world` | Types consumed |
|---|---|---|
| `kirra-world-store` | 1 | `EntityId`, `ObservationId` |
| `kirra-world-service` | 1 | `ResolutionOutcome` |
| **Total** | **2** | **3** |

Verbatim, this is the entire seam:

```rust
// crates/kirra-world-store/src/lib.rs:92
pub use kirra_world::{EntityId, ObservationId};

// crates/kirra-world-service/src/lib.rs:24
pub use kirra_world::ResolutionOutcome;
```

Both are `pub use` re-exports. **Neither crate calls a method, constructs a
value, or matches on a variant of any core type.** The dependency is declared in
both manifests and, in the direction that matters, carries nothing.

### The emptiness is deliberate, not neglect

Worth stating so this baseline is not read as an accusation. Both lines carry a
comment explaining why they exist:

> `// Re-exported so the dependency edge is real rather than declared-and-unused.`
> — `kirra-world-store/src/lib.rs`
>
> `// Both edges exercised, so the proposed three-node graph is genuinely built and`
> `// not merely described in a manifest.`
> — `kirra-world-service/src/lib.rs`

They were placed to satisfy ADR-0040's ratification criterion asking for a
*prototype crate graph* — a real compile-time edge rather than a manifest entry.
They do that job. They were never intended to carry domain types, because at the
time there were none to carry.

### All three types are unconstructible

Each is a crate-root placeholder with a private unit field:

| Exported | Definition | Constructible outside the crate |
|---|---|---|
| `kirra_world::EntityId` | `pub struct EntityId(());` | No |
| `kirra_world::ObservationId` | `pub struct ObservationId(());` | No |
| `kirra_world::ResolutionOutcome` | `pub struct ResolutionOutcome(());` | No |

So the measurement is not merely "near-empty" — the seam's three types cannot
be instantiated by the crates importing them. What crosses it is three names.

## A defect this measurement surfaced

`kirra_world` currently exports **two distinct types named `EntityId`**:

| Path | Definition | Status |
|---|---|---|
| `kirra_world::EntityId` | `EntityId(())` | Crate-root placeholder, unconstructible |
| `kirra_world::entity::EntityId` | `EntityId(String)` | The real one, added with the §6 taxonomy |

The store re-exports the **placeholder**, so `kirra_world_store::EntityId` is an
unconstructible type that is *not* the domain model's entity id, while looking
exactly like it.

Introduced by the entity-taxonomy slice: a real `EntityId` was added in
`entity.rs` without retiring the root placeholder. Inside the crate nothing is
confused — `relationship.rs` correctly names `crate::entity::EntityId` — so the
collision lives only on the public surface, which is also where it is least
likely to be noticed.

**It propagates two hops.** `kirra-world-service` re-exports the store's
re-export:

```rust
// crates/kirra-world-service/src/lib.rs:25
pub use kirra_world_store::EntityId;
```

So `kirra_world_service::EntityId` is also the unconstructible placeholder, now
three names away from the real `kirra_world::entity::EntityId`. Each hop makes
the substitution harder to see, and the service is the layer an integrator would
import from.

`ObservationId` and `ResolutionOutcome` do not yet have real counterparts, so
they are placeholders rather than shadows. They will acquire the same shape if
their real versions land beside them rather than replacing them.

**Not fixed here.** This document records; the fix is a separate change, and is
worth making before the store begins consuming core types in earnest, since
every new import is a chance to bind the wrong one.

## What this implies for Tier 1 — and what it does not

It does **not** imply the seam should collapse. The ruling's reasoning explains
this measurement rather than being challenged by it: *"an empty core means an
empty seam"*. The core was placeholders until 2026-08-06; four modules of real
domain logic landed that day, and the store has not yet been asked to use any of
them.

What it does clarify is that **Tier 1's remaining work is implementation, not
decision.** The store already has ULID generation, content hashing and a schema;
the core now has the four trust axes, the observation model's pure half, the
entity taxonomy and the relationship model. What is missing is the wiring
between them — which is the same sentence as the revisit trigger's precondition,
*"the store consumes them."*

This corrects a claim made in conversation on 2026-08-06 and worth stating
plainly, since it would have sent work in the wrong direction: §6's and §7's
open fields (`entity_id` generation, `observation_id`, `evidence_digest`,
`prev_hash`, `frame`/`map`) were described as blocked on an owner ruling on Q1.
They are not. Q1 is dispositioned, the seam is retained, and the retained seam
already answers where those fields live — the **store**. No further ruling is
required to proceed.

## When the trigger fires

Re-run the same measurement — same counting unit, same two consumers — and
compare against the table above. Two outcomes, both already authorized:

* **The seam carries real consumption** (constructed values, matched variants,
  called methods): retained, and this baseline is the evidence of what changed.
* **The seam is still near-empty**: collapse it, per the authorization ADR-0040
  already gives, treating `kirra-world-store` as a feature of `kirra-world`.

Repeating the measurement is cheap — two greps — and the point of recording it
now is that "near-empty" should be a comparison, not an impression.

---

Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
SIL 3 requirements. Independent third-party assessment has not yet been
performed.
