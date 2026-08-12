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
| **Knowledge** | Rebuild-from-zero **==** incremental **given the evidence retained**; **no projection-only fact** | Every belief traces to evidence |
| **Access** | Rebuildable from the tier below — **losing an index loses performance, never truth** | The test for "is this an index or a projection?" |
| **Answer** | Every answer carries provenance; a **degraded answer says so** | `Explain` is possible at all |

**"Given the evidence retained" is a clarification, not a relaxation.** Compaction
is the one operation that changes the evidence set, so a rebuild after one is not
folding the same log. What the invariant forbids is compaction quietly turning
missing history into apparently complete knowledge — so where evidence has been
removed, **the difference is reported rather than absorbed**. `SummaryCoverage`
is that report for `subject_summary`, whose aggregates are a MIN and a COUNT over
all contributing events and therefore depend on the whole log rather than on a
head. Contrast `world_current`, where removing a head makes an answer *wrong*
rather than *coarser* — which is why that one is protected outright
(`ProjectionHeadInRange`) and this one is surfaced instead.

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

## 4. Tier 1 — The domain core — **COMPLETE, 2026-08-07**

> **RULED — 2026-08-07 by Justin Looney, World Model owner. Tier 1 is DONE.**
> Recorded under `KIRRA-WM-TIER1-DONE-001`
> ([proposal](WM_TIER1_COMPLETION_PROPOSAL.md)), which found this tier could not
> complete as written: §6's box was held open entirely by work its own text
> assigned to Tier 2. Both boxes are now ticked and **both residues are listed
> at the tiers that own them** — `entity_id` minting at Tier 2 (§5),
> `evidence_digest`/`prev_hash` as core types at Tier 3 (§6). Neither was
> deleted; a residue that vanishes when a box is ticked is exactly what the
> proposal's second constraint forbids.
>
> **This fires ADR-0040's Q1 revisit trigger**, on both of its clauses rather
> than only the substantive one. The measurement it asks for is already on
> record — `KIRRA-WM-Q1-BASELINE-001` Measurement 3, taken and merged
> (`ed0a82e5`) *before* this ruling, which is the property that makes it
> evidence rather than justification. **Q1's own disposition remains a separate
> act** and is not taken here.
>
> **One approver.** Recorded by the same person holding every role, as with
> every other World Model ruling.

`kirra-world` **was** ten unconstructible placeholders. As of 2026-08-07 it is
**six real types and five remaining placeholders**, across seven implemented
modules (`trust`, `entity`, `observation`, `relationship`, `reference`,
`retention`, `kind`) carrying **144 unit tests + 4 doctests** — still
zero-dependency.

The counting unit is stated because this figure is quotable and the per-module
figures below **do not sum to it**. Those are *as-landed* snapshots carrying the
date they were recorded; the modules have since grown (summing them today gives
124, not 144). They are left as recorded rather than restated, for the same
reason the seam measurements are: a dated record revised to stay tidy stops being
a record. The crate total above is the current one, read from `cargo test`.

Six real against five remaining does not sum to ten, and should not: `EventId`
is an addition, not one of the original ten.

Real: `TrustAxes`, `EntityId`, `ObservationId`, `FrameId`, `MapId`, and
`EventId`, which was not one of the original ten (the storage layer had carried
that concept as a bare `&str` since it was written). Still placeholders:
`Source`, `Provenance`, `ValidTime`, `TransactionTime`, `ResolutionOutcome` —
the first two waiting on the provenance model Tier 4 needs, the two temporal
ones largely superseded in practice by `observation::ValidInterval` and the
store's `txn_time_ms` and due a decision on whether they survive at all, and
`ResolutionOutcome` belonging to Tier 3's query boundary.

Everything below depends on this crate. The domain-logic gate that once held it
is **self-releasing and already released** (ADR-0042 Decision 5, recorded
2026-08-05) — so this is sequencing, not permission.

- [x] **Retention driver** — **exit criterion, added 2026-08-06 by the ADR-0040
      deployment-ownership decision.** D-20/D-21 measured **15.79 days** to fill
      8 GiB at 10 Hz on the ratified schema, and Kirra World is now decided to
      run **co-located with the verifier on local SQLite** — so its disk
      pressure is the safety host's disk pressure. Tier 1 is **not done** until
      something empties the store. This is the one item here that is not about
      the domain model; it is here because that decision put it here rather than
      leaving the fill date unowned.
      **DECIDING HALF DONE 2026-08-06**, `crates/kirra-world/src/retention.rs`,
      15 tests, still zero-dependency: `RetentionPolicy` (OQ2's 30/365-day
      horizons, with **protected ≥ raw refused at construction** — the inversion
      would age protected classes out *before* the traffic they exist to
      outlive), saturating cutoffs (an underflow that wrapped would make
      *everything* eligible, and compaction is irreversible), a wall-clock
      requirement (a 30-day horizon read against the boundary timing domain is
      meaningless), `RetentionSurvey` refusing logs that cannot exist, and
      `decide` returning **four outcomes rather than `Option<Range>`** — because
      "nothing is old enough" and "a protected event is pinning the store" both
      compact nothing, and only the second is worth waking someone for.
      `Blocker::may_compact_around` encodes §11.3's asymmetry: the pre-agreed
      escalation to compact *around* a blocker applies to projection heads and
      **not** to protected classes. `CompactablePrefix`/`Eligibility` make two
      meaningless survey states unrepresentable — a refusal naming no blocker,
      and a prefix over a range nothing aged into — which is what makes `decide`
      **infallible**: every survey that exists maps to a decision.
      **ACTING HALF — survey + pass DONE 2026-08-06**,
      `crates/kirra-world-store/src/retention_driver.rs`, 7 tests:
      `WorldStore::retention_survey` (asks the log the four questions the pure
      policy needs and takes no decision) and `WorldStore::run_retention_pass`
      (survey → `decide` → act) — **the only call to `compact_range` made on a
      policy's authority anywhere in the workspace.** It reports which refusal
      stopped the prefix, which `largest_compactable_prefix` discards, because a
      driver that cannot tell `ProtectedClass` from `ProjectionHead` cannot act
      on §11.3's asymmetry.
      **Retention ages on `txn_time_ms`, not `valid_from_ms`** — recorded as a
      decision, not a column choice: retention bounds disk and disk grows on
      insertion, whereas ageing on valid time would delete a backdated import on
      arrival and never age out a future-dated claim.
      **SWEEPER DONE 2026-08-06**, `crates/kirra-world-store/src/retention_sweeper.rs`,
      3 tests — **something now empties the store without being asked**, which
      is what this exit criterion asked for.
      **It is NOT in the verifier, and that is the load-bearing part.**
      `src/campaign_monitor.rs` and `src/cert_expiry_monitor.rs` are the obvious
      precedent, but they live in the root crate, which is **inside the safety
      closure** — spawning the sweeper there would pull `kirra-world` into that
      closure and breach ADR-0039's **Fence B**. The precedent copied is the
      *shape* (sweep interval, explicit start, fail-closed on anything
      unestablished), not the location.
      `std::thread` + `mpsc::recv_timeout`, so no async runtime enters
      `kirra-world`'s dependency closure to schedule a SQLite `DELETE`; the
      sweeper opens its own connection because `rusqlite::Connection` is not
      `Sync`, which also isolates it from a caller's in-flight transaction.
      Fail-closed at start (an unopenable database is the caller's error, not a
      thread rediscovering it hourly); a failed pass is **counted and skipped**,
      never retried tight and never fatal, because retention failing is not a
      reason to stop bounding the disk. `SweepCounters` keeps `compacted`,
      `pinned` and `failed` apart, since **`pinned` climbing while `compacted`
      stays flat is the alertable condition** and one success counter cannot
      show it.
- [x] **Entity taxonomy** (§6) — **structure and kinds DONE 2026-08-06**,
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
      **`first_observed` / `last_observed` / `provenance_head` DONE 2026-08-07**,
      `crates/kirra-world-store/src/entity_projection.rs` + the store's
      `fold_subject_summary`, 10 unit + 9 integration tests.
      **They are a PROJECTION, not a table** — `WM2_EVENT_SCHEMA.md` §7 rules
      `entities_projection` a rebuildable view that "follows from the fold, not
      from this table", so no new DDL entered the ratified schema surface and no
      version bump was involved. Derived by folding the event log, which is what
      stops them drifting from the evidence — the same argument that leaves
      validity without a column. Installed lazily like `PROJECTIONS_V1`, because
      creating projection tables at `open` would move D-20's `log_only_bytes`
      and invalidate the comparison the retention horizons rest on.
      Ages on `txn_time_ms` (same choice, same reason, as the retention driver);
      `provenance_head` is a **chain digest**, so a subject's summary can be
      *cited* rather than merely read; the head follows **generation**, not time,
      because generation is unique and a time tie-break would not be reproducible.
      **Keyed on `subject`, not on an entity id — a recorded limit, not an
      oversight.** `SubjectRef` distinguishes `Entity` / `Candidate` / `Frame` /
      `Unbound`; storage flattens all four into one `subject TEXT NOT NULL` with
      no discriminant, so the fold cannot restrict itself to resolved entities
      and the module is named `subject_summary` for what it actually computes.
      Same shape as the `writer_class`-vs-`origin` finding. Carrying the
      discriminant touches `subject`, which is inside the hashed bytes, so it
      needs the append-only-when-present treatment the trust axes got — its own
      slice.
      **Moved out, 2026-08-07 by `KIRRA-WM-TIER1-DONE-001`:** identity
      *adjudication* — candidate clustering, merge/split events — and
      **`entity_id` generation** are Tier 2, and are now **listed there** (§5)
      rather than sitting under a Tier 1 box they could never let tick. Minting
      an id is deciding that something is a distinct thing, which is
      adjudication, whereas the three fields above are arithmetic over evidence
      that already exists. This list previously grouped it with them, and then
      held this box open on it.
- [x] **Observation model** (§7) — **pure half DONE 2026-08-06**,
      `crates/kirra-world/src/observation.rs`, 17 tests, still zero-dependency.
      Delivered: `Confidence`/`ConfidenceBasis` (§7.3), `SourceClass` + its
      mapping to the trust `Origin`, `SubjectRef` (including `Unbound`, which is
      why this did not need the entity taxonomy first), `ClockDomain`/
      `DomainInstant`/`ValidInterval` and the projection into
      `trust::ValidityWindow`.
      **Three more rules made structural**, following rule 6's shape: cross-modal
      confidence comparison **errors** unless the caller names the decision
      (`compare_across_bases`); clock domains **cannot** be compared at all —
      unsound rather than merely unwise, so there is deliberately no escape hatch;
      and **rule 4's geometry half / P10** — `Payload` + `PayloadSource`, added
      2026-08-06, which is what closed the transition rules at 7/7 above.
      **Identity and spatial reference DONE 2026-08-07** (Strand A),
      `crates/kirra-world/src/reference.rs`, 11 tests + 4 doctests, still
      zero-dependency. `ObservationId`, `FrameId` and `MapId` stop being
      crate-root placeholders and become validated newtypes; `EventId` joins them.
      The store's `NewEvent` is rebuilt out of all four, so the seam now carries
      constructed values and called methods rather than re-exports (re-measured in
      `WM_Q1_SEAM_BASELINE.md` "Measurement 2").
      **The rule made structural:** the storage layer held `event_id`/
      `observation_id` and `frame_id`/`map_id` as two adjacent pairs of the same
      type, so **either pair could be passed in the wrong order and still compile,
      write and hash** — and the frame/map swap additionally *satisfied* SD-4's
      presence check while carrying the wrong reference. Four distinct types make
      both unrepresentable; paired `compile_fail` doctests are the negative
      control.
      **A second rule, from the read path:** constructors **validate but never
      normalize**. `verify_chain` rebuilds each record from its stored strings and
      rehashes, so a constructor that trimmed would produce bytes the write never
      produced and report untampered rows as broken chains. A stored value the core
      refuses is a `CorruptRow`, never a `ChainBroken`.
      **Minting stays out of the core** — ULID needs a dependency and this crate
      has none by ratification criterion, so the core owns the *type* and the layer
      with a clock mints the *value*. No loss: an id arriving from a replayed log
      or another fleet must be admissible regardless.
      **`ObservationKind` + the per-kind versioned `TypedPayload` contract DONE
      2026-08-07**, `crates/kirra-world/src/kind.rs`, 16 tests, still
      zero-dependency. This was the **specification** gap, not an implementation
      one — the blueprint named the field and never enumerated it — so it was
      ruled rather than written: `KIRRA-WM-OBSKIND-001`, option 2, the three
      variants **attested by what the system already writes** (`observation`,
      `spatial`, `relationship`) plus `Unrecognised` as a degrade target.
      `Existence` was proposed and **deferred**, on an asymmetry rather than a
      preference: adding a variant later is an additive enum change, while
      removing one already written into a hash-chained log would mean rewriting
      rows inside the chain. Revisit trigger: a perception producer with a
      saw-something-claiming-nothing output.
      **Three rules made structural.** The tokens are FROZEN, because `kind` sits
      inside the canonically-hashed bytes twice — re-spelling one is not a rename,
      it breaks verification on every existing store. `Unrecognised` has **no
      token**, so `as_str` returns `Option` and the degrade target cannot be
      written at all. And `requires_frame()` answers `false` for `Unrecognised`
      deliberately: SD-4's `CHECK` keys on the literal `'spatial'`, so a kind this
      build cannot name is not spatial *to the schema* either, and answering
      otherwise would put the type and the schema into disagreement about one row.
      **`TypedBody`** gives §7.1 its two halves — `KIND` so a spatial body cannot
      be attached to a relationship claim, and `SCHEMA_VERSION` so the store's
      long-standing `payload_schema` column finally has a stated meaning. Checked
      in the order kind → version → content, because a body offered under the
      wrong kind should never have been considered and reporting its version
      mismatch would send a reader to the wrong question. Fails closed against the
      **future**: a newer producer's body is refused rather than decoded as the
      known version, since a silently truncated record is worse than an unread one
      in an evidence log. Encoding stays the implementor's job — a trait demanding
      a serializer would break the crate's zero-dependency criterion.
      **The stored bytes are unchanged**, and the ruling explicitly did not
      authorize changing them in this slice: `NewEvent::kind` and `payload` stay
      as they are. This types the *boundary*.
      **Moved out, 2026-08-07 by `KIRRA-WM-TIER1-DONE-001`:**
      `evidence_digest`/`prev_hash` as core types — the store computes both today
      as bare hex strings, so this is a typing gap rather than a missing
      capability, and the tier's stated job (a real domain core the store
      consumes) does not depend on it. **Now listed at Tier 3** (§6), which is
      where it is first *required* rather than merely desirable: Tier 3's
      no-bare-values rule demands every answer carry a `ProvenanceHandle`, and
      Tier 4's `Explain` needs derivation edges to be real structure. Recorded as
      relocated, not resolved — it is still core-crate work.
      **Since DONE, 2026-08-07** — see §6. Recorded as a pointer rather than by
      editing the note above, which is a dated record of the move.
- [x] **Relationship model** (§8) — **DONE 2026-08-06**,
      `crates/kirra-world/src/relationship.rs`, 20 tests, still zero-dependency.
      **All ten §8 record fields**, all 15 predicates across the four groups.
      Bitemporal: `valid_time` on the shared `ValidInterval` (so clock domains
      still cannot mix within it) plus `transaction_time`, which is deliberately
      NOT forced into the same domain — valid time is a fact about the world,
      transaction time a fact about the recorder, and `DomainInstant::compare`
      already refuses to order one against the other.
      **Three of §8's four design notes are structural, not documented:**
      an inference **carries its `DerivationRef` in the enum variant**, so
      "inferred, derivation missing" is unrepresentable — and
      `Direct(SourceClass::Derivation)`, the hole that would route around it, is
      refused; **`caused_by` combined with `Inferred` is refused**, which is what
      §8's *"deliberately weak"* means written as a type; and there is **no
      `update` and no `valid_time` setter** — `supersede` returns *both* the
      closed predecessor and the replacement, so history cannot be dropped by
      omission. `supersede` takes the closing **instant**, not an interval, so
      neither an open "closed" predecessor nor a rewritten start is
      representable.
      **The fourth note is half-doable in a pure module.** A pure crate cannot
      stop a store writing both `contains` and `inside`; `canonical()` normalizes
      them to the same *direction*, so the two rows carry one
      subject/predicate/object triple (`canonical_triple()`) and a dedupe has
      something to compare. Not the same record — identity, times, source and
      confidence differ between two separately-written rows and should. The
      canonical direction is the lexicographically smaller token — mechanical on
      purpose, since choosing on meaning would be a domain judgement.
      **The relation algebra is deliberately sparse.** §8 states exactly one
      implication (`contains` → `inside`) and says nothing about the other
      thirteen predicates, so `symmetry()` returns `Unspecified` for all of them
      rather than guessing. Recorded as an open question with the candidates
      named: `near`/`adjacent_to` look symmetric, `connected_to` probably but a
      one-way corridor breaks it, `supports`/`on_top_of` look like an inverse
      pair, and `part_of` has no `has_part` in the table at all.
- [x] **The four orthogonal trust axes** (§9):
      `Origin × Corroboration × Adjudication × Validity` — **DONE 2026-08-06**,
      `crates/kirra-world/src/trust.rs`. Pure, zero-dependency, 27 tests.
      Note the shape it took: **three stored axes, not four.** `TrustAxes` has no
      validity field, so rule 6 ("computed at read time, never stored") is
      **structurally unbreakable** rather than a rule someone remembers —
      `validity_at` takes the clock as an argument and there is nowhere to write
      its answer down.
- [x] **The seven transition rules** (§9.2) — **all seven, DONE 2026-08-06.**
      Rules 1, 2, 3, 5, 6, 7 landed with the trust axes. **Rule 4's geometry
      half** (an operator assertion may never silently rewrite a measured pose,
      P10) landed with `observation::Payload` — its *adjudication* half was
      already `TrustAxes::operator_confirm`.
      **It did not need the geometry types this entry previously assumed.** The
      rule asks that an operator's payload be *"visibly distinct from a sensed
      one"* — a claim about **provenance**, not about pose contents — so a crate
      with no pose type can keep it in full. `Payload`'s body is a type
      parameter for exactly that reason, leaving §7.1's versioned `TypedPayload`
      with the store where ADR-0040's Q1 seam decision put it.
      Two things make the failure unavailable rather than checked:
      `Payload::correction` is an **associated function that never receives the
      payload it corrects** (the measured record is not reachable from the
      operation), and `PayloadSource::Correction` has **no source-class field**
      to inherit (so an operator's numbers cannot carry `Sensor` provenance —
      the *invisible* rewrite is the one the rule actually forbids).
      Three limits are stated in the type's docs rather than papered over: this
      cannot verify that `of` names the right record (identity is the store's),
      cannot catch a producer lying about its own class (that is §7.2's
      producing edge), and cannot judge the numbers at all (the body is opaque
      by construction).

### Why the axes are not one enum

The store **carried** `writer_class` plus a two-value `claim_status`, which was
an **adjudication proxy** and nothing more. The blueprint is explicit that
collapsing the axes is *"exactly why trust states in most systems become mush
after eighteen months — every new case forces either a wrong assignment or a new
variant."*

**Resolved 2026-08-07 (Strand C), schema v2.** The three stored axes now have
columns — `origin`, `corroboration` + `corroboration_n`, `adjudication` — added
additively, so `KIRRA-WM2-SCHEMA-001`'s ratified v1 grows rather than being
replaced and every existing row stays readable.

**The finding that shaped the ruling: `writer_class` is not the origin axis in
disguise.** It looks like one, and neither derives the other. `writer_class`
records *who held the pen* — it is what **D-2** keys on, and `llm_candidate` is
not an origin at all, because an LLM can propose a claim of any provenance and
the rule constraining it is about the writer's authority. `origin` records
*where the claim came from*, carries `imported` which no writer class expresses,
and cannot say "an LLM wrote this". Replacing `writer_class` would have deleted
D-2's enforcement basis, so it is kept **permanently**, not transitionally.

`claim_status` is retained for read compatibility and is now **derived**: a
`CHECK` makes `claim_status = 'confirmed'` hold exactly when
`adjudication = 'confirmed'`, so the proxy cannot drift from the axis it stands
for. D-2 is additionally restated against `adjudication`, so the rule survives
`claim_status` being dropped later.

Two states the proxy could never express are now storable: **`Rejected`**
(terminal under rule 7) and **`Ambiguous`**, which rule 3 requires to be a
stable, reportable state — *"I have conflicting information about that."*

**The axes are inside the hashed bytes**, appended to the canonical form only
when present. An unlabelled row therefore hashes byte-identically to v1 — the
compatibility property, pinned by tests written against the pre-v2 code — while
stripping the axes from a labelled row breaks the chain rather than quietly
reverting it to a valid unlabelled one. Same argument as SD-2: a trust label
that is not hashed can be relabelled in place.

**Rule 3 is not baked into storage.** `adjudication_stored()` is persisted, not
`adjudication()` — `Contradicted + Pending` reads as `Ambiguous` at read time,
and storing that derivation would fix a conclusion that must be recomputed when
the corroboration changes. Same reason validity has no column at all.

The migration also closed a gap it did not create: the store had **no schema
version check on an existing database**. A future-stamped store is now refused
(`SchemaFromTheFuture`) rather than opened by a binary that would write rows
missing columns it never heard of.

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

Entity resolution was **one box** here until 2026-08-08, reading *"matching
incoming observations to existing entities"*. It is two halves, and that wording
named only one of them — §6.3's pipeline marks the difference itself
(`candidate clustering (pure) ──► identity assertion (recorded Event)`), and the
redirect half is the one §6.3 attaches its cost note to: *"identity queries need
an indirection through the merge graph, and the merge graph grows."* Splitting
the box rather than part-ticking it, because ticking a box for work its own text
does not describe is how a residue disappears.

- [x] **Entity resolution, the *reading* half — resolving an identifier through
      the adjudication graph** — **DONE 2026-08-08**,
      `crates/kirra-world/src/resolution.rs`, 22 unit tests, still
      zero-dependency.
      Fills the `ResolutionOutcome` placeholder the crate has carried since it
      was scaffolded, in the shape that placeholder argued for: **not
      `Option<T>`**, because `None` collapses *we looked and it is not there*,
      *we could not look* and *we looked and are not sure* into one value. Those
      are `Unknown` / `Refused(reason)` / `Ambiguous`, alongside `Located` —
      §14.2's `WhereIs -> Located | Unknown | Ambiguous`, with §14.4's
      outcome-versus-error line drawn so that a self-contradicting graph is an
      *outcome* a caller must handle rather than an exception to unwrap.
      `Ambiguous` exists **because of** `KIRRA-WM-SPLIT-SURVIVAL-001`: a merged
      id redirects to one thing and a partitioned id to *N*, and this is the call
      site the ruling said a widened `Merged { into }` would have hidden.
      Gray/black walk (the house idiom, `kirra_safety_authority::dag`) so a
      **diamond is not a cycle** — a partition whose successors both merge into
      one entity reconverges to `Located`, and a resolver conflating the two sets
      would report that legitimate history as corrupt. Three refusals, each for a
      failure **no constructor can prevent**: a redirect cycle (two individually
      valid merge events, neither able to see the other), a dangling redirect
      (distinct from `Unknown` — the queried id exists, its history is broken),
      and a traversal budget — bounding **total edges, not depth**, so a wide
      partition spends it as a long chain does; the constant is named for the
      check rather than the other way round. A fourth refusal covers an **empty
      supersession**: `Lifecycle` is a plain public enum, so
      `Superseded { by: vec![] }` is constructible and a corrupt decoded row can
      carry it, and without the arm such an id reported `Unknown` — "no such
      entity" about an id the graph HAS, the same confusion `DanglingRedirect`
      exists to prevent one hop later. Prose held that invariant and the type did
      not. A one-element supersession is deliberately *answered* rather than
      refused: it breaks the same documented rule but has an unambiguous answer,
      and rejecting a malformed row belongs at the decoding boundary. Six
      negative controls fire, including conflating gray with black, treating
      `Retired` as a non-answer, and removing the empty-supersession refusal.
      **Pure, and not yet wired to the store** — same standing as
      `adjudication`: it walks judgements, nothing persists them. (Sub-slice 3
      below closes that.)
- [x] **Adjudication events persist as evidence rows** — **DONE 2026-08-08**,
      `crates/kirra-world-store/src/adjudication_record.rs` plus the
      `WorldStore::append_adjudication` door, 15 integration tests. The schema slice `KIRRA-WM-SPLIT-SURVIVAL-001` named as unblocked
      and did not build. Sub-slice 1 of 3.
      **No new table, and that is the finding rather than a shortcut.** Three
      sources agree that adjudications are `world_events` rows and that entity
      lifecycle is a *projection* over them: ADR-0041 fixes `world_events` as
      *"the only writable table"* and lists `entities_projection` as DERIVED;
      §6.3 says *"identity is a projection like everything else"*; and the
      **ratified v1 baseline already anticipated it** — `retention_class` has
      carried `'adjudication'` in its closed vocabulary since the beginning and
      `compaction::is_protected` holds for it, so such a row is never compacted,
      which is exactly what §6.3's *"resolvable forever"* needs.
      Naming the **predicate**, not `compaction::PROTECTED_CLASSES`: the two are
      not the same thing, and the difference is easy to state wrongly (this
      section did, and review caught it). `is_protected` is
      `retention_class != "raw"` — everything except raw is protected — and the
      constant is the enumeration OQ2 ruled, kept as documentation and not
      consulted by the compaction path, so editing it would change nothing. The
      guarantee is asserted end-to-end rather than by token comparison: an
      adjudication row makes its own window **refuse to compact** when asked of
      the real planner, with an ordinary raw row in the same test proving the
      refusal is the retention class talking and not an empty window.
      **ADR-0041's provisional list also names `identity_adjudications`,
      unannotated, among the derived tables** — read as a second *writable*
      table it contradicts the "only writable table" parenthetical three lines
      above it. The projection reading is stated explicitly in the module rather
      than left to pass as obvious, since the two readings build different
      systems.
      **The decoder never assembles a verb field by field.** It goes back
      through `MergeEntities::new` / `SplitEntity::partition` / `subtract` /
      `ForgetEntity::new` / `AssertIdentity::new`, so every refusal those make is
      a refusal at the storage boundary *structurally*, rather than by a parallel
      list of checks that could drift. This is where the cardinality check
      `resolution` deliberately declined now lives: a stored partition naming
      fewer than two destinations is refused here, so the row never becomes a
      `Lifecycle::Superseded` for the resolver to meet.
      An unknown verb or split shape is **refused, never degraded** — the
      opposite of `ObservationKind`, and deliberately: degrading a
      *classification* costs a consumer nothing, degrading a *judgement* would
      drop a redirect and leave a merged-away id resolving to itself. The
      justification rides the `provenance` column rather than a second copy in
      the payload. Five negative controls fire, including dropping `shape` from
      the encoding (a partition would round-trip as a subtraction, undoing the
      split ruling at the storage layer) and setting a compactable retention
      class.
- [x] **`entities_projection`** — **DONE 2026-08-08**,
      `crates/kirra-world-store/src/entity_projection.rs` + the fold wiring on
      `WorldStore`, 11 unit + 7 integration tests. Sub-slice 2 of 3.
      **No migration and no schema bump**: `WM2_EVENT_SCHEMA.md` §7 rules
      `entities_projection` a rebuildable view that *"follows from the fold, not
      from this table"* — named in the ratified document as deliberately outside
      the schema surface. The DDL installs on the **first fold, never at
      `open`**, because ADR-0041 D-20's `log_only_bytes` is the size of a
      log-only store and adding root pages at `open` would move that figure for
      every store, invalidating the D-2 comparison the retention horizons rest
      on.
      The reducer does not restate what a verb means — it applies
      `resulting_lifecycles`, already walked against `advance_to` by the
      adjudication seam test, so there is only one implementation to drift.
      **Contradiction poisons the ENTITY, not the fold.** Two individually valid
      events can disagree in aggregate and no constructor can refuse either.
      Failing the fold is not fail-closed but fail-bricked — one bad pair stops
      identity answers for every entity, and since the log is append-only a
      rebuild replays it and wedges again. Skipping produces a projection that
      disagrees with the log while looking healthy. So the entity is poisoned,
      the projection picks NO winner, and everything else folds — matching the
      precedent `resolution` set for a redirect cycle, which is one instance of
      the same fault. Poison is sticky and keeps the FIRST contradiction, for
      diagnostic stability. **Stated as a real limitation: no sequence of
      today's four verbs clears a contradiction** — resolution needs a fifth
      verb and its own ruling.
      Rebuild-from-zero equals incremental (`WM_SCOPE` §0a) is asserted against
      a real store with genuinely interleaved append-and-fold, and a corrupt row
      is refused rather than repaired, with a rebuild proven to recover.
- [x] **Resolution against a real store** — **DONE 2026-08-08**,
      `entity_projection::IdentityView` + `WorldStore::identity_view`. Sub-slice
      3 of 3; the schema slice is complete and `resolution::resolve` is now live
      against recorded evidence rather than a test fixture.
      **Not an `AdjudicationGraph` impl on `WorldStore`, and that is the
      finding.** `lifecycle_of` returns `Option<Lifecycle>` with **no error
      channel**, so a per-query storage reader would have to turn a read failure
      into `None` — and `None` means *"the graph has no such entity"*. That
      reports an existing id as absent: precisely the bug `EmptySupersession`
      was added to fix, reintroduced one layer down where the resolver cannot
      see it. The fallible work therefore happens **once, at load**:
      `identity_view()` returns a `Result` and refuses a corrupt projection
      rather than resolving over a partial one. The trait's contract is honoured
      by construction. A snapshot also means one walk sees ONE state of the
      world, where a per-query reader could observe a fold landing mid-walk and
      answer from two generations at once.
      `RefusalReason::ContradictoryHistory` closes the loop from sub-slice 2: a
      contradicted entity refuses **per query**, including for a question that
      merely routes *through* it, since an answer that travelled through a
      contradicted identity is not trustworthy because the question named
      something else. The `is_contradicted` seam is a defaulted trait method, so
      every graph that does not model contradiction is behaviourally unchanged.

- [ ] Entity resolution, the *matching* half — **candidate clustering**, deciding
      that two observations are about the same thing. Marked *pure* by §6.3.
      Still open, and still the thing `Corroboration(n)` waits on (§4): the axis
      and its fold are implemented, and nothing populates the count because no
      matcher exists. `KIRRA-WM-CANDIDATE-ID-001` constrains rather than supplies
      it — a candidate's identifier may not enter the hashed evidence record, so
      candidate membership has to be projected, not frozen.

      **Written up for a ruling on 2026-08-08**:
      [`WM_CANDIDATE_CLUSTERING_PROPOSAL.md`](WM_CANDIDATE_CLUSTERING_PROPOSAL.md)
      (`KIRRA-WM-CLUSTERING-001`). Drafting it turned up why this box is unlike
      the rest of Tier 2: **the blueprint specifies candidate clustering in one
      word and a parenthetical** — the §6.3 diagram and one roadmap row, with no
      definition of matching, no features, no thresholds, no output type. The
      other Tier 2 questions were recoverable by reading carefully; this one is
      not, so building a matcher would mean inventing a similarity model and
      calling it a reading.
      The proposal recommends splitting the box: rule the **contract** (a cluster
      is a proposal and never evidence; its id is a projection key; purity is
      rebuild-from-log; every co-reference judgement names its source) and land
      **declared co-reference** as the first driver of `Corroboration(n)` — using
      the finding that the model already expresses "these two are the same
      thing" as an `ObservationKind::Relationship` claim with a `same_as`
      predicate, which inherits source, confidence, provenance, the chain and
      SD-2 for free. A similarity matcher is then *relocated* rather than
      forbidden: it becomes a `derivation`-class producer of `candidate`
      `same_as` claims, admitted through the same door and visible as inferred,
      instead of reaching into a trust axis directly. Deliberately left open:
      the similarity model itself, what confirms a `same_as` candidate, and
      **transitivity**.

      **RULED 2026-08-08 — adopted.** *Clustering may PROPOSE co-reference; it
      may never CONFIRM identity.* A heuristic or learned matcher is authorized
      as a candidate producer; confirmed identity still arrives only through
      explicit adjudication over recorded evidence. **This box is therefore
      retired and replaced by the four below** — one vague box is exactly how a
      matcher gets mistaken for identity truth, which is the failure the ruling
      exists to prevent.

- [ ] **2a — Candidate generation / matching.** A `derivation`-class producer
      emitting `candidate` `same_as` claims: the two subjects, the similarity or
      evidence that prompted it, the source and its model/rule **version**, and
      confidence provenance. It writes through the same door as every other
      producer (§4.2) and is visible as inferred.
      **It may not write `claim_status = confirmed`, and may not touch
      `Corroboration(n)`.** ⚠️ **This is a POLICY REQUIREMENT, not an enforced
      boundary — and the difference was found by review, not by the schema.**
      SD-2's `CHECK` refuses `llm_candidate` + `confirmed` and *only* that:
      `world_events` has `CHECK (writer_class <> 'llm_candidate' OR claim_status
      = 'candidate')`, so **`derivation` + `confirmed` is currently accepted by
      the store.** An earlier draft here said the boundary was "extended to
      `derivation`". It is not, and describing an unenforced policy as an
      existing guard is how a reader stops looking for the guard.

      In 2a the constraint holds because the *type* cannot express a confirmed
      candidate — `SameAsCandidate` has no claim-status field and pins its class
      to `Derivation` — so nothing in that path can write one. That is a
      producer-side guarantee, and it says nothing about a producer written
      later, by someone else, against the same store.

      **OPEN — closing it is a schema change, not a doc change:** extend the
      `CHECK` to `writer_class NOT IN ('llm_candidate','derivation') OR
      claim_status = 'candidate'`, with a migration. Until then the write door
      admits what `KIRRA-WM-PROMOTION-001` forbids, and only convention stops
      it.

      **`Corroboration(n)` is NOT 2a's to drive.** The axis folds *confirmed*
      `same_as`, so its first driver is promotion (2b) or an operator-declared
      confirmation — never the matcher. An earlier note in this session said
      building 2a "gives `Corroboration(n)` its first driver"; that contradicted
      the bar above and is withdrawn. A matcher score must not become trust
      corroboration, quietly or otherwise.

      **The seam, fixed by `KIRRA-WM-PROMOTION-001` before this box is coded:**
      a promotion cites candidates by `ObservationId` through `Justification`,
      and `KIRRA-WM-CANDIDATE-ID-001` keeps a candidate's identifier out of the
      hashed record — so a cluster **cannot** be cited, only a candidate
      observation can. **2a therefore emits candidates as observations**, not as
      in-memory cluster objects passed to an adjudicator. 2a needs to know
      nothing else about how confirmation works.

- [ ] **2b — Adjudication / promotion.** The deterministic, auditable step that
      turns a confirmed `same_as` into identity, and the **only** boundary at
      which a relation may affect canonical identity.
      **Unblocked: transitivity was ruled 2026-08-08**
      (`KIRRA-WM-TRANSITIVITY-001`). Evidence is pairwise and never transitively
      closed; a confirmed `same_as(A,C)` is never synthesized from an
      `A=B`, `B=C` chain. Promotion accepts / rejects / marks ambiguous **per
      relation**.
      **Promotion authority ruled 2026-08-08** (`KIRRA-WM-PROMOTION-001`):
      a candidate never becomes confirmed because a matcher produced it or
      because candidates agree — promotion requires an **explicitly authorized
      adjudicator**, and v1 is **`WriterClass::Operator` only**, enforced at the
      same write door as SD-2. Automated adjudicators each need their own ruling.
      Rejection is a **separate append-only record** (not a deletion, and not a
      reversal); reversing an actual promotion is existing `SplitEntity`;
      `ForgetEntity` is erasure and is not a reversal mechanism.

- [ ] **2c — Deterministic resolution over promoted identity.** Rebuild-from-log
      must stay exact: the same log yields the same identity, with no dependence
      on matcher output, tie-break order or wall-clock. The existing
      `resolution::resolve` invariants are the floor, not a new standard.
      Per `KIRRA-WM-TRANSITIVITY-001` rule 4, traversal of accepted merges
      **preserves the path and its provenance**, and a contradictory promoted
      graph resolves to `Ambiguous` / `Refused` rather than being repaired.
      **Union-find is disqualified as the representation** — merging is its only
      operation, so it cannot express "contradictory" and would decide an
      adjudication question at read time. `resolve`'s existing refusal of a
      redirect cycle is the precedent.

- [x] **2d — Historical / as-of resolution.** **DONE 2026-08-09**,
      `WorldStore::identity_view_at` / `resolve_at` +
      `entity_projection::HistoricalIdentityView`, 13 integration tests.
      **RULED 2026-08-08: as-of identity resolution stays INSIDE Tier 2 and gets
      built** — the tier is not redefined to avoid it. As built, it **composes an
      as-of projection view with the existing resolver**: the fold is re-run over
      the adjudications recorded by the instant, and
      `kirra_world::resolution::resolve` — unmodified, the same function the
      present-tense path calls — walks the smaller graph. No second resolution
      algorithm, no historical outcome variants, no bitemporal substrate built
      here (it already ships, #1353).
      **The cut is on transaction time** (`txn_time_ms`), the `as_known_at_ms`
      axis of `as_of`. Valid time is deliberately not consulted: an adjudication
      is a judgement whose effect begins when recorded, not a claim holding over
      an interval, and `append_adjudication` hardcodes `valid_to_ms = NULL`, so
      filtering on that axis would look rigorous and do nothing. A genuine
      valid-time identity question is a different query needing its own ruling.
      **`KIRRA-WM-CLUSTERING-001` holds at every instant, not just the present:**
      the fold's `claim_status = 'confirmed'` predicate is inherited rather than
      restated, so a candidate `same_as` observation is invisible to the
      historical view too — it cannot be the back door into the confirmed graph
      that the write door refuses.
      **Degradation is derived, not asserted.** The answer carries a
      `Resolution`; it reads `Full` over a compacted store because
      `compaction::is_protected` holds for the `adjudication` retention class, so
      a recorded citation is itself evidence its window held no adjudication. The
      derivation consults that predicate at runtime, so a future compaction mode
      that could remove a protected class makes identity answers degrade instead
      of silently keeping their completeness claim. In that fallback **every
      recorded citation degrades**, with no narrowing by time: `compacted_at_ms`
      is when compaction RAN and `as_known_at_ms` is the instant asked ABOUT, so
      a compaction running *after* the queried instant is exactly the one that
      removes evidence bearing on it — and the removed rows are the only record
      of their own transaction times, so a span cannot be shown irrelevant after
      the fact.
      §6.3 makes historical identity part of the intended semantics, and the
      incident-reconstruction goal is not served by a resolver that can only
      answer "who is this *now*" — reconstruction asks who it was *then*.
- [x] **`MergeEntities` / `SplitEntity` / `ForgetEntity` as recorded events** —
      **DONE 2026-08-07**, `crates/kirra-world/src/adjudication.rs`, 24 unit
      tests, still zero-dependency.
      The three verbs are constructor-validated records, not commands: private
      fields, accessors only, so an "event" cannot be amended in place — which
      is the edit-wearing-an-event's-name failure §6.3 describes. Refused at
      construction rather than by convention: a merge into one of its own
      sources (a self-redirect, which would surface as a resolution loop in a
      projection long after the event that caused it), a split into fewer than
      two, duplicate sources/destinations/citations, and an unjustified
      adjudication. **`ForgetEntity` has no sibling `Redact`**, so this module
      cannot express erasure at all — a caller reaching for deletion finds
      nothing to reach for.
      **`Evidence` was a specification gap, ruled not invented.** §14.1 writes
      `MergeEntities(from[], into, Evidence)` and defines `Evidence` nowhere; it
      appears as an unelaborated parameter name in three verb signatures and
      nothing else. Supplied reading: **evidence is the observations that
      justify the judgement** (`Justification` — non-empty, duplicate-free,
      order preserved). Deliberately **not** an `EvidenceDigest`: a digest is
      the adjudication's own chain position, which the store computes *after*
      appending it, so requiring one at construction would mean inventing a
      value that does not exist yet. Operator teaching needs no exemption — an
      operator's ruling is already recorded as a `SourceClass::Operator`
      observation, so "the operator said so" cites a real `ObservationId`.
      **Seamed to the lifecycle algebra**: every consequence
      `resulting_lifecycles` states is a transition `Lifecycle::advance_to`
      permits, walked from every live state, so the event model and the state
      model cannot drift into contradiction. Non-vacuity anchored — reverting
      terminality in `advance_to` fails the anchor test *and* `entity.rs`'s own.
      **The stamp is a `DomainInstant`, not an integer** — matching
      `relationship.rs`'s transaction time, so an adjudication has to name the
      clock it was recorded on. Two stamped on unsynchronized clocks are refused
      a comparison rather than ordered confidently and wrongly; a bare `i64`
      would also have made a negative timestamp representable and would have been
      the store's SQLite spelling leaking up into the domain core.
      **One consequence is deliberately not stated** — see the open question
      below.
- [x] **`entity_id` minting** — **DONE 2026-08-08**,
      `WorldStore::mint_entity_id` + the `entity_id_mint` ledger (schema v4),
      6 integration tests.
      Moved here from §6 on 2026-08-07 by `KIRRA-WM-TIER1-DONE-001`. It was
      always described as belonging here (*"minting an id is deciding that
      something is a distinct thing, which is adjudication"*) while being listed
      under Tier 1, which is what held that tier's box open on Tier 2 work.
      Listed rather than deleted: §6's residue was a real work item, and a
      residue that disappears when a box is ticked is the failure the ruling's
      own second constraint names.
      §6.1 asks for ids that are *"stable, opaque, monotonic. Never reused, never
      encodes semantics."* Stable and opaque are properties of the **type**; the
      other two are properties of the **generator**, which is why they need
      durable state and could not live in the zero-dependency core. Monotonicity
      takes its floor from the durable high-water rather than the clock, so an
      NTP step or a VM restore cannot regress an id — tested by moving the clock
      backwards and across a reopen. Never-reuse is the ledger's `PRIMARY KEY`,
      demonstrated by writing a minted id twice and finding the constraint fire,
      rather than described.
      **This box was left unticked when the work merged (2026-08-08) and was
      caught on re-reading, not by a gate.** Prose cannot fail CI, which is why
      it is the thing that goes stale.

**RULED 2026-08-08 — `KIRRA-WM-CANDIDATE-ID-001`**
([proposal](WM_CANDIDATE_ID_PROPOSAL.md)): a candidate's identifier **may not
enter the hashed evidence record**. The blueprint marks candidate clustering
*pure* and identity assertion the *recorded event*, so a candidate id is derived
from other rows in the same store; freezing one into an append-only row records a
derivation a later clustering run cannot correct, and nothing detects the
disagreement. The store's subject discriminant therefore admits `entity` and
`frame` only, narrowed before release. `CandidateId` is deferred to entity
resolution, where it would be a projection key rather than an evidence value.

This is a **constraint on the first box above, not a completed part of it** —
entity resolution still has to say how candidate membership is projected, and the
ruling narrows the options rather than supplying one.

Merge and split are *events, never destructive edits*. This is what makes an
`EntityId` revisable, and it is precisely what a store built on a bare opaque
key can never retrofit — the key would have already lost its own history.

`ForgetEntity` retires an entity and suppresses it from default projections. It
is **not** deletion. Genuine erasure, if ever required, is a distinct audited
`Redact` with its own ADR, and must leave a tombstone or the chain breaks.

### Open question, now blocking — what becomes of the entity that was split?

`entity.rs` has carried this since the lifecycle went in: *"is `Split(from)` a
live origin marker or a terminal marker on the entity that was split? The two
readings differ in whether the **original survives** a split."* It was a note
while nothing depended on it. Writing `SplitEntity` made it load-bearing,
because a split event has to say what happened to both sides or admit it cannot.

The two readings are not close together:

* **Original survives.** An entity that no longer corresponds to anything stays
  live in the model — a phantom that answers queries.
* **Original is superseded**, terminal like `Merged`, still resolvable. But a
  redirect needs a target, and a split has *N* of them. That is unanswerable by
  a single redirect, and it is suggestively close to `WhereIs`'s third return
  value in §14.2: `Located | Unknown | **Ambiguous**`.

The second reading has the better of the argument, and it needs a `Lifecycle`
state that does not exist today (`Merged { into }` cannot express *N* targets).
Widening `Lifecycle` inside an event-model slice would have buried a ruling in a
helper function, so it was **not** taken.

**What was done instead**: `IdentityAdjudication::resulting_lifecycles` states
no fate for the split source, and `unresolved_consequence` **names** it. A
caller that does not handle the source is visibly declining to, rather than
consuming a list that quietly dropped an entity. Pinned by test, and the
negative control (fabricating a fate) fails three tests including the seam
count.

This has to be ruled before `SplitEntity` can be persisted — a store needs a
row for the source, and "undecided" is not a column value.

**RULED 2026-08-08 — §5 adopted.** `Lifecycle::Superseded { by }` is the
terminal, still-resolvable state for a partitioned source; `SplitEntity` now has
`partition` and `subtract` constructors, so the **subtraction** shape that was
unrepresentable (both spellings refused) is expressible; `unresolved_consequence`
is deleted. This does **not** authorize persisting `SplitEntity` — that is still
the schema slice, and the adjudication verbs remain domain-only.

**Written up for a ruling on 2026-08-07**:
[`WM_SPLIT_SOURCE_PROPOSAL.md`](WM_SPLIT_SOURCE_PROPOSAL.md)
(`KIRRA-WM-SPLIT-SURVIVAL-001`). Drafting it turned up something the summary
above got wrong: **the constructor had already chosen.** The sole constructor at
the time — `SplitEntity::new`, since renamed to `partition` and joined by
`subtract`, so do not go looking for it — refused both spellings of a surviving
original: `into = [piece]` by `SplitTooNarrow`, and `into = [source, piece]` by
`SplitIntoSelf`. The "deliberately undecided" claim held in the prose and not in
the type. The
proposal also finds that the two readings answer *different questions*, and
recommends admitting partition and subtraction as distinct shapes rather than
picking one.

---

## 5.5. Tier 2.5 — The first sanctioned consumer

Numbered 5.5 rather than inserted as §6 because every cross-reference in this
document names a section by number; renumbering to make room would silently
break them all.

**This milestone adds no features.** Its output is a placement decision, a seam,
and a list of contracts that broke — and that is the point. §9 already argues
for wiring a consumer *before* Tier 3, on the grounds that a real caller is what
falsifies the contracts. The 2026-08-07 attempt proved the argument twice over
(Tier 3's rule 1 against `ProjectedClaim`; the emergent
`inadmissible_never_read` guarantee) from a consumer that never shipped. What it
also proved is that the advice is not actionable as written.

### The gap this closes

Fence B refused the attempted consumer, correctly, and the refusal exposed
something the tier plan does not provide for:

> **There is no sanctioned place for semantic world knowledge to become
> operational behaviour.**

That is an architectural gap, not a task. Until it has a named home, every
future consumer rediscovers it after writing the same code.

### What the workspace already answers

`kirra-world-service` is **the only crate that depends on `kirra-world*` and
implements no `CorridorSource`** — its runtime dependencies are `kirra-world` and
`kirra-world-store`, and nothing else. (It also carries one dev-dependency: the
same `kirra-world-store` with `test-support`, for the raw-SQL escape hatch that
plants a corrupt chain digest. Worth naming rather than eliding, because the
§11 finding below turns entirely on how dev edges are classified — the same
distinction, seen where it does no harm.) It is outside every barred set: not a
safety-closure member, not a corridor producer, nothing transitive to drag in.
The **hosting** question is therefore already answered by construction.

**Hosting is not consuming, and this is the trap.** Nothing depends on
`kirra-world-service` today except workspace membership, so it carries the same
*built for nobody* problem one level up. A service crate is a transport surface;
this milestone needs something that turns world knowledge into behaviour.

### Where a consumer could live — the survey, run rather than argued

The placement question was settled by executing the fence's own predicates over
the workspace manifests, not by reading crate names. What that produced:

* **54 workspace packages.** Fence B's safety closure — the transitive
  dependencies of the 10 `SAFETY_ROOTS` — is **19** of them; 35 sit outside it.
* The behaviour-shaping crates are **not** caught by Fence B. `kirra-planner`,
  `kirra-map`, `kirra-taj`, `kirra-sidecars` and `kirra-mick` are all outside the
  safety closure. What bars them is the *other* gate: `check_4_trait_impls`, whose
  conjunction is *implements `CorridorSource`* ∧ *`pkg_reaches_world`*. The crates
  carrying a non-`cfg(test)` `impl CorridorSource` — the set the gate actually
  keys on — are `kirra-core`, `kirra-map`, `kirra-ros2-adapter`, `kirra-sidecars`
  and `kirra-taj`. **`kirra-core` is on that list for a reason worth stating:** its
  impl is `MockCorridorSource`, documented in place as "a straight-line test
  stand-in, not drivable space" whose selection for a real deployment "is a
  configuration error". It is not production drivable space — but it is not
  `cfg(test)`-gated either, so it compiles into the library and the gate counts it.
  The gate keys on *what ships*, not on what was intended, which is the correct
  behaviour for a fence and the reason the list must not be read as five
  production corridor producers.
* **Two of the five are barred directly and three transitively**, which is worth
  separating because only the first kind is obvious from the crate itself.
  `kirra-map` and `kirra-taj` implement `CorridorSource`, so a world edge on
  either satisfies the conjunction on the spot. `kirra-planner` and `kirra-mick`
  implement nothing — they are barred because `kirra-sidecars` *does* implement it
  and depends on both (its full **Kirra-internal** dependency list is `kirra-core`,
  `kirra-planner`, `kirra-trajectory`, `kirra-taj`, `kirra-mick`; it carries
  third-party crates besides, which the closure walk follows but which cannot
  reach Kirra World), so a world edge added at either makes `kirra-sidecars` reach
  world and the gate fires there. That is the transitivity direction §9 relies on,
  confirmed rather than assumed.
* **27 packages pass both gates mechanically.** Nearly all are harnesses, benches,
  fuzz targets or proof crates. A consumer placed in one of them would satisfy
  goal 1 *vacuously* — the fence would say yes to a crate that ships no behaviour,
  which is precisely the acceptance that carries no information.

So the survey does not converge on an existing host. Every crate that would make
goal 4 real is barred by the corridor conjunction, and every crate that is
mechanically clear would make goal 1 vacuous. That is not a gap in the survey; it
is the answer the survey produced.

### `KIRRA-WM-CONSUMER-PLACEMENT-001` — RULED 2026-08-10

> **Tier 2.5 requires a dedicated non-authoritative consumer crate whose sole
> role is to translate Kirra World knowledge into proposal-shaping inputs. It may
> influence what is proposed, but it may not implement or feed `CorridorSource`,
> checker bounds, release authority, or actuation.**

The shape:

```
Kirra World → world-consumer / mission-context crate → proposal-shaping context → planner / mission logic
```

**The key is that it outputs proposal context, not bounds.** The crate sits
upstream of the doer and downstream of nothing safety-authoritative. It is the
named home the gap section says does not exist — created, rather than discovered,
because the survey above shows there is nothing to discover.

This restates, at crate granularity, the invariant Tier 2.5 exists to defend:

> **Kirra World may change what is proposed. It may not change the inputs from
> which the checker derives what is permitted.**

Two consequences worth stating so they are not rediscovered:

* **"Non-authoritative" is a dependency fact, not an intent.** The crate is
  non-authoritative because the two gates say so with the edge present — not
  because its documentation says it only advises.
* **A new crate is the weaker-coupling choice, not the heavier one.** Adding the
  world edge to an existing behaviour crate would put every *other* thing that
  crate does inside the blast radius of a future world-authority argument. A
  single-purpose crate keeps the surface the fence must defend equal to the
  surface that actually consumes.

### Two consumers, proving different things

Stated explicitly because the natural first choice proves the *weaker* of the
two properties:

* **A typed Rust caller** — falsifies the **contracts**. This is what §9's
  argument is actually about: `ProjectedClaim`'s defect was that public fields
  let a caller reach `.payload` with no validity, trust or handle. That class of
  defect is a *type* problem.
* **An operational caller** — falsifies the **seam**. The Rabbit Channel A path
  is the candidate: pure speech, zero actuation authority by existing design,
  and `rabbit_ask.py` is grounded Q&A with no grounding source today.

**If only one is built, it must be the typed one.** An HTTP/JSON consumer cannot
falsify a Rust answer boundary — serialization hides exactly the misuse the
boundary exists to prevent, so a Python caller would report success against a
contract it never tested.

### Goals

- [x] **Name the non-authoritative host** for world knowledge, and record why it
      is outside the checker's closure rather than merely believed to be.
      *Placement ruled* (`KIRRA-WM-CONSUMER-PLACEMENT-001`): a **new** crate,
      because the survey found no existing one that satisfies both halves.
      **The box stays open, and the reason is the non-vacuity condition on it:**
      the named host must be one whose removal would change observable proposal
      behaviour. A host that passes the fence only because it does nothing
      satisfies the *letter* of this goal and none of its purpose. Naming is
      therefore half of it; goal 4's differential proof is what earns the other
      half, and this box closes when that proof runs — not when the crate exists.
- [x] **Define the one-way seam** from Kirra World into operational software.
      The seam's type-level obligation: what crosses it is *proposal context*, and
      nothing in its output type is admissible as a checker bound.
- [x] **Prove the consumer cannot influence the checker — mechanically.** The
      acceptance criterion is that
      `ci/check_kirra_world_bidirectional_fence.py` passes **with the new
      dependency edge present**, not an argument in a document.
- [x] **Prove the consumer changes proposals and only proposals — differentially.**
      Two runs over one scenario, world-consumer off and on: the proposals must
      differ (else goal 1 is vacuous) *and* the checker's bound-derivation inputs
      must be bit-identical (else the invariant is breached).
- [x] **Exercise the Tier 3 contracts with a real caller** (the typed one).
- [x] **Capture every contract that breaks** before the API is expanded.

### The acceptance proof — four parts, and why each is separate

Each part fails in a way the other three cannot detect. That is the reason they
are not collapsed into one criterion.

1. **Fence positive control.** `check_kirra_world_bidirectional_fence.py` passes
   with the new edge present. *Negative control:* the fence's existing refusals —
   the 2026-08-07 attempt recorded in §9 — prove it can say no. Both directions
   are required; the fence has so far only ever been observed refusing, and a gate
   that only refuses is indistinguishable from one that refuses everything.
2. **Corridor-conjunction control.** The consumer crate implements no
   `CorridorSource` and appears in no `CorridorSource` implementor's closure. This
   is checked by contents (gate t24's technique), not by dependency direction —
   an inverted implementation would be invisible to a closure walk.
3. **Behavioural non-vacuity.** Turning the consumer off changes what is proposed.
   Without this, parts 1 and 2 are satisfied by a crate that does nothing.
4. **Bound-derivation invariance.** Across the same two runs, the inputs the
   checker derives its bounds from are **bit-identical**. Parts 3 and 4 are the
   two halves of the invariant and must be asserted over the *same* pair of runs;
   asserted separately, part 3 would pass on a run pair that also moved the bounds.

### The differential proof's substrate — what `kirra-replay` does and does not give us

Inspected before designing the harness, because reusing existing capture is
strictly better than minting a new artifact if the fields are there. **The
question asked was: does the captured record expose the checker's authoritative
bound-derivation inputs separately enough to compare two runs bit-identically,
independent of proposal/verdict differences?**

**`kirra-replay` itself: no, and not for a fixable reason.** Its comparator is
`VerdictImage` — `{outcome, deny_code, safe_value_bits, mrc}` — and its entire
contract is *same inputs → same verdict*. Part 4 needs the opposite framing:
inputs deliberately differ on the proposal axis, so `ReplayResult::Divergent`
would fire on exactly the case the proof is designed to produce. Reusing it would
invert the meaning of its alarm.

**`kirra-capture-schema`'s `CaptureRecord`: no.** Three specific shortfalls:

* **Proposal and bound-derivation inputs are commingled.** All five fields sit in
  one `ProposedCommandSnapshot`. `linear_velocity_mps` / `steering_angle_deg` are
  the proposal; `current_velocity_mps`, `current_steering_angle_deg` and
  `delta_time_s` are the ego state and time base the envelope is computed against
  (P5b's rate ceiling and P3/P4's accel bound read them directly). Splitting them
  would be a harness-side naming convention, not a property of the record — and a
  field added later carries no signal about which side it belongs on.
* **The contract identity is absent entirely.** `VehicleClass` reaches the checker
  as `contract_for(class)`, and `kirra-replay` takes it as a CLI argument. Nothing
  in the record pins which envelope was in force, so the artifact cannot *prove*
  two runs used the same one — which is the single most important thing part 4
  must hold fixed.
* **The derate cap is a bool.** `derate_enabled` records that a perception cap
  composed, never its value. `kirra-replay` is honest about this and classifies
  such records `NotReplayable`; that honesty is exactly why the field cannot be
  used as evidence of an unchanged bound.

**`kirra-cycle-record`'s `JoinedCycleRecord`: yes, and it is already the right
shape.** It separates the three axes the proof needs, which is not a coincidence —
it was built for incident review, which has the same separation problem:

* **Bound-derivation inputs → `PerceptionEvent.evidence_digest`**, a SHA-256 over
  the complete accepted safety-relevant Taj output: the corridor `left`/`right`
  polylines, objects (id/x/y/vx/vy/coasted), pedestrians with their classification
  and fusion reason, `clear_distance_m`, `minimum_corridor_width_m`,
  `required_corridor_width_m`, `speed_cap_mps`, and the `profile_digest` that binds
  evidence interpretation to the configuration that produced it. One 64-hex value,
  bit-comparable, computed from the Taj response alone.
* **The enforced cap → `raw_speed_cap_mps` / `stabilized_speed_cap_mps`**, carried
  as a *pair* precisely so a review can see whether the enforced cap was
  perception's own or the stabilizer holding a stale anchor.
* **The proposal axis → `ProposalEvent.proposal_digest`**, distinct from the
  evidence digest, with an existing echo-check that the planner bound the evidence
  it claims.
* **Cross-run alignment → `scan_sequence`**, the cycle's primary key.

**The smallest extension needed, therefore, is one field, not a new artifact:**
the **kinematic contract identity** (vehicle class, or a digest over the resolved
`VehicleKinematicsContract`). Without it, two runs can be shown to have had
identical perception evidence and still not be shown to have been judged against
the same envelope. Everything else part 4 requires is already captured.

**Which record it goes on is decided by which witness can know it, and that is a
narrower answer than "the record".** The stage events have three separate
witnesses, and the contract identity is not available to the two that would be
convenient:

* The **interceptor** builds the release event by decoding the signed 176-byte V2
  payload, and refuses anything else — a truncated payload decoded best-effort
  "would attest fields that were never signed". The class is not in that payload.
* **No witness on the robot can read `KIRRA_VEHICLE_CLASS`.** It lives in the
  verifier's env (`kirra.env`, root 0600); `robot/doctor` records this explicitly
  and infers the class was set only from the verifier being up.

So a class field added to `PerceptionEvent` or `ProposalEvent` would be
witness-*asserted*, not witness-*known* — the exact thing the release decoder
refuses to do. **It belongs on `CaptureRecord`, which the verifier itself
writes**, and it reaches the joined artifact through the `CaptureRecordRef` link
that `kirra-cycle-record` already defines for precisely this division of labour:
the joined record proves chain continuity, the verifier capture supports decision
recomputation, and the two stay separate artifacts joined by reference. That the
same field also closes `CaptureRecord`'s own gap above is not a coincidence — it
is the same gap seen from two ends.

**One honest cost, so step 2 is not mis-scoped: the schema change is one field,
the wiring is not.** `gateway_capture_ref` is `Option`al and additive, and the
Python emitter does not populate it today — the reference route exists in the Rust
schema with no producer. Step 2 is therefore *field + producer*, and
`CaptureRecord`'s wire shape is byte-pinned by two tests, so the pins move
deliberately rather than incidentally. That is what a pin is for.

**A cheaper route exists and is recorded as rejected-for-now rather than
unnoticed.** `EffectiveConfig::effective_digest` is a SHA-256 over the boot-config
snapshot that *includes* `vehicle_class`, is already committed as an
`EffectiveConfigDigest` audit event at startup, and is already exportable through
the auditor-tier audit export. Two arms with equal digests provably shared a
class. It is rejected because it is too coarse in the wrong direction: the digest
covers every captured config value, so two arms differing in anything incidental
(a DB path, which a two-arm harness would plausibly vary) produce different
digests and the check says *something* differed without saying what. A coarse
check that fires on the harness's own setup trains people to ignore it.

Two things deliberately **not** added:

* **A Kirra World provenance field on the cycle record.** The harness controls
  which arm is which, so run identity is known out of band. Adding it would put
  world-derived data into the transport of a safety artifact — close enough to the
  fence that the convenience is not worth the precedent.
* **`profile_digest` as its own field.** It is folded into `evidence_digest`, so a
  perception-config change is *detected* (the digests differ) but not *explained*.
  That is a diagnosability gap, not a soundness gap, and part 4 needs soundness.

### `KIRRA-WM-SYMBOLIC-SEAM-001` — RULED 2026-08-10

> **World-derived proposal context is SYMBOLIC ONLY. Its public API may carry
> identities, relations, ordering, categorical state, and opaque references; it
> may not carry numeric quantities that could encode checker bounds.**

This is strictly stronger than the placement ruling, and the difference is worth
stating because the weaker version is the one that sounds sufficient.
`KIRRA-WM-CONSUMER-PLACEMENT-001` constrains **where** the consumer sits — a
property of the dependency graph, true today, and one `use` away from false. This
constrains **what the seam can hold**. Every checker bound in this codebase is a
magnitude with physical units, and a type with nowhere to put a magnitude cannot
carry one however much a future caller wants it to.

**Integers are banned too, not only floats.** `speed_mm_s: u32` is a bound in
disguise, and it is the *more* likely accident: someone reaching for integer
millimetres is usually being careful about precision. The enforcement inverts the
burden — no primitive numeric field on a public type, with individual allowlist
entries requiring a written non-physical justification. A quantity with units
cannot write that justification.

Enforced by `ci/check_proposal_context_symbolic.py`. Its allowlist is **empty**,
which is the strongest state it can be in.

**Honest limit, stated rather than discovered later:** the gate checks values that
*cross* the seam — fields of public types — not function parameters. `now_ms` is a
bitemporal query instant; the store cannot be read without one and it is never
carried on the context. A producer could in principle take a bound as an argument
and encode it into an id string. Nothing here would catch that, and nothing cheap
would.

### Tier 2.5 step 3 — the evidence that preceded closure

`crates/kirra-proposal-context` exists: the sanctioned consumer, whose only Kirra
dependencies are `kirra-world` and `kirra-world-store`. Deliberately **not**
`kirra-core` — that is where `CorridorSource` and `VehicleKinematicsContract`
live, and a crate that can *name* a checker-bound type is one refactor from
producing one.

**What the differential harness shows** (`tests/differential.rs`), with the three
controls that make it non-vacuous:

1. World silent vs world knowing `package_17 last_seen_at dock_b` yields a
   different symbolic context, and a proposal-producing function fed by it
   chooses a different destination. *(the positive result)*
2. **A context-BLIND producer shows no difference.** Without this, the positive
   result could hold because the two runs differed incidentally, and the harness
   would report success having tested nothing about the seam.
3. **The gate refuses a synthetic bound** — `ci/test_proposal_context_symbolic.py`
   feeds the real scanner nine bound-bearing shapes and fails if it stays quiet.
   This one already earned its keep: it caught a hole in the first
   implementation, where a single-line struct variant
   (`Envelope { lateral_accel_mps2: f64 }`) matched none of the field patterns.
   The scanner now reads whole lines inside a public type body, because
   enumerating declaration shapes is only ever as good as the list, and the list
   was already wrong once.

**The fence positive control fires.** `check_kirra_world_bidirectional_fence.py`
passes **with the new world edge present**: 55 workspace packages now, Fence B's
closure still 19, and the consumer outside it. That is the first time the fence
has been observed saying *yes* to a legitimate route.

**And what this does NOT show, which is why Tier 2.5 stays open.** The proposal
producer in the harness is test-local. Nothing in production consumes the context,
so no production behaviour path has changed — and §5.5's Goal 1 requires a host
whose *removal changes observable proposal behaviour*. This is Tier 2.5
**evidence**, not Tier 2.5 closure.

The reason the shortcut was refused rather than taken: wiring `kirra-planner` to
Kirra World would create `kirra-sidecars → kirra-planner → kirra-world*`, and
`kirra-sidecars` implements `CorridorSource`, so check 4's conjunction should
refuse it. Building that route to make the evidence look stronger would be
building the forbidden route.

**What closure needs** is one more ruling: the production **proposal-orchestration
boundary** — a seam through which symbolic context enters real proposal generation
*without* the existing planner gaining a dependency on Kirra World. It is made
directly below.

### `KIRRA-WM-ORCHESTRATION-BOUNDARY-001` — RULED 2026-08-10

> **A production orchestration layer may consume `kirra-proposal-context` and
> pass symbolic preferences into proposal generation, but the proposal producer
> itself must remain World-blind, and no type crossing the seam may encode
> checker bounds.**

The clause that does the work is *"the proposal producer itself must remain
World-blind."* The orchestration layer holds **both** edges — to Kirra World's
consumer and to the planner — and neither the planner nor anything below it ever
gains a world dependency. Direction is the whole mechanism:

```text
kirra-world → kirra-proposal-context → ORCHESTRATION HOST → kirra-planner → proposal
                                                                                │
──────────────────────────────────────────────────────────────────── checker boundary
                                            CorridorSource / contract inputs → checker
```

**A new crate again, and the dependency direction is why.** Simulated before
ruling rather than argued: a host depending on both `kirra-proposal-context` and
`kirra-planner` was added to the workspace and the fence's own predicates run
over it.

| package | reaches `kirra-world*` |
|---|---|
| `kirra-mission-orchestrator` (the host) | **true** |
| `kirra-sidecars` | false |
| `kirra-planner` | false |
| `kirra-core`, `kirra-map`, `kirra-taj`, `kirra-ros2-adapter` | false |

The fence reported INTACT with the edge present, closure still 19 of 56. Every
`CorridorSource` implementor stays world-free because the arrow points *from* the
host *to* the planner: `kirra-sidecars → kirra-planner` is unchanged, and nothing
depends on the host.

### The one hop past the seam that needed its own rule

`PlanInput` carries `Goal { target: Pose }`, and a `Pose` is coordinates. So
somewhere between a symbolic `dock_b` and a real plan, a symbol becomes numbers.
That hop is not optional, and leaving it unruled would quietly undo the symbolic
seam one call later.

> **The symbol→coordinate resolution must come from the mission/map
> configuration, never from Kirra World.**

Kirra World may say *which* destination; it may not say *where* that destination
is. The orchestrator resolves the chosen symbol against coordinates it already
held, so world knowledge selects among numbers that already existed rather than
authoring one. Without this, "symbolic only" would hold at the seam and be lost
at the first hop past it — and a world-authored `Pose` is a world-authored
number sitting one type away from the planner's input.

Note what this does *not* claim: `Goal` is a proposal input, not a checker input.
The checker bounds the trajectory the planner emits; it never reads the goal. The
rule exists because the discipline should not evaporate the moment it becomes
inconvenient, not because a goal is a bound.

**Two further consequences,** both following from "may not name or produce
authoritative checker-input types":

* The host may not construct, wrap, or modify the `CorridorSource` it passes
  through to `PlanInput`. It forwards the map it was given.
* The host may not read a checker verdict and re-plan against its numeric
  content. Reading *that* a proposal was refused is operational; reading *by how
  much* is reading a bound.

### Tier 2.5 closure conditions — the eight, fixed

Tier 2.5 closes when all eight hold, over one production path:

1. The production host depends on `kirra-proposal-context`.
2. It passes symbolic context into a **real** proposal producer.
3. The proposal producer remains World-blind.
4. `kirra-planner` gains no Kirra World dependency.
5. `kirra-sidecars` remains World-free transitively.
6. Fence B stays green with the new production edge.
7. A differential scenario proves World-present vs World-absent changes the
   proposal.
8. The checker-bound evidence and contract identity are **unchanged** across that
   pair — `evidence_digest` and `contract_digest` identical, `proposal_digest`
   different.

Criterion 8 is the one the whole milestone is for, and it is only assertable
because #1423 put the resolved-contract identity on the record. Criteria 7 and 8
must be asserted over the **same** pair of runs; asserted separately, 7 would pass
on a run pair that also moved the bounds.

Which preserves, mechanically rather than as a slogan:

> **Kirra World may change what is proposed. It may not change the inputs from
> which the checker derives what is permitted.**

### Tier 2.5 — **CLOSED, 2026-08-10**

`crates/kirra-mission-orchestrator` is the production host, and all eight
conditions hold.

| # | condition | how it is established |
|---|---|---|
| 1 | host depends on `kirra-proposal-context` | manifest |
| 2 | passes symbolic context into a **real** producer | `plan_for_intent` + `GeometricPlanner`, unmodified |
| 3 | proposal producer remains World-blind | `kirra-planner` reaches `kirra-world*`: **false** |
| 4 | `kirra-planner` gains no world dependency | same, mechanically |
| 5 | `kirra-sidecars` World-free transitively | reaches `kirra-world*`: **false** |
| 6 | Fence B green with the production edge | INTACT, closure 19 of 56 |
| 7 | differential: proposal differs | bit-compared trajectory, world-silent vs world-knowing |
| 8 | checker inputs unchanged across that pair | corridor + objects are the **same borrow** (`ptr::eq`) |

**Criterion 8 came out stronger than specified.** The plan was to digest the
bound-derivation inputs and compare them. That is not needed. The host builds no
`PlanInput` at all — it passes the caller's straight to `plan_for_intent`, which
performs the goal override itself as `PlanInput { goal, ..world.clone() }`, and
that clone copies the `&` fields rather than rebuilding what they point at. Both
runs therefore receive *one* `&dyn CorridorSource` and *one* `&[PerceivedObject]`.

The attribution matters, so it is stated precisely: **the re-borrow is performed
by production planner code, not by the host.** The runs do not merely have equal
checker inputs; there is only ever one of each, `ptr::eq` says so, and no future
edit to the host can replace that without changing `kirra-planner`'s own bridge.
A digest could only have shown the bytes matched.

**The contract half is discharged by construction, and the first draft of the
test got this wrong.** It compared `contract_digest_hex(&contract)` against
itself — true for every possible implementation, therefore proof of nothing, in
the one assertion the milestone exists for. What is actually true is stronger:
`PlanInput` carries no `VehicleKinematicsContract` and the host has no access to
one, so the envelope cannot differ because neither run can reach it. End-to-end,
#1423's digest pins the resolved envelope at the gateway, where it actually
bounds a command.

**One fixture fact worth keeping**, because it looks like a test bug and is not:
`GeometricPlanner` follows the corridor centerline, so two destinations displaced
only *laterally* within one corridor yield a bit-identical trajectory. The first
fixture used a lateral offset and criterion 7 failed — correctly. The docks now
differ longitudinally. A harness that had "fixed" that by loosening the
comparison would have passed while proving less.

Goal 1's non-vacuity condition — *a host whose removal changes observable
proposal behaviour* — is now met: remove the host and both runs plan to the
caller's own goal.

---

## 6. Tier 3 — The query engine

Eight verbs in §14.2; about five exist in partial form.

- [ ] `Resolve` · [ ] `Related` (bounded graph) · [ ] `WhatIsAt` ·
      [ ] `Capabilities` · [ ] `Freshness`

### `KIRRA-WM-QUERY-VOCAB-001` — RULED 2026-08-09

> **Tier 3 implements the existing §14.2 query families. New helper names may
> exist as aliases or domain-specific wrappers, but they do not replace the
> blueprint verbs without a separate ruling.**

The tracked set above stays intact, `Capabilities` and `Freshness` included. A
convenience name (`WhereIs`, `WhatIsHere`, `History`, `ChangedSince`) is a
wrapper over a blueprint verb, never a rename of one — a silent rename makes the
blueprint and the code disagree about what the system offers, and the blueprint
is the authority.

**`Freshness` is BOTH an answer axis and a query family, and they are different
things.** The distinction is worth stating because the duplication reads as an
error until you see it:

* the **axis** is metadata carried on an answer — *is this answer recent enough*;
* the **verb** is a first-class question *about* that metadata — *what is the
  freshness state of this subject*, asked directly rather than inferred from
  some other answer's envelope.

Exactly the shape of provenance: every payload carries provenance, **and**
lineage retrieval is a query about provenance. One is a field, the other is a
question. Neither removes the need for the other. (Lineage *retrieval* is the
Tier 3 half; `Explain` — the rendering that consumes it — is Tier 4 by
`KIRRA-WM-EXPLAIN-TIER-001` below.)
- [x] **`evidence_digest` / `prev_hash` as core types** — **DONE 2026-08-07**,
      `crates/kirra-world/src/evidence.rs`, 11 unit tests + 5 seam tests in the
      store, still zero-dependency.
      Moved here from §7 the same day by `KIRRA-WM-TIER1-DONE-001`: core-crate
      work, listed at the tier that first *requires* it, since rule 1 below
      demands every answer carry a `ProvenanceHandle` and a handle over two bare
      hex strings is what that rule exists to prevent.
      **`EvidenceDigest` admits 64 lower-case hex characters, verbatim.** The
      case rule is the load-bearing one and looks like pedantry until you see
      why: `verify_chain` compares digests as **strings**, so an upper-case value
      is the same *hash* and a different *string*, and a constructor that
      helpfully lower-cased would report intact chains as broken depending on
      which side normalized. Same "validate, never normalize" discipline as
      `reference.rs`, for the same reason — the stored bytes are the evidence,
      and a constructor that improves them is corrupting them. `UppercaseHex` is
      a distinct error from `NonHexCharacter` because *"well-formed, wrong case"*
      and *"not a digest"* send an investigator to different questions.
      **`PrevHash` is an ENUM, not a digest**, because the first record has no
      predecessor: its previous-hash position holds `kirra-world:genesis`, which
      is not a hash and is not hex. A single digest type there would force either
      widening the invariant or **fabricating a digest for genesis** — a
      synthetic value in a hash-chained evidence log, which is much the worse.
      `parse` never falls back to `Genesis` on a corrupt link, since a truncated
      chain whose first surviving row claimed to be the beginning would verify as
      a complete one.
      **The genesis sentinel now has ONE definition.** It was a literal in both
      the core and the store; the store re-exports the core's constant, because
      the value is inside the hashed bytes of every first record and two
      definitions of a frozen constant are two chances to drift.
      **Seam-tested against the real producer** (`kirra-world-store/tests/
      chain_identity_types.rs`): a digest the store actually emitted is
      admissible, every head along a growing chain is admissible and distinct,
      and stored digests really are lower case — so the case rule is tied to a
      producer rather than to an opinion. A type whose admission rule disagreed
      with the thing it types would refuse genuine evidence while looking like a
      tightening.
      **Still open:** the store's own chain path and `ProjectedClaim.chain_digest`
      remain `String`. This types the *concept*, not yet every position that
      holds one — the same staging the trust axes had, core first and the read
      path after. `WorldAnswer::provenance()` returning `EvidenceDigest` is the
      next hop and waits on #1388.

Three rules matter more than the verb count:

1. **No API returns a bare value.** Every answer carries the value, the trust
   axes, the validity at the supplied clock, and a `ProvenanceHandle`. The
   blueprint calls this *"a deliberate ergonomic cost: it makes 'I got a number
   and lost where it came from' impossible to write."*
   **This is a breaking change to the API that exists today**, which returns
   bare `ProjectedClaim`s.

   **Falsified against a real caller, 2026-08-07** (§9): the rule **cannot be
   met by `ProjectedClaim`**, and cannot be met by convention either.
   `ProjectedClaim`'s fields are public, so

   ```rust
   let payload = &store.current("robot-01", now)?[0].payload;
   ```

   compiles — no validity, no trust, no handle. `validity_at` and `grade_at` are
   *methods a caller must remember to call*, and forgetting is the default.

   Not a defect in the store: `ProjectedClaim` is the projection **row**, and
   that is the honest shape for a row. The finding is about *where* the rule has
   to live — at an **answer boundary**, in a type with no constructor that omits
   validity, trust or provenance, so an answer in hand always carries them.

   The honest bound, since overclaiming here would be the same failure: such a
   type closes the hole at *retrieval*. It does not stop a caller destructuring
   and passing the value onward alone. Rust cannot prevent that without
   infecting every downstream signature.
2. **Queries are bounded.** Not a preference: D-9 measured **10.5 s p99**
   temporal queries at 100 000 entities, and ADR-0041 D-12 already records that
   neither graph nor temporal queries may sit on a control or safety deadline
   path, and that an unbounded query has no bounded cost whatever its scaling
   verdict.
3. **`Unknown` is a success.** The error channel is for malformed queries and
   storage faults — never for absence of knowledge. Conflating the two is how
   *"I don't know"* becomes an exception somebody catches and turns into a
   default value.

### `KIRRA-WM-ANSWER-IDENTITY-001` — RULED 2026-08-09

> **Tier 3 answers have no durable stored identity. An `AnswerRef` is a
> reproducible DESCRIPTOR, not a persisted answer row.**

An `AnswerRef` serializes *how to reconstruct the answer* — query kind, query
parameters, `as_known_at`, the requested valid instant, the projection/rule
version set, the snapshot coordinate, and the pagination bound. Resolving a ref
therefore means **re-execute this exact deterministic query against the same
snapshot and return its lineage**, not *fetch the stored answer*.

That resolution is Tier 3's **lineage retrieval** (3f). Tier 4's `Explain` is a
consumer of it, not the thing being defined here — see
`KIRRA-WM-EXPLAIN-TIER-001` below. The ruling constrains what a ref *is*; which
tier renders it is a separate question with a separate answer.

Ruled before the envelope is designed, because the alternative builds a second
store by accident. A durable answer row would need its own retention horizon,
its own compaction story, and its own provenance — recursively, since an
archived answer is evidence about an answer — and it would become a mutable
"current truth" cache that cannot be reconstructed, which §10 already puts out
of scope.

**What the ruling costs, stated because it constrains the API rather than merely
describing it:** every public Tier 3 query must be **fully serializable and
deterministic**. No closures, no caller-supplied predicates, no ad-hoc filters,
and no unversioned sort orders — an unversioned sort makes a pagination cursor
non-reproducible across releases, so the answer re-executes identically while
page 2 does not. **Cursor stability is part of the query contract**, not a
pagination detail.

### The three axes of an answer

Rule 1 above says every answer carries value, trust axes, validity and a
provenance handle. Recorded here, since the shape was nearly got wrong: those
are **three orthogonal axes**, and they must not be folded into one enum.

| Axis | Values | Question |
|---|---|---|
| **payload outcome** | `Located` / `Ambiguous` / `Unknown` / `Refused` | what the answer IS |
| **completeness** | `Full` / `Degraded` | did the evidence SURVIVE |
| **freshness** | `Fresh` / `Stale` / `NotApplicable` | is it RECENT ENOUGH |

An answer can be `Located` **and** `Degraded` **and** `Stale` at once —
`HistoricalAnswer` (2d) already carries the first two separately for exactly this
reason. A single mega-enum forces a caller to discard two of the three facts, and
is the `Option<T>` collapse of §"Why the axes are not one enum" one level up.

**The envelope never restates the payload's outcome.** `Ambiguous` / `Unknown` /
`Refused` belong to `ResolutionOutcome`; if the envelope also carried them, two
values would mean the same thing and would have to be kept in sync. Envelope owns
completeness, freshness, provenance and versions; payload owns the domain answer.

**Open sub-question — is a FOURTH freshness variant (`Unknown`) reachable?** The
table above carries three. `NotApplicable` is clearly reachable: a historical
query at a fixed instant is never stale, by construction. `Unknown` is the
doubtful one — if a recency-sensitive query with no supplied threshold REFUSES
(below), then every answer returned has a threshold and freshness is always
computable, so `Unknown` would arise only from a positive claim with no
timestamp, which should not exist. An unreachable variant invites being returned
as a shrug, which is the `Option::None` failure in miniature. Decide by finding a
reachable case or leaving it out; do not carry it undecided.

### Freshness thresholds are supplied, never defaulted

Freshness is computed at **read time**, like validity (§9 rule 6) — never stored
as `is_stale = false` and trusted later.

The **threshold** is not Tier 3's to invent. A default staleness window inside
the query engine is precisely how an answer becomes a safety input without
anyone deciding it should be, which ADR-0042 Decision 5 (*safety-related,
**non-authoritative***) exists to hold at bay. The precedent is
`KIRRA_VEHICLE_CLASS`: **fail-closed, no default**, because a wrong default
silently selects another class's envelope. So: the threshold comes from the
caller or from a ruled policy table, and a recency-sensitive query with neither
**refuses**.

### Rule / projection versioning — declared and enforced, not derived

A digest says *what state you got*; a version says *which semantics produced it*.
`state_digest_of` already provides the first and cannot provide the second.

The version must be **declared** and must change whenever reducer behaviour
changes. Two mechanisms, because neither alone is enough:

* **Conformance corpus** — fixed event sequences with expected folded outputs. A
  diff here proves *behaviour* moved, which is what justifies a version bump.
* **Source pin** — the frozen-talisman technique (a git blob hash, as
  `validate_vehicle_command` already uses) over the reducer, so a silent edit
  reds CI. Hash the comment-stripped form via `ci/check_orphan_cores.py`'s
  already-unit-tested `strip_noncode`, or ordinary comment churn trips the gate
  and trains reflexive version bumps — which destroys the signal the gate exists
  to give.

The corpus proves meaning; the pin proves nobody edited quietly. A version
checked by neither is decorative metadata.

**Built 2026-08-11**, and one thing this section did not anticipate: neither
mechanism forces the BUMP. Both are satisfied by re-pinning, so a behaviour
change that updates its own declaration keeps its version and stays green. That
needed a third check — a recorded history the declaration is accountable to, so
a version's corpus digest cannot be redefined after the fact. See *"3b closed"*
below.

### `KIRRA-WM-EXPLAIN-TIER-001` — RULED 2026-08-09

> **`Explain` stays at Tier 4. Tier 3 builds only the deterministic lineage
> CONTRACT that Tier 4 consumes.**

The split: Tier 3 owns the typed answer — provenance, completeness, freshness,
versions, a reproducible `AnswerRef`, and **bounded lineage retrieval**. Tier 4
owns `Explain` — derivation-edge traversal and the human-facing rationale.

Ruled this way because §7 records Explain as depending on *"derivation edges
being real structure rather than a JSON array of identifiers."* Folding Explain
into Tier 3 would drag that structural prerequisite in as a **Tier 3 blocker**,
and Tier 3 would stop being closeable on contracts and representative query
families — which is its stated closing condition. Tier 3 closes once answers are
reproducible and carry enough lineage for Tier 4 to explain them later.

Two constraints Tier 3's lineage retrieval carries regardless of where the
rendering lives, because the traversal it performs is the one most likely to
breach rule 2:

* **Bounded and paginated, with truncation visible.** Answer → projection →
  adjudication → observations → source is unbounded exactly where D-9's 10.5 s
  p99 says it must not be. A lineage response that silently stops is worse than
  one that says it stopped.
* **Historically correct.** Lineage for an answer true at *T* traverses the
  evidence visible at *T*, not today's graph. This is 2d's trap
  (`KIRRA-WM-...` box 2d: *"resolve current state and label it historical"*)
  re-appearing one tier up, and it will look like reuse when it arrives.

### The Tier 3 boxes

A continuation of rules 1–3 above, not a replacement: **rule 1 becomes the formal
answer-boundary contract (3a), rule 2 remains the bounded-query rule
(cross-cutting), rule 3 stands verbatim** as the payload axis's `Unknown`.

- [x] **3a — Answer envelope + provenance contract** — **COMPLETE for the
      `current()` / `ask()` answer family, 2026-08-10, with the exclusions
      below.** Not complete for families that do not yet exist; each will have
      to meet this box again on its own terms, and two of the exclusions below
      exist *because* those families are absent rather than because the fields
      are unwanted.
      Payload owns the domain
      outcome; envelope owns completeness, freshness, provenance and versions.
      **This box already has a falsified predecessor, which is why it exists in
      this shape rather than as a fresh idea:** rule 1 was tested against a real
      caller on 2026-08-07 and **cannot be met by `ProjectedClaim`** — its fields
      are public, so `…current("robot-01", now)?[0].payload` compiles with no
      validity, trust or handle, and `validity_at`/`grade_at` are methods a caller
      must remember. The fix is an **answer-boundary type with no constructor
      that omits validity, trust or provenance**. The honest bound stands:
      such a type closes the hole at *retrieval*, not against a caller who
      destructures and passes the value onward alone.

      **Substantially BUILT already — corrected 2026-08-10.** This entry said the
      next hop was *"`WorldAnswer::provenance()` returning `EvidenceDigest`,
      **blocked on #1388**"*. That was true when written and went stale the day
      **#1388 merged (2026-08-07)**, which is the PR that *built* the boundary.
      It was then carried forward into this box on 2026-08-09 without being
      checked, and read by the next reader — twice — as a live blocker. Recorded
      at length because the failure is the one this tier exists to prevent, in
      the document that defines the tier.

      What `kirra-world-service::read_view` ships **today**: `WorldAnswer` with
      `WorldView::ask` as its only constructor, **validity resolved at
      construction** (the whole difference from `ProjectedClaim`), the trust
      **axes** carried beside the collapsed grade, `provenance()` **already
      returning `&EvidenceDigest`**, and `WorldLookup::Unknown` as a success
      variant. Staleness is **already reported, not swallowed**: `is_admissible`
      filters on `Expired` and `Inadmissible` only, so a stale claim is served
      carrying `Validity::Stale` — pinned by test.

      Three things are absent, and they are absent for three different reasons —
      the first must **stay** absent, the second has nothing to carry yet, and
      the third is blocked on a capability:

      * **Completeness is deliberately absent at THIS boundary, and adding it
        would be a defect.** `ask` is built on `WorldStore::current`, whose own
        docs refuse a `TemporalAnswer` because *"compaction can never degrade
        it"* — `compact_range` clamps its window below any generation joined to
        `world_current`, so the events `current` reads are exactly the ones
        compaction is forbidden to remove. A completeness field here is
        structurally always `Full`, and *"would suggest the check is doing
        something"*. It becomes real when the boundary serves a query that **can**
        degrade — `as_of`, `history`, lineage retrieval — none of which exist yet.
      * **Version** — no reducer version exists to carry. Minting one here would
        be the decorative metadata 3b forbids; it lands with 3b's enforcement.
      * **`AnswerRef`** — absent, and blocked on a **capability** rather than on
        effort: `KIRRA-WM-ANSWER-IDENTITY-001` requires re-execution *against the
        same snapshot*, and there is no generation-pinned read of `world_current`
        to re-execute against. `projection_generation()` can report the
        coordinate; nothing can read *at* it.

### `KIRRA-WM-ANSWERREF-NAMING-001` — RULED 2026-08-10

> **The name `AnswerRef` is reserved for the ruled guarantee — re-execution
> against the same snapshot. A weaker capability may be built, but not under
> that name.**

A ref that records the observed projection coordinate and lets a later ask
**detect drift** is genuinely useful, and is *not* snapshot replay. Shipping it
as `AnswerRef` would put the ruled guarantee's name on a type that cannot honour
it — and the cost is paid later and by someone else: a migration spent teaching
callers which flavour of `AnswerRef` they were handed, with no way to tell from
the type.

If the weaker thing is wanted before pinned reads exist, it gets an honestly
weaker name — `ObservedAnswerRef` or `AnswerCheckpoint` — whose contract reads:

> records the query and the observed projection coordinate; re-execution may
> **detect** drift, and does **not** promise snapshot replay.

Recorded as a ruling rather than a preference because the pressure to reuse the
ruled name will come from wanting the checklist to close, which is the same
pressure that produced every stale claim this box already documents.

- [x] **Generation-pinned read (prerequisite for the ruled `AnswerRef`).**
      ✅ **DONE 2026-08-10** — `WorldStore::read_at_generation` /
      `ReadSnapshot::read_at_generation`; see *"the pinned read, and what ends
      one"* below. Scoped separately because it is a **store capability**, not an
      answer-boundary one: a way to read `world_current` *as of* a projection
      generation, so a recorded coordinate can actually be re-executed against.
      `KIRRA-WM-ANSWER-IDENTITY-001` now has a mechanism behind it; the ruled
      `AnswerRef` itself is the next step and is still open.
- [x] **3b — Rule / projection versioning.** ✅ **DONE 2026-08-11** — see *"3b
      closed — the version stopped being a promise"* below. Declared,
      behaviour-changing, and enforced by corpus + source pin (above). Not
      decorative metadata. Four rules declared (three reducers + the answer
      boundary's admissibility rule), each corpus-pinned and source-pinned;
      `ci/check_world_semantics.py` refuses a corpus-digest change at a fixed
      version, which is the check that makes the bump unavoidable and the one a
      Rust test structurally cannot perform. `KIRRA-WM-REDUCER-VERSION-001`.
- [x] **3c — Snapshot consistency.** ✅ **DONE 2026-08-10** — see *"3c closed
      against its consumer"* below. An answer composing several projections
      (identity, claims, relationships, summaries) must read them at ONE coherent
      point, or report each coordinate explicitly. Projections carry independent
      checkpoints and can sit at different heads, so an envelope naming a single
      `as_known_at` over a multi-projection answer is otherwise approximately a
      lie. `IdentityView` already records the single-snapshot argument for one
      walk; this is that argument across projections.
- [~] **3d — Typed query engine.** The RATCHET half is ✅ **DONE 2026-08-10**
      (`ci/check_world_answer_boundary.py`); the typed engine itself is still
      open — see *"3d's ratchet closed ahead of its engine"* below. The only
      supported path for **domain questions**. Operational reads are explicitly carved out — `verify_chain`,
      `schema_version`, backup/export, the retention driver, the compaction
      planner and the WM-2 measurement harness all legitimately read below it,
      and a rule that forbade them would be false on the day it was written.
      Direct domain reads of projection tables below the engine are
      **mechanically gated**, on the `ci/check_reexport_shims.py` zero-tolerance
      ratchet pattern — an invariant with no gate is prose.
- [x] **3e — Freshness.** ✅ **DONE 2026-08-11** — see *"3e closed: Timeless is
      granted, never assumed"* below. Computed at read time; threshold supplied
      by caller or ruled policy; no implicit default for recency-sensitive
      semantics. `KIRRA-WM-FRESHNESS-POLICY-001`. The fourth `Unknown` freshness
      variant is **decided and omitted** — no reachable case exists; see the
      state machine in the closure.
- [x] **3f — Lineage retrieval contract.** ✅ **DONE 2026-08-12** — see *"3f
      closed: lineage is a query family, not a field"* below. Deterministic,
      bounded, paginated, truncation visible, historically correct. Its own
      query family with a `LineageRef` and a one-rule version set
      (`lineage_selection`, `RuleId`'s first non-reducer); compaction degrades
      rather than refusing; `provenance` is carried verbatim, since walking it
      is the Tier 4 structure `KIRRA-WM-EXPLAIN-TIER-001` keeps out.
- [x] **3g — Degradation propagation.** ✅ **DONE 2026-08-11**, follow-up closed
      **2026-08-12** (`history` + `subject_summary`; see *"3g follow-up closed:
      two families, two mechanisms"*) — see *"3g:
      the boundary finally carries completeness"* below. Every answer family preserves
      `Full`/`Degraded` **independently of the payload outcome**, not just
      `subject_summary`. Retention may reduce answer precision; Tier 3 makes the
      loss observable.
- [x] **3h — Historical composition.** ✅ **DONE 2026-08-11** — see *"3h closed:
      the graph as it stood then"* below. Historical queries use historical
      identity (2d) and historical evidence — never today's entity graph applied
      to old evidence. `ReadSnapshot::read_composed_at_generation` reconstructs
      claims AND identity at ONE coordinate with ONE refusal, and `AnswerRef`
      resolves objects through it. Scope bound: this closes the box for the
      **generation-pinned** family (`AnswerRef`); the transaction-time family
      (`ask_as_of`) still reports `NotResolvedInReplay` and is the follow-up.

**Cross-cutting, applying to every box above:**

- [ ] Every interactive query bounded (existing rule 2; D-9, ADR-0041 D-12)
- [ ] Pagination and truncation explicit, and **cursors stable across releases**
      — an unversioned sort re-executes the answer identically while page 2
      differs
- [ ] No dependency path from the query engine to checker or actuation, on the
      `ci/check_mick_actuation_fence.py` fence pattern — this is what makes
      ADR-0042 Decision 5's *non-authoritative* mechanical rather than declared
- [ ] `ChangedSince` takes an **opaque domain cursor**, never the SQLite
      `generation`. The adapter may encode a generation inside it; a caller that
      knows this makes the API un-implementable over another backend, against
      ADR-0040's domain-core/swappable-adapter shape. Same opaque-newtype
      discipline as `reference.rs`.

**Closing condition.** Tier 3 closes on the contracts plus a representative
proving set — not on every conceivable query. The minimum set:

1. current entity query;
2. historical entity query;
3. relationship / history query;
4. degraded / compacted query;
5. lineage-retrieval query;
6. bounded temporal query;
7. **contradicted identity → `Refused` with its reason preserved through the
   envelope** — the case Tier 2 spent three slices establishing, and the one an
   envelope most easily flattens into an empty answer;
8. **discrimination: `Unknown` ≠ `Refused` ≠ empty-but-`Full`** — three distinct
   facts that are one keystroke from collapsing into each other.

New domain queries can then be added without changing the Tier 3 trust model.

### `KIRRA-WM-CLAIM-SHAPES-001` — RULED 2026-08-10

> **An object-bearing claim requires a predicate. `predicate = None,
> object = Some(_)` is invalid and is rejected at admission and by schema
> constraint.**

| `predicate` | `object` | shape |
|---|---|---|
| `None` | `None` | payload-only claim |
| `Some` | `None` | predicate + payload claim |
| `Some` | `Some` | subject–predicate–object + payload claim |
| `None` | `Some` | **INVALID** |

**The evidence is stronger than "this shape feels odd."** `world_current` keys on
`(subject, predicate_key)` where `predicate_key` is the predicate or `''`, so an
object-without-predicate claim occupies the **same slot** as a payload-only claim
about that subject. Measured, not theorised — appending a payload-only claim and
then an object-without-predicate claim for one subject left exactly one row, and
the payload-only claim was gone. The shape is not merely unsupported, it is
**projection-destructive**: the store admitted two semantically distinct claims
the deterministic projection cannot tell apart, and one vanished without a signal.

**Enforced at two layers, because one is not enough.** Admission refuses it with
an error naming the rule; the **v5 trigger** makes SQLite itself refuse, so a raw
`INSERT` cannot route around a polite decoder. Both were mutation-verified:
neutering the trigger fails only the raw-SQL test, and neutering the admission
check fails only the admission and aliasing tests — so neither layer is carrying
the other. (With admission off, the trigger still refuses the row; the *error
type* changes, the *refusal* does not.)

**A trigger rather than a `CHECK`, and the reason is not preference.** SQLite's
`ALTER TABLE` cannot add a constraint to existing columns — only `ADD COLUMN`,
which is how v2 and v3 carried their inline checks. A real `CHECK` would require
the 12-step table rebuild on the hash-chained append-only log, the one table
whose value is that it is never rewritten. A `BEFORE INSERT` trigger enforces the
same rule at the same layer while staying additive.

**Historical rows are reported, never repaired.** A trigger constrains future
inserts and touches nothing already written, so a store carrying the invalid
shape migrates cleanly and keeps it — repairing a hash-chained log is a far
larger decision than a migration is entitled to make.
`WorldStore::invalid_shape_rows` makes such rows a finding rather than a silence.
Nothing in the workspace relied on the invalid shape: every `predicate: None`
construction site was checked and all pair it with `object: None`.

### `KIRRA-WM-CONSUMER-WITNESS-001` — RULED 2026-08-10

> **Every Tier 3 contract change must be exercised by at least one real consumer
> whose behaviour would fail visibly if the contract became semantically empty.**

The word carrying the rule is *semantically*. Unit coverage catches a contract
that returns the **wrong** thing; it does not catch one that returns a
well-formed **nothing**, because a test asserting `Unknown` against an empty
store passes identically whether the query is correct or broken. That is exactly
the shape of the `rebuild_entity_projection` bug found building Tier 2.5 — every
write succeeded, every fold succeeded, and the query returned a perfectly valid
empty answer that was indistinguishable from "the world knows nothing".

**The witness must fail on the POSITIVE arm.** A consumer that merely *reads* an
answer and logs it satisfies the letter of this rule and witnesses nothing — the
same vacuity, one level up. `mission_context` qualifies because a semantically
empty answer collapses run B onto run A and the closure differential goes red.

### Tier 3 audited against its first real consumer — 2026-08-10

Run before implementing further boxes, because *"what would this consumer force
us to get right"* is a different question from *"what does the box say"*, and
where they disagree the consumer holds the evidence. Every claim below was
checked against the code; two of them contradict what a first reading suggested.

**FINDING 1 — the first real consumer does not use the answer boundary, and
that is 3a's exact defect.** `mission_context` calls `WorldStore::current`
directly and reads `ProjectedClaim`'s public `.predicate` / `.object` with no
validity, no trust and no provenance — verbatim the hole 3a records
(`…current(...)?[0].payload` compiling with none of them). It also makes the
consumer a **direct domain read of a projection table**, which 3d says must be
mechanically gated. So the consumer is currently the violation 3d would forbid.

Not because the boundary is wrong: because of a dependency ceiling chosen without
noticing the consequence. `WorldAnswer` lives in `kirra-world-service`, and the
consumer's ceiling was set at `kirra-world` + `kirra-world-store`. That is **not**
a real barrier — `kirra-world-service` is a library crate (`pub mod read_view`)
whose own dependencies are exactly those two, so routing through `WorldView::ask`
leaves the transitive set, and therefore the capability limit, unchanged.

**FINDING 2 — 3e is live, and the default is affirmative rather than absent.**
`current()` filters the validity WINDOW (`holds_at`: not-yet-in-force and
past-`valid_to` are both excluded), so the consumer never reads an expired claim.
But nothing supplies a staleness budget, and `validity_at` with
`staleness_budget_ms: None` returns **`Validity::Timeless`** — not "unknown
freshness" but a positive claim of time-independence. "Where was the package last
seen" is about as recency-sensitive as this domain gets, so a year-old
observation is currently served with the same standing as a fresh one, under a
label asserting that is fine. 3e's *"no implicit default for recency-sensitive
semantics"* has a sharper meaning here than the box implies.

**FINDING 3 — the trust gap is narrower than it first appears.** `world_current`
folds `claim_status = 'confirmed'` only, so an `LlmCandidate` writer — which may
only ever produce `Candidate` — cannot reach this consumer at all. The residual
gap is not exposure to untrusted claims; it is that the consumer never asks for a
grade, and `grade_at` returns `None` for an unlabelled claim rather than a
default. Recorded because the alarming version of this finding is the wrong one.

**FINDING 4 — 3c is not yet exercised, and the trigger is identifiable.** One
projection, one read, so there is no multi-projection coherence question today.
It becomes live the moment the consumer resolves its subject through identity —
`package_17` reached via an alias — because that composes `world_current` with the
entity projection, and the two carry independent checkpoints.

**FINDING 5 — 3g is satisfied vacuously here, for a reason already recorded.**
`current()` cannot degrade (3a's first exclusion: `compact_range` clamps below any
generation joined to `world_current`), so a completeness axis on this path would
be structurally always `Full`. 3g becomes real for this consumer only when it
asks something that can degrade.

**FINDING 6 — the answer boundary cannot express the claim the consumer reads.
The migration was stopped here rather than completed.**

Finding 1's fix looked mechanical: point `mission_context` at `WorldView::ask`.
Checking the mapping before writing it showed it is not.

```rust
// read_view.rs — WorldView::bind
value: claim.payload.clone(),
```

`WorldAnswer` carries `subject` · `predicate` · `value(= payload)` and **drops
`object` entirely**. But `mission_context` matches candidates against the claim's
`object` (`dock_b`), and the store's projection carries `object` and `payload` as
separate columns. The boundary models a **subject–predicate–payload** claim; the
projection stores **subject–predicate–object–payload**.

**What a mechanical translation would have done.** In this consumer's fixture the
payload is `"{}"`, so the rewrite would have compared candidates against `"{}"`,
matched nothing, and collapsed run B onto run A. It would have compiled. Both
negative controls would still have passed — a context-blind producer shows no
difference either way, and an unplaceable destination changes nothing either way.
Only the positive assertion would have failed, which is exactly why
`KIRRA-WM-CONSUMER-WITNESS-001` insists the witness fail on the positive arm.

**So this is a gap in the 3a contract, not in the consumer.** A triple-shaped
fact whose object *names an entity* — `package_17 last_seen_at dock_b` — cannot
be expressed through the answer boundary at all. That is the clearest evidence
yet that the boundary was specified without a consumer: nothing had ever needed
to read an object, so nothing noticed the field was gone.

**The question it forces, which is a contract question rather than a code one:**
is `object` first-class, or is a relationship-to-an-entity meant to live inside
`payload` for the caller to parse? The projection has both columns and the
consumer uses `object`, so the store already answers *first-class* — and a
boundary that drops it is lossy in a way no caller can work around.

**And it pulls 3c forward.** If `object` is first-class and names an entity, then
comparing it as a raw string is the wrong operation — it should resolve through
the 2d identity graph, which composes `world_current` with the entity projection.
Those carry independent checkpoints, so 3c's snapshot-consistency question
arrives with the very first correct implementation of this query, earlier than
finding 4 predicted.

**What the audit changes about the order.** Boxes 3a and 3d have a live consumer
that violates them today; 3e has a live consumer that is silently mislabelled;
and 3a additionally cannot represent the fact that the consumer reads. The revised
sequence, each step forced by the consumer rather than by the checklist:

1. **3a** — ✅ **DONE 2026-08-10.** Make `WorldAnswer` faithfully represent the
   stored claim: add first-class `object`, keep `value()` as `payload`, leave
   subject / predicate / validity / axes / grade / provenance unchanged. Migrate
   `mission_context` onto `WorldView::ask`, **requiring the staleness budget at
   construction** so 3e's no-implicit-default rule arrives with it rather than as
   a later discipline. Keep `NoClaim` and `NoneAdmissible` distinguishable —
   before this, both collapsed to "no preference", losing exactly what Tier 3
   exists to retain. Propagate the categorical grade into `ContextHint` (a grade
   is categorical, not a magnitude, so the symbolic-seam gate is unaffected).
   **Do not** resolve object identity inside `WorldAnswer`. See *"3a closed
   against its consumer"* below.
2. **3c** — ✅ **DONE 2026-08-10.** The composed read now exists:
   `WorldAnswer.object` → identity resolution → one snapshot coordinate. Closed
   on the STRONG arm (one snapshot) rather than the degraded one, and the
   subject is deliberately excluded — see below for both reasons.
3. **3d** — ✅ **RATCHET DONE 2026-08-10.** Direct projection reads by domain
   consumers fail mechanically, with the exact pre-fix `mission_context` pattern
   as the negative fixture. The typed query engine it will eventually guard is
   still to be built; the gate does not wait for it, because the bypass it stops
   is reachable today.

### 3a closed against its consumer — 2026-08-10

Finding 6 said the boundary could not express the claim the consumer reads. It
can now, and the consumer reads through it.

**What shipped.**

| Change | Where |
|---|---|
| `WorldAnswer::object()` — first-class, uninterpreted | `kirra-world-service::read_view` |
| `mission_context` routes through `WorldView::ask` | `kirra-proposal-context` |
| `staleness_budget_ms` is a required parameter | `mission_context`'s signature |
| `WorldSilence::{NoClaim, NoneAdmissible, NoCandidateMatched}` | `kirra-proposal-context` |
| `ContextHint::{FactTrust, FactFreshness}` — categorical | `kirra-proposal-context` |

`value()`, `subject()`, `predicate()`, `validity()`, `axes()`, `grade()` and
`provenance()` are unchanged; the object is added beside them, not in place of
anything.

**The dependency ceiling did not move, and that was the deciding constraint.**
`kirra-world-service`'s own dependencies are exactly `kirra-world` +
`kirra-world-store` — the ceiling finding 1 said was set without noticing the
consequence — so depending on the answer boundary leaves the consumer's
transitive set, and therefore its capability ceiling, byte-identical. Had the
service crate carried anything more, the fix would have been to move
`WorldAnswer`, not to widen the ceiling.

**Three silences instead of one, and why the third exists.** The audit named two.
Building it turned up a third: a claim that *is* admissible and names something
the caller never offered. Flattening that into `NoClaim` would report "the world
has never heard of this" about a subject the world had just answered about —
finding 6's failure mode in miniature, so it gets its own variant.

**The freshness budget is required at the signature.** `Validity::Timeless` is
what the world returns when no budget is supplied, and it is a *positive claim
that age does not matter*. For `last_seen_at` that claim is false. 3e's
no-implicit-default rule is therefore enforced by the type system here rather
than by discipline: `None` is still expressible and now means *I considered this
and the fact is genuinely timeless*, which is a different act from never having
been asked. The verdict crosses the seam as `FactValidity::Timeless` so a
consumer can notice.

**What is deliberately NOT done.** `object()` returns the stored string,
unresolved. Resolving it through the 2d identity graph composes two projections
with independent checkpoints — 3c's question — and an answer boundary that did it
silently would be answering a snapshot-consistency question by accident. The
doc comment says so at the accessor, so the next reader finds the reason where
they find the limitation.

**Non-vacuity.** Three mutations were run against the new tests and each is
caught: mapping `NoneAdmissible → NoClaim` (the collapse the box exists to
prevent), mapping `Timeless → Fresh` (the silent-default 3e forbids), and
matching candidates against `value()` instead of `object()` — the exact
mechanical translation finding 6 predicted would compile, match nothing, and pass
both negative controls. `NoneAdmissible` itself is unreachable through the
sanctioned write path (pinned by `inadmissible_never_read.rs`), so its test
plants the state by writing `world_current` directly; what that proves is that
the *mapping* does not collapse, not that the store can be made to serve a
rejected claim.

**An expired fact is refused, not labelled** — caught in review. The first draft
folded `Validity::Expired` into `FactValidity::Timeless` while its own comment
said doing so would be a lie. It would have been: `Timeless` is a positive claim
that age does not matter, which is the strongest possible misstatement about a
fact that has run out. There is no honest symbol for an expired fact, so
`FactValidity` has no variant for one and `mission_context` returns
`NoneAdmissible` instead of describing it. Same for a claim graded
`Inadmissible`. Both arms are unreachable today — `WorldView::is_admissible`
filters them first — and both are kept for the reason `UnknownReason::
NoneAdmissible` is kept: the guarantee is emergent rather than a stated
contract, and *"cannot happen"* is exactly when a mapping gets written
carelessly. Neither can be exercised end-to-end, and no test pretends to.

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
      **Superseded in part:** ADR-0040 promoted this to a **Tier 1 exit
      criterion** (§4), so it no longer waits on that precondition, and its
      *deciding* half landed 2026-08-06 as `kirra_world::retention`. This entry
      now covers only the scheduled driver, tracked at §4.
- [ ] `kirra-world-service` as real CQRS — 9 commands, 8 queries, 10 emitted
      events — still inside Fence A
- [ ] Operator teaching surface (§17): `AssertEntity`, corrections

---

## 9. Two sequencing calls

**Wire a small consumer EARLY — before Tier 3, not after.**

Everything built so far is built for nobody: no planner, perception or LLM crate
depends on `kirra-world*`, and **nothing depends on the service crate** either.
The "no bare values" rule and the shape of the trust axes are exactly the
decisions a real caller will falsify, and discovering that across eight verbs
costs far more than discovering it against one.

*Corrected 2026-08-09.* The sentence above read *"and the service crate is
deliberately empty"*, which is no longer true and would send the next reader
looking for an empty crate: `kirra-world-service` carries a populated
`read_view.rs` with its own test suite. Counts are deliberately not quoted here
— a correction that pins exact line numbers goes stale by the same mechanism it
is fixing, and this note exists because that already happened once.

The claim that survives is the one that matters, and it is about **callers, not
contents** — the crate has none, so it reproduces the built-for-nobody problem
one level up rather than solving it. See §5.5.

There are **no callers today**, so the breaking change is free *now* and never
again.

### Attempted 2026-08-07 — and it has nowhere to land

Recorded because the call above reads as straightforward advice and **is not**.
Anyone who acts on it next will otherwise rediscover this after writing the same
code.

A consumer was built and wired into `kirra-sidecars` — a doer-side crate,
outside Fence B's safety closure, and cleared against
`ci/check_mick_actuation_fence.py` before the dependency was added. **Fence B
refused it anyway**, on a check the closure walk does not cover:

> `impl CorridorSource for ReqCorridor` is Kirra World-derived: the implementing
> crate `kirra-sidecars` depends on a `kirra-world*` package.
> **This route requires a superseding ADR, not an allowlist entry.**

The refusal is correct. A corridor is *authoritative to the checker*, so a crate
that produces one must derive it from the safety path's own inputs and never
from accumulated semantic belief — which is the hidden-adapter route ADR-0042
Decision 5 exists to close.

**The constraint §9 does not mention:** seven workspace crates implement
`CorridorSource`. Three — `kirra-core`, `kirra-trajectory`, `kirra-ros2-adapter`
— are safety-closure members and were already barred from depending on Kirra
World at all. The other four are `kirra-map`, `kirra-planner`, `kirra-taj` and
`kirra-sidecars`: **exactly the "planner, perception" crates this section
nominates.** It is transitive, so anything `kirra-sidecars` depends on is barred
too — which takes `kirra-mick`, the "LLM crate" of the same list, with it.

So the consumer needs a host that consumes world knowledge and **never feeds the
checker**. No such crate exists today, and the tier plan does not provide for
one. That is an open placement decision, not a task.

**What the attempt produced anyway**, because the falsification §9 predicted did
happen — twice:

* **Tier 3's rule 1 cannot be met by `ProjectedClaim`.** Recorded at the rule
  itself, in §6 (Tier 3) above.
* **An emergent guarantee, now pinned.** Nothing a reader can see through
  `current()` is ever graded `Inadmissible` — it composes out of three
  mechanisms added for unrelated reasons, was written down nowhere, and would
  have been lost silently by a change to any one of them. Pinned in
  `crates/kirra-world-store/tests/inadmissible_never_read.rs`.

Both were found by a caller that never shipped. That is §9's argument working,
not failing.

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

### Finding — `parko-kirra` is a checker that the dev-edge exclusion treats as a doer

Surfaced by the Tier 2.5 placement survey and recorded **here rather than in
§5.5**, because it is not Tier 2.5's to fix and folding it in would make that
milestone's acceptance hostage to an unrelated question.

**First, the mechanism that is *not* at issue.** The fence enumerates manifests by
`rglob` from the repo root, so it sees `parko/`'s crates despite `parko/` being a
separate Cargo workspace — and `parko-core` is already **inside** Fence B's
closure, reached by a normal dependency edge from the root crate. "A second
workspace is invisible to the walk" would be a tidy explanation and it is false.

The actual mechanism is the closure walk's **deliberate exclusion of
dev-dependency edges**, whose stated rationale is load-bearing and, for its
intended targets, correct:

> the safety roots dev-depend on the DOER crates for their test harnesses. Those
> crates are the ones that SHOULD one day depend on Kirra World — Occy consuming
> semantic knowledge to generate a proposal is the intended design, not a breach.
> Counting dev edges would drag them inside the safety closure and make the fence
> fire on the architecture working as specified.

That reasoning holds exactly for `kirra-planner`, `kirra-taj`, `kirra-sidecars`
and `kirra-mick`. It names **`parko-kirra` alongside them, and `parko-kirra` is
not a doer.** It hosts `KirraGovernor::apply_mrc_profile`
(`parko/crates/parko-kirra/src/lib.rs:682`) — one of the four enforcement points
of the Degraded decel-to-stop-and-HOLD envelope, and the one that additionally
gates an independent angular-velocity channel through `STOP_EPSILON_RAD_S`
(`:114`). A world edge added there would be a *checker* edge admitted by an
exclusion justified for *doers*.

`parko-kirra` misses the other route too, for an unrelated reason: it is not one
of the 10 `SAFETY_ROOTS`, so it is never a closure root either. Two independent
mechanisms, two different reasons, same blind spot. `parko-ros2` — which runs
`run_pipeline_tick`, where governor divergence escalates the effective posture —
is outside for a third reason: nothing in this workspace depends on it at all.

**No world edge exists at any of these today and none is proposed.** The finding
is about what the gate would fail to notice if one were added, which is the only
kind of gap worth recording about a gate.

Three ways to close it, none obviously right, which is why this is a finding and
not a task:

1. **Add `parko-kirra` to `SAFETY_ROOTS`.** Most direct: a root is walked
   regardless of how it is reached, so this closes the gap without touching the
   dev-edge policy at all. Cost: `parko-kirra`'s own dependency closure joins the
   safety closure, and that set has not been reviewed for this purpose.
2. **Replace the blanket dev-edge exclusion with an explicit doer allowlist.**
   Turns "dev edges don't count" into "these four named doer crates don't count",
   so a *new* dev-depended crate is caught by default rather than admitted by
   default. This is the fail-closed shape; it costs a list that must be
   maintained, which is the thing the manifest-computed closure exists to avoid.
3. **Record the boundary as an assumption of use** and gate it socially. Cheapest,
   and the weakest — this section's own argument is that a gate can refuse a
   dependency but not an argument.

**Owner: an ADR, not a tier box.** Option 2 is the one that generalizes, but it
reopens how `parko/`'s separateness is meant to be understood — an independent
product, or an implementation split of one — and that question predates Kirra
World.

---

## 12. What this document is not

It is not a plan, a schedule, or an estimate. There are no dates and no effort
figures, because none could be defended — the tiers are ordered by dependency,
not by duration.

It is not a ruling. Tier 0 is where rulings live, and four of them are still
open.


### 3c closed against its consumer — 2026-08-10

The audit predicted 3c would arrive with the first *correct* implementation of
`mission_context`'s query, and it did: once `object` is first-class and names an
entity, comparing it as a raw string is the wrong operation.

**Closed on the strong arm.** The box allows *"one coherent point, OR report each
coordinate explicitly"*. The second arm was available cheaply and is strictly
weaker — it turns a concurrent fold into a refused answer where the first turns
it into a correct one. Every projection is a table in ONE SQLite database and
every fold commits atomically, so a single read transaction sees the same set of
commits for all of them. `WorldStore::read_snapshot` is that transaction;
coherence is by construction, not drift detection after the fact.

| Change | Where |
|---|---|
| `ReadSnapshot` — one read transaction over every projection | `kirra-world-store::snapshot` |
| `SnapshotCoordinate` / `ProjectionCoordinate` — generation + state digest | `kirra-world-store::snapshot` |
| `ReadSnapshot::identity_is_current` — is identity behind the LOG? | `kirra-world-store::snapshot` |
| `ObjectIdentity` + `WorldAnswer::object_identity` | `kirra-world-service::read_view` |
| `ComposedLookup` — the answer and its coordinate, inseparable | `kirra-world-service::read_view` |
| `WorldSilence::ObjectUnresolved(ObjectResolution)` | `kirra-proposal-context` |

**Two traps found in the machinery, both load-bearing, neither guessed.**

1. **The generations are not comparable across projections.** `world_current`
   and `subject_summary` advance their checkpoint past every event *considered*;
   the entity fold advances only to the last *adjudication* it folded. Append one
   ordinary claim and the checkpoints separate, with both folds complete and
   nothing wrong. The intuitive way to "prove" snapshot consistency — assert the
   coordinates are equal — would therefore report constant false drift on a
   healthy store, and the fix someone reaches for under deadline is to delete the
   check. `the_two_projection_generations_differ_on_a_healthy_store` pins it as a
   fact so that tightening gets a red test naming the reason.
2. **`identity_view` mislabelled its own snapshot.** It read the rows and then
   the generation in two unrelated statements, so a fold landing between them
   stamped the view with a generation newer than the rows it held. It now takes
   both from one snapshot.

**The staleness check is against the LOG, not the projection — and the first
draft had it wrong in both directions.** Gating on *"has the entity projection
been folded"* refuses every object-bearing claim on a store that simply has no
adjudications (most stores, and an availability failure bought for no safety),
**and admits the genuinely dangerous case** — a projection folded once with
merges recorded since is not unfolded, so it would have resolved against
known-stale data and called it success. The Tier 2.5 closure differential caught
the first half by going red; the second half was found while fixing it.
`identity_is_current` asks whether identity has consumed every adjudication the
log holds, which is the question that separates the two.

**The SUBJECT is deliberately not resolved, and this is a limitation rather than
an oversight.** `world_current` is keyed by the subject string *as written*, so
rewriting a queried alias to its canonical id would look up a key nothing was
ever stored under and return **fewer** claims than asking plainly. Reading the
whole equivalence class and merging it is the operation that would be correct,
and it is a query design of its own. Resolving the subject "for symmetry" would
have been a regression wearing the box's name.

**What this is NOT.** `SnapshotCoordinate` is not an `AnswerRef`. A snapshot is
coherent for as long as it is held and cannot be re-entered once dropped;
re-executing against a *recorded* coordinate still needs a generation-pinned read
of `world_current`, which still does not exist and still has its own open box
above. `KIRRA-WM-ANSWERREF-NAMING-001` reserves the name for the day that closes,
and this deliberately does not take it.

**The seam stayed symbolic, and it cost a mirror.** The world's `ObjectIdentity`
carries `Resolved { hops: usize }`, and its refusal reasons carry
`TraversalBudgetExceeded { limit: usize }`. Re-exporting it into
`kirra-proposal-context` would have put primitive numerics on the seam and given
it somewhere to put a number — the one thing `KIRRA-WM-SYMBOLIC-SEAM-001` exists
to prevent. `ObjectResolution` mirrors it symbolically on the `FactGrade`
precedent, with a lock-step test walking the real `RefusalReason` enum so the
tags stay total and pairwise distinct.

**Non-vacuity.** Four mutations were run and each is caught by exactly one test,
leaving the others green:

| Mutation | Caught by |
|---|---|
| fall back to the raw object when the graph is stale | `a_stale_identity_graph_refuses_…` |
| ignore resolution, match the stored object | `a_merged_object_resolves_to_the_candidate_it_became` |
| assume identity is always current | `a_stale_identity_graph_refuses_…` |
| two refusal reasons share one tag | `every_refusal_reason_has_its_own_tag` |

The stale-graph test is the sharp one: the literal match would **succeed** there,
so a refusal that only fired when the literal match also failed would be
indistinguishable from doing nothing.

The store-level guarantee carries a permanent negative control rather than a
one-off mutation — `the_unguarded_composition_observes_the_fold_it_should_not`
runs the identical interleaving through the ordinary `&self` readers and asserts
it DOES observe the concurrent fold. If that control ever goes
green-by-agreement, the snapshot test has stopped being asked a real question.


### 3d's ratchet closed ahead of its engine — 2026-08-10

Box 3d has two halves: a typed query engine, and a mechanical gate making it the
only path for domain questions. **The gate shipped first, deliberately.** The
engine is a design still to be done; the bypass it will guard is one autocomplete
away right now, and 3a's repair is worth exactly as much as the thing stopping it
from being undone.

`ci/check_world_answer_boundary.py` + `ci/world_answer_boundary_baseline.json`,
wired into the guardrails job with its self-test.

**The rule.** A crate that asks the world DOMAIN questions goes through
`kirra_world_service::WorldView`. It may not call the projection-read API on
`WorldStore` (`current`, `current_all`, `as_of`, `history`, `candidates`,
`read_snapshot`, `identity_view*`, `resolve_at`, `load_entity_projection`).

**The negative fixture is the code that shipped**, quoted verbatim from
`0ad203ee` rather than approximated — `store.current(..)` followed by
`c.predicate.as_deref()` / `c.object.as_deref()`. A gate whose fixture is a
synthetic `store.current()` proves it can find a string, not that it would have
found *this* bug. Same discipline as the symbolic-seam gate's control 3, and it
gives the gate a provenance story: this bypass existed, was repaired, and cannot
return. The self-test also pins the *positive* case — the repaired function still
NAMES `&WorldStore` and passes it to `WorldView::new`, and a gate that flagged
that would be unusable.

**Scope is self-maintaining, which is the part that matters in six months.**
Every crate whose `src/` can reach `kirra-world-store` — a normal or build
dependency, parsed from the manifest — is checked; permitted readers are named in
the baseline with a reason each. So a NEW domain consumer is covered from its
first commit, without anyone remembering to add it: undoing 3a by writing a fresh
consumer is the same regression as undoing it in place, and a hand-maintained
list of watched crates would have missed it. Crates that cannot make the call are
not scanned, since doing so could only manufacture false positives from unrelated
`history()` / `candidates()` methods.

**Three scope corrections came out of review, and they were all the same
mistake** — deciding scope from text the gate did not understand. The first draft
substring-matched the manifest, so `wm2-persistence-harness` was pulled in on a
COMMENT saying it deliberately does not take the dependency, and then carried a
written justification for an exemption it never needed. `kirra-world-store` was
listed too, though it cannot depend on itself and is therefore never scanned. And
`kirra-mission-orchestrator` reaches the store only as a DEV-dependency, which
`src/` cannot use — its own module docs say it does not depend on `kirra-world*`
at all. The manifest is now parsed, and the baseline records all three under
`not_listed_because_never_in_scope` rather than pretending they were decisions.

**The gate now fails on a stale exemption**, which is the general form of that
mistake. An exemption for a crate the gate never scans is a written
justification for a decision nobody is making, and that is how a carve-out list
rots: every entry looks reasoned, and nothing checks whether it is still
load-bearing. Both stale entries above were found by this check on its first run,
against the baseline I had just written.

**Both call syntaxes are detected.** `.current(..)` is how anyone would write it,
but `WorldStore::current(store, ..)` is the same call in UFCS form — also caught,
along with the aliased and fully-qualified spellings. A gate advertising zero
tolerance while leaving a syntactic door open is the exact overclaim it exists to
prevent elsewhere.

**The operational carve-out needs no exception list.** WM_SCOPE names
`verify_chain`, `schema_version`, backup/export, the retention driver, the
compaction planner and the WM-2 harness as legitimate readers below the engine,
and says a rule forbidding them "would be false on the day it was written". None
of them is in the domain-read method list, so none is ever matched. The carve-out
is achieved by scoping the *symbols* rather than exempting *callers*, so there is
nothing to keep in sync as those subsystems grow.

**`src/` only, and the reason is not laziness.** Several tests read rows directly
on purpose — `answer_boundary.rs` plants `world_current` rows with raw SQL to
reach a state the sanctioned write path cannot produce, which is how the
fail-closed behaviour is proven at all. Gating tests would block the tests that
prove the boundary works. The bypass lived in `src/`, and `src/` cannot call test
code, so gating `src/` closes the production path completely.

**Non-vacuity, end to end rather than in a string.** Reintroducing
`store.current(subject.as_str(), now_ms)?` into the live
`kirra-proposal-context/src/lib.rs` reds the gate with the file, line and rule;
removing it greens — in both the method-call and the UFCS spelling. And the
anti-neutering cases are pinned: moving `kirra-proposal-context` onto the
permitted list fails `t08_the_gate_watches_the_repaired_consumer`; a baseline
where *every* dependant is permitted fails with *"no crate is in scope — that is
how a ratchet dies quietly"*; and re-adding a stale exemption fails the gate
itself. Twelve self-test cases, with the same uncollected-case guard the
symbolic-seam suite carries.


### The pinned read, and what ends one — 2026-08-10

`KIRRA-WM-ANSWER-IDENTITY-001` rules that resolving an `AnswerRef` means
*"re-execute this exact deterministic query against the same snapshot"*. Until
now that was a ruling with no mechanism: `projection_generation()` could report
the coordinate, and nothing could read AT it.

`ReadSnapshot::read_at_generation(g)` reconstructs `world_current` as it stood at
generation `g`, by replaying the confirmed log through `projection::fold_all` —
the SAME reducer the live fold uses, over the same confirmed-only filter, in the
same order. It is not a second implementation of the projection that could drift
from the first; it is the one implementation given a bounded input, which is the
property `rebuild_from_zero_equals_incremental` already pins.

**It fails closed and never falls forward.** `PinnedRead` is
`Reproduced | Irreproducible`, so there is no code path that answers with current
state when the requested generation cannot be rebuilt. That failure mode is the
one worth naming: falling forward is not merely wrong, it is wrong in the way
that looks right — the caller asked what was true at generation 40 and receives
what is true at 90, with nothing in the value to say so.

**Generation is the right axis to pin on, and this is the one place the two axes
genuinely differ.** `identity_degradation` records that a transaction-time filter
over citations was FAIL-OPEN and could not be repaired: the removed rows are the
only record of their own `txn_time_ms`, so a compacted span can never be shown
irrelevant to a past instant after the fact. Generation does not have that
problem. A `Citation` records the exact `lo_generation..=hi_generation` it
removed — the same axis being pinned — so *"did compaction take anything at or
below g"* is an EXACT test rather than a necessary condition.

**Compaction is what ends a pinned read's life, and it ends it for every
generation at or above the removed span, not merely inside it.** Rebuilding at
`g` folds every confirmed event `<= g`; if one is gone, the fold cannot be
reproduced whatever its result would have been. That is deliberately stricter
than necessary, on the asymmetry `Resolution` already documents: a removed event
may well have been superseded and made no difference, but it cannot be *shown* to
have made none. Over-refusing costs availability; under-refusing returns a
silently wrong reconstruction wearing the word "pinned".

**`KIRRA-WM-REPRODUCIBILITY-HORIZON-001` (recorded here, to be carried into the
`AnswerRef` contract):**

> **Retention policy sets the historical reproducibility horizon for durable
> answer references.** An `AnswerRef` is only as durable as the oldest generation
> still reproducible from retained evidence and citations. "Durable reference"
> must never be read as "forever replayable".

That is a real operational consequence of the retention horizon and it was not
visible before this existed: whoever sets a compaction policy is also setting how
far back a recorded answer can be resolved. It belongs ON the ref's contract
rather than in a footnote here, because the failure it prevents is someone
storing refs as an audit artifact and discovering years later that the horizon
swallowed them.

The precise shape: a span removed BELOW a pinned generation ends its
reproducibility, because the pin folds that prefix. A span removed entirely ABOVE
it does not, because the prefix is untouched — both directions are pinned by
tests, and the pair is what makes `lo_generation <= g` a decision rather than an
accident.

**A negative generation is an error, not an outcome.** Rule 3's split:
`Irreproducible` reports facts about the DATA — this was compacted, this has not
happened yet — and a negative generation is neither. Generation `0` is legal and
reconstructs the empty projection that preceded every event.

**Non-vacuity.** Four mutations, each caught:

| Mutation | Caught by |
|---|---|
| fall forward to current state when compacted | 3 compaction tests |
| clamp a future generation to the head | `a_generation_ahead_of_the_head_refuses` |
| read the live table instead of replaying | 5 tests, incl. the positive witness |
| test span containment instead of overlap | both compaction tests |
| **refuse on ANY citation, ignoring `lo_generation`** | `compaction_above_the_pinned_generation_leaves_it_reproducible` |

**The last row is there because the suite was vacuous without it, and the gap was
found in review rather than by writing it.** Every other compaction fixture
removes a span BELOW the pin, so an implementation that refused on the mere
existence of any citation passed all eight original tests. The missing control is
the mirror case — a span removed entirely ABOVE the pin leaves the folded prefix
intact, so the reconstruction is exact and refusing would be over-refusal rather
than fail-closed. Without it, `lo_generation <= g` was an accident the tests could
not tell from a blanket refusal, and the pinned read would have been useless in
the regime it is most needed: an old, heavily-compacted store where the
interesting coordinates are precisely those below the compaction floor.

The compaction fixture is built so that falling forward returns a *plausible,
non-empty, wrong* answer — `dock_a` where `dock_old` was the truth — because a
refusal that only fired when the fallback was empty would be indistinguishable
from doing nothing. `re_execution_at_the_same_generation_is_stable_across_later_writes`
pins the determinism the ruled `AnswerRef` will rest on, with a guard proving the
head really moved.

`read_at_generation` was added to the 3d answer-boundary gate's method list in
the same change, so the new capability cannot become a fresh bypass.


### The ruled `AnswerRef` — 2026-08-10

`KIRRA-WM-ANSWERREF-NAMING-001` reserved this name for a descriptor that
re-executes against the same snapshot, and forbade putting it on a drift
detector. **The name is taken now**, because the mechanism exists.

`AnswerRef` carries query kind, parameters, pinned generation and rule version —
everything needed to re-execute, and nothing that is the answer. `resolve()`
returns `Resolved | Irreproducible | VersionMismatch`, with **no silent fallback
to current state**; that absence is the contract.

**The version check runs before the store is touched.** Re-executing under new
semantics and then noticing is not a check: the answer would already have been
computed under rules the ref never described. A version mismatch is the subtler
half of falling forward — the coordinate is honoured and the SEMANTICS are
silently swapped, so the answer looks right and describes a query nobody asked.

**`RULE_VERSION` is pinned to behaviour, and the first pin measured the wrong
thing.** A hand-bumped constant is what 3b calls decorative metadata, so a corpus
test anchors it. The first draft digested `projection_state_digest()` — the LIVE
projection — and was insensitive to the rule a ref actually uses. The reason is a
real property of this store, now recorded: **supersession has two
implementations.** The incremental fold does it in SQL
(`WHERE (excluded.valid_from_ms, excluded.generation) > (world_current…)`), while
`projection::supersedes` / `fold_step` is the pure reducer used by
`rebuild_projections` and by the pinned replay. `rebuild_from_zero_equals_incremental`
holds them equal — but a corpus digesting the live table measured the SQL, so
mutating `fold_step` left it green while changing what every ref resolves to.
The pin is now over the resolved ANSWER, which covers the fold rule (through the
replay) and admissibility (through the binding), and fails on either.

> **Superseded 2026-08-11 by box 3b.** `RULE_VERSION` is gone; a ref carries a
> `SemanticVersions` SET naming each rule it depends on, and the refusal reports
> which one moved. The end-to-end pin described above survives — it is the only
> check covering the SQLite replay path between the per-rule corpora — but it is
> now one of several. See *"3b closed"* at the end of this document.

**What a resolved ref deliberately does NOT do.** It does not resolve object
identity: identity is a second projection with its own coordinate, the pinned
read exists only for `world_current`, and `identity_view_at` cuts on transaction
time — so resolving here would pair a generation-pinned claim with a
transaction-time-pinned identity, mixing the axes box 3c closed. Pinned answers
carry `ObjectIdentity::NotResolvedInReplay`, which `matchable` refuses, so a pinned
answer cannot shape a proposal by accident. A generation-pinned IDENTITY read is
the natural next prerequisite if refs ever need resolved objects.

**One construction funnel, restored rather than re-worded.** Adding a second
producer of `WorldAnswer` made two claims stale at once — `bind`'s *"the one
place a `WorldAnswer` is built"* and the module's *"`ask` is the only way to
obtain one"*. Caught in review. The fix is a private `assemble` that both `bind`
and `bind_pinned` delegate to, so the property is true again instead of the claim
being weakened to match: rule 1 says every answer carries value, axes, validity
and provenance, and a second hand-written construction site is how one of those
quietly stops being populated. Verified load-bearing — hard-coding one field
inside `assemble` reds three tests.

**Non-vacuity.** The acceptance set is the eight cases agreed for this step, plus
mutations: ignoring the version check, falling forward when irreproducible,
dropping the generation from ref identity, and two fold-rule changes — each
caught, the last two only after the corpus was re-anchored.


### 3g: the boundary finally carries completeness — 2026-08-11

**What was already true, and what was missing.** The STORE decides `Full` vs
`Degraded` correctly, on both temporal axes, and `degraded_answers.rs` already
pins an `as_of` pair that cannot pass by always answering `Full`. Building that
again here would have been duplication wearing a closed box — and closing 3g on
it would have been the "looks green, proves nothing" outcome the box is most
prone to.

The missing half was **propagation**. `WorldLookup` is `{Answered, Unknown}` and
carried no completeness at all. `ask` reads `world_current`, which `compact_range`
structurally protects by refusing to remove a live projection head — so its
completeness is `Full` by construction and could never fail, which is exactly why
`current()` cannot prove this box. `WorldView::ask_as_of` is the first boundary
query that can genuinely degrade; `TemporalLookup` is the first boundary type
that carries the verdict.

**The property, as ruled:**

> If retained evidence is sufficient for the query, completeness is `Full`. If
> the query depends on evidence removed by compaction, completeness is
> `Degraded`. Tier 3 may over-report degradation, but it must **never** report
> `Full` after relevant evidence has been lost.

**Both arms answer, and the degraded one answers WRONG.** The fixture keeps a
surviving observation either side of a compacted middle, so at `T0+150` the truth
was `dock_beta` — deleted — and the replay returns `dock_alpha`: non-empty,
well-formed, believable. Same payload as the `Full` arm, opposite verdict. That
is what *"independently of the payload outcome"* means, and it is why a pair
distinguishing an answer from silence would measure emptiness rather than
completeness.

**Completeness rides on `Unknown` too.** The tempting shape attaches a resolution
only to `Answered`. Then *"nothing was known"* and *"we deleted it"* become the
same value — the confusion an incident reconstruction can least afford. Pinned by
its own test, and by a mutation.

**The verdict is propagated, not recomputed.** A second judgement at the boundary
would be a second implementation of the rule deciding whether an answer can be
trusted, and the two would drift — precisely what the `AnswerRef` corpus found
when supersession turned out to have two implementations. A test pins boundary
and store equal on both arms.

**This suite is deliberately STRICTER than the contract, in one direction, and
that was measured rather than assumed.**

| Mutation | Result |
|---|---|
| force `Full` in the degraded arm | 3 tests fail — the load-bearing direction |
| drop completeness on `Unknown` | the independence test fails |
| force `Degraded` everywhere | **the Full arms fail** |

The third matters. `Resolution` PERMITS over-reporting, so a move that way is
legal and these tests would red anyway: they pin current behaviour, not the
contract's outer bound. That is the right trade rather than an oversight —
without asserting `Full`, an implementation answering `Degraded` unconditionally
would pass everything else and the degraded arm would prove nothing. If a future
change legitimately makes an arm `Degraded`, update the test *with reasoning*;
do not relax the degraded arm to match. **Only one direction is a bug: `Full`
after evidence was lost.**

**Scope, stated because it is narrower than the box's wording.** `ask_as_of` is
one family. 3g says *every* family; the ones that exist at the boundary are `ask`
(structurally `Full`, and now knowably so) and this. `history` and
`subject_summary` were unpropagated at the time of writing; the follow-up below
closed both. A replayed answer also does not resolve object identity —
`ObjectIdentity::NotResolvedInReplay`, renamed from `NotResolvedAtPin` since it
now covers both replay families. Here the axes would actually AGREE (both this
query and `identity_view_at` cut on transaction time), so that is a scope
decision rather than an impossibility, unlike the generation pin.

### 3b closed — the version stopped being a promise — 2026-08-11

**`KIRRA-WM-REDUCER-VERSION-001` — RULED 2026-08-11**

> **A reducer version changes whenever the reducer's behaviour changes in a way
> that can alter a derived answer. A pinned answer reference refuses replay
> under a different semantic version rather than replaying under the new one.**

The second sentence is what gives the first one teeth. A version nothing
consumes is a comment.

#### What existed before, and why it was not 3b

The `AnswerRef` shipped on 2026-08-10 carried `RULE_VERSION: u32 = 1` — one
number, hand-bumped, pinned by one corpus. That was real: it refused on a
mismatch, and its corpus was anchored to the resolved answer rather than to the
live projection (a distinction that had already caught one vacuous draft). But
it fell short of this box in three specific ways, and naming them is the
difference between closing 3b and declaring it closed:

1. **It covered two of four rules.** The identity fold and the subject-summary
   fold had no declared version at all. A change to either moved what the store
   answers with nothing recording that it had.
2. **It could not say what moved.** `VersionMismatch { recorded, current }` told
   an operator the rules had changed, not *which* rule — which is the only part
   they can act on.
3. **Nothing forced the bump.** The corpus test asserted `RULE_VERSION ==
   PINNED_FOR_VERSION` against a local constant, so re-pinning the expected
   answer and leaving both alone was green. That is precisely the decorative
   metadata this section warns about, in the code that quoted the warning.

#### What is there now

Four rules are declared — three reducers in `kirra_world_store::semantics`
(`world_current_fold`, `entity_fold`, `subject_summary_fold`) and one at the
answer boundary in `kirra_world_service::semantics` (`answer_admissibility`).
Each carries a version, a conformance-corpus digest, and a source pin over a
marker-delimited span. Both crates declare in the same shape and are read by one
parser (`ci/check_world_semantics.py`) against one recorded history
(`ci/world_semantics_baseline.json`), because a boundary rule checked by a
second, differently-shaped gate is a boundary rule checked more weakly.

A query family carries the **set** of versions it depends on, not a composite
number. `SemanticVersions::for_query(CurrentSubject)` names two rules and
deliberately excludes the other two: `entity_fold` cannot reach a pinned ref's
answer (it reports `NotResolvedInReplay`), and `subject_summary_fold` is a
different family. Including them would refuse recorded references for rules they
never consulted, and a refusal that fires constantly is one people route around.
The exclusion of `entity_fold` is a claim about *today's* code, asserted by test
— box 3h's identity-resolving ref will put it in the set.

> **It did, the next day.** 3h made refs compose identity at the pin, the
> two-rule assertion went red, and the set became three. Left standing rather
> than rewritten because the prediction and its resolution are the evidence that
> this set is maintained by tests rather than by hand — see *"3h closed"* below.

#### The three checks, and which hole each closes

| Check | Where | Catches |
|---|---|---|
| corpus digest == declared | `tests/semantics_corpus.rs`, `tests/boundary_semantics.rs` | behaviour moved and the declaration did not |
| source pin == span digest | `ci/check_world_semantics.py` | the reducer was edited at all — including on an axis the corpus does not exercise |
| **corpus digest may not move at a fixed version** | `ci/check_world_semantics.py` | behaviour moved, the declaration was updated, and the version was not |

The third is the one that makes the bump unavoidable, and it is the one the Rust
test structurally cannot perform: re-pin the digest and the test is satisfied.
The baseline records what each version's digest *was*, so re-declaring a
different digest for a recorded version has nowhere to hide.

`strip_noncode` grew a `keep_strings` flag for the pin (default off; every
existing caller byte-identical). Blanking string literals would leave the pin
blind to `lifecycle_token` and `SummaryKind::as_str`, where the literal **is**
the behaviour. The cost is a false red on error-message churn — the cheap
direction, since re-pinning costs one commit and the corpus digest proves
nothing moved, while blindness costs correctness silently.

#### Measured, not asserted

Run against the shipped `projection::supersedes` with the
generation-leads-valid-time flip applied:

| Mutation | Rust conformance | Gate |
|---|---|---|
| flip the fold rule | **RED** — corpus digest moved | **RED** — source pin moved |
| …re-pin BOTH digests, leave `version` at 1 | **green** | **RED** — digest moved at a fixed version |
| …bump to v2, add a baseline row | green | green — and the end-to-end ref pin reds until the recorded version set is re-pinned |

Row two is the whole box. Row three shows the bump genuinely reaching a
reference rather than stopping at a constant. The gate's own 20 self-tests were
mutation-checked the same way: stubbing `check` to return no problems reds six
of them.

#### The corpus is challenged, permanently

The user's requested control — *change fold behaviour without changing the
version and confirm the gate catches it* — is in the repo as a standing table
rather than a one-off run, because a one-off answer expires the moment someone
edits the corpus. Each rule carries deliberately divergent reimplementations,
one per behavioural axis, and every one must render differently from the real
fold over the same input: `generation_leads_valid_time`, `no_tiebreak`,
`subject_only_key`, `never_supersedes`, `assert_overwrites`,
`no_create_from_consequence`, `last_observed_follows_head`, `head_follows_time`,
`kind_not_in_key`, plus four at the boundary including the over-strict
`stale_refused`. Each variant harness also carries a **faithfulness control**
asserting it reproduces the real reducer at its real settings — otherwise a
transcription slip in the variant would render differently and "pass" while
proving nothing about the axis it names.

This earned itself immediately. On its first run
`the_claim_corpus_catches_generation_leading_valid_time` **failed**:
`world_current_corpus` had the backdated claim mid-sequence, where the mutated
and real folds converge on the same final state. A fold is only observable
through its final state, so an intermediate divergence that later converges is
invisible to any corpus — the corpus was blind to the axis it existed to cover,
and would have shipped that way.

#### The residual, stated rather than papered over

An author who edits the Rust declaration **and** rewrites the baseline's
historical row in one commit still passes. No gate can force a human to
increment an integer. What these three checks remove is doing it silently, doing
it by accident, and doing it without a reviewer seeing a diff — in a file whose
only purpose is to be that record — saying a historical fact was rewritten.

Two further bounds worth keeping visible: the variant table proves sensitivity
only to the axes it names, and the source pin is what covers the rest; and the
boundary rule's corpus is challenged in **both** directions, because
over-strictness there (refusing a stale claim) is a regression that reads as
conservatism.

### 3h closed: the graph as it stood then — 2026-08-11

The box: *"historical queries use historical identity (2d) and historical
evidence — never today's entity graph applied to old evidence."*

#### The failure survives a correct evidence pin, which is why it needed its own box

Box 3b's `read_at_generation` pinned the *evidence* and worked. The failure this
box names goes straight through it: replay the claims at generation `g`, then
resolve the object they name against the identity graph **as it is now**. Every
claim is historically correct, the coordinate on the answer is honest, and the
object is silently wrong — because a merge recorded last week says the thing
that claim pointed at is now called something else.

Nothing about that reads as a bug at the call site. It is what you get from
writing the obvious code with a live `WorldStore` in scope. So the fix is a
**composed read** rather than a rule about which functions to call in which
order: `read_composed_at_generation` returns claims and identity together, and
`PinnedComposition`'s halves are private and reachable only as a pair.

#### One coordinate, one compaction check, one refusal

Both halves replay from the same log at the same cut, so the compaction check
runs **once** and covers both. A half-reproducible composition is
unrepresentable rather than merely discouraged.

#### The trap this box had to walk past

The obvious head for the identity half — `entities_projection`'s own checkpoint
— is **wrong**, and wrong in the direction that looks careful. Box 3c already
recorded why the two checkpoints are not comparable: `world_current` advances
past every event *considered*, the entity fold only to the last *adjudication*
it folded, so appending one ordinary claim leaves the entity checkpoint
legitimately behind with both folds complete. Bounding the composed read on it
would refuse perfectly reproducible generations on a healthy store — the exact
false drift that finding exists to prevent, re-appearing one box later in new
clothes. `a_lagging_entity_checkpoint_does_not_refuse_a_reproducible_generation`
pins the correct bound, and its fixture forces the checkpoints apart so the
assertion is not vacuous.

A related simplification falls out: a pinned composition needs no staleness gate
at all. `identity_is_current` exists because a LIVE read consults a projection
that may lag the log; a pinned read folds the adjudications itself, up to the
coordinate, so the graph is complete there by construction.

#### `entity_fold` entered the version set because a red test said so

This is the part worth keeping. Box 3b shipped with `entity_fold` **excluded**
from `SemanticVersions::for_query(CurrentSubject)`, and the exclusion was correct
then: a resolved ref reported `NotResolvedInReplay`, so the identity fold could
not reach the answer, and including it would have refused references for a rule
they never consulted. 3b asserted that exclusion in
`the_current_subject_query_depends_on_exactly_two_rules`.

3h changed what the answer is derived from, and that test **failed** — which is
the whole point of having written it. The set was widened because an assertion
said the old claim had stopped being true, not because someone edited a list to
match the code. A dependency set maintained the other way round is one that will
eventually describe a composition that no longer exists. The test is now
`..._exactly_three_rules` and records the flip in its own docs.

Three further tests moved with it, all for the same reason: `entity_fold` had
been serving as the *"a dependency this build does not have"* case, and that role
passed to `subject_summary_fold` — a rule the store really declares and this
query family really does not consult, so the case stays honest.

#### Measured, not asserted

| Mutation | Caught by |
|---|---|
| resolve identity against **today's** graph | `a_ref_pinned_before_the_merge_resolves_identity_as_it_stood_then` **and** `the_historical_and_live_identities_genuinely_differ` |
| drop `entity_fold` from the version set | `the_current_subject_query_depends_on_exactly_three_rules` (unit) **and** `a_refs_recorded_versions_are_pinned_to_what_it_resolves_to` (end-to-end) |
| bound the composed read on the **entity checkpoint** | `a_lagging_entity_checkpoint_does_not_refuse_a_reproducible_generation` |

The suite carries its own controls, because each arm is worthless without the
other: `the_live_read_resolves_the_object_through_the_merge` proves the fixture
exercises identity at all (otherwise the historical arm would be "unmerged" for
reasons unrelated to the pin), and `a_ref_pinned_after_the_merge_does_follow_it`
proves the pin is a coordinate rather than a preference for old answers
(otherwise an implementation that never resolved identity would pass). The two
arms differ in the OBJECT RESOLUTION and not in the claim — the stored object
string is `dock_alpha` in both — so the pair cannot be satisfied by the evidence
pin alone.

#### What remains

`read_composed_at_generation` is gated by the 3d answer-boundary ratchet, for a
sharper reason than its siblings: it hands back a `ProjectedClaim` and an
`IdentityView` *together*, so calling it directly gives a consumer everything
needed to compose an historical answer while bypassing the boundary. The
convenience is what makes it worth gating.

The honest scope bound at the time of writing: 3h was closed for the
**generation-pinned** family only, with `ask_as_of` still reporting
`ObjectIdentity::NotResolvedInReplay`.

> **Closed 2026-08-11, immediately after** — see *"3h's other axis"* below.
> The remainder was taken while the machinery was fresh rather than deferred,
> on the reasoning that it is the same architectural problem on the other
> temporal axis rather than a separate feature.

### 3h's other axis: `ask_as_of` composes identity — 2026-08-11

3h closed *"historical queries use historical identity, never today's entity
graph applied to old evidence"* for the generation-pinned family and left
`ask_as_of` reporting `NotResolvedInReplay`. That was honest, but it answered
the same architectural question on one axis and left it open on the other.

Taken immediately rather than deferred behind 3e, on the reasoning that this is
**the same problem on the other temporal axis, not a separate feature** — and
that the moment to close an inconsistency is while the machinery that closes it
is fresh. Everything needed already existed: `ask_as_of` and `identity_view_at`
both cut on transaction time, the composed-read pattern came from 3h, the
version machinery from 3b, the one-snapshot rule from 3c.

#### What was built, and what deliberately was not

`WorldStore::as_of_composed` reads both halves inside one transaction. It lives
on the store rather than on `ReadSnapshot` because its halves are store-level
temporal queries rather than projection replays — so the existing `as_of` and
`identity_view_at` are called **unchanged**, keeping one cut per axis with no
second copy of the bitemporal filter and no second adjudication replay to drift
from the original.

Held to the stated scope: no new resolver (object resolution goes through the
same `resolve_object` the live and pinned paths use), no valid-time
interpretation of identity, no fallback to the current graph, and contradiction
/ ambiguity outcomes propagate unchanged.

#### The asymmetry between the two compositions, stated so it does not read as an oversight

| | Generation-pinned | `as_of` |
|---|---|---|
| promises | exact reconstruction of a recorded coordinate | what was known then, from what remains |
| on lost evidence | **refuses** (`Irreproducible`) | **degrades** (`Resolution`, already on the answer) |

Refusing on the `as_of` path would discard an answer the caller can legitimately
use while being told exactly what is missing; reconstructing a generation pin
from a log with holes would be a reconstruction wearing the word "pinned". Both
are honest about which they did.

#### The version set, and a finding

`QueryKind::AsOfSubject` joined, and `entity_fold` is in its set for the same
reason it is in `CurrentSubject`'s. The two families turn out to depend on the
**same three rules** — a finding rather than a coincidence: they differ in which
COORDINATE they cut on, not in which rules produce the answer. They are still
derived per-family, because *"identical today"* is not *"identical by
construction"*: a temporal-resolution rule would belong to one and not the other,
and a shared arm that has to be split later is one nobody remembers to split.

`TemporalLookup` now carries that set, which spends a 3a exclusion. 3a said the
envelope owns *"completeness, freshness, provenance and versions"* and then
excluded versions because *"no reducer version exists to carry. Minting one here
would be the decorative metadata 3b forbids; it lands with 3b's enforcement."*
3b built the enforcement, so the field is no longer decorative.

**Carried, not enforced** — and the docs say so. A recorded `AnswerRef` REFUSES
on a version mismatch; a `TemporalLookup` states which rules produced it. There
is no `as_of` ref to refuse yet, and implying otherwise would be the overclaim
3b exists to prevent.

#### Measured

| Mutation | Caught by |
|---|---|
| fall back to today's identity graph | **5** tests, including the box's own assertion and the no-fallback arm |
| cut identity at the claim's **valid** time instead of transaction time | the same 5 |
| drop `entity_fold` from the family's version set | `an_as_of_answer_carries_the_familys_version_set` |

The suite's controls: an arm asking the same query LATER (so an implementation
that never resolved identity fails), an arm asserting the CLAIM is byte-identical
across both cuts (so the pair cannot be satisfied by the bitemporal claim filter
alone — only the resolution moves), and a re-assertion that 3g's completeness
still rides on the answer, because this change rewrote the lines that carry it.

`as_of_composed` joins the 3d answer-boundary ratchet for the same reason its
generation-axis twin did: claims plus an identity view in one return value is
everything a consumer needs to build an historical answer without the boundary.

#### Where Tier 3 stands on this question

Both temporal axes now use the same identity semantics. Historical composition
is no longer a property of one query family.

### 3e closed: `Timeless` is granted, never assumed — 2026-08-11

**`KIRRA-WM-FRESHNESS-POLICY-001` — RULED 2026-08-11**

> **Freshness semantics are centrally ruled by claim kind. `Timeless` must be
> explicitly granted. Bounded facts require an explicit age limit. Unclassified
> semantics refuse.**

And the invariant that follows, which is the one to remember:

> **`Timeless` is an affirmative semantic classification, never the absence of a
> freshness policy.**

#### The defect was live, and this document had already found it

FINDING 2 above recorded it before the box was built: `validity_at` maps
`staleness_budget_ms: None` to `Validity::Timeless`, and `WorldView` accepted
`None` from anyone. `Timeless` is not *"we did not check"* — it is a **positive
claim that the fact's age does not matter**. The engine asserted that claim
about every fact in the store whenever nobody supplied a budget, including
`last_seen_at`, for which it is false.

#### Why a ruled table and not a caller flag

The alternative considered was letting the caller declare a query
recency-sensitive. Rejected, and the reason is decisive: it merely moves the
trust decision outward. A careless caller says *"insensitive"* and manufactures
`Timeless` exactly as the engine did — the API satisfied, the architectural rule
defeated. So the disposition is ruled centrally, keyed by **semantics**
(`kind` + `predicate`), because `last_seen_at` and a floor plan can be identical
in storage shape and opposite in temporal meaning.

#### The state machine, and the `Unknown` question settled

```text
policy = Timeless               -> Timeless
policy = Bounded, age <= bound  -> Fresh
policy = Bounded, age  > bound  -> Stale
no policy                       -> the QUERY refuses
```

The open sub-question above asked whether a fourth `Unknown` freshness variant is
reachable, and said to decide it by finding a reachable case or leaving it out —
*"do not carry it undecided."* **Decided: omitted.** There is no successful
answer for which freshness is unknown; a missing policy is a
*policy-resolution failure*, not a freshness state, so it travels in the error
channel as `AskError::UnclassifiedFreshness`. A fourth variant would be
uninhabited — the decorative-semantics failure this tier keeps removing.

The one case that would justify it, recorded so it is recognised if it appears:
a claim whose age genuinely **cannot be determined**, from missing or invalid
time provenance. If that is ever found, the variant becomes justified *then*,
established by a reachable test rather than reserved speculatively.

#### What `None` became

`WorldView::new` took `Option<u64>`; it now takes a `FreshnessSource` with **no
variant meaning "nothing supplied"**, so the old default is unrepresentable
rather than discouraged. `mission_context`'s caller-supplied `Option` maps to an
affirmative policy — `Some(b)` to `Bounded`, `None` to `Timeless` — which is
what its signature already *meant* (*"I have considered this and this fact is
genuinely timeless"*), now said in a type where the other reading cannot be
written.

#### The table is small on purpose

Three ruled rows, each carrying its reasoning; everything else refuses. A large
speculative table would be decorative metadata in another costume — entries
nobody decided, read as though somebody had. Because absence refuses, the table
grows one argued row at a time and no interim is unsafe.

The classes this repository writes but has **not** ruled are recorded in
`ci/freshness_unruled_baseline.json` as knowingly unruled. All three are test
fixtures; inventing dispositions for them would have been the same overclaim.

#### The silent failure the gate exists for

The refusal is fail-closed, so a missing class is never *unsafe* — it is
*invisible*. The table starts correct and quietly goes incomplete as new claim
kinds land. `ci/check_freshness_coverage.py` makes that mechanical: every
`(kind, predicate)` the repository writes must be ruled or baselined, and one in
neither reds. It reports — rather than silently skips — `NewEvent` literals whose
`kind` comes from a variable, since those are classes it cannot see.

#### Measured, and one bug the tests caught mid-build

| Mutation | Caught by |
|---|---|
| missing policy falls back to `Timeless` | **5** tests (3 integration + 2 unit) |
| a benign new ruled row | nothing — the control, confirming the suite is not brittle |

The adversarial pair uses claims of the **same `valid_from`**, read at the
**same clock**: `last_seen_at` is `Stale`, `colour` is `Timeless`. Nothing in the
data distinguishes them, so only the ruling can.

The first draft of `ask` filtered with `.unwrap_or(false)`, which **swallowed
the refusal and silently dropped the unclassified claim** — turning a policy
fault into a narrower answer. That is the exact failure
`one_unruled_claim_refuses_the_whole_query_rather_than_narrowing_it` was written
to catch, and it caught it during the build rather than in review.

#### What remains

`mission_context` still classifies for itself via `FreshnessSource::Caller`. That
is safer than the global default it replaced — the classification is a value
somebody wrote, greppable and reviewable — and it should move to the ruled table
once an applicable entry exists. The `Caller` variant stays as the honest interim
while `RULED` is small.

---

### 3g follow-up closed: two families, two mechanisms — 2026-08-12

3g shipped with a stated limit — `ask_as_of` carried completeness and `history`
and `subject_summary` did not. This closes both, under one acceptance rule:

> A boundary answer must never report `Full` when evidence required by that
> family may have been removed. Conservative `Degraded` is allowed; silent loss
> is not.

#### Neither family lost completeness — neither family EXISTED at the boundary

`WorldView` exposed exactly `ask` and `ask_as_of`, so "unpropagated" understated
it: there was no `history` query and no `subject_summary` query to propagate
through. That made the acceptance rule a construction constraint rather than a
migration, and it is why this landed as two new queries rather than two patches.

#### The two mechanisms are genuinely different, and forcing one type would have lied

Both PROPAGATE rather than recompute — a second verdict at the boundary is a
second implementation of the rule governing whether an answer can be trusted.
But they propagate different types, because the store computes them from
different things:

| Family | Signal | Computed from |
|---|---|---|
| `history` | `Resolution` | citations overlapping a queried RANGE |
| `subject_summary` | `SummaryCoverage` | the evidence behind ONE folded row |

Coercing `SummaryCoverage::Degraded { summaries }` into
`Resolution::Degraded { spans, summaries }` requires inventing `spans: vec![]`,
which reads as *"no compacted span bore on this"* — false, and false in the
reassuring direction. So `SummaryLookup` carries the native type. **One source
of truth per family beats one type across families.**

#### `answer_admissibility` is in history's version set for REFUSAL, not filtering

The subtlest membership call in the module, and the one most likely to be
"fixed" wrongly. History does **not** filter on admissibility: a claim `ask`
declines to serve is still part of the record, and hiding it would make history
lie about the past exactly as an admissibility filter on a lineage would hide
the evidence an investigator came for.

It is in the set because history still RESOLVES each claim's policy, so 3e's
fail-closed refusal on unclassified semantics reaches this family. Changing the
rule can turn a history answer into a refusal — that is the membership test, and
the test does not care that the rule is consulted for refusal rather than for
dropping rows. Two separate tests pin the two halves, because as one assertion
they would read as one fact.

`world_current_fold` and `entity_fold` are both OUT and asserted so: history
returns raw confirmed claims in generation order, folding nothing and resolving
no identity.

#### The conservative citation rule is PINNED, not sharpened

`resolution_for`'s citation check is store-wide, so one retained citation
degrades every subject's history — including subjects the compacted span never
touched. The tempting cleanup is to scope it to the queried subject. That would
convert a conservative signal into an exact-loss detector, and an exact-loss
detector that is wrong is silent.

The consequence is structural rather than stylistic: the `Full` arm is only
reachable in a store with **no citations at all**, so the two history controls
need two separate stores. A reader who consolidates them will find the Full arm
unreachable, which is the intended tripwire.

#### Numerical reconciliation is not evidence coverage

The invariant this half exists for:

> Successful numerical reconciliation does not imply complete evidence coverage.

`reconciled_observation_count` and `reconciled_first_observed_ms` genuinely
reconstruct their pre-compaction values from citations. `provenance_head` and
`last_event_id` cannot be reconstructed at all — a citation names a span, not
the events inside it — and for a fully compacted subject `retained` is `None`
and those fields do not exist. `SummaryLookup::is_degraded` therefore reads
`coverage` and only `coverage`; there is deliberately no constructor taking a
reconciliation result.

#### Six mutations, and the one that separates two controls

| # | Mutation | Reds |
|---|---|---|
| 1 | history: force `Full` | `a_compacted_history_reports_degraded` |
| 2 | history: force `Degraded` | `an_uncompacted_history_reports_full` |
| 3 | summary: `is_degraded` → `false` | both summary-degraded controls |
| 4 | summary: `is_degraded` defers to reconciliation | both summary-degraded controls |
| 5 | history: swallow the `policy_for` refusal | `an_unruled_claim_refuses_the_whole_history` |
| 6 | fixture: drop the post-compaction rebuild | `reconciliation_does_not_upgrade_completeness` ONLY |

Mutations 3 and 4 red the same pair, so on mutation evidence alone the
reconciliation control looks redundant. Mutation 6 is the separating case,
reddening it alone at `left: 4, right: 3`: what the extra control buys is proof
that reconstruction genuinely SUCCEEDS in the fixture, so the degraded verdict
is held *despite* working reconciliation rather than alongside broken
reconciliation.

#### Review follow-up: the per-subject query stood on a whole-store scan

Caught in review, and it is the SAME finding as #1440's unbounded lineage
fetch — against the same clause, one PR later.

`WorldView::subject_summary` called `subject_summaries_with_coverage()` and
filtered. That call is a BULK API and optimised as one: it loads every retained
row and every citation in the store and groups once, *because* the per-subject
rescan it replaced was quadratic. Standing a single-subject query on it inverts
that optimisation exactly — `O(total subjects + total citations)` to read one
row, on tables that grow for a store's whole life.

`KIRRA-WM-ANSWER-IDENTITY-001` clause 2 is *"queries are bounded"*, and it was
violated the same invisible way as on #1440: the returned answer was correct and
bounded, so only the WORK was unbounded and nothing showed it.

That the same class of defect recurred one PR after being written up is the
useful part. Fixing the instance did not generalise; **a per-query bound is not
a property anything currently checks**, and both times it took a reviewer. A
gate that flags a bounded-looking boundary query built on an unbounded store
call would be the real remedy, and is recorded here as not built.

The fix follows #1440's shape. `WorldStore::subject_summary_with_coverage`
narrows both sources in SQL (`subject_summaries_for`,
`load_summaries(Some(..))`), and the coverage verdict is EXTRACTED
(`subject_projection::coverage_from_citations`) so the bulk and narrowed paths
run the same code rather than two implementations agreeing.
`the_narrowed_and_bulk_coverage_paths_agree` sweeps every subject the bulk call
knows — including the citation-only one a naive per-subject read drops — plus a
subject in neither.

Three mutations, run:

| Mutation | Reds |
|---|---|
| narrowed path drops citation-only subjects | the agreement test |
| narrowed path reports `Complete` for a degraded subject | the agreement test |
| the SHARED rule stops matching `None`-kind citations | **four pre-existing tests** |

The third is the one that justifies the extraction: mutating the shared
derivation reds the tests that guarded it when it was inline, so moving it out
of `subject_summaries_with_coverage` weakened nothing.

#### Three fixture facts that cost a red test each

Recorded because the wrong version of both looks entirely reasonable:

* **`fold()` does not build `subject_summary`.** `fold_subject_summary()` is a
  separate call, and a fixture running only the first produces ZERO summaries —
  every subject-summary control would then pass vacuously against a store that
  had never summarised anything.
* **The summary must be rebuilt AFTER compaction.** Folding first leaves the
  pre-compaction total in the row and the citation adds the removed event back
  on top of a count that never lost it: measured at `reconciled = 4` against a
  true 3. The fixture would have looked green while double-counting.
* **`is_admissible` drops `Expired` and `Inadmissible`, not staleness.** A stale
  claim is served by `ask` carrying a `Stale` grade, so the first draft of the
  history/`ask` contrast failed on its own premise and now uses expiry.

### 3f closed: lineage is a query family, not a field — 2026-08-12

`KIRRA-WM-EXPLAIN-TIER-001` asked Tier 3 for *"only the deterministic lineage
CONTRACT that Tier 4 consumes"*, with two constraints attached: **bounded and
paginated, with truncation visible**, and **historically correct**.

The scoping call was between a *walk from an answer* — follow the
`EvidenceDigest` a `WorldAnswer` already carries — and a **query family of its
own**, with a reference and a version set. The family was chosen, and the
difference turned out to be more than symmetry: a walk has no pagination story,
so *"truncation visible"* would have had nothing to bite on, and no version set,
so a change to which evidence is returned would have reached recorded references
silently.

#### The rule is versioned because it can change an answer four ways

`lineage_selection` (`kirra_world_store::lineage::select_lineage`) is
`RuleId`'s fourth member and its first non-reducer. The membership test this
tier applies is *"can changing this alter a derived answer"*, not *"is this a
fold"*, and lineage selection can alter one along four axes at once: which
events are chosen, the generation bound, the order, and where a page ends.

The sharpest of those is the **cursor**. A recorded page-2 reference carries a
cursor minted by the *old* ordering; replaying it under a new one returns a set
that is neither the old page 2 nor the new one, and looks entirely ordinary. So
a moved version refuses rather than replays, exactly as `KIRRA-WM-REDUCER-
VERSION-001` requires.

#### The dependency set is ONE rule, and the three absences are the claim

| Rule | In? | Why |
|---|---|---|
| `lineage_selection` | yes | it decides which events, in what order, where the page ends |
| `world_current_fold` | **no** | lineage returns evidence, not folded claims — nothing asks which claim won a key |
| `entity_fold` | **no** | the subject is matched as written; no identity edges are followed |
| `answer_admissibility` | **no** | an inadmissible claim is still evidence |

The `answer_admissibility` exclusion looks careless and is the important one.
Lineage exists to answer *"why does this answer say what it says"*, and an event
that was rejected, or expired, or that an LLM proposed and nobody confirmed, is
frequently the whole explanation. A lineage showing only servable claims would be
silent in exactly the cases somebody is investigating. So `select_lineage`
deliberately does **not** filter on `claim_status`, where `world_current` does.

Each exclusion is also a **tripwire**, asserted in
`the_lineage_query_depends_on_exactly_one_rule` rather than written down. The
moment lineage follows an identity edge, `entity_fold` can change what it says
and must join the set — and the assertion goes red first. That is how
`entity_fold` entered `CurrentSubject`'s set in 3h.

#### Compaction DEGRADES a lineage page; it does not refuse it

`read_at_generation` refuses a compacted coordinate, and must: a projection
folded from a log with holes is silently *wrong* and looks exactly like a
correct one. Lineage is not folded — it is the evidence itself — so a page
missing a compacted span is *incomplete* rather than wrong, and the citations
name exactly which generations went and under which digest. The split mirrors
one the store already makes (`read_composed_at_generation` refuses,
`as_of_composed` degrades), and it is why `PinnedLineage::Irreproducible` carries
only `NotYetReached`.

Truncation and degradation are kept **independent**, both observable: a page cut
short by the caller's own limit is complete evidence, bounded; a page missing a
compacted span is evidence that no longer exists. Conflating them would cry wolf
on every paginated read.

#### The tier boundary is where the JSON array starts

§7 records `Explain` as depending on *"derivation edges being real structure
rather than a JSON array of identifiers"*. `provenance` **is** that array today,
so a lineage entry carries it **verbatim and unparsed**. Walking it here would be
Tier 3 inventing the structure whose absence is the reason `Explain` is Tier 4.
The stopping point is the ruling, not an oversight.

#### Review follow-up: the fetch was bounded by the rule, not by the query

Shipped as reviewed. The first cut of `lineage_candidates` fetched **every**
event recorded under the subject and let `select_lineage` bound the result,
reasoning that a query which pre-applied the generation bound, the ordering and
the page would be a second implementation of a versioned rule — the #1437 drift,
*"in a place where nothing would notice."*

That reasoning was right about the hazard and wrong about the remedy, and review
caught it. A two-event page over a long-lived subject loaded that subject's
entire history into memory: the page bound governed the *answer* while nothing
governed the *work*, on an auditor-reachable read, with a ceiling that grows with
how long the system has been running.

It also sat against a standing ruling. `KIRRA-WM-ANSWER-IDENTITY-001` clause 2
is *"queries are bounded"* — not a preference, but a consequence of D-9's
measured 10.5 s p99 at 100 000 entities and ADR-0041 D-12's finding that **an
unbounded query has no bounded cost whatever its scaling verdict.** A new query
family whose fetch was unbounded by construction was in tension with the ruling
the box was written under, and the tension was invisible because the answer it
returned was correctly bounded.

The remedy the #1437 lesson actually points at is not *"never pre-narrow"* — it
is *"never pre-narrow anywhere that nothing would notice."* So the fetch is now
narrowed to `subject`, the as-of bound, the cursor and `LIMIT limit + 1`, and the
narrowing ships **with the thing that notices**:
`narrowing_never_removes_what_the_rule_would_keep` runs the rule over the
narrowed candidates and over the unnarrowed ones and asserts the two selections
are identical — including the boundary — swept across 75 (limit, cursor,
generation) combinations, since the two bounds that can disagree are the cursor
and the probe and both are invisible on a first page that fits.

`limit + 1` is what keeps `More` detectable: the rule's own comment already said
*"one over the limit is fetched conceptually here"*, so the narrowing made actual
what the rule had assumed.

**Two tests, because one cannot catch both failures.** Four mutations were run:

| Mutation | Agreement test | Bound test |
|---|---|---|
| no probe row (`limit` not `limit + 1`) | red | red |
| as-of bound tightened to `<` | red | green |
| cursor made inclusive | red | green |
| **unbounded fetch restored** | **green** | **red** |

The last row is the one worth keeping in view: the agreement test *cannot* catch
a re-widening, because the unbounded fetch is the reference it compares against —
re-widening makes the two sides identical, which is exactly what it asserts. That
is why `a_small_page_over_a_long_history_fetches_a_small_number_of_rows` exists
as a separate test rather than an extra assertion, and the division of labour is
now demonstrated rather than argued.

#### The mutation battery

Ten mutations, run against the shipped code:

| Mutation | Caught by |
|---|---|
| drop the generation bound | integration (the load-bearing test) |
| drop the ordering | store unit + corpus control |
| inclusive cursor | integration (pagination walk) |
| `More` on a merely-full page | integration + store unit + corpus |
| version check after the store read | integration (ordering test) |
| `answer_admissibility` joins the set | 2 unit tests |
| filter to confirmed claims | integration (candidate test) |
| compaction refuses instead of degrading | integration (degradation test) |
| drop the subject filter | store unit + corpus control |
| `next_page` mints a successor past the end | integration |

**Two survived the integration tests and were caught only at store level**, both
found by running the mutation rather than by reading the code:

* **ordering** — `generation` is `world_events`' primary key, so SQLite returns
  rows in that order anyway and the unsorted rule accidentally agrees on every
  store the tests can build;
* **the subject filter** — the SQL pre-filters by subject, so
  `another_subjects_evidence_is_not_in_this_lineage` tests the *query* and not
  the *rule*.

Both are recorded in the test file's own header, because the tempting conclusion
— *"the integration tests cover these"* — is false, and a later reader deleting
the store-level tests as redundant would remove the only coverage there is.

The `More`-on-a-full-page mutation also survived the **first** draft of
`the_final_page_yields_no_next_reference`, which asked for a 256-limit page over
two events — a page that could not have been full. Sizing the bound to exactly
the lineage length put the off-by-one under the test a reader would look in.

A **faithfulness control** sits under the six corpus variants: the variant
harness with every switch off must reproduce the real rendering byte for byte.
Without it the six only assert that *something* differs, and a harness wrong in
some seventh way would satisfy them all while naming the wrong axis.

#### The honest limits

* **Lineage follows no identity edges.** An adjudication recorded under a
  merged-away alias is not in the canonical subject's lineage. Same limitation
  `WorldView::ask` states for the subject side, same reason — reading the whole
  equivalence class is a different query, not a flag on this one — and it is what
  keeps `entity_fold` legitimately out of the version set.
* **`LineageRef` is a separate type from `AnswerRef`.** `KIRRA-WM-ANSWER-
  IDENTITY-001` lists the pagination bound among what a reference serializes;
  `AnswerRef` has none because its family has no pages, and this one has no clock
  or staleness budget because evidence does not go stale. One struct holding the
  union would let a caller build a lineage reference carrying a staleness budget
  — meaningless, but hashed into the reference's identity, so two references for
  the same query would compare unequal. They share `QueryKind`,
  `SemanticVersions`, the refusal ordering and the reproducibility horizon.
