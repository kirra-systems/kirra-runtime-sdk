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
`retention`, `kind`) carrying 144 tests — still zero-dependency.

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

- [ ] Entity resolution — matching incoming observations to existing entities
- [ ] `MergeEntities` / `SplitEntity` / `ForgetEntity` as **recorded events**
- [ ] **`entity_id` minting** — moved here from §6 on 2026-08-07 by
      `KIRRA-WM-TIER1-DONE-001`. It was always described as belonging here
      (*"minting an id is deciding that something is a distinct thing, which is
      adjudication"*) while being listed under Tier 1, which is what held that
      tier's box open on Tier 2 work. Listed rather than deleted: §6's residue
      was a real work item, and a residue that disappears when a box is ticked
      is the failure the ruling's own second constraint names.

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
- [ ] **`evidence_digest` / `prev_hash` as core types** — moved here from §7 on
      2026-08-07 by `KIRRA-WM-TIER1-DONE-001`. **Core-crate work, listed at the
      tier that first requires it**, not reclassified as query work: rule 1 below
      demands every answer carry a `ProvenanceHandle`, and a handle over two bare
      hex strings is the thing that rule exists to prevent. Tier 4's `Explain`
      needs the same edges as real structure.

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
