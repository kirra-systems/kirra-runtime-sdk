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

**Not fixed here** — this document records. **Fixed in the next commit on this
branch**, before the store begins consuming core types in earnest, since every
new import is a chance to bind the wrong one: the root placeholder is replaced
by `pub use entity::EntityId`, so all three hops now resolve to the domain type,
and `kirra-world-service` carries a regression test that fails to *compile* if
the placeholder returns.

The measurement above is left as it was taken, describing the tree at
`ea06132d`. It is a baseline, and rewriting it to match a later tree would
defeat the point of having one.

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

---

# Measurement 2 — after the retention driver and the reference types

**recorded 2026-08-07** · workspace at `26c4c19b`

## Why this is appended rather than edited in

A baseline you revise is not a baseline. The table above stands as recorded at
`ea06132d` and is not corrected below — including in the one place it went stale
almost immediately, noted next.

## The baseline was superseded by the same pull request that took it

Worth stating plainly rather than leaving for someone to notice. `#1375` took the
baseline in its first commit (`58fa1188`, at `ea06132d`) and then, three commits
later, added the retention driver and sweeper — which import `kirra_world`
directly. So the "2 lines / 3 types" figure was accurate when measured and
outdated before that PR merged.

That is not a flaw in the measurement; it is the trigger's own logic working.
The ruling said the seam would fill as the core gained real types and the store
consumed them, and it began filling within the hour. But it does mean the
baseline's *number* has a much shorter shelf life than its *method*, and the
method is what should be reused.

## What the seam carries now

Same counting unit: **one `use`/`pub use` item naming a `kirra_world` path**, in
the `src/` tree of a crate that depends on `kirra-world`. Same independence unit:
the consuming crate. Held fixed: default features, no `cfg` gating except where
counted separately below.

| Consumer | Lines (non-test) | Lines (test-only `mod tests`) |
|---|---|---|
| `kirra-world-store` | 7 | 2 |
| `kirra-world-service` | 1 | 0 |
| **Total** | **8** | **2** |

Test-only lines are reported separately because they answer a different
question. A seam carrying only test traffic would still be near-empty in the
sense the ruling cares about; these eight are production paths.

## The qualitative claim has inverted, and that is the load-bearing part

The baseline's finding was not really "2 lines". It was this:

> **Neither crate calls a method, constructs a value, or matches on a variant of
> any core type.** The dependency is declared in both manifests and, in the
> direction that matters, carries nothing.

**That sentence is now false in every clause.** In `kirra-world-store` alone:

- **Constructs values** — `EventId::new`, `ObservationId::new`, `FrameId::new`
  and `MapId::new` are called on both the write path and the chain-verification
  read path.
- **Calls methods** — `as_str()` on all four, feeding both the SQL parameters and
  the canonically-hashed JSON. 16 such call sites in `lib.rs`.
- **Matches on variants** — `retention::decide`'s `RetentionDecision`, plus
  `Eligibility`, `CompactablePrefix` and `Blocker`, drive `run_retention_pass`.
- **Is bound by core types in its own public API** — `NewEvent`'s four reference
  fields are core types, so a caller of the store cannot build an event without
  going through them.

That last point is the one that matters most for a future collapse decision. A
seam carrying only re-exports can be dissolved by moving two lines. A seam whose
consumer's public struct is *made of* the core's types is load-bearing in the
ordinary sense.

## What this does NOT authorize

The trigger has still **not fired.** It is keyed to *completion of `WM_SCOPE.md`
Tier 1*, and Tier 1 is not complete: the entity taxonomy's store-dependent half
(`entity_id` minting, `first_observed`/`last_observed`, `provenance_head`) and
the observation model's payload half (`TypedPayload`'s body, `ObservationKind`)
are open. This is a second data point on a trend, not a verdict.

If anything, the direction of the trend argues for taking one further
measurement *at* completion rather than treating this one as sufficient — the
ruling asked for a measurement at a defined moment, and answering early with a
more convenient number is the failure mode it was written against.
