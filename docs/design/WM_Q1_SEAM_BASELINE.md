# Q1 seam — baseline measurement

**KIRRA-WM-Q1-BASELINE-001** · recorded 2026-08-06

> **Reading order.** This document is a series of **dated measurements**, each
> standing as taken. Statements below about the trigger *not* having fired were
> true when written and are **not** revised — see
> [THE TRIGGER HAS FIRED — 2026-08-07](#the-trigger-has-fired--2026-08-07) at the
> end for current state. A pointer, not an edit: a baseline you revise is not a
> baseline, but one a reader can misquote as current is not much better.

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

---

# Measurement 3 — at the Tier 1 boundary

**recorded 2026-08-07** · `main` at `9a0712da`, with `claude/wm-typed-payload`
(#1381) at `ff394e89`

> #1381's head advanced to `230edc4f` after this was recorded — a base-branch
> update, not new work. `ff394e89` remains an ancestor of it, so the command
> below still resolves, and the empty-diff claim was re-verified against **both**
> heads. Noted rather than silently restamped: this document's SHAs say which
> tree was measured, and quietly moving them would make a later reader trust a
> pairing nobody checked.

## The precondition, stated before the number rather than after it

The revisit trigger fires on *"completion of `WM_SCOPE.md` Tier 1"*. On a strict
reading of Tier 1's own checklist, **it has not fired**, and this document does
not claim it has. Two boxes are unticked even with #1381 applied:

| Tier 1 box | What is still open under it |
|---|---|
| **Entity taxonomy (§6)** | identity *adjudication* — candidate clustering, merge/split, `entity_id` minting — all of which that entry explicitly assigns to **Tier 2** |
| **Observation model (§7)** | `evidence_digest` / `prev_hash` as core types; the store computes both today as bare hex strings |

ADR-0040's own ruling on Q1 supplies the discipline for this situation, and it
is quoted here because it cuts against the convenient reading:

> Ticking on a half-satisfied conjunction is exactly the drift these rulings
> exist to prevent.

So the box is not ticked and no disposition is declared. What follows is the
measurement, plus the argument for why taking it *now* is not the early answer
[Measurement 2](#measurement-2--after-the-retention-driver-and-the-reference-types)
warned against.

### One record says otherwise, and the disagreement is worth naming

#1381's pull request describes itself as firing this trigger — *"Closes Tier 1's
last implementation item, and with it **fires ADR-0040's Q1 revisit trigger**"*.
This document takes the narrower reading, so a reader meeting both should not
have to guess which governs.

**The durable record agrees with this document.** #1381 leaves both §6's and
§7's checkboxes `- [ ]`; it ticks neither. `WM_SCOPE.md` is the record the
trigger names, and by that record Tier 1 is open. The pull request's prose is
commentary on a slice, and it is defensible on its own terms — Tier 1's
*implementation* work is what that slice closed, and §6's residue really is
assigned to Tier 2.

The gap between the two is the word **completion**. "Every implementation item
is done" and "the tier is complete" are not the same claim while two boxes are
unticked, and the trigger is keyed to the second. Resolving that is the owner's
call, not a measurement's — which is precisely why nothing here is ticked and no
disposition is declared.

## Why measuring now is not answering early

Measurement 2 closed by warning that *"answering early with a more convenient
number is the failure mode it was written against."* Three checks, each
mechanical rather than argued, establish that this measurement is not that.

**1 — It is invariant to the merged-pending item.** `#1381` is the slice that
marks §7's last implementation item done. Its diff against `main`, restricted to
the two consumer crates, is **empty**:

```
git fetch origin refs/pull/1381/head
git diff --stat 9a0712da ff394e89 \
  -- crates/kirra-world-store/ crates/kirra-world-service/
(no output)
```

Written against the **pull request ref** rather than the branch name it was
originally run with, because both of the obvious forms stop working. The branch
`claude/wm-typed-payload` is deleted on merge; and #1381 is **squash**-merged, so
`ff394e89` is not an ancestor of `main` afterwards and a fresh clone cannot
resolve it either. `refs/pull/1381/head` outlives both. A reproduction command in
a document about reproducibility should survive the merge it describes.

It changes `crates/kirra-world/src/` and documentation only. The seam is counted
in the *consumers*, so every number below is identical on both trees. Waiting for
#1381 to merge would produce the same table.

**2 — It is monotone in what remains.** `evidence_digest` / `prev_hash` becoming
core types would mean the store *importing two more core types*, not fewer.
§6's residue is Tier 2 by that entry's own assignment. Neither outstanding item
can remove a line from the table.

**3 — Therefore it is a lower bound.** Everything below is the *smallest* the
seam will be when Tier 1 formally closes. The direction that would favour
collapse is the one the remaining work cannot travel.

That third point is what makes recording now defensible rather than premature. A
lower bound that already clears "near-empty" by a wide margin cannot be reversed
by finishing the tier — so the number was fixed before anyone could know whether
it would be convenient, which is the property a baseline is supposed to have.

## The measurement — the baseline's unit, unchanged

Counting unit: **one `use`/`pub use` item naming a `kirra_world` path**, in the
`src/` tree of a crate that depends on `kirra-world`. Independence unit: the
consuming crate. Held fixed: `main` at `9a0712da`, default features, no `cfg`
gating other than the test/non-test split, which is reported separately.

**Population checked, not assumed.** Still exactly two crates depend on
`kirra-world` directly. `tools/wm2-schema-growth` and
`tools/wm2-persistence-harness` were inspected and are not in the population —
the former depends on `kirra-world-store`, the latter on neither, by its own
manifest's stated design.

| Consumer | Lines (non-test) | Lines (test-only) |
|---|---|---|
| `kirra-world-store` | 8 | 2 |
| `kirra-world-service` | 1 | 0 |
| **Total** | **9** | **2** |

On the baseline's own unit the trend is **2 → 8 → 9**.

## The baseline's unit is the weakest thing in this document

Said plainly, because the number above is the one most likely to be quoted and
it understates the change by a wide margin.

**One `use` item is insensitive to a braced list growing.** Two examples from
this very tree:

* `lib.rs:108` went from `{EntityId, ObservationId}` at baseline to
  `{EntityId, EventId, FrameId, MapId, ObservationId, ReferenceError}` — six
  names — and did not move the count.
* `lib.rs:104` imports **nine** names from `kirra_world::trust` in a single item.

So a second unit is reported. It is not a replacement — the first is what makes
this comparable to the baseline, and swapping units between measurements would
destroy the comparison the baseline exists to enable.

| Unit | Baseline | Measurement 3 |
|---|---|---|
| `use` items naming a `kirra_world` path | 2 | **9** |
| **distinct core paths named** | 3 | **26** |

The 26 are: the six reference/identity types, nine from `trust::`, seven from
`retention::`, two from `observation::`, `ReferenceError`, and
`ResolutionOutcome`.

## What the seam *does*, which is the part the ruling actually asked about

The baseline's finding was never really "2 lines". It was this sentence:

> **Neither crate calls a method, constructs a value, or matches on a variant of
> any core type.** The dependency is declared in both manifests and, in the
> direction that matters, carries nothing.

Measurement 2 reported that sentence had become false. Measurement 3 puts
numbers on it. All counts are non-test code, doc comments excluded:

| Property | Baseline | Measurement 3 |
|---|---|---|
| constructor call sites (`EventId::new`, `ObservationId::new`, …) | 0 | **10** |
| variant references in code (`RetentionDecision::…`, `Blocker::…`, …) | 0 | **29** |
| **public struct fields typed by a core type** | 0 | **7** |
| **public fn signatures naming a core type** | 0 | **5** |

The last two rows are the ones that decide a collapse question, so they are
listed rather than summarised.

**Public fields (7)** — five of them are on `NewEvent`, which is the store's
only write-path entry:

```
lib.rs:388               pub event_id: &'a EventId,
lib.rs:392               pub observation_id: &'a ObservationId,
lib.rs:411               pub frame_id: Option<&'a FrameId>,
lib.rs:415               pub map_id: Option<&'a MapId>,
lib.rs:442               pub trust: Option<&'a TrustAxes>,
projection.rs:163        pub trust: Option<crate::TrustAxes>,
retention_driver.rs:56   pub decision: RetentionDecision,
```

**Public signatures (5)**:

```
projection.rs:180        pub fn validity_at(…) -> crate::Validity
projection.rs:199        pub fn grade_at(…) -> Option<crate::TrustGrade>
retention_driver.rs:91   pub fn retention_survey(&self, policy: &RetentionPolicy, now: DomainInstant, …)
retention_driver.rs:234  pub fn run_retention_pass(&mut self, policy: &RetentionPolicy, now: DomainInstant, …)
retention_sweeper.rs:137 pub fn start(path: &Path, policy: RetentionPolicy, interval: Duration, …)
```

Reduced to one sentence: **a caller cannot append an event to the store, or run
a retention pass over it, without constructing core values first.** A seam
carrying re-exports dissolves by moving two lines; a seam whose consumer's write
path is *made of* the core's types does not.

## The method is now an instrument, and it immediately corrected me

Measurement 2 said the method outlives the number:

> the baseline's *number* has a much shorter shelf life than its *method*, and
> the method is what should be reused.

Three measurements have now re-derived that method by hand. It is committed as
`ci/measure_q1_seam.py`, which reproduces every figure above.

**Its first act was to correct this document.** The constructor count was
hand-counted as 11; the instrument reported 10. The difference was
`schema.rs:155` — `/// \`TrustAxes::new\` refuses it` — a doc comment. The hand
count excluded comments when tallying variant references and forgot to when
tallying constructors, which is exactly the kind of inconsistency that survives
review and quietly inflates a series.

Worth stating rather than silently shipping the corrected figure: the error
inflated the number in the direction of the conclusion this measurement reaches.
A hand-counted instrument that drifts toward its own finding is the failure the
baseline's method discipline exists to catch, and here it took a script to catch
it.

**Its second act was to expose a fail-open in itself.** Reverting the population
check to confirm it fires -- a guard that cannot fire is not a guard -- was run
from a copy outside the tree. It duly reported a changed population, and also
reported `actual: []`: finding no crates, it had measured *nothing* and rendered
that as an empty seam.

That is the wrong failure direction here, and not marginally. "The seam is
near-empty" is the exact finding that authorizes collapsing it, so an instrument
that returns zero when it cannot find the tree returns the collapse-authorizing
answer on its own error. It now refuses to measure instead, because a missing
tree and an empty seam produce identical numbers and mean opposite things.

Both controls are recorded rather than merely run: the population check fires on
a real tree with a shortened `RECORDED_CONSUMERS`, and the lost-tree guard fires
on a copy, while the committed script passes.

**It is deliberately not a CI gate.** Every figure it reports is *supposed* to
change — the seam filling is the outcome the trigger was written to detect — so
a gate would red on progress and be silenced. The one genuine invariant is
offered as `--check-population`: if a third crate starts depending on
`kirra-world`, the independence unit has changed and the recorded series is no
longer comparable without saying so. That is a fact about the measurement, not
about the code, so it is left for whoever takes Measurement 4 to run.

## The finding the total would hide: one edge filled, the other did not

Reporting "9 lines" for "the seam" would leave a reader with the impression that
the graph filled. It did not. **8 of the 9 are one edge.**

| Edge | Baseline | Measurement 3 |
|---|---|---|
| `kirra-world-store` → `kirra-world` | 1 line, 2 names, 0 constructions | 8 lines, 25 names, 10 constructions |
| `kirra-world-service` → `kirra-world` | 1 line, 1 name, 0 constructions | **1 line, 1 name, 0 constructions** |

The service edge is **verbatim what it was at baseline** — a single
`pub use kirra_world::ResolutionOutcome;`.

This is **not** a counter-finding against Q1, and should not be read as one. Q1
names the `kirra-world` / `kirra-world-store` seam specifically; the service is
outside its scope. And that crate's emptiness is deliberate and documented in
its own module header — it exists so the fence has something to walk:

> A *service* crate is where that would most plausibly erode — a transport
> dependency added "just to publish status", a ROS handle threaded through
> "temporarily". … A fence that arrives with the code is a fence argued with.
> This one is already here.

It is recorded here because the *aggregate* row is the one that travels, and it
would carry a claim about the service edge that the evidence does not support.

## What this selects — and what remains the owner's act

ADR-0040 pre-authorized both outcomes, so the selection is mechanical:

* **The seam carries real consumption** — 10 constructions, 29 variant
  references, 12 public API bindings, and a write-path struct made of core
  types. **Retained.**
* *The seam is still near-empty* — not the case, by a wide margin, at a
  measurement that is a lower bound.

Two things are worth separating, because collapsing them is how a measurement
turns into a ruling nobody made.

**Recording this is low-cost, and that asymmetry is why it can be done now.**
The outcome it selects — retain — is the one that requires *no action*. Collapse
would be a restructuring; retention is the status quo continuing. A
retain-pointing measurement filed before the tier formally closes therefore
commits nothing that a later ruling could not simply override.

**Closing Q1 is not this document's to do.** The trigger's precondition is two
unticked boxes away, and Q1's disposition was recorded by the World Model owner,
not derived from a table. What this removes is the measurement from the critical
path: when those boxes tick, the number is already here.

**One more thing the measurement does not carry.** The original ruling retained
the seam on the **blueprint §5 layering argument** — *"the domain core must stay
pure, and collapsing the store into it would place `rusqlite`, `serde_json`,
`sha2` and `hex` beside the domain types."* That argument is independent of
anything measured here and is untouched by it. The measurement is a second
support for retention, not its only one — and had the number come out the other
way, the two would have been in tension rather than the number simply winning.

## THE TRIGGER HAS FIRED — 2026-08-07

Appended after the fact, and deliberately not folded into the measurement above.

`KIRRA-WM-TIER1-DONE-001` was adopted on 2026-08-07: **Tier 1 is done.** Both
Tier 1 boxes are ticked and both residues were relocated to the tiers that own
them. So the precondition this measurement was careful to say had *not* been
met is now met — on **both** of the trigger's clauses, not only the substantive
one, since ticking the boxes satisfies the proxy outright.

**Nothing above is revised.** The measurement stands exactly as taken, including
its own statement that the trigger had not fired, which was true when written.
Editing it to read as though it were taken *at* the moment of firing would
destroy the one property that makes it evidence: it was recorded and merged
(`ed0a82e5`) **before** the criterion that fired the trigger existed, so it
cannot have been shaped to suit it.

That ordering is worth stating plainly, because the reverse is the ordinary
failure: a measurement taken after a decision is due, by the person who wants a
particular answer, is a justification wearing a measurement's clothes.

**What this measurement puts in front of the ruling.** The two outcomes
ADR-0040 pre-authorized, and which one the evidence selects:

| Outcome | Selected? |
|---|---|
| The seam carries real consumption → **retained** | **yes** — 10 constructor sites, 29 variant references, 26 distinct core paths, 12 public API bindings |
| The seam is still near-empty → **collapse it** | no — and the figures are a *lower bound*, per the monotonicity argument above |

**Q1's disposition is still a separate act, and is not taken here.** The
adopting ruling says so in its own words. What has changed is only that the
question is now ripe, on evidence that predates the ripeness.

---

## Provenance

Measured, and recorded, by the same person holding every role — as with every
other World Model measurement. Stated so that nobody infers independence from a
number being written down.

---

Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
SIL 3 requirements. Independent third-party assessment has not yet been
performed.
