# What "candidate clustering" is — a ruling request

**KIRRA-WM-CLUSTERING-001** · drafted 2026-08-08 · **status: PROPOSED — not a
ruling, not an authorization**

| | |
|---|---|
| **Asks** | What candidate clustering *is*, before anything is built to do it |
| **Blocks** | `WM_SCOPE.md` §5's last open box; `Corroboration(n)`, which has no driver |
| **Blocked by** | Nothing. Every dependency it had is now merged |
| **Constrained by** | `KIRRA-WM-CANDIDATE-ID-001` (2026-08-08); ADR-0040 writer classes; §9's four axes |
| **Related** | `KIRRA-WM-SPLIT-SURVIVAL-001`, `KIRRA-WM-CANDIDATE-ID-001` |

---

## 0. Why this is a ruling request and not a slice

The two prior Tier 2 rulings each resolved a **tension**: the blueprint said two
things that could not both be true of a stored field. This one is different, and
the difference is the reason to stop before writing code.

**The blueprint specifies candidate clustering in one word and a parenthetical.**
It appears exactly twice in the whole document:

```
observations ──► candidate clustering ──► identity assertion ──► entity
                       (pure)              (recorded Event)
```

— §6.3, and one roadmap row: *"E3 — Entity resolution · Candidate clustering,
merge/split events, redirects"*. That is the entire specification. There is no
definition of what matching means, no features, no thresholds, no output type,
no worked example.

Everything else in Tier 2 was recoverable by reading carefully. This is not.
Building a matcher would mean **inventing a similarity model and calling it a
reading**, which is the failure both prior rulings were written to avoid.

---

## 1. What is already decided, and does real work

Four constraints exist. Together they rule out more than they look like they do.

### 1.1 It is marked **pure**

The only property §6.3 states. It is not decorative: pure means clustering is a
deterministic function of its inputs and produces **no recorded event**.
Identity *assertion* is the recorded event; clustering is what happens before
one. So a cluster is a **proposal**, never a fact — and nothing downstream may
treat it as evidence.

### 1.2 A candidate's identifier may not enter the hashed record

`KIRRA-WM-CANDIDATE-ID-001`, ruled 2026-08-08. The store's subject discriminant
admits `entity` and `frame` only, and `WorldStore::append` refuses a
`SubjectRef::Candidate` with `CandidateSubjectNotStorable`. **A cluster cannot
be written to the log at all.** Its output must be a projection or an in-memory
value.

That ruling also said, in terms: *"`CandidateId` is deferred to entity
resolution, where it would be a projection key rather than an evidence value."*
This is that deferral coming due.

### 1.3 `Corroboration(n)` is the concrete consumer, and it is idle

`kirra_world::trust` says so itself:

> **It does not populate `Corroborated(n)`.** Counting agreeing evidence
> presupposes knowing that two observations are *about the same thing*, which is
> entity resolution. The axis and its monotonic transitions are here; the matcher
> that would drive them is not.

This gives clustering a **usable definition it does not otherwise have**:
whatever it produces must answer *"are these two observations about the same
thing?"*, because that is the question `agreed()` needs answered before it may
be called.

### 1.4 An LLM may never write a confirmed fact

ADR-0040's writer classes, enforced at the write door: `writer_class =
llm_candidate` with `claim_status = confirmed` is refused (SD-2). Whatever
clustering becomes, an LLM-driven matcher is already boxed in by a rule that
predates it.

---

## 2. The trap, stated before the options

The obvious build is a similarity matcher: extract features, compute a distance,
threshold it, emit clusters. It should be named as a trap before it is
considered on its merits, because it fails in a way that is invisible from
inside the matcher.

**A threshold silently becomes a trust input.** `Corroborated(3)` feeds
`trust_grade`, which feeds answers. If a heuristic decides co-reference, then
that 3 means *"three observations a tuned threshold believes are the same
thing"* — and no consumer of the grade can tell that from three independent
sensors genuinely agreeing. The distinction is unrecoverable downstream, which
is the exact property §9's four-axis model exists to preserve:

> a single number cannot distinguish "one trusted sensor said so once" from
> "three sources agree but the claim is stale"

A tuned matcher adds a fourth thing it cannot distinguish — "a threshold thinks
these are the same" — and hides it inside a count that already means something
else. That is laundering inference into evidence.

**It is also unfalsifiable here.** Judging a matcher needs ground truth about
which observations were really about the same object. The store has none, and
manufacturing it would mean an operator labelling pairs — which is an
`Operator`-class observation, i.e. the declared route in §4.2 below, arrived at
by a longer path.

---

## 3. The readings

### 3.1 Reading A — inferred similarity clustering

Features per `ObservationKind`, a distance, a threshold, emitted clusters.

**Against:** §2, entirely. Also unbounded: the blueprint defines no feature set,
so every one would be invented here. And *per-deployment* — what makes two lidar
returns the same object on a warehouse robot is not what makes two operator
notes the same asset.

### 3.2 Reading B — exact-key clustering

Cluster observations that already share a resolved subject id.

**Against:** it presupposes the answer. Two observations sharing an `EntityId`
are already known to be about the same thing; clustering them discovers nothing
and populates `Corroborated(n)` with a tautology. Worth stating because it
*looks* like progress and would tick the box.

### 3.3 Reading C — declared co-reference

A producer **states** that two observations are about the same thing. Matching
becomes a *carried claim* rather than an inferred one, with a source, a
confidence and a provenance chain like every other claim.

**For:** it is honest about where the judgement came from; it populates
`Corroboration(n)` immediately; and it needs no similarity model.

**Against:** it does not do the hard part. A perception stack that cannot say
"this is the same track as last frame" gets no help.

### 3.4 Reading D — split the box

As the entity-resolution box was split into reading and matching halves, split
matching into the **contract** (what a cluster is, what it may key on, what it
may never become) and the **matcher** (which needs a similarity model that is a
research question, and likely per-deployment).

---

## 4. Recommendation — adopt D, and land C as its first driver

Two parts, and the second is smaller than it looks.

### 4.1 Rule the contract now

Settle what a cluster *is*, so any future matcher is constrained by something:

- A cluster is a **proposal**, never evidence. It has no chain position, no
  `event_id`, and cannot be cited by an adjudication's `Justification` — which
  cites `ObservationId`s, so this holds by construction today.
- A cluster's identifier is a **projection key**, minted by the projection and
  meaningless outside it. Already ruled (§1.2); restated because it is the rule
  a matcher is most likely to break by wanting stable candidate ids.
- Clustering is **pure** in the strong sense: a function of the observations
  folded, reproducible from the log, with the clock passed in. A cluster that
  cannot be rebuilt from the log is a projection-only fact.
- **Every co-reference judgement names its source.** No anonymous edges.

### 4.2 Co-reference is an observation, not new machinery

The finding that makes this cheap: **the model already expresses "these two are
the same thing".** `ObservationKind::Relationship` is *"that two subjects are
related"* and is the only kind whose `predicate` / `object` columns carry
meaning. A co-reference claim is a `Relationship` observation with a `same_as`
predicate.

That means it inherits, for free and unforgeably:

| Property | Where it comes from |
|---|---|
| Who said it | `source` + `writer_class` |
| How sure | `Confidence`, structured |
| What it rests on | `provenance` (SD-3) |
| Tamper-evidence | the hash chain, like any row |
| An LLM cannot confirm one | SD-2, already enforced |
| A derived matcher is visible as derived | `writer_class = derivation` |

So Reading A is **not forbidden by this proposal** — it is *relocated*. A
similarity matcher becomes a producer of `WriterClass::Derivation` `same_as`
observations with `claim_status = candidate`, and its output is admitted through
the same door as everything else, visible as inferred, and refusable. What it
may not do is reach into `Corroboration(n)` directly.

`Corroboration(n)` is then driven by folding **confirmed** `same_as` claims.
That is what keeps a threshold out of the trust axes: a heuristic's `candidate`
claim does not count until something adjudicates it, which is precisely the
`candidate → confirmed` line ADR-0040 already draws.

### 4.3 What this leaves genuinely open

Stated so adoption is not mistaken for completion:

- **No similarity model is supplied.** A perception stack still has nothing to
  match tracks with. That is the matcher, and it stays open.
- **What confirms a `same_as` candidate** is unruled. Operator confirmation is
  the obvious first answer and may be the only one for a while.
- **Transitivity is unruled.** If `a same_as b` and `b same_as c`, is `a same_as
  c`? Union-find says yes; evidence says only that two producers each made one
  claim. This matters because a wrong transitive closure merges two real
  entities, and §6.3 makes merges *revisable but recorded*. It should be ruled
  before any fold computes closures.

---

## 5. What adoption would and would not authorize

**Would**: rule the four contract points in §4.1; establish `same_as` as a
`Relationship` predicate; authorize a projection that folds confirmed `same_as`
claims into `Corroboration(n)`.

**Would not**: authorize a similarity matcher, a threshold, a feature set, any
transitive closure, or a `CandidateId` type. It does not tick `WM_SCOPE.md` §5's
matching box — it makes the box *smaller and specified* rather than large and
undefined.

---

## 6. Constraints on adoption

1. **The §0 finding is not deleted by adopting this.** The record stands that
   the blueprint specifies clustering in one word; a future reader must be able
   to see that the contract below was supplied, not derived.
2. **The three open items in §4.3 stay listed** in `WM_SCOPE.md` after adoption.
   A residue that disappears when a box is ticked is the failure
   `KIRRA-WM-TIER1-DONE-001` named.
3. **Any `same_as` fold ships with a negative control** proving a `candidate`
   claim does *not* advance `Corroboration(n)`. That is the load-bearing
   separation of §4.2 and the one most likely to be quietly relaxed for
   convenience.
4. **Transitivity is ruled before it is computed**, not discovered by a fold
   that already does it.

---

## Appendix — where each claim was checked

| Claim | Checked |
|---|---|
| Clustering appears twice in the blueprint, marked `(pure)` | `grep` over `WORLD_MODEL_ARCHITECTURE.md`: §6.3 diagram (line 338) and the E3 roadmap row (line 1077). No other occurrence |
| A candidate subject cannot be stored | `StoreError::CandidateSubjectNotStorable`; `subject_kind` vocabulary narrowed to `('entity','frame')` by the v3 migration |
| `CandidateId` was deferred *to here* | `WM_CANDIDATE_ID_PROPOSAL.md` §"would not"; `WM_SCOPE.md` §5 |
| `Corroboration(n)` has no driver | `kirra_world::trust` module docs, verbatim quote in §1.3 |
| `Corroboration::agreed` exists and is monotonic | `trust.rs:130`, `Uncorroborated → Corroborated(1) → Corroborated(n+1)`, saturating |
| An LLM cannot write a confirmed fact | `WorldStore::append` → `StoreError::LlmCannotConfirm`, refused before the statement is built |
| `Relationship` is the kind whose predicate/object carry meaning | `kirra_world::kind`, `ObservationKind::Relationship` docs |
| `Justification` cites `ObservationId`, so a cluster cannot be cited | `adjudication.rs`, `Justification(Vec<ObservationId>)` |
| The four writer classes include `derivation` | `world_events` CHECK: `('sensor','operator','derivation','llm_candidate')` |

| No producer already emits co-reference | `grep` for `same_as`/`coreference` across `.rs`/`.py`/`.md`: **zero hits**. The only predicates written anywhere in the workspace are `at`, `colour`, `located_in`, `position` — none co-reference-shaped, so `same_as` is unclaimed and §4.2 does not collide with an existing convention |

**Everything above was checked before drafting, not asserted.** The one place
this proposal is thin is stated in §4.3 rather than hidden here: it supplies no
similarity model, and a perception stack still has nothing to match tracks with.
That is deliberate — see §2 for why supplying one here would be worse than
leaving it open.
