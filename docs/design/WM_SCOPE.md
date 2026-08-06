# Kirra World — what is left, and what "done" means

| | |
|---|---|
| **Identifier** | KIRRA-WM-SCOPE-001 |
| **Status** | **SCOPE — not a ruling and not an authorization.** It records what remains against a definition of done taken from the blueprint, so that "how much is left" stops being re-estimated from memory. It ratifies nothing and authorizes no implementation. |
| **Blueprint** | `KIRRA-WM-ARCH-001` — [`WORLD_MODEL_ARCHITECTURE.md`](WORLD_MODEL_ARCHITECTURE.md), especially §9, §12, §14, §16, §22, §25 |
| **Depends on** | [ADR-0039](../adr/0039-world-model-bidirectional-governor-fence.md) · [ADR-0040](../adr/0040-world-model-ownership-and-boundary.md) · [ADR-0041](../adr/0041-world-model-persistence-architecture.md) · [ADR-0042](../adr/0042-world-model-terminology-and-safety-boundary-scope.md) |
| **Date** | 2026-08-05 |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

---

## 0. Naming, before anything else

Canonical name: **Kirra World**. Accurate prose gloss: **evidence ledger**.

**Not "world model"** — ADR-0042 Decision 1 ruled that off a measured collision,
and the reason is safety communication rather than taste: *"the world model was
wrong"* must not be able to mean a perception fault and a knowledge fault at
once. Two of the three colliding uses were **inside the safety closure** and
have since been renamed to *independent perception channel*; one is still live
(`robot/world_model.py`, whose rename ADR-0042 puts behind safety review).

To an outside reader the term also suggests a *learned predictive model*. Kirra
World predicts nothing. It records.

---

## 0a. What Kirra World contains — the tiers and their invariants

Naming the tiers, because "the world model" collapses several different things
and the collapse is what makes the safety conversation hard. **This is
vocabulary for architecture already ratified in ADR-0041, not new
architecture** — the log-plus-projections shape, the rebuild property and the
no-projection-only-fact rule are all existing normative requirements. What is
new here is only that they have names.

### The tiers

```
   OUTSIDE Kirra World ─────────────────────────────┐
   Cognitive systems (predictive belief, LLM)       │
   ──────────────────────▲──────────────────────────┘
                         │  Cognitive Interface — one-way READ seam
╔════════════════════════╪═══════════════════════════════════════════╗
║ KIRRA WORLD            │                                           ║
║  ┌─────────────────────┴─────────────────────────────────────────┐ ║
║  │ ANSWER TIER — the only read path                              │ ║
║  │   Query Engine · Explain                                      │ ║
║  └─────────────────────▲─────────────────────────────────────────┘ ║
║  ┌─────────────────────┴─────────────────────────────────────────┐ ║
║  │ ACCESS STRUCTURES — spatial · temporal · subject · text       │ ║
║  └─────────────────────▲─────────────────────────────────────────┘ ║
║  ┌─────────────────────┴─────────────────────────────────────────┐ ║
║  │ KNOWLEDGE TIER — deterministic projections (pure folds)       │ ║
║  │   identity resolution · relationships · trust · semantics     │ ║
║  └─────────────────────▲─────────────────────────────────────────┘ ║
║  ┌─────────────────────┴─────────────────────────────────────────┐ ║
║  │ ADMISSION — the only write door                               │ ║
║  └─────────────────────▲─────────────────────────────────────────┘ ║
║  ┌─────────────────────┴─────────────────────────────────────────┐ ║
║  │ EVIDENCE TIER — immutable, append-only, hash-chained          │ ║
║  └───────────────────────────────────────────────────────────────┘ ║
║  Cross-cutting: Frame & Time · Provenance Chain · Retention        ║
╚════════════════════════════════════════════════════════════════════╝
```

### One invariant per tier — the part that earns its keep

A component list tells you what exists. **An invariant tells you what may be
thrown away**, and that is the question that actually governs the design.

| Tier | Invariant | Consequence |
|---|---|---|
| **Evidence** | Immutable, hash-chained, **never deleted** — compaction *cites*, it does not erase | Incident reconstruction stays possible |
| **Admission** | The **only** write door; writer class, confidence basis and frame requirement are all decided here | A rule with no other place to live has one |
| **Knowledge** | Rebuild-from-zero **==** incremental; **no projection-only fact** | Every belief traces to evidence |
| **Access** | Rebuildable from the tier below — **losing an index loses performance, never truth** | The test for "is this an index or a projection?" |
| **Answer** | Every answer carries provenance; a **degraded answer says so** | `Explain` is possible at all |

That fourth row is the discriminator worth keeping. A flat component list puts
spatial index, temporal index and identity resolution side by side — but two of
those are rebuildable caches and the third is a projection whose history must
survive forever, and a flat list cannot tell you which.

### Why ADMISSION is named, when it was not before

**The rules already exist; the component did not.** `WorldStore::append` already
refuses an `LlmCandidate` writing `Confirmed`, and already requires `frame_id`
on a spatial claim (SD-4). Those are admission decisions, living inside a
function rather than in a named place.

Naming the tier matters right now because **[ADR-0040](../adr/0040-world-model-ownership-and-boundary.md)'s
`PerceivedObject` condition is an admission rule with nowhere to live.** That
ruling forbids an import path until a stated rule exists for where the datum's
confidence and validity come from, with any synthesis visible in the store.
`ConfidenceBasis::Assumed` supplies the mechanism; the Admission tier supplies
the place. Until both exist the condition rests on being remembered, which the
ADR records as its known weakness.

### Retention is cross-cutting, not a feature

The retention driver is a **Tier 1 exit criterion** carried by ADR-0040's
deployment-ownership decision, and it appeared in no earlier picture. It touches
three tiers at once — the ledger (compaction), the knowledge tier (rebuild after
compaction) and the answer tier (a compacted window must report itself degraded)
— which is the definition of cross-cutting. The code already has that shape:
`compaction.rs` returns `Resolution::Degraded` carrying citations.

### Explain sits in the ANSWER tier deliberately

Not decoration. §16 calls it *"a **product requirement**, not a debugging
tool"* and §25 makes it the Year 1 deliverable. Placing it at the top is what
forces every tier below to retain what it needs — it is the reason provenance is
cross-cutting rather than a feature of one component.

### Three tensions with the blueprint, flagged rather than picked

This tiering was drafted against a proposed eight-component decomposition
(Evidence Ledger · Deterministic Knowledge · Semantic Projections · Identity
Resolution · Spatial Index · Temporal Index · Query Engine · Cognitive
Interface). Three differences are **deliberate**, and each contradicts something,
so they are recorded for the blueprint owner to rule on rather than settled here:

1. **Spatial and temporal are access structures, not tiers.** Blueprint §5.1
   makes Frame & Time *"cross-cutting, not a layer"*. Listing the indexes as
   peers of the query engine re-layers what §5.1 de-layered, and invites a
   pipeline reading ("through the spatial index, then the temporal one") that is
   not how they are used.
2. **Semantic projections are not separated from deterministic knowledge.**
   §5.3 states *"Projection Engine replaces Entity Resolution + Semantic
   Model"* — an earlier draft carried that split and it was **rejected**.
   Re-introducing it also raises a question with no good answer: is a semantic
   projection deterministic? If yes, why is it separate?
3. **Identity resolution lives INSIDE the knowledge tier**, not beside it. §6.3:
   *"identity is a projection like everything else"*, and *"a query at a past
   instant resolves identity as it was adjudicated then."* Making it a peer of
   the indexes invites a mutable side-table, which is the exact failure §6.3
   opens with — *"the rows are merged, and the fact that the system ever
   believed otherwise is gone."* Of the three, this is the one that could cause
   real harm.

### The two tiers that already exist, in prose

The Evidence and Knowledge tiers are the ones with shipped code, and they carry
the normative rules ADR-0041 and the blueprint already impose. Expanded here
because those rules are what the invariant table compresses.

**1. Evidence Ledger** — immutable, provenance-carrying, bitemporal events.
*"What happened."*

**2. Deterministic World Knowledge** — materialized projections derived from
**confirmed** evidence. *"What we currently know."* Governed by the rules
ADR-0041 and the blueprint already impose:

* rebuild-from-zero must equal the incremental state;
* **no projection-only fact** — everything traces to events;
* a reducer or rule version change forces a rebuild;
* validity is computed at read time, never stored.

### Kirra World may expose a Cognitive Interface

A **one-way read seam** for external predictive systems. A seam, not a
container.

### Predictive state is not part of Kirra World

Not unless a future ruling changes `KIRRA-WM-ARCH-001` §9.1 and §20. See
*Open question 6 — predictive containment* in
[ADR-0040](../adr/0040-world-model-ownership-and-boundary.md).

> **Citation note.** §9.1 (trust model) and §20 (AI prediction integration) are
> sections of the **blueprint**, `KIRRA-WM-ARCH-001`, *not* of ADR-0041. They
> are easy to attribute to the ADR because that is where the persistence
> decisions live; sending a reader to the wrong document over a boundary rule
> is worth one sentence to prevent.

### The distinction that must not be smoothed over

Two things are both called "LLM output" and only one of them is admitted:

| | Example | Status |
|---|---|---|
| **LLM-originated candidate** — proposes something confirmable | *"I think that is the toolbox"* | **Inside Kirra World, already fenced** — `writer_class = llm_candidate`, excluded from the confirmed-only fold, reachable only by naming `candidates()` |
| **Predictive belief** — infers a probability over unobserved state | *"The keys are probably still near the door"* | **Outside Kirra World.** §9.1: `Predicted` never appears in the evidence store |

A diagram that nests "the cognitive layer" inside Kirra World collapses these,
and the collapse is invisible because both are "the LLM's opinion."

### The precompute rider

The layering invites *"maintain a projection for everything."* That is not free,
and this project has the numbers:

* **D-16** — rebuild write amplification **2.8×–35.8×**, and a *dial* rather
  than a constant (it is a property of how finely the fold is chunked);
* **D-21** — projections cost ~3 % storage, but that is **one** projection over
  an entity-bounded stream; a projection per question changes the arithmetic;
* the real hazard is that **a stale projection is worse than none, because a
  consumer trusts it**.

> **Precompute only when freshness, invalidation, rebuild cost, write
> amplification and provenance are explicit.**

Otherwise the trade is *"the LLM might be wrong"* for *"the cache is silently
stale"* — which is harder to notice and looks like success. `robot/world_model.py`
already encodes the defence: TTL'd fields read `UNKNOWN` when stale rather than
returning the last value they held.

---

## 1. What "done" means here

Taken from the blueprint rather than invented, so this document cannot quietly
raise its own bar.

§25 sets **Year 1** as:

> *Append-only log, projections, entity resolution, `Explain`, fences, Mick's
> six flows, registries migrated. **A robot that can justify every fact it
> states.***

And §16 names the flagship inside it:

> *"Explain why you believe that" is the flagship capability. It is the reason
> provenance is mandatory rather than nice-to-have, and it is the single feature
> most likely to distinguish Kirra from every other robotics knowledge layer. It
> should be treated as a **product requirement**, not a debugging tool.*

**So: done is a robot that can be asked why it believes something and answer
with the evidence.** Everything below is scoped to that sentence. Years 2–5 —
fleet knowledge exchange, predictive integration, the public schema — are out of
scope for this document entirely.

---

## 2. What is built

| Capability | Where | Landed |
|---|---|---|
| Event schema (SD-1…SD-4), write path, SHA-256 chain | `kirra-world-store` | #1350 |
| Bytes/event against the ratified schema; with projections | `tools/wm2-schema-growth` | #1351 / #1353 (D-20, D-21) |
| Current-state projection, confirmed-only fold, rebuild-equals-incremental digest | `kirra-world-store::projection` | #1353 |
| Bitemporal queries — `current` / `as_of` / `history` / `candidates` / `changed_since` | `kirra-world-store` | #1353 |
| Compaction-with-citation; chain verifies **across** a hole | `kirra-world-store::compaction` | #1354 |
| Per-key degraded summaries; a compacted window says so | same | #1355 |
| Evidence attestation — the growth instrument can refuse to be cited | `wm2-schema-growth` | #1358 |

Roughly **the persistence third of Year 1**. The read path exists; the *trust*
and *explanation* halves do not.

---

## 3. Tier 0 — Governance — **COMPLETE, 2026-08-06**

Not code. It blocked authorized implementation by the ADRs' own words. **All
four World Model ADRs are now Accepted**, every ratification block at zero
unticked.

| Item | State |
|---|---|
| ADR-0041 | **Accepted** (2026-08-04), carrying one outstanding obligation (R2's alongside rebuild-and-swap) |
| ADR-0042 | **Accepted** (2026-08-06) — carries **M5**, the `docs/safety` terminology migration, due before Kirra World runs as a service |
| ADR-0039 | **Accepted** (2026-08-06) — fences structurally enforced and machine-checked; no runtime evidence, no independent review |
| ADR-0040 | **Accepted** (2026-08-06) — carries **two conditions**: no `PerceivedObject` import path without a stated confidence/validity rule, and a **retention driver as a Tier 1 exit criterion** |

### What acceptance did NOT do

**It authorized no implementation.** Every one of the four says so in its own
Status row. Tier 1 proceeds because the domain-logic gate is self-releasing and
already released, not because ratification licensed it.

**It was one approver throughout.** Every sign-off across all four ADRs was
recorded by the same person holding every role, plainly stated in each
*Acceptance record* so no reader infers independence from a count of ticks.
Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
SIL 3 requirements; independent third-party assessment has not yet been
performed.

**Not every acceptance is equally well supported.** ADR-0039's fences are
transitively machine-checked — a breach reds CI with the path named. ADR-0042's
Decision 1 terminology has **no** machine check at all and is held by convention.
ADR-0040's `PerceivedObject` condition likewise rests on being remembered. The
difference is the enforcement mechanism, not the care taken, and it is recorded
in each ADR rather than averaged away here.

### The ratification order was forced, and was followed

ADR-0040 *"depends on ADR-0039"*; ADR-0039's checklist required *"ADR-0042
itself accepted."* The chain **0042 → 0039 → 0040** was resolved in that order
on 2026-08-06 — 0042 first, which satisfied 0039's terminal criterion, which
freed 0040. ADR-0042 was the critical path and behaved like it.

### Evidence prepared, so the boxes are decisions rather than work

**Every box below is now ticked** — all were ruled on 2026-08-06. The table is
kept because it records *what each ruling rested on*, and a reader auditing an
acceptance should be able to see the evidence without reconstructing it:

| Box | Prepared |
|---|---|
| ADR-0040 dependency review | **Written** — ADR-0040 *Repository dependency review — findings* |
| ADR-0040 compatibility inventory | **All five rows checked** against the tree; one citation error found and fixed; the `tracked-object inputs` row's factual claim corrected. Dispositions still need their owners |
| ADR-0040 Q1 | **Circular as stated**; a deferral disposition is drafted for accept/amend/reject |
| ADR-0040 Q4 | **Appears already dispositioned** — C1 in ADR-0039 via ADR-0042 Decision 1, C3 in ADR-0040's own compatibility table |
| ADR-0042 Decision 5 template | **Already satisfied** — recorded 2026-08-05; the ADR requires it *recorded*, not favourable |
| ADR-0042 M1 rename | **Already executed** — `61dbf57f`; the box confirms a completed change |
| ADR-0042 OQ1 | **Evidence prepared**, disposition drafted; its revisit trigger is self-announcing (a Fence B breach *is* the request arriving) |
| ADR-0039 safety-assurance box | **Flagged as possibly tickable** — its own text says the ruling need only be *recorded* |

**All of it was judgement rather than research by the end** — and the one item
no preparation could move was **deployment ownership**, which is now decided.

**Deployment ownership — DECIDED 2026-08-06.** Kirra World runs **co-located
with the verifier**, stores to **local SQLite** (not the Postgres shared tier),
and inherits the **verifier's existing backup regime**, which already respects
ADR-0038's per-instance local audit chain. It was answered *before* a service
exists rather than after one is running, which is what the item asked for.

**Its cost is carried into Tier 1, not absorbed silently.** Co-location means a
knowledge store shares a host with the safety verifier, so the measured
**15.79-day fill** is now the verifier's disk pressure — which is why a
**retention driver is a Tier 1 exit criterion** (§4), the one item there that
is not about the domain model.

---

## 4. Tier 1 — The domain core

`kirra-world` is ten unconstructible placeholders. Everything below depends on
it. The domain-logic gate that once held it is **self-releasing and already
released** (ADR-0042 Decision 5, recorded 2026-08-05) — so this is sequencing,
not permission.

- [ ] **Retention driver** — **exit criterion, added 2026-08-06 by the ADR-0040
      deployment-ownership decision.** D-20/D-21 measured **15.79 days** to fill
      8 GiB at 10 Hz on the ratified schema, and Kirra World is now decided to
      run **co-located with the verifier on local SQLite** — so its disk
      pressure is the safety host's disk pressure. Tier 1 is **not done** until
      something empties the store. This is the one item here that is not about
      the domain model; it is here because that decision put it here rather than
      leaving the fill date unowned.
- [ ] **Entity taxonomy** (§6) — **structure and kinds DONE 2026-08-06**,
      `crates/kirra-world/src/entity.rs`, 18 tests, still zero-dependency.
      Delivered: the 19-kind root-closed taxonomy + `EntityGroup`, `Lifecycle`
      with validated transitions, `EntityId`/`Alias` (each alias carrying its own
      `SourceClass`), `ResolutionConfidence`, and the `Entity` spine.
      **Two more rules made structural:** an unrecognised kind has **no group to
      read** (`group()` returns `Option`, `None` for `Unknown`), so §6.2's
      *"degrade to `Unknown`, not guess a supertype"* is unavailable to violate;
      and `ResolutionConfidence` is a newtype so the "is this **one** thing"
      claim cannot be passed where an attribute confidence was wanted.
      **`Entity` has no `kind` field** — kind is adjudicated from classification
      evidence, so reclassification cannot contradict a stored value.
      `adjudicated_kind` returns a three-way `KindAdjudication`
      (`NoEvidence` / `Settled` / `Unrankable`) rather than a bare kind, so
      "I hold evidence I have no grounds to rank" is reportable instead of
      collapsed into a guess — and it ranks **through** the §7.3 cross-basis
      guard rather than around it. That
      follows §6.2 over §6.1's field table, which **contradict each other**; the
      tension is recorded in the module as an open question rather than resolved.
      **Still open:** identity *adjudication* — candidate clustering, merge/split
      events — is Tier 2. `entity_id` generation, `first_observed`/
      `last_observed` and `provenance_head` need the store (ULID, hashing).
- [ ] **Observation model** (§7) — **pure half DONE 2026-08-06**,
      `crates/kirra-world/src/observation.rs`, 17 tests, still zero-dependency.
      Delivered: `Confidence`/`ConfidenceBasis` (§7.3), `SourceClass` + its
      mapping to the trust `Origin`, `SubjectRef` (including `Unbound`, which is
      why this did not need the entity taxonomy first), `ClockDomain`/
      `DomainInstant`/`ValidInterval` and the projection into
      `trust::ValidityWindow`.
      **Two more rules made structural**, following rule 6's shape: cross-modal
      confidence comparison **errors** unless the caller names the decision
      (`compare_across_bases`), and clock domains **cannot** be compared at all —
      unsound rather than merely unwise, so there is deliberately no escape hatch.
      **Still open, and it needs dependencies:** `observation_id` (ULID),
      `evidence_digest`/`prev_hash` (hashing), `frame`/`map`, and the per-kind
      versioned `TypedPayload`. Those belong to the **store**, which already has
      all three — pulling them into the core would spend ADR-0040's Q1 seam
      decision without revisiting it. `ObservationKind` is also absent because
      the blueprint names the field but never enumerates its variants.
- [ ] **Relationship model** (§8)
- [x] **The four orthogonal trust axes** (§9):
      `Origin × Corroboration × Adjudication × Validity` — **DONE 2026-08-06**,
      `crates/kirra-world/src/trust.rs`. Pure, zero-dependency, 27 tests.
      Note the shape it took: **three stored axes, not four.** `TrustAxes` has no
      validity field, so rule 6 ("computed at read time, never stored") is
      **structurally unbreakable** rather than a rule someone remembers —
      `validity_at` takes the clock as an argument and there is nowhere to write
      its answer down.
- [ ] **The seven transition rules** (§9.2) — **six and a half of seven.**
      Rules 1, 2, 3, 5, 6, 7 are implemented and tested, including the two
      load-bearing ones. **Rule 4 is half done**: its *adjudication* half is
      `TrustAxes::operator_confirm`; its *geometry* half (an operator assertion
      may never silently rewrite a measured pose, P10) constrains the
      **observation payload**, so it cannot be enforced until the observation
      model exists. Deliberately left unticked rather than counted as done —
      the module cannot reach a payload, which is why it cannot yet be
      sidestepped, but "cannot be sidestepped from here" is not "enforced".

### Why the axes are not one enum

Today the store carries `writer_class` plus a two-value `claim_status`, which is
an **adjudication proxy** and nothing more. The blueprint is explicit that
collapsing the axes is *"exactly why trust states in most systems become mush
after eighteen months — every new case forces either a wrong assignment or a new
variant."*

Two of the seven rules are load-bearing and genuinely hard:

* **Derived inherits the weakest input** on every axis. This is the
  anti-laundering rule, and it prevents the most common knowledge-graph
  pathology: a chain of plausible inferences producing a high-confidence
  conclusion from low-confidence roots.
* **Validity is computed at read time, never stored.** `Fresh` is not a state
  the system enters; it is a question asked with the clock passed in. The store
  already does half of this in `ProjectedClaim::holds_at`.

`Corroboration(n)` presupposes cross-observation matching — i.e. it cannot land
before entity resolution (Tier 2). **This held, and shaped the delivered slice:**
the axis, its monotonic `agreed`/`disagreed` transitions and its weakest-wins
fold are all implemented, but **nothing populates the count** — no matcher exists
to decide that two observations are about the same thing. The algebra is ready
for Tier 2; it is not driven by anything yet.

**Largest single body of work in this document.**

---

## 5. Tier 2 — Identity adjudication

- [ ] Entity resolution — matching incoming observations to existing entities
- [ ] `MergeEntities` / `SplitEntity` / `ForgetEntity` as **recorded events**

Merge and split are *events, never destructive edits*. This is what makes an
`EntityId` revisable, and it is precisely what a store built on a bare opaque
key can never retrofit — the key would have already lost its own history.

`ForgetEntity` retires an entity and suppresses it from default projections. It
is **not** deletion. Genuine erasure, if ever required, is a distinct audited
`Redact` with its own ADR, and must leave a tombstone or the chain breaks.

---

## 6. Tier 3 — The query engine

Eight verbs in §14.2; about five exist in partial form.

- [ ] `Resolve` · [ ] `Related` (bounded graph) · [ ] `WhatIsAt` ·
      [ ] `Capabilities` · [ ] `Freshness`

Three rules matter more than the verb count:

1. **No API returns a bare value.** Every answer carries the value, the trust
   axes, the validity at the supplied clock, and a `ProvenanceHandle`. The
   blueprint calls this *"a deliberate ergonomic cost: it makes 'I got a number
   and lost where it came from' impossible to write."*
   **This is a breaking change to the API that exists today**, which returns
   bare `ProjectedClaim`s.
2. **Queries are bounded.** Not a preference: D-9 measured **10.5 s p99**
   temporal queries at 100 000 entities, and ADR-0041 D-12 already records that
   neither graph nor temporal queries may sit on a control or safety deadline
   path, and that an unbounded query has no bounded cost whatever its scaling
   verdict.
3. **`Unknown` is a success.** The error channel is for malformed queries and
   storage faults — never for absence of knowledge. Conflating the two is how
   *"I don't know"* becomes an exception somebody catches and turns into a
   default value.

---

## 7. Tier 4 — `Explain`, the flagship

- [ ] `Explain(FactHandle) → ProvenanceTree`
- [ ] Prose rendering through Mick (§16)

Depends on the provenance model **and** on derivation edges being real structure
rather than a JSON array of identifiers. This is the capability the whole
evidence-first inversion exists to buy, and the one that makes the difference
between a database and something that can be asked to justify itself.

Mick's three non-negotiables apply unchanged and are already precedented in
`robot/mick_chat_contract.py`: **never invent**, **never state stale as
current**, **never supply geometry**.

---

## 8. Tier 5 — Surfaces

- [ ] Semantic projections beyond `world_current` — relationships, capabilities,
      map layers
- [ ] **Retention policy driver** — the horizons OQ2 ruled are still applied by
      hand. Its precondition is already recorded in ADR-0041's WM-2 milestone:
      *the first doer-side consumer wired to the store ends the deferral.*
- [ ] `kirra-world-service` as real CQRS — 9 commands, 8 queries, 10 emitted
      events — still inside Fence A
- [ ] Operator teaching surface (§17): `AssertEntity`, corrections

---

## 9. Two sequencing calls

**Wire a small consumer EARLY — before Tier 3, not after.**

Everything built so far is built for nobody: no planner, perception or LLM crate
depends on `kirra-world*`, and the service crate is deliberately empty. The
"no bare values" rule and the shape of the trust axes are exactly the decisions
a real caller will falsify, and discovering that across eight verbs costs far
more than discovering it against one.

There are **no callers today**, so the breaking change is free *now* and never
again.

**Land the trust axes before the query engine.** Retrofitting four axes into an
API that already returns claims means touching every verb twice.

---

## 10. Explicitly out of scope

The blueprint defers eight items in §22, and this document holds every one:

distributed consensus and partition-tolerant merge · erasure/redaction under a
privacy regime · semantic similarity search over embeddings · multi-map topology
and map-to-map transforms · person entities and the privacy question they open ·
cross-robot entity identity · formal verification of projection determinism ·
compaction *thresholds* beyond what measurement supports.

Plus **Years 2–5 entirely** (§25): fleet knowledge exchange, predictive
integration, the stable public schema.

Naming them here is the point. An undeclared deferral becomes a surprise
obligation the first time someone asks why it is missing.

---

## 11. The risk that is not on any checklist

Every tier above makes Kirra World more **useful**, and usefulness is what
generates pressure to let the checker read it.

> **ADR-0042 Decision 5's condition (1) reopens the entire safety ruling the
> moment Kirra World gains authority over actuation, release, safety decisions,
> or required safety inputs — any one of the four.**

The protection is as strong as this kind of protection gets: Fence A walks
Kirra World's dependency closure for any route to an actuator or an
authorization; Fence B walks the safety closure (19 workspace packages from 10
roots, computed from the manifests rather than a hand-maintained list) for any
dependency on Kirra World; and gate t24 checks *by contents* that the store
implements no `CorridorSource`, because that dependency would be inverted and
the closure walk would not see it.

But a gate can refuse a dependency. It cannot refuse an argument. Holding the
line is part of this scope, not a footnote to it.

---

## 12. What this document is not

It is not a plan, a schedule, or an estimate. There are no dates and no effort
figures, because none could be defended — the tiers are ordered by dependency,
not by duration.

It is not a ruling. Tier 0 is where rulings live, and four of them are still
open.
