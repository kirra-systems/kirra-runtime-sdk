# What a candidate is identified by — a ruling request

**KIRRA-WM-CANDIDATE-ID-001** · drafted 2026-08-08 · **status: RULED —
§6 adopted 2026-08-08**

> **RULED — 2026-08-08 by Justin Looney, World Model owner.** §6 is adopted: the
> derived value stays out of the evidence, and the stored vocabulary was narrowed
> to `entity`/`frame` **before release**, in the same pull request that introduced
> the column.
>
> **§1's finding is not deleted by this ruling** (constraint 1). The record stands
> that a pure computation's output was placed inside canonically-hashed bytes,
> that the justification written for it never mentioned purity, and that it was
> removed before shipping rather than never having been there.
>
> **What the ruling cost, recorded because it was predicted.** Constraint 3 said
> an observation about a candidate would store `unbound`. It does. It also cost a
> test: `promoting_a_candidate_to_an_entity_breaks_the_chain` was the sharpest
> tamper case in the suite — silently upgrading something unadjudicated into a
> resolved entity — and that edit can no longer be staged, because no such row can
> exist. The surviving form relabels a frame. Named in the test file rather than
> quietly rewritten.
>
> **Deferred, not settled**: `CandidateId`, and whether candidate membership as a
> projection needs its own key. Both belong to entity resolution. If storing
> `unbound` proves unacceptable in practice, that is evidence for reading A and
> reopens this ruling.
>
> **One approver.** Recorded by the same person holding every role, as with every
> other World Model ruling.

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

| | |
|---|---|
| **Question** | What identifies a `SubjectRef::Candidate`, and may that identifier enter the hashed evidence record? |
| **Blocks** | Giving `SubjectRef` validated newtypes; the resolved-entity projection; `entity_id` minting (`WM_SCOPE.md` §5) |
| **Raised by** | `crates/kirra-world/src/observation.rs` — `Candidate(String)`, unchanged since 2026-08-06 |
| **Made load-bearing by** | #1394 / #1395, which made the candidate token storable inside canonically-hashed bytes |
| **Decides** | Whether a derived value may sit in an append-only evidence row |

---

## 1. The finding — I made a *pure* computation's output part of the immutable record

The blueprint draws identity as a three-step pipeline, and annotates which steps
are which (§6, line 338):

```
observations ──► candidate clustering ──► identity assertion ──► entity
                       (pure)              (recorded Event)
```

**Candidate clustering is marked pure. Identity assertion is the recorded
event.** Those annotations are the specification's own, and they are the whole
of what it says about the boundary.

#1394 gave `SubjectRef` a stored discriminant; #1395 bound it to a `subject_kind`
column *inside the canonical form*, so the token and its id are covered by the
row's SHA-256 and by the chain. The stored vocabulary is `entity`, `candidate`,
`frame`.

Admitting `candidate` there puts **the output of a pure, re-runnable computation
into an append-only row that no later run can correct**. Nothing in the store
detects the resulting disagreement: after a clustering change, historical rows
cite `cand-7` while a re-run of the same pure function over the same evidence
groups those observations differently or not at all. Both are then "true" — the
frozen label and the recomputed one — and the chain vouches for the frozen one.

This is not the ordinary case of a chain recording something unreproducible.
`source_version` and a sensor's `frame_id` are *inputs* — observed facts about
the world, and the row is their only record. A candidate id is an **output of a
computation over other rows in the same store**. The store already separates
these: derived values live in projection tables (`projection.rs`,
`subject_projection.rs`), which are rebuilt by folding the log. A candidate id is
a projection value that I placed in the evidence.

Recorded first because it is the more useful half: I shipped this, the
justification I wrote for it never mentioned purity, and #1395 is still open.

## 2. Two smaller findings, both about the same type

**`SubjectRef::id()` collapses three kinds into one accessor.**

```rust
pub fn id(&self) -> Option<&str> {
    match self {
        Self::Entity(id) | Self::Candidate(id) | Self::Frame(id) => Some(id),
        Self::Unbound => None,
    }
}
```

A caller receiving `Some("cup-1")` cannot tell whether entity resolution has
adjudicated that thing. This is the same collapse `KIRRA-WM-SPLIT-SURVIVAL-001`
§5 refuses for `redirects_to` — *"collapsing them into one accessor would make
'redirects to one thing' and 'was several things' indistinguishable at the call
site"*. Here it makes *adjudicated* and *not yet adjudicated* indistinguishable,
which is the distinction the enum exists to draw.

**`Entity` and `Candidate` are adjacent same-type variants.** `Entity(String)`
and `Candidate(String)` accept each other's values and compile. That is precisely
the failure `reference.rs` was written to remove one day later, on 2026-08-07,
when `NewEvent`'s two adjacent same-type pairs became validated newtypes because
*"either pair could be passed in the wrong order and still compile"*.
`observation.rs` (2026-08-06) predates that hardening and never received it.

So #1395's tamper-evidence is real but narrow: the column proves nobody
*relabelled* a candidate as an entity after the fact, while nothing stops a
caller putting a candidate's id inside `Entity(...)` at construction.

## 3. What the specification actually says

Three mentions, in full:

| Location | Text |
|---|---|
| §6, line 338 | the pipeline diagram above, with `(pure)` and `(recorded Event)` |
| §7, line 362 | `subject : SubjectRef  // entity, candidate, frame, or unbound` |
| §24, E3 | *"Candidate clustering, merge/split events, redirects"* |

**Nowhere defines what identifies a candidate, how long that identifier lives, or
what happens to it on promotion.** This is a specification gap of exactly the
shape already ruled on twice — `Evidence` as an unelaborated parameter name in
three verb signatures (#1391), and split survivorship stated for merge and
omitted for split (#1392). The house treatment is to supply the reading
explicitly and record that it was supplied rather than found.

## 4. The readings

### A — a `CandidateId` newtype, stored like an `EntityId`

Clustering mints candidate ids; they are first-class and durable.

Cost: it makes clustering a **recorded** step, contradicting the `(pure)`
annotation. Something must persist the minting or the ids are not stable across
runs — and once minting is recorded, "candidate clustering" and "identity
assertion" are both recorded events, which erases the pipeline's only structural
distinction.

### B — candidates carry an `EntityId`; a candidate is a provisional entity

Tempting, because `Lifecycle::Provisional` reads *"newly created, not yet
corroborated"*.

**Refuted by the code.** `Entity::provisional(id: EntityId, …)` takes an id
already in hand, so minting precedes even the earliest entity state. A candidate
is pre-assertion; a provisional entity is post-assertion and merely
uncorroborated. B also inverts `WM_SCOPE.md` §5's own argument for why minting
sits at Tier 2 — *"minting an id is deciding that something is a distinct
thing"* — by minting before the deciding.

Worth stating because B is the reading a reader arrives at from the two doc
comments alone, and it is wrong.

### C — `Candidate` carries no id

Collapses toward `Unbound`. Loses the ability to say *"this observation is about
the same unadjudicated thing as that one"* — which is the entire output of
clustering, and the reason the variant is not already `Unbound`.

### D — `Candidate` stays in the domain, but is **not storable**

The stored vocabulary narrows to `entity` and `frame`. Candidate membership lives
in a **projection**, rebuilt by folding the log, where a derived value belongs and
where a clustering change simply produces a different fold.

Cost: an observation recorded while its subject was only a candidate stores
`unbound` plus a projection row, rather than saying so in the evidence. That is a
real loss and it should be named: the row no longer records *what the system
believed at the time*.

## 5. The case that decides it — promotion

Candidate `c1` is asserted as entity `e1`. Historical rows labelled
`Candidate("c1")` must not be rewritten — `SubjectRef::Unbound`'s own docs make
this structural: *"an observation may be recorded before anything decides what it
is about, and re-attribution later must not rewrite it."*

So promotion needs a **redirect**, `c1 → e1`, the same shape the blueprint
already settles for merge (*"both original IDs remain resolvable forever and
answer with a redirect"*).

A durable redirect needs a durable left-hand side. And that is the contradiction
in one line:

> **Clustering is specified as pure, but a stored reference to its output must be
> durable.**

Reading A resolves it by making clustering recorded — paying the specification's
own distinction. Reading D resolves it by keeping the derived value out of the
evidence, so no durable reference to it ever exists. C cannot express promotion
at all. B is refuted.

Neither A nor D is free, and the choice is not a matter of taste: **A changes
what the pipeline means, D changes what a row records.**

## 6. Recommendation

**Adopt D now, and revisit A when entity resolution lands — while narrowing the
stored vocabulary before it ships, not after.**

1. **Drop `candidate` from the stored vocabulary** in #1395, leaving `entity` and
   `frame`. The v3 column is unreleased; narrowing a `CHECK` and a match arm
   today costs a diff, and after release costs a migration over hashed rows,
   where the old token is inside digests that must keep verifying.
2. **Keep `SubjectRef::Candidate` in the domain.** It is a legitimate in-memory
   reference and #1394's core is unaffected. `from_stored_parts` may keep
   admitting the token — a re-admission path that accepts a value the writer
   never produces is harmless, and tightening it would be a breaking change to a
   merged type for no gain.
3. **Give `SubjectRef` its newtypes** — `Entity(EntityId)`, `Frame(FrameId)` —
   which becomes mechanical once `Candidate`'s id is not a storage concern, and
   removes the adjacent-same-type hazard in §2 for the two variants that have
   types. Split `id()` so an entity id and a frame id are not returned through
   one `Option<&str>`.
4. **Defer `CandidateId`** to the entity-resolution slice, where it will be a
   *projection key*. If that slice finds it needs durability in the evidence,
   that is reading A and it should be taken as its own ruling, with the pipeline
   annotation changed to match rather than left contradicted.

**Not recommended: leaving the token stored and deciding later.** The cost of
this decision is asymmetric in time and only in one direction. Every row written
with a `candidate` label between now and the ruling is a row whose digest covers
a value the ruling may say does not belong there — and those digests cannot be
recomputed away.

## 7. What adoption would and would not authorize

**Would**: narrowing #1395's `CHECK` and its vocabulary test before merge;
`SubjectRef::Entity(EntityId)` / `Frame(FrameId)`; splitting `id()`; designing
candidate membership as a projection.

**Would not**: authorize entity resolution or `entity_id` minting (both remain
open Tier 2 boxes); settle how clustering groups observations; create
`CandidateId`; or decide whether `Provisional` and "candidate" should be
reconciled in the lifecycle — §4 B is refuted as an *identity* reading, which is
not the same as ruling the two concepts never converge.

## 8. Constraints on adoption

1. **The §1 finding is not deleted by ticking a box.** If D is adopted, the
   record should keep that a pure computation's output was placed in hashed
   evidence and removed before release — not just that the vocabulary is two
   tokens. A proposal whose finding vanishes on adoption is the failure
   `KIRRA-WM-TIER1-DONE-001`'s second constraint names.
2. **Narrowing needs its own negative control.** The existing closed-vocabulary
   test passes against a `CHECK` admitting anything; removing a token needs a
   control proving the test fires on its return.
3. **§4 D's cost is stated, not solved.** An observation about a candidate will
   store `unbound`. If that proves unacceptable in practice it is evidence for A,
   and it should reopen this ruling rather than be worked around.
4. **One approver.** As with every World Model ruling, recorded by the same
   person holding every role. Stated so nobody infers independence from a
   decision being written down.

---

## Appendix — where each claim was checked

| Claim | Checked against |
|---|---|
| Clustering is annotated `(pure)`, assertion `(recorded Event)` | `WORLD_MODEL_ARCHITECTURE.md:338` — read, not recalled |
| Those three are the *only* mentions of candidates | grep for `candidate`, case-insensitive, across the blueprint: lines 338, 362, 1077 |
| `Candidate(String)`, adjacent to `Entity(String)` | `observation.rs:344-350` |
| `id()` returns `Option<&str>` for all three | `observation.rs:439-444` |
| `Entity::provisional` takes an `EntityId` | `entity.rs:613` |
| `Lifecycle::Provisional` is *"newly created, not yet corroborated"* | `entity.rs:415-416` |
| No `CandidateId` type exists in code | `grep -rnE '(struct\|enum\|type) +CandidateId' --include=*.rs crates/` — zero hits. **Narrowed after review**: the first wording claimed zero hits across all `*.rs` and `*.md`, which was true when run and self-falsifying the moment this document was written. An appendix claim that stops being re-runnable is worse than a narrower one |
| Nothing constructs `SubjectRef::Candidate` outside its own module and tests | grep across `crates/` |
| `NewEvent`'s newtype hardening postdates `observation.rs` | `WM_SCOPE.md` §4 — reference.rs Strand A dated 2026-08-07, observation.rs 2026-08-06 |
| Merge's redirect is settled in the blueprint | `WORLD_MODEL_ARCHITECTURE.md:342-344` |
| The split proposal refuses the same accessor collapse | `WM_SPLIT_SOURCE_PROPOSAL.md` §5 item 1 |
