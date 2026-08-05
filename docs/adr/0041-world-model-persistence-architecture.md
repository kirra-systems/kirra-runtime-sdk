# ADR-0041 (WM-2): Use a SQLite event log with deterministic projections

| Field | Value |
|---|---|
| Status | **Accepted** — 2026-08-04. All seven measurement gates **Met** (tier C closed by five physical power cuts, D-11), and open question 8 resolved by adopting R1–R5. Acceptance carries one **outstanding obligation** and does **not** release the domain-logic gate — see *Acceptance record*. |
| Date | 2026-08-02 (proposed) · 2026-08-04 (accepted) |
| Accepted by | **Justin Looney**, holding the World Model owner, architecture owner and deployment owner roles. One approver across all three — see *Acceptance record* for why that is recorded plainly rather than as three sign-offs. |
| Blueprint | `KIRRA-WM-ARCH-001` §7, §10, §11, §13 (WM-2) — [`docs/design/WORLD_MODEL_ARCHITECTURE.md`](../design/WORLD_MODEL_ARCHITECTURE.md) |
| Deciders | World Model owner · architecture owner · deployment owner |
| Depends on | [`ADR-0039`](0039-world-model-bidirectional-governor-fence.md) (WM-6) · [`ADR-0040`](0040-world-model-ownership-and-boundary.md) (WM-1) |
| **Clarified by** | **[ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md)** — canonical terminology (Decision 1). The persistence recommendation is unchanged. |
| Cross-refs | [`crates/kirra-persistence`](../../crates/kirra-persistence/) (migrations, WAL, durability tiers) · [`crates/kirra-audit-hash`](../../crates/kirra-audit-hash/) (shared chain primitives) · [`src/audit_chain.rs`](../../src/audit_chain.rs) · [`ADR-0038`](0038-postgres-shared-state-hybrid.md) (hybrid backend precedent) · [`ADR-0037`](0037-epoch-fenced-generation-ordering.md) |

> **Convention deviation** — as ADR-0039/0040: *not* ratified on merge. This one
> additionally required **measured evidence on target hardware**. A merged
> document is not a benchmark. That evidence was produced and the ADR was
> accepted separately on 2026-08-04; the merge that introduced it ratified
> nothing.

---

## Context

The blueprint recommends SQLite. **This ADR does not treat that as
predetermined.** The recommendation is the most contestable in the blueprint —
the domain is a graph, and the obvious reading is that a graph database should
win. This ADR states the comparison and, more importantly, states what
measurement would overturn it.

### Provisional scale assumptions

**These are assumptions requiring measurement, not production values.**

| Quantity | Provisional order of magnitude |
|---|---|
| Active entities | hundreds to low thousands |
| Historical observations | thousands to millions, depending on retention |
| Relationship predicate types | bounded — tens, not open-ended |
| Query families | known and enumerable (§12 of the blueprint) |
| Deployment | single-robot, embedded, offline-first |
| Concurrent writers | one process |

If measurement shows entities in the millions or genuinely unbounded ad-hoc
traversal, **Option B becomes materially stronger** and this ADR should be
reopened.

---

## Decision drivers

Ranked for this project specifically:

1. **Offline-first embedded operation** — the robot may be disconnected for
   hours; a server-based store is an availability hazard, not a preference.
2. **Deterministic replay** — the blueprint's projections must be a pure fold;
   determinism is testable only if the substrate is.
3. **Auditability** — provenance is mandatory (blueprint P3); tamper evidence
   must be available.
4. **Migration over a decade** — the store outlives every consumer.
5. **Operational burden** — one substrate, one backup story, one corruption story.
6. **Inspectability** — an operator or assessor should be able to open the store.
7. **Jetson resource budget** — competing with perception and inference.

---

## Options considered

| Criterion | **A** SQLite log + projections | **B** Embedded graph DB | **C** RocksDB / KV | **D** PostgreSQL | **E** In-memory + snapshots | **F** Event log + graph index |
|---|---|---|---|---|---|---|
| Jetson suitability | ✅ | ⚠️ heavier | ✅ | ❌ server | ✅ | ✅ |
| Offline-first | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ |
| Deterministic replay | ✅ | ⚠️ | ✅ | ✅ | ✅ | ✅ |
| Crash consistency | ✅ WAL, drilled in-repo | ⚠️ varies | ✅ | ✅ | ❌ loses tail | ✅ |
| Migrations | ✅ framework exists | ⚠️ hand-rolled | ⚠️ hand-rolled | ✅ | ❌ | ✅ |
| Backup / export | ✅ single file | ⚠️ | ⚠️ | ⚠️ | ✅ | ✅ |
| Bitemporal queries | ✅ indexed columns | ⚠️ | ❌ manual | ✅ | ⚠️ | ✅ |
| Transactions | ✅ | ⚠️ | ⚠️ | ✅ | ❌ | ✅ |
| Graph traversal | ⚠️ recursive CTE / index | ✅ native | ❌ | ⚠️ | ✅ in RAM | ✅ |
| Inspectability | ✅ any SQLite tool | ⚠️ | ❌ opaque | ✅ | ❌ | ✅ |
| Corruption handling | ✅ integrity_check | ⚠️ | ⚠️ | ✅ | n/a | ✅ |
| Operational burden | ✅ lowest | ⚠️ | ⚠️ | ❌ | ✅ | ⚠️ two things |
| Fleet evolution | ✅ ADR-0038 path | ⚠️ | ⚠️ | ✅ | ❌ | ✅ |

**A and F are the same decision at different resolutions.** F is A plus an
in-memory graph index built from projections. The proposal is A *with* F's
index — the index is a runtime accelerator, not a second durable store.

**Why not B, despite the domain being a graph.** At the provisional scale the
graph fits in memory; the query families are known and bounded; and the
operational, migration, and determinism costs land on an embedded target that is
already resource-constrained. The graph shape belongs in an **index**, not the
durable substrate. This is the argument most likely to be wrong if the scale
assumptions are wrong — hence the ratification gate.

**Why not D.** Disqualified on offline-first alone. ADR-0038 already
established the pattern that shared state may go to Postgres while the local
hash-chained ledger stays local; the World Model's evidence log inherits the
*local* half of that ruling.

**Why not E.** An in-memory store with snapshots loses the un-snapshotted tail
on power loss — unacceptable for evidence whose whole purpose is accounting for
what was observed.

---

## Proposed decision

> **An append-only event log plus materialized projections, both in SQLite,
> with an in-memory graph index built from projections at startup and
> maintained incrementally.**

### Provisional table structure — not a finalized schema

```
world_events              the append-only evidence log (the only writable table)
entities_projection       derived
observations_projection   derived
relationships_projection  derived
aliases_projection        derived
identity_adjudications    merge/split events, resolvable forever
provenance_edges          derivation DAG
schema_migrations         versioned, fail-closed on future schema
projection_checkpoints    replay resume points
compaction_citations      what was compacted, and its digest
```

Column-level schemas are deliberately **not** fixed here.

### Event semantics

Every event record requires:

| Field | Requirement |
|---|---|
| Event ID | Immutable, unique, sortable (ULID-class) |
| Generation | Monotonic — reusing the ADR-0037 epoch-fenced ordering |
| Transaction timestamp | When the system learned it |
| Valid-time interval | Where applicable to the payload |
| Schema version | Per-payload, versioned |
| Source | Producer identity + producer version |
| Payload digest | Content hash — the reproducibility anchor |
| Provenance reference | Parent events / derivation inputs |
| Replay order | Deterministic and total |

### Projections

Rebuildable derived views. Requirements:

- **deterministic reducers** — pure functions of `(prior state, event)`, with
  the clock passed in, never read ambiently;
- **checkpointing** — resume without full replay;
- **replay validation** — rebuild from zero must equal the incremental state;
- **projection version** — bumped when a reducer changes, forcing a rebuild;
- **corruption detection** — checkpoint digests;
- **an explicit rebuild command**;
- **no projection-only fact.** Anything in a projection must be traceable to
  events. A value that cannot be derived is a defect, not a cache.

### Compaction — resolving append-only vs finite storage

The blueprint (§11.3) names this as the one place P2 (append-only forever) is
knowingly bounded. Ratifying **compaction-with-citation**:

- raw observations may be compacted **only** under an explicit retention policy;
- the retained summary **must cite** the source event range and its digest;
- **longer retention required** for: safety-relevant events, incident windows,
  calibration, identity adjudications, and operator-confirmed events;
- deletion and privacy rules are represented **explicitly** as records, not as
  absence — a redaction must leave a tombstone, or the chain breaks;
- **compaction must never silently rewrite history.** A time-travel query into a
  compacted window returns the summary *and says so* — degraded resolution,
  never silent fabrication.

**Thresholds are deferred until measured.** Choosing them now would be a guess
presented as a policy.

### Compaction is not reclamation — two operations, not one

**Measured** (`tools/wm2-persistence-harness`, `compact`; host-indicative,
target confirmation required): compacting 9 898 events across 104 spans left the
database file **byte-identical in size**. SQLite row deletion returns pages to a
free list inside the file; it does not shrink it. The ~50 % reduction appeared
only after a `VACUUM`, which rewrites the entire database.

The operational model is therefore:

> **logical compaction → *optionally, separately scheduled* reclamation**

and explicitly **not** "compaction → disk immediately freed". A retention policy
written against the second model will fail in the field: the device keeps
compacting, the free-space alarm keeps firing, and nobody can see why.

`VACUUM` is a maintenance operation with power, I/O, thermal and availability
consequences, on a Jetson sharing storage with perception and logs. Reclamation
should therefore be **gated on preconditions**, not run opportunistically. The
proposed gate — measurement on target required before any of these numbers are
fixed:

| Precondition | Why |
|---|---|
| Robot stationary, no active mission | The rewrite competes with perception for the same storage; a mission is the worst time to find out how much |
| Adequate battery or external power | A rewrite interrupted by power loss is the one operation where the WAL's protection is doing the most work |
| Bounded free-space reserve available | `VACUUM` needs room for a second copy before it has freed the first; running it at 98 % full is how a full disk becomes a corrupt one |
| Crash-safe scheduling | It must be safe to lose power mid-`VACUUM`; the drill's tier B covers the shape, tier C the medium |
| Defined interruption behaviour | Aborted reclamation must leave a usable store, and must not leave the policy believing it succeeded |

**Consequence for the storage-growth gate.** Days-to-full must be computed
against *logical* growth plus whatever reclamation actually runs, and a policy
that assumes continuous reclamation is assuming a maintenance window that may
never open. The conservative planning figure is the un-reclaimed one.

### Retention class is immutable evidence

**Measured**: the retention class is inside the canonically-hashed event bytes,
so rewriting it breaks the chain at that generation. This was found by a test
that tried to relabel events in bulk and got a broken chain instead.

That is the correct behaviour and it is worth stating as a decision rather than
leaving as an implementation detail, because it constrains operators in a way
they will not expect:

> **A retention policy change cannot be applied retroactively by relabelling
> existing events.** Editing an event's class is tamper-evident, and the log
> will correctly report itself broken.

The security property this buys: the obvious attack on a retention policy is to
downgrade a protected event to `raw` so a later compaction pass is allowed to
delete it. That attack is structurally unavailable — it does not fail a check,
it fails the chain.

The cost is that policy evolution has exactly three legitimate routes:

1. **Forward-only** — the new policy governs newly created events. The default,
   and the only one needing no ceremony.
2. **Explicit migration through a new cited event** — old evidence is
   re-classified by *appending* a record that cites what it re-classifies and
   why, leaving the original intact. The same citation discipline compaction
   uses.
3. **A policy-supersession record** — the policy itself is an event in the log,
   so "which retention rules were in force when this was written?" is
   answerable after the fact. Without this, a compacted span cannot be audited
   against the policy that authorized compacting it.

Routes 2 and 3 are **not implemented and not ratified**; they are named here so
the constraint does not read as an oversight, and so nobody builds an operator
tool that silently attempts route 0.

---

## Audit-chain relationship

Investigated. The repository already separates the algorithm from the state:

- [`crates/kirra-audit-hash`](../../crates/kirra-audit-hash/) — **pure**
  SHA-256 record-hash computations and domain-separated canonical encoders. No
  DB, no state. Already consumed by `kirra-persistence` and
  `kirra-safety-authority`.
- [`src/audit_chain.rs`](../../src/audit_chain.rs) — the stateful
  `AuditChainLinker` that appends into a rusqlite transaction and calls those
  primitives.

**Proposed: reuse the algorithm through the shared crate
(`kirra-audit-hash`), with the World Model maintaining its own chain instance
over its own events.**

- ✅ Not a second incompatible hash-chain design — same primitives, same
  domain-separation discipline.
- ✅ A distinct domain tag for World Model events, so the two chains cannot be
  confused or cross-verified by accident.
- ❌ **Not** appended into the platform audit ledger. That ledger is the
  safety/verifier record; mixing high-rate observation events into it would
  change its volume and retention characteristics, and ADR-0039 keeps the two
  concerns apart.

`kirra-audit-hash`'s own warning applies: *byte-exactness is load-bearing* —
these encoders **are** the on-disk format, and any change re-defines it.

### Four distinctions this ADR insists on

| Property | Means | Provided by |
|---|---|---|
| **Tamper evidence** | Undetected modification is infeasible | Hash chain |
| **Durability** | A committed write survives power loss | SQLite WAL + `synchronous` policy |
| **Correctness** | The projection is the right fold over the events | Deterministic reducers + replay validation |
| **Truth** | The claim matches the world | **Nothing here provides this** |

The fourth is the point of the whole architecture: the store can guarantee the
first three and can never guarantee the fourth. That is why it holds evidence
and derives views, rather than storing facts.

---

## SQLite configuration

| Setting | Proposal | Note |
|---|---|---|
| `journal_mode` | WAL | Matches `kirra-persistence` |
| `synchronous` | **To be decided by measurement** | See below |
| `foreign_keys` | ON for projections; the event log is self-contained | |
| Transactions | One event append per transaction; projection updates may batch | |
| Migration locking | Exclusive during migration; fail-closed on a future schema | ADR-0035 / `migrations.rs` precedent |
| DB ownership | Single writer process (ADR-0040) | |
| Read-only degraded mode | Serve projections read-only if the log is unwritable — **never** silently drop writes | **Tested** (harness `pressure`) |
| Corruption response | `integrity_check` on open; refuse to serve rather than serve partial evidence | |
| Disk-full | Refuse new observations with `Unavailable`; never overwrite | **Tested** (harness `pressure`) |
| Backups | Single-file copy + `VACUUM INTO` | |

**Do not inherit the verifier's durability claim.** `kirra-persistence` uses
`synchronous=FULL` for the durable tier and `NORMAL` for the chain tail — a
deliberate two-tier choice for *safety* state. World Model assurance needs
differ: losing the last few perception observations on power loss is tolerable
in a way that losing a verifier state transition is not, while losing an
operator calibration is **not** tolerable. The likely outcome is a per-source
durability tier rather than one global setting — but that is a measurement
question, not a copy-paste from the verifier.

---

## Consequences

**Positive.** One substrate, one backup story, one corruption story. Reuses the
migration framework, WAL discipline, crash-consistency drill, and hash
primitives already in the repository and already understood. Deterministic
replay is testable with the existing virtual clock.

**Negative / accepted.** Graph traversal is index-mediated rather than native —
acceptable at the assumed scale, and the assumption is gated. Projection
rebuilds cost startup time; checkpointing bounds it. Two representations
(durable rows + memory index) must not diverge, which the "no projection-only
fact" rule exists to prevent.

---

## Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | Scale assumptions wrong → SQLite is the wrong call | Ratification is measurement-gated; reopening conditions stated |
| R2 | Projection rebuild too slow at startup | Checkpointing; measured in the benchmark |
| R3 | Index and projections diverge | No projection-only facts; rebuild-and-compare in tests |
| R4 | Compaction loses something needed for an incident | Retention classes protect safety/incident/calibration/operator events |
| R5 | A second hash-chain design emerges by accident | Shared `kirra-audit-hash`, distinct domain tag |
| R6 | Verifier durability settings copied without thought | Explicitly forbidden above; per-source tiers to be measured |
| R7 | Growth unbounded before thresholds are measured | Storage-growth estimate is a ratification gate |
| R8 | A retention policy assumes compaction frees disk, and the device fills anyway | Measured: it does not. *Compaction is not reclamation* makes reclamation a separately scheduled, precondition-gated operation; the conservative planning figure is the un-reclaimed one |
| R9 | An operator expects a retention-policy edit to apply to existing evidence | Structurally impossible — retention class is chained. Stated as a decision, with the three legitimate evolution routes named |

---

## Alternatives rejected

- **Embedded graph database (B).** Rejected on operational burden, migration,
  determinism, and embedded resource use — *not* on query expressiveness, where
  it wins. Reopened if scale assumptions fail.
- **RocksDB / KV (C).** No query layer, no transactions across the shapes
  needed, poor inspectability.
- **PostgreSQL (D).** Fails offline-first.
- **In-memory + snapshots (E).** Loses the un-snapshotted tail.
- **Appending World Model events into the platform audit ledger.** Rejected:
  changes that ledger's volume and retention profile and blurs the ADR-0039
  boundary.

---

## Assurance impact

**No new safety claim is made here, and no scope determination is asserted.**

The *intent* is that the Kirra World store remains outside the safety scope if
Fence A and Fence B hold. **That determination is PENDING an explicit
safety-assurance ruling**
([ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md)
Decision 5). This ADR makes no safety claim and inherits no ASIL allocation.

Explicitly: the World Model's hash chain provides **tamper evidence for
knowledge**, not safety evidence. It must not be cited as a safety-case artifact
without a superseding ADR.

No existing safety claim, ASIL rating, or standards mapping changes.
Kirra is designed in alignment with ISO 26262 ASIL-D requirements and
IEC 61508 SIL 3 requirements. Independent third-party assessment has not yet
been performed.

---

## Migration impact

**None in this PR.** No schema is created, no code changes.

When implementation begins: the store adapter is the *second* phase, after
domain types (blueprint §23.1 step 5). The existing registries are an import
source; they are not migrated destructively (ADR-0040).

---

## Open questions

1. `synchronous` policy per source class — measurement required. **Now
   measurable, and one obstacle removed.** The stall matrix (D-10) supplies
   stall-robust median throughputs, so the corrupted `NORMAL`/batch=64 figure is
   superseded: 19 351 ev/s across 20 runs rather than the single run's 3 123.
   **The batch=64 inversion survives that correction, however** — medians run
   `OFF` 55 275 > `FULL` 31 497 > `NORMAL` 19 351, from a configuration with
   zero stalls. `NORMAL` slower than `FULL` is not predicted by any durability
   model, and until it is explained a per-source-class policy should not be
   fixed at batch=64. The batch=1 ordering is correct and could be decided now.
   **The inversion REPRODUCED on target 2026-08-04 (D-15):** `OFF` 54 636 >
   `FULL` 30 545 > `NORMAL` 19 881 — the same ordering, all three medians within
   3 % of D-10, from configurations recording zero stalls in both runs. Two
   independent observations make it a real effect rather than noise, so this
   blocker is **stronger**, not weaker. D-15 does not touch it: the stall
   mechanism turned out to be a driver defect independent of `synchronous`,
   which is a different question from why `NORMAL` is slower than `FULL`.

   **NARROWED 2026-08-04 — the inversion does NOT reproduce (D-17).** Re-run on
   the same target later the same day, two instruments, same parameters: `OFF`
   55 267 > `NORMAL` 35 924 > `FULL` 31 083, the conventional ordering. `FULL`
   and `OFF` reproduce D-15 within 2 %; `NORMAL` is **81 % away**. Those two are
   the controls that make the third interpretable. **So the premise above — an
   unexplained inversion — fails, and this open question no longer blocks
   fixing a per-source-class policy at batch=64.** The paragraphs above are
   retained as the record of what was observed and why it was treated as a
   blocker, not as a live finding.

   **What remains open is narrower and should not be read as closed:** *why
   D-15's `NORMAL` figure was low* is unexplained. D-17 rules out the
   instrument, a healthier device (the NVMe defect was live during the re-run),
   and dirty-page pressure. The decision this question gated can proceed on the
   reproducible property D-17 does establish — `NORMAL` trades tail latency for
   median throughput — while the anomaly itself stays on the record.

   **RULED 2026-08-05 — P-1 through P-4 adopted as written.** The policy in
   `docs/design/WM2_SYNCHRONOUS_POLICY.md` is now the rule, not a proposal:

   | | Adopted |
   |---|---|
   | **P-1** | `synchronous=FULL` on the evidence log, **universally** — one log, one chain, one setting |
   | **P-2** | Per-class differentiation moves to **commit grouping**, a property of the writer rather than the store |
   | **P-3** | Grouping is the class-visible durability knob, stated as a **loss window** — at most N events or T ms of *uncommitted* tail. Committed events are never at risk: under P-1 every commit fsyncs, so acknowledgement means durable |
   | **P-4** | `synchronous=OFF` is never used for the log |

   **What this commits to:** tier C's five physical power cuts (D-11) were run
   at `Durability::Full`, so adopting `FULL` keeps the closed durability gate
   covering the shipped configuration. Moving to `NORMAL` later would re-open
   tier C — five more power cuts — for a 16 % median gain that costs 32 % at
   p99. The falsifiers in §7 of the policy document remain the conditions under
   which this ruling should be revisited.

   The residual anomaly below is **not** closed by this ruling, and the ruling
   does not depend on it: D-19 gives a third observation agreeing with D-17
   within 0.2 %, so the reproducible property the decision rests on is now
   observed three times.

   The proposal (now the rule) said: `synchronous=FULL` universally, with
   per-class differentiation moved to commit grouping. It rests on three
   findings — that a per-class `synchronous` value **is** implementable via
   separate writer connections, but buys no per-class guarantee, because
   `fsync` flushes the shared WAL rather than a transaction, so a lax class's
   durability is set by other classes' traffic; that batching is a 9.8× lever
   where the setting is 1.16× at batch=64; and that tier C's five power cuts
   (D-11) were run at `FULL`, so
   `NORMAL` would leave a closed gate not covering the shipped configuration.
   The proposal also records that **"source class" is undefined in this ADR** —
   only *retention* class is — so it supplies a rule rather than a class table.
2. Retention classes: exact list and their durations. The harness models the
   §11.3 protected set (safety, incident, calibration, adjudication, operator)
   and enforces it — a window containing any of them is refused whole — but the
   *durations* remain unset. **D-2 now bounds them**: on 8 GiB at 10 Hz the
   store fills in 21.7 days, so any horizon beyond three weeks requires the
   event rate to fall (2.4 events/s for 90 days). The durations and the
   sampling policy are a single coupled decision, not two.

   **RULED 2026-08-05.** Budget is **18 033 812 events** — 8 GiB with
   projections, from D-2. The protected classes are low-volume, so `raw` is
   essentially the whole cost and the decision reduces to one trade: `raw`
   horizon against the coalescing factor it forces.

   | Class | Horizon | Sustained rate | Events | Budget |
   |---|---|---:|---:|---:|
   | `raw` | **30 days** | ≤ 4.5 /s (**~2× coalescing** from 10 Hz) | 11 664 000 | 65 % |
   | `safety`, `incident`, `calibration`, `adjudication`, `operator` (aggregate) | **365 days** | ≤ 0.12 /s | 3 784 320 | 21 % |
   | — | — | headroom | 2 585 492 | **14 %** |

   **Why 30 days rather than 7 or 90.** 7 days needs no coalescing at all and
   uses 55 % of budget — genuinely cheaper — but it only works if every incident
   is diagnosed inside a week; a cause found late has nothing to reach back to.
   90 days forces ~6.7× coalescing, which risks discarding the resolution that
   made the evidence worth keeping. 30 days at ~2× is the knee, and 2× on a
   10 Hz sensor is modest. **This choice turns on how far back an incident
   reconstruction must reach**, and should be revisited if that answer changes.

   **Three rules come with it, none optional:**

   1. **Forward-only is the operative evolution route.** Retention class is
      inside the canonically-hashed event bytes, so a policy change cannot be
      applied retroactively — routes 2 and 3 above are unimplemented and
      unratified, and OQ2b is still open.
   2. **Compaction buys summaries, not observations.** D-4's ~50 % recovery is
      lossy (`DegradedSummary`), so it roughly doubles the horizon for
      compactable regions without relaxing the sampling conclusion.
   3. **Plan against the un-reclaimed figure.** D-3: reclamation needs a ~1×
      free-space reserve and a total write blackout (~21.5 s on a full store),
      and a policy assuming continuous reclamation assumes a maintenance window
      that may never open.

   **REOPENED 2026-08-05 by D-20 — the allocation no longer closes.** The
   budget above is 8 GiB at D-2's 476.32384 B/event, and D-2 measured the
   harness's **stand-in** schema. Re-measured against the ratified schema the
   budget falls to 15 161 596 events (`lean`) or 14 031 527 (`populated`),
   against an allocation of 15 448 320. Headroom goes from +14 % to **−1.9 %**
   / **−10.1 %**, and that understates it: these are log-only figures against a
   budget that included projections the ratified store has not built.

   **What is unchanged is the input the ruling turns on** — how far back an
   incident reconstruction must reach. That question was answered 30 days and
   nothing here bears on it. What changed is how many events fit, so the
   30-day/4.5-per-second *pair* is what needs re-deriving, not the reasoning
   behind it. The three levers, at the `populated` end with the protected
   classes held at 365 days and 14 % headroom restored:

   | Lever | Consequence |
   |---|---|
   | Hold `raw` at 30 days, coalesce harder | sustained rate **4.5 → 3.20 /s** (~3.1× from 10 Hz, was ~2×) |
   | Hold ~4.5 /s, shorten `raw` | 30 days → **21.3 days** |
   | Raise the budget | 8 GiB → **~10.3 GiB** restores the ruled allocation |

   Coalescing harder is the lever that preserves the answer the ruling was
   built on — 30 days of reach is retained, at ~3× rather than ~2× on a 10 Hz
   sensor. Shortening `raw` to 21 days spends the thing the ruling explicitly
   bought ("a cause found late has nothing to reach back to"), and 21 days is
   close to the 7-day case it rejected for that reason. **This is a decision
   about incident reconstruction and it is not made here.**

   **Coupling to OQ1.** P-2/P-3 make commit grouping the per-class knob; the
   classes those budgets attach to are the six named here. The grouping budgets
   themselves are still unset and are a WM-2 design task, not a further ruling.
2a. Reclamation scheduling: the precondition thresholds in *Compaction is not
   reclamation* were proposed, not measured. **D-3 measures them**: ~399 MB/s,
   a ~1× free-space reserve, a total write blackout for the duration (~21.5 s
   on a full store), and `/var/tmp` non-volatile on this device. What remains
   unmeasured is **interruption behaviour** — what a `VACUUM` killed midway
   leaves behind — which the harness does not exercise and a power-cut trial
   would.
2b. Whether policy-supersession records (route 3 above) are needed at WM-2 or
   can wait. Without them, a compacted span cannot be audited against the policy
   that authorized compacting it — which may or may not be acceptable before the
   store holds anything an assessor would ask about.
3. Whether projections live in the same database file or a rebuildable
   sidecar file (a corrupt sidecar is cheaper to discard).
4. Index rebuild strategy: eager at startup vs lazy per query family.
5. Does ADR-0038's hybrid path ever apply — shared world knowledge in Postgres
   for a fleet, local evidence staying local?
6. Event payload encoding — the blueprint does not fix one; JSON is
   inspectable, a binary format is compact.
7. **Partial projections under disk pressure — RESOLVED IN DESIGN 2026-08-04**,
   with the implementation condition below still outstanding. A fold writes, so
   it is refused when the store is full. A fold interrupted that way leaves
   partial projections with nothing marking them as incomplete. The question
   asked for "either a fold-in-progress marker, a transactional whole-fold, or
   an explicit rule that projections are invalid until a checkpoint confirms
   them". **The rebuild protocol supplies the first and third and shows they are
   the same thing** — see `docs/design/WM2_PROJECTION_REBUILD_PROTOCOL.md`
   (KIRRA-WM2-REBUILD-001), prototyped as a pure state machine in
   `tools/wm2-persistence-harness/src/rebuild.rs`. A projection is authoritative
   only in `Active`; a fold refused midway leaves `Building`, which cannot serve
   and cannot cut over. The marker OQ7 said was missing **is the protocol state
   itself**, so it need not be inferred from row counts or a sentinel row.
   **Disposition, ruled 2026-08-04:**

   > OQ7 is resolved at the protocol level. Full closure is conditional on the
   > production store implementing and testing durable rebuild state,
   > pinned-generation equivalence verification, and atomic cutover with restart
   > recovery.

   The split behind that ruling. The protocol answers **restart/recovery**
   (`on_restart` is total), guarantees **old-active preservation** (no
   transition retires a projection at all), requires an **equivalence proof
   before activation** (`Active` is reachable only from `Verified`), and makes
   the **state transition** atomic (`Verified → Active`, no intermediate). Two
   load-bearing properties still depend on the store and are **not** built:
   the equivalence *comparison* at the pinned generation (design §8, S-4), and
   the *database* cutover being atomic for readers and durable across restart
   (S-1, S-2). Those are store work gated by ADR-0042 Decision 5.

   Recorded this way so the state machine does not stand in for unbuilt
   persistence behaviour — the same posture as open question 8, where the design
   is adopted and the obligation is carried in the open rather than quietly.
8. **Migration strategy — RESOLVED 2026-08-04** by adopting R1–R5 (see *Open
   question 8 — resolution*), subject to the outstanding R2 prototype obligation
   in the *Acceptance record*. Was blocking for acceptance (D-6, D-13).
   D-6 measured a whole-store offline migration at ~101 minutes at the 8 GiB
   ceiling and offered three routes: bound the store, migrate lazily/online, or
   accept a documented maintenance window. D-13 shows that framing rests on an
   artifact — migration cost is the product of events and entities *for that
   particular backfill statement*, and the same schema change written as a
   grouped pass is orders of magnitude cheaper. See *Open question 8 — drafted
   resolution*.
9. **Multi-second write stalls (O-1, D-10, D-15). MECHANISM IDENTIFIED
   2026-08-04 — a lost or delayed NVMe completion. `io_timeout` bounds how long
   the host waits for one; it is the backstop, not the cause** (a later 8 496 ms
   stall with the same idle-device signature resolved on its own, well under the
   30 s bound — D-15 *Refinement*). Five stalls in 120 repetitions coincide
   one-for-one with five
   `nvme0: I/O ... timeout, completion polled` entries in the kernel log; the
   stall durations are the timeout plus 19.4 ms and 182.4 ms of handler latency;
   and the device was **1–2 % busy** across windows that cover the stalls
   themselves, against 107–214 % on ordinary windows. The commands had all
   completed — **durability is unaffected**, and D-11 stands independently. This
   is a **platform/driver defect, not a persistence-architecture property**:
   SQLite, the schema and `synchronous` are bystanders. See **D-15** and
   `docs/evidence/wm2-jetson-oq9-rerun-20260804/`.
   **What remains open** is the *root* cause of the lost interrupt — controller
   firmware, PCIe ASPM, or Tegra MSI-X routing are the candidates and this run
   does not discriminate. That is a hardware-qualification question, and it
   belongs in Assumptions of Use: at 10 Hz a 30 s stall is ~300 observations
   that cannot be recorded, at ~4 % of batch=1 runs on this drive. PSI remains
   unavailable on this kernel.
   *The pre-2026-08-04 classification is preserved below, because the reasoning
   that narrowed the field is what made the kernel-log check the obvious next
   move.*
   **Measured on target; the
   original theory is rejected and the mechanism is partly unresolved.**
   `NORMAL`/batch=64 — the suspect — produced **zero** stalls in 20 repetitions.
   The stalls appeared in `FULL`/batch=64 and `NORMAL`/batch=1 instead: 3 events
   in 120 repetitions, crossing both fsyncing modes and both batch sizes.
   Classified **intermittent and block-device/environment-correlated**. Thermal
   is *ruled out by measurement* (hottest zone 59.6 °C against an 85 °C
   threshold). What remains unknown is the mechanism: **both attributions are
   now `UNATTRIBUTED` or withdrawn**, PSI unavailable on this kernel, and no
   SMART data. See the follow-ups in *Design implications*.

   **Instrument fixed 2026-08-04; the mechanism question is unchanged and the
   re-measurement is outstanding.** `attribute_stall` was comparing a
   *whole-repetition* `disk_io_ms` against a *single-commit* `stall_ms` — a
   denominator mismatch that clears the "block layer busy" bar from ordinary
   background I/O alone. The `IO-DEVICE` attribution on `NORMAL`/batch=1 is
   therefore **withdrawn** (D-10, boxed note): it is not weak evidence, it is
   not evidence. `stall.rs` now samples a timestamped series at
   `SAMPLE_INTERVAL_MS` and deltas across the slowest commit's own window, and
   refuses to attribute when the sampled span is materially wider than the
   stall — so the defect cannot recur silently. **What this does NOT do:** it
   measures nothing new. The stall counts, latencies, throughput medians and
   thermal reading in D-10 stand; the mechanism is exactly as unknown as before,
   minus one attribution that looked like progress and was not. **Next step:**
   re-run the six configurations on target with the fixed instrument, and
   pursue PSI availability (the kernel lacks `/proc/pressure`) since without it
   one of the two I/O signals is absent regardless of windowing.
10. **Graph and temporal query placement (D-1, D-9).** Neither may sit on a
   deadline path, and the sweep sharpens by how much. At 100 000 entities the
   graph family is 159 ms p99 (1.6× a 10 Hz period) and the temporal family is
   **10.5 s p99** (105×). Both scale acceptably in *shape*; neither is
   interactive at that size. Whether a planned consumer needs either at tick
   rate is unresolved — if one does, that is a re-evaluation trigger, and it is
   now a question about absolute latency rather than about scaling.

---

## Acceptance record — 2026-08-04

**Accepted by Justin Looney**, holding the World Model owner, architecture owner
and deployment owner roles.

**All three decider roles are held by one person.** That is recorded plainly
here rather than presented as three sign-offs, because a reader six months from
now should be able to tell the difference between three independent reviews and
one person wearing three hats. It is the latter. The ADR's *Deciders* field
names three roles, not three people, and this project currently has one holder
for all of them.

### What was accepted

| | |
|---|---|
| The persistence decision | Option A — SQLite append-only event log + materialized projections, with an in-memory graph index built from projections |
| Open question 8 | **Resolved** by adopting **R1–R5** below as the migration strategy |
| Evidence base | Seven measurement gates Met, all `JETSON-TARGET-MEASURED`; D-1…D-14 |

### Outstanding obligation, accepted with the decision rather than before it

**R2's alongside-rebuild-and-swap has not been prototyped.** D-14 establishes
that a migration *can* be cheap — it does not establish that the protocol R2
specifies has been built, or what it costs in code and in peak disk.

**A second projection is not a second store.** D-2 measured 458.51 B/event
log-only against 476.32 B/event with projections, so the projection overhead is
17.82 B/event — **3.74 % of total store size** (equivalently 3.89 % measured
against the log alone; both denominators are stated because the two differ and
the smaller one is not the flattering one). At the 8 GiB ceiling that is
**≈306 MiB (321 MB)** of additional projection storage for an alongside rebuild,
against another 8 GiB implied by "a second copy" — a factor of **≈27×**.

The prototype must still measure peak storage, write amplification and cutover
behaviour. **Capacity is not presently the primary risk**; cutover atomicity and
the partial-projection state (open question 7) are.

> **Partial discharge, 2026-08-04 — the code half only.**
> `docs/design/WM2_PROJECTION_REBUILD_PROTOCOL.md` (KIRRA-WM2-REBUILD-001)
> specifies the protocol and prototypes it as a pure state machine
> (`tools/wm2-persistence-harness/src/rebuild.rs`, 15 tests), which answers *what
> it costs in code* and resolves the partial-projection state named in the
> paragraph above. **Peak storage, write amplification and cutover latency remain
> unmeasured**, and the design deliberately says so rather than letting a
> protocol document read as a cost result. The ≈306 MiB figure above is an
> arithmetic consequence of D-2, not a measurement of a rebuild. This obligation
> is therefore reduced, not closed.

> An earlier revision read "a second projection is a second copy, and D-2's
> budget is already tight." It is corrected above and noted rather than silently
> replaced, because it went into a ratified document and the arithmetic was
> available in D-2 the whole time.
>
> The first correction of it, in this same section, said the original
> "overstated the risk by roughly two orders of magnitude." That was itself
> wrong: 8 GiB against ≈306 MiB is **26.7×, about 1.4 orders of magnitude.**
> Recorded because a paragraph about undone arithmetic is the worst possible
> place to do arithmetic loosely.

This was item 3 of *before this can be ruled on*, and it is **not** closed. It
is carried forward as a condition of the acceptance: **WM-2 must prototype
alongside-rebuild-and-swap far enough to cost it before the first migration
ships.** If that spike shows the protocol is impractical on this hardware, R2
must be revisited, and revisiting R2 reopens open question 8.

Recording it this way is deliberate. The alternative — waiting for the spike —
was available and was not chosen; the alternative of quietly implying item 3 was
done would have been a false record.

### What acceptance does NOT do

- **It makes no safety claim and asserts no scope determination.** The
  determination in *Assurance impact* stays **PENDING** an explicit
  safety-assurance ruling (ADR-0042 Decision 5). Accepting a persistence
  architecture is not a safety argument.
- **It does not release the domain-logic gate.** ADR-0042 Decision 5 remains a
  separate and independent hold, and its ruling is still `PENDING`. Kirra World
  domain logic, storage, APIs and services remain blocked.
- **It does not ratify the stand-in schema.** Every measurement describes the
  harness's stand-in (`standin_schema_digest 630eb690aaef…`). When the real
  schema lands its digest differs and the figures become figures about something
  else — the *shape* conclusions survive, the constants do not.
- **It does not close the open questions other than 8.** Questions 7, 9 and 10
  in particular remain open, and 9 (the intermittent multi-second write stall)
  is unresolved as to mechanism. *(Later the same day, question 7 was resolved
  **in design** — see its entry in *Open questions*. That happened after this
  acceptance, not as part of it, and it did not close the question's
  implementation condition. The sentence stands as written at the time.)*

### Reopening conditions

Unchanged from the body of this ADR, plus one: **if the R2 spike shows
alongside-rebuild-and-swap is impractical, open question 8 reopens.** The
existing scale reopening condition (entities in the millions, or genuinely
unbounded ad-hoc traversal) also stands — D-9 found Option A survives at
100 000 entities, which is not the same as surviving at 10 000 000.

---

## Open question 8 — resolution, adopted 2026-08-04

> **ADOPTED.** This was drafted as a proposal for the deciders to accept, amend
> or reject. It was **accepted unamended** on 2026-08-04 (see *Acceptance
> record*), which closes open question 8 — subject to the outstanding R2
> obligation recorded there. The reasoning below is preserved as written,
> because the argument is the decision's justification and should not be
> retroactively smoothed.

### The framing has to change first

D-6's three routes — bound the store, migrate lazily/online, accept a
maintenance window — all treat migration cost as a fixed property of the store,
to be endured, capped or scheduled around. D-13 shows it is not. The measured
cost came from one backfill statement whose correlated subquery rescans the log
per projection row; the same schema change as a grouped pass is ~3 100× cheaper
on identical data.

**A strategy picked from that number would be a strategy picked from an
artifact.** The proposal below therefore constrains what a migration is
*permitted to do*, and treats downtime as a consequence of those constraints
rather than as the thing being budgeted.

### Proposed resolution

**R1 — Migrations never rewrite the event log.** The chain hashes each event's
canonical bytes, so re-encoding a stored event invalidates its payload digest
and every link after it: an in-place log migration does not merely cost time, it
destroys the tamper evidence the log exists to provide. Event encodings are
**versioned and append-only** — new events are written in the new encoding, old
rows are read forward in theirs, and nothing already written is rewritten. This
is the load-bearing rule; the rest follows from it.

**R2 — Projection changes are rebuilds, not backfills, and they run alongside.**
Projections carry no independent truth — they are derived, and the replay gate
already proves rebuild-equals-incremental determinism, which the ADR notes
"gates the rest". So a projection schema change builds the new projection beside
the live one, catches up, and swaps. The robot keeps serving the old projection
throughout. This is the online route, and it is available *because* projections
are derived rather than because of new machinery.

**R3 — Any migration statement must be O(events), never O(events × entities).**
D-13's cost was a query plan, and a plan is reviewable before it ships. A
migration that cannot be expressed in a single pass over the log is not ready.
Enforcement: the migration ladder becomes a harness command run per candidate
migration, and a plan that is not flat in `ms/entity` fails review.

**R4 — Bound the store, but size the bound from storage and query latency, not
from migration downtime.** Retention is already mandatory — D-2 measures
458.5 B/event and 21.7 days to fill 8 GiB at 10 Hz, and D-9 measures a 10.5 s
temporal p99 at 10 M events. Those are the constraints that should set the
bound. Sizing it to keep a quadratic backfill tolerable would trade away memory
depth to work around R3's defect, and for a companion robot memory depth is not
a spare resource: recall is the product.

**R5 — Maintenance window is a recovery fallback, not a route.** It stays
available for the case R1 is supposed to prevent — a defect that makes existing
events genuinely unreadable. Using it requires a recorded decision naming the
defect and the expected outage. It must not become the default by omission.

### The availability trade, stated

This is a deliberate availability decision, not a database detail: **WM-2 keeps
the robot available across schema evolution, and pays for it in migration
discipline** — versioned encodings, alongside-rebuilds, and a plan review on
every migration. The alternative — a simpler implementation that accepts an
offline window — was rejected because a companion robot that is unavailable is
not degraded, it is absent, and the outage would recur on every schema change
for the life of the product.

R4 records the second half of the trade honestly: bounded retention means the
world model forgets on a schedule. That cost is accepted for storage and latency
reasons, which are real and measured, and explicitly **not** for migration
reasons, which D-13 shows are avoidable.

### Before this can be ruled on

1. ~~**Re-measure on target.**~~ **Done — D-14.** The two-axis ladder ran on the
   Jetson under both statements, `JETSON-TARGET-MEASURED`. Legacy is linear in
   each axis independently (31.70× over a 32× entity increase at a fixed log
   size); grouped is flat (1.33×). The product model holds on target.
2. ~~**Re-measure the grouped-pass rewrite end to end**, as an `UPDATE`.~~
   **Done — D-14.** Measured as the real `migrate_to_v2_using()` statement, not
   the `SELECT` D-13 timed: **472×** at 50 000 events / 1 000 entities and
   **1 184×** at 30 000 / 3 200, with the projection result and the chain
   identical. **R3 is satisfiable in practice.**
3. **Prototype R2's alongside-rebuild-and-swap** far enough to know what it
   costs in code, in peak disk, in write amplification and in cutover latency.
   Storage is the *smallest* of those: projections are 3.74 % of total store
   size, so a duplicate is ≈306 MiB (321 MB) at the 8 GiB ceiling rather than a
   second 8 GiB. **Measured 2026-08-04 on host (D-16) and on target (D-16a).
   Three of the four sub-costs are settled; write amplification is NOT, and
   cannot be on this target.** The protocol was specified and prototyped
   (`docs/design/WM2_PROJECTION_REBUILD_PROTOCOL.md`), answering the *code* cost
   and settling cutover ordering and the partial-projection state; the
   `rebuild` harness command then measured the other three against a control arm
   running identical ingest without the rebuild.

   | Sub-cost | Status |
   |---|---|
   | Code | **Closed** — the protocol, 319 lines, exhaustively tested |
   | Cutover latency | **Closed on target** — 2.33–2.48 ms, flat, ≈0.3 % of a rebuild (D-16a) |
   | Peak disk | **Closed, but not by a target measurement** — 2.58 % of store, a *deterministic* function of schema and workload, identical host and target (D-16) |
   | Write amplification | **OPEN, and unmeasurable on this target** — `/proc/self/io` absent (no `CONFIG_TASK_IO_ACCOUNTING`). Host says 2.8×–35.8×, a dial rather than a constant, and it **does not transfer** (D-16a) |

Items 1 and 2 are closed on target. **Item 3 is now three-quarters discharged
and the remaining quarter is the one that matters for flash wear** — the
obligation narrows to write amplification alone, accepted as open by ruling
rather than left unattempted. D-14 establishes that a migration *can* be cheap,
not that the alongside-rebuild protocol R2 specifies has been built.

### What was ruled on, and on what basis

| | |
|---|---|
| **The resolution** | R1–R5 above, accepted as a whole and unamended |
| **Target evidence for R3** | D-14 — the statement, not the store, sets the cost |
| **Accepted while still unevidenced** | R2's alongside-rebuild-and-swap (item 3), carried as an outstanding obligation; R1 and R4 are argued from existing measurements (the chain's construction, D-2, D-9) rather than newly measured |
| **Decided separately, same day** | ratification of ADR-0041 itself — see *Acceptance record* |

---

## Measurement harness

The instrument for the gates below exists:
[`tools/wm2-persistence-harness`](../../tools/wm2-persistence-harness/), with
the operator runbook at
[`docs/hardware/JETSON_WM2_PERSISTENCE_DRILL.md`](../hardware/JETSON_WM2_PERSISTENCE_DRILL.md).

**Its existence ratifies nothing and ticks nothing.** It is workspace-detached,
depends on `rusqlite` alone, and is covered by ADR-0039's Fence A as an extra
root — so it cannot become a dependency of anything, and a transport crate added
to it reds CI exactly as one added to `kirra-world` would.

Three properties of the harness are load-bearing for how its output may be used.

**It is not the store, and is built so it cannot quietly become one.** The
schema it benchmarks is a *stand-in* — this ADR states that column-level schemas
are deliberately not fixed, so anything concrete enough to measure is
necessarily invented. Every emitted record therefore carries the stand-in
schema's SHA-256, and when the real schema is ratified its digest will differ,
making an old measurement visibly about something else rather than quietly
authoritative. The harness also uses a local SHA-256 with a harness-only domain
tag rather than `kirra-audit-hash`, precisely so its bytes cannot be mistaken
for the on-disk format the store will owe.

**It refuses to let a host run be cited.** Following the `TBD-QNX-TARGET`
convention already used for governor timing, every record is stamped
`JETSON-TARGET-MEASURED` or `HOST-INDICATIVE-NOT-TARGET`. The first requires
both machine corroboration (aarch64, Tegra evidence, a durable filesystem under
the database, a release build) *and* an explicit operator assertion — neither
alone. A `tmpfs` path forfeits target status outright, because a run that never
fsyncs produces the best numbers the harness can emit while measuring none of
the property open question 1 is about.

**It turns §11.3's compaction policy into a mechanism.** Compaction-with-citation
is ratified above as policy while its mechanism is left open, and there is a
real problem in that gap: deleting events from a hash-chained append-only log
breaks the chain. The harness implements and measures one answer — a compacted
span becomes a `Summary` plus a `compaction_citations` row carrying the removed
range's digest and the chain digests on **both** sides, so verification links
the summary from one and resumes from the other. Nothing downstream is
re-chained and no history is rewritten. Nine §11.3 requirements are checked as
conditions rather than assumed, including the non-vacuity control that tampering
with a summary must break the chain.

The honest consequence, which any retention policy built on this must carry:
after compaction the *contents* of a removed span can no longer be verified,
only that a span was removed, how large it was and what it hashed to. Full
tamper evidence degrades to tamper-evident citation of a removed span. The
verifier reports `compacted_windows` and a time-travel query into a compacted
window returns a degraded summary rather than a value or a bare absence, so the
degradation is visible instead of silent.

**It tests the two disk-pressure claims that had never been exercised.** The
configuration table above asserts read-only degraded mode and clean disk-full
refusal; the harness's `pressure` stage checks both, including the one that
decides whether running out of space is recoverable at all — that a batch
refused for lack of room rolls back **whole**. A half-committed batch would tear
the generation sequence and fork the chain, converting a transient out-of-space
condition into permanent evidence corruption. It also times `VACUUM` separately
from compaction, since the two are now modelled as separate operations and the
scheduled one needs its own cost.

That stage surfaced a gap this ADR does not currently cover, recorded here
rather than left implicit: **a projection fold writes**, so it is refused under
pressure exactly as an append is. What a store that fills *mid-fold* leaves
behind is undefined — the projections would be partial with nothing marking them
as such, and a consumer could not distinguish a partial projection from a
complete one. The "no projection-only fact" rule does not address it. Open
question 7.

**It reports what it cannot establish.** The corruption gate is three tiers:
`SIGKILL` crash-consistency and WAL-loss prefix validity are automated; the
actual power cut is not, because nothing in software distinguishes an honest
`fsync` from a device cache that acknowledged and buffered it. An automated run
therefore reports that tier as `NOT-RUN` with the reason attached, so a results
file can never imply a durability test that did not happen. It reads `PASS` only
from a ledger of manually performed cuts, and only when those cuts are
**distinct armings** — five verifications of one power cut count as one (#1322,
and D-11).

---

## Ratification criteria

**Proposed. Measurement-gated.** Accepted only when **all** are recorded.
The right-hand column names the instrument, not a result — **no gate below is
satisfied, and none may be ticked from a `HOST-INDICATIVE-NOT-TARGET` run:**

| | Gate | Produced by | Status |
|---|---|---|---|
| [x] | **Measured Jetson prototype** — the log + projection path on target hardware, not a development host | the whole run, only when it reports `JETSON-TARGET-MEASURED` | **Met** — see *Target measurements* |
| [x] | **Replay benchmark** — full rebuild time at the assumed observation volume, with the checkpointed case measured separately | `replay` (also asserts rebuild-equals-incremental determinism, which gates the rest) | **Met** — `deterministic: true` |
| [x] | **Representative query benchmark** — one measurement per query family in blueprint §12 (point, set, graph, temporal) | `query`, plus a separate bitemporal point query the projection cannot answer | **Met — and adverse.** See D-1 |
| [x] | **Corruption / restart experiment** — power-loss-class behaviour, in the spirit of the existing audit-chain crash-consistency drill | `crash` tiers A and B; **tier C is manual** (drill §8) and this gate is not complete without it | **Met** — A and B PASS; **tier C `PASS`, 5 of 5** on target. See D-11 |
| [x] | **Storage growth estimate** — observations/day at realistic sensor rates, projected against the device budget | `growth` | **Met — and adverse.** See D-2 |
| [x] | **Migration proof of concept** — a schema change applied to a populated store, fail-closed on a future schema version | `migrate` | **Met — and adverse.** See D-6 |
| [x] | Scale assumptions confirmed or corrected; if materially wrong, Option B is re-evaluated before acceptance | the drill §9 sweep, emitting a **computed** verdict against this ADR's own reopening condition | **Met — and Option A survives.** Graph `SUBLINEAR` (0.45), temporal `LINEAR` (1.14), both on target. See D-9. What the deployed robot actually reaches remains an operational fact no benchmark produces |

**All seven measurement gates now read *Met*.** The last of them — tier C, the
one that could not be closed from software — was closed by five physical power
cuts on the target device (D-11).

### The gates were necessary, not sufficient — and both remaining conditions are now met

The checklist above is a *measurement* checklist. Its preamble says acceptance
requires all of it to be recorded; it does not say the measurements are the only
condition. Two further things were required, neither of them a measurement, and
both were satisfied on 2026-08-04 (*Acceptance record*):

1. **Open question 8 — migration strategy — was named "a blocker for acceptance
   rather than a note."** D-6 measured a whole-store offline migration at ~101
   minutes at the 8 GiB ceiling, which is not an OTA. D-13 and D-14 showed that
   figure described a quadratic backfill rather than an inherent cost. **Resolved
   by adopting R1–R5**, with R2's prototype carried as an outstanding obligation.
2. **The named deciders had not recorded approval.** **Recorded 2026-08-04** —
   one approver holding all three roles, stated as such in the *Acceptance
   record* rather than presented as three independent sign-offs.

Separately, the safety-scope determination in *Assurance impact* stays PENDING an
explicit safety-assurance ruling (ADR-0042 Decision 5). That governs what may be
claimed about scope, not whether this ADR is ratified, and it is unaffected here.

**Acceptance of this ADR does not authorize Kirra World domain implementation.**
The domain-logic gate (ADR-0042 Decision 5) is a separate and independent hold,
its ruling is still `PENDING`, and **nothing here releases it.** What this ADR
now authorizes is the persistence architecture itself, subject to the
outstanding R2 obligation.

---

## Target measurements

**Evidence:** `JETSON-TARGET-MEASURED`, `citable: true`, `blockers: []`.
NVIDIA Jetson Orin NX Engineering Reference Developer Kit Super, `aarch64`,
`ext4` on `/dev/nvme0n1p1`, release build, `rustc 1.94.1`, harness at
`git_commit 021ec82379be` (clean), `source_digest a0c2c1c870d6…`,
`standin_schema_digest 630eb690aaef…`, 100 000 events at a 96-byte payload.

> The `standin_schema_digest` is load-bearing when reading these numbers: they
> describe the harness's **stand-in** schema, not a ratified one. When the real
> schema lands its digest differs, and every figure below becomes a figure about
> something else.

The two questions this run existed to answer were both answered, and both
answers are adverse to the proposal as written.

### D-1 — the graph family is ~1 500× the point family, and worse on target than on host

| Family | Median | p99 | vs point | Share of a 100 ms (10 Hz) period, at p99 |
|---|---|---|---|---|
| `point_latest` | 7.7 µs | 14.9 µs | 1× | 0.01 % |
| `point_time_travel` | 25.6 µs | 40.8 µs | 3.3× | 0.04 % |
| `set_entities_in` | 11.6 µs | 18.0 µs | 1.5× | 0.02 % |
| **`graph_bounded_reach`** | **11.54 ms** | **13.44 ms** | **1 496×** | **13.4 %** |
| **`temporal_changes_since`** | **27.41 ms** | **58.68 ms** | **3 554×** | **58.7 %** |

The host-indicative ratio was ~230×. On target it is ~1 496× — **6.5× worse in
ratio, not merely slower across the board.** The point and set families are
fast enough to be uninteresting; the graph and temporal families are the entire
cost.

This is the measurement most likely to overturn the SQLite recommendation, and
it does not. It qualifies it:

- The proposal's claim that **graph shape belongs in an index, not the
  substrate** survives — but only if the index is real. A bounded-depth reach
  that costs 13.4 ms at p99 cannot be issued per perception tick.
- **Decision:** graph and temporal queries are **not** permitted on the
  synchronous path of any loop with a deadline. They are background or
  on-demand operations. If a future consumer needs bounded reach at tick rate,
  that is the trigger to re-evaluate Option B, not a reason to optimise this
  one.
- The 420 983 rows matched by `graph_bounded_reach` and 197 315 by
  `temporal_changes_since` indicate the cost is fan-out, not per-row overhead.
  A depth or result bound is a design requirement, not a tuning knob.

### D-2 — 458.5 B/event means retention and sampling are mandatory, not deferred

`bytes_per_event: 458.50624` log-only, `476.32384` with projections.
Against an 8 GiB budget at the assumed 864 000 events/day (10 Hz):

| | Events | Days to fill 8 GiB |
|---|---|---|
| Log only | 18 734 608 | **21.7** |
| With projections | 18 033 812 | **20.9** |

Inverting it is the decision-forcing form — **the maximum sustained event rate
for a given retention horizon on 8 GiB**:

| Retention horizon | Max sustained rate |
|---|---|
| 30 days | 7.2 events/s |
| **90 days** | **2.4 events/s** |
| 180 days | 1.2 events/s |
| 365 days | 0.6 events/s |

10 Hz sustained is viable for **three weeks**. Any longer horizon requires the
event rate to come down by roughly 4× (90 days) or more. **Sampling/coalescing
is therefore a design requirement of WM-2, not a later optimisation**, and the
"observations/day at realistic sensor rates" assumption in *Provisional scale
assumptions* is now measured rather than assumed.

Compaction recovers ~50 % (D-4), which roughly doubles the horizon for
compactable regions — but compaction is **lossy** (`DegradedSummary`), so it
buys retention of *summaries*, not of observations. It does not change the
sampling conclusion.

### D-3 — reclamation cannot be scheduled naively, and bounds the store size

`vacuum_ms 57.34` on a 22.9 MB store → **~399 MB/s processed**.
`transient_overhead_ratio 1.294`, `transient_overhead_bytes 6.73 MB`.
`temp_dir /var/tmp`, `ext4`, `temp_fs_is_volatile: false`,
`temp_on_same_fs_as_db: true`.
`concurrent_appends_blocked: 2 of 2`, `max_append_stall_ms 50.87`.

Three decisions follow:

1. **The tmpfs hazard is not present in this configuration.** `/var/tmp` on
   this Orin is `ext4`, so the `VACUUM` copy is not built in RAM. This is a
   property of *this* device's layout, not of Jetsons, and must be re-checked
   per deployment — the harness reports it precisely so it is checked rather
   than assumed.
2. **The free-space reserve is ~1× the database size**, not the measured
   0.294×. The measured overhead scales with what will *remain*, so the worst
   case is a store with nothing to reclaim. **Consequence: an 8 GiB store on an
   8 GiB budget can never be vacuumed.** The practical database ceiling is
   about **half the partition**, or reclamation must become incremental. This
   is a hard constraint on the deployment layout, and it was not visible before
   measurement.
3. **Reclamation is a total write blackout for its duration.** Every concurrent
   append was blocked (2/2), longest stall 50.9 ms on a 22.9 MB store. Scaled
   to a full store the `VACUUM` runs **~21.5 s**, during which the world model
   records nothing. At 10 Hz that is ~215 unrecorded observations.
   **Reclamation therefore requires the robot to be stationary and
   out-of-mission — it is not an idle-time background task**, which is what
   R8's precondition table already required and this now quantifies.

### D-4 — compaction with citation holds on target

49 486 of 100 000 events compacted into 516 cited windows; **49.83 %
reclaimed** after `VACUUM`; 1 206 ms to compact, 243 ms to re-verify. All nine
§11.3 conditions hold on target, including the non-vacuity control
`tampered_summary_breaks_chain: true` — without which the other eight would be
worthless.

`compaction windows: 516` for ~49 k events implies a mean window of ~96 events.
Compaction throughput is ~41 k events/s; re-verification of the whole chain
costs 243 ms. Both are cheap relative to the `VACUUM` that must follow, which
reinforces R8: the two operations have different costs and different
preconditions, and must be scheduled separately.

### D-5 — checkpointing is mandatory, not an optimisation

Cold rebuild 324.9 ms per 100 k events; checkpointed resume 19.1 ms — **17×**.
`deterministic: true` (rebuild equals incremental), which is the property the
rest of the replay argument rests on.

Extrapolated to a full 18.7 M-event store, a cold rebuild costs **~61 s of
boot**. **Decision: projection checkpoints are a WM-2 requirement.** A store
that can only be rebuilt cold does not meet any plausible availability target
on this hardware.

### D-6 — schema migration does not scale, and this is the sharpest finding

50 000 events migrated v1→v2 in **16.24 s** — 0.325 ms/event.
`future_schema_refused: true`, `chain_intact_after: true` (the fail-closed
policy holds on target).

Extrapolated linearly:

| Store size | Offline migration |
|---|---|
| 1 M events | 5.4 min |
| 5 M events | 27.1 min |
| 18.7 M events (full 8 GiB) | **101 min** |

> **The 16.24 s measurement stands; the extrapolation below does not.** The
> table extrapolates **linearly in events** with the entity count pinned at the
> measured 1 000. A host ladder over both axes (D-13) finds migration cost is the
> **product** of events and entities, because the backfill's correlated subquery
> rescans every observation event for every projection row. Under constant
> density — D-8's own rule — the full-store figure is far worse than 101 minutes,
> and rewritten as a single grouped pass the same migration is orders of
> magnitude *better*. Read the row below as "an offline whole-store backfill is
> not viable", which is the conclusion it supports; do not plan against the
> minutes. **The target re-measurement is done: D-14 reproduces this figure
> within 1.46 % and measures the corrected statement 472–1 184× faster.**

**A whole-store offline migration is not viable on this hardware.** An OTA that
takes 101 minutes with the world model unavailable is not an OTA. Three routes
exist and WM-2 must choose one before acceptance:

- bound the store so migrations stay minutes (which D-2's sampling decision
  tends to do anyway);
- make migrations **lazy/online** — new schema written forward, old rows
  migrated on read or in the background;
- accept a documented maintenance window, which conflicts with a robot that is
  expected to be available.

This is now **open question 8**, and it is a blocker for acceptance rather than
a note.

### D-7 — the two SQLite-config claims hold on target

The SQLite-configuration table asserts read-only degraded mode and clean
disk-full refusal. Both were previously exercised only on a development host.
On target, all seven checks hold:

| Check | Result | |
|---|---|---|
| `write_refused` | true | the append past the cap errored rather than silently succeeding |
| `refusal_is_disk_full` | true | `"database or disk is full"` — the error names the condition |
| **`partial_batch_rolled_back`** | **true** | **the one that matters most** — a half-committed batch would tear the generation sequence and fork the chain, turning a recoverable out-of-space condition into permanent evidence corruption |
| `chain_intact_after_refusal` | true | |
| `reads_serve_while_full` | true | read-only degraded mode: log *and* projections still answer |
| `recovers_when_space_returns` | true | full is a condition, not a state the store is stuck in |
| `chain_intact_after_recovery` | true | recovery does not fork the chain |

`projection_rows_while_full: 100` — non-zero, so `reads_serve_while_full` was
answered from real projection rows rather than passing vacuously on a store with
nothing to serve.

**The honest limit is unchanged by running on target.** `PRAGMA max_page_count`
(here `page_cap: 247`, filling after 2 000 events) exercises SQLite's full-
*database* path, not the filesystem's `ENOSPC` path, and not what an Orin does
when `/dev/nvme0n1p1` is genuinely at 100 % and every other process sharing that
mount is failing too. This establishes that the store refuses cleanly; it does
not establish that the device stays healthy. Confirming the second still needs a
deliberately filled partition — and open question 7 (partial projections under
pressure) remains open regardless, because a fold that is *interrupted* is a
different case from one that is refused.

### D-8 — the reopening condition is now decidable, and the obvious sweep was wrong

This ADR states the condition under which it should be abandoned — *"entities in
the millions or genuinely unbounded ad-hoc traversal"* — and the drill
previously discharged it with a shell loop and the instruction to *look at* how
`graph_bounded_reach` grows. Whether a curve has a knee is precisely the
judgement that gets made favourably under deadline and re-made unfavourably by
an assessor a year later, from the same numbers. It is now a function
(`sweep`, drill §9) with a fail-closed verdict: `SUBLINEAR` / `LINEAR` /
`SUPERLINEAR` / `KNEE` / `INSUFFICIENT`, where both `KNEE` and `INSUFFICIENT`
exit non-zero.

**Building it exposed a defect in the sweep the drill specified.** Sweeping
entity count at a *fixed total event count* reduces observations-per-entity as
the ladder rises, so graph fan-out — and cost — go **down**. On a host ladder of
100/1 000/10 000 entities at a fixed 20 000 events the medians were 1 687 µs →
73.7 µs → 10.9 µs, a log-log slope of **−1.10**.

That ladder would have reported excellent sublinear scaling and closed this
gate, while measuring density rather than scale. It is the most dangerous shape
available here, because it looks like unusually good news.

The sweep therefore holds observations-per-entity **constant** by default
(`--events-per-entity`, deployment-realistic: more entities means more
observations, not the same observations spread thinner), and the classifier
**refuses** a steeply falling curve as `INSUFFICIENT` with the reason and the
fix rather than praising it.

What this changed was that the gate could then be closed with a verdict instead
of an impression. **It since has been — see D-9**, which records the target
sweep and its two verdicts. This section is the account of how the instrument
came to be trustworthy; D-9 is the result it produced.

### D-9 — the scale gate closes: Option A survives on target

Evidence: `docs/evidence/wm2-jetson-scale-stall-20260803/`,
`JETSON-TARGET-MEASURED`, harness `ba818b0b22b3`, constant density at 100
observations per entity.

| Entities | Events | `graph_bounded_reach` median / p99 | `temporal_changes_since` median / p99 |
|---:|---:|---:|---:|
| 1 000 | 100 000 | 11.46 / 13.56 ms | 28.44 / 61.64 ms |
| 10 000 | 1 000 000 | 50.92 / 71.43 ms | 427.76 / 751.68 ms |
| 100 000 | 10 000 000 | **91.95 / 158.95 ms** | **5 536.23 / 10 503.70 ms** |

| Family | Verdict | Overall slope | Segments |
|---|---|---:|---|
| `graph_bounded_reach` | **`SUBLINEAR`** | **0.45** | 0.65 → 0.26 |
| `temporal_changes_since` | **`LINEAR`** | **1.14** | 1.18 → 1.11 |

**Neither curve bends upward.** The reopening condition in *Provisional scale
assumptions* — "entities in the millions or genuinely unbounded ad-hoc
traversal" — is not met at 100 000 entities and 10 M events. The graph
exponent *falls* along the ladder. Option A stands, and the claim that graph
shape belongs in an index rather than the substrate survives its sharpest test.

**The verdict is about shape, and the shape is not the whole story.** At the top
rung 100× the entities costs 8.0× (graph) and **195×** (temporal) the time.
Against a 100 ms (10 Hz) period, the p99s are **1.6×** and **105×**
respectively. A 10.5-second p99 is linear *and* unusable interactively; the two
are not in tension, and reading `LINEAR` as reassurance would be the error this
row exists to prevent.

What is still not established is what entity counts a deployed robot actually
reaches. That is an operational fact no benchmark produces, and this ADR said
so before the measurement. The gate is met; the assumption is not thereby
confirmed for all time.

### D-10 — the stall theory is rejected, and the mechanism is partly unresolved

Six configurations, 20 repetitions each, all exiting 0.

| Config | Stalls | Worst commit | Median throughput | Attribution |
|---|---:|---:|---:|---|
| `FULL` batch 1 | 0/20 | 18.75 ms | 3 099 ev/s | `NO-STALL` |
| `FULL` batch 64 | **2/20** | **19 644.91 ms** | 31 497 ev/s | `UNATTRIBUTED` |
| `NORMAL` batch 1 | **1/20** | **12 864.32 ms** | 5 143 ev/s | `IO-DEVICE` |
| `NORMAL` batch 64 | 0/20 | 63.16 ms | 19 351 ev/s | `NO-STALL` |
| `OFF` batch 1 | 0/20 | 29.83 ms | 15 079 ev/s | `NO-STALL` |
| `OFF` batch 64 | 0/20 | 4.00 ms | 55 275 ev/s | `NO-STALL` |

**O-1's theory is rejected.** The 29.27 s event was recorded under
`NORMAL`/batch=64, which made that configuration the suspect. Twenty repetitions
of exactly it produced **zero** stalls, worst commit **63 ms** — three orders of
magnitude below the original. The stalls appeared in `FULL`/batch=64 and
`NORMAL`/batch=1 instead, neither of which was suspected. A single observation
had pointed at the wrong variable, which is what a single observation is prone
to do.

**Classification: intermittent, block-device/environment-correlated, mechanism
partly unresolved.**

- **Intermittent** — 3 in 120 (2.5 %). Rare enough that a single run misses it,
  as four of six configurations here did.
- **Not durability-specific** — it crossed both fsyncing modes and both batch
  sizes. `synchronous` is the only variable the harness controls, so a stall
  indifferent to it is not explained by it.
- **Never seen under `OFF`** — 0/40 against 3/80 for the fsyncing modes.
  Suggestive that fsync is involved; three events cannot carry that claim, and
  it is recorded as a hypothesis rather than a finding.
- **Thermal ruled out by measurement.** Hottest zone across all six runs:
  **59.6 °C**, against the 85 °C threshold. Not an assumption — a reading.
- **One attribution, held loosely.** `IO-DEVICE` on `NORMAL`/batch=1 (block
  layer busy, no large dirty backlog → NVMe garbage collection or an SLC-cache
  cliff). `UNATTRIBUTED` on `FULL`/batch=64, which is the expected outcome and
  not a failure.

Two measurement limits bound how far that attribution can be pushed. **PSI was
unavailable on this kernel** (`psi_io_stall_us` is `None` in every record), so
one of the two I/O signals was simply absent. And **the counter delta is taken
across a whole repetition while the stall is a single commit**, which can
over-state I/O evidence.

> **The second limit is now fixed in the instrument, and the `IO-DEVICE`
> attribution above is WITHDRAWN — 2026-08-04.**
>
> The limitation was worse than "can over-state". `attribute_stall` tested
> `disk_io_ms >= stall_ms * 0.5` with `disk_io_ms` accumulated over the whole
> repetition and `stall_ms` a single commit — a **denominator mismatch**. A
> repetition with a few seconds of ordinary device busy-time clears that bar for
> any stall in this range *regardless of whether the device did anything during
> the stall itself*. So `IO-DEVICE` on `NORMAL`/batch=1 is not a weakly-supported
> finding; it is a number the pre-fix instrument would have produced from an idle
> device, and it carries no information about the mechanism.
>
> `stall.rs` now samples a timestamped series at 20 ms and computes the delta
> across **the slowest commit's own window** (`bench::CommitWindow`), and
> attribution **refuses** when the sampled span is materially wider than the
> stall it claims to describe. The stall counts, worst-commit latencies,
> throughput medians and the thermal reading in the table are unaffected — they
> never depended on the window. Only the attribution column does.
>
> **This is an instrument correction, not a new measurement.** Nothing here says
> the device was idle during that stall; it says the run cannot tell. Re-running
> the six configurations on target with the fixed instrument is the open work —
> see open question 9.

**The stall-robust medians resolve the corrupted benchmark row** — and expose
something else. `NORMAL`/batch=64 reads 19 351 ev/s across 20 runs against the
original 3 123, confirming that row was a stall artefact. But at batch=64 the
medians run `OFF` 55 275 > `FULL` 31 497 > `NORMAL` 19 351: **`NORMAL` slower
than `FULL`**, from a configuration with zero stalls. No durability model
predicts that. It is not a tail artefact and it is not explained; open question
1 should not be settled at batch=64 until it is.

### D-11 — the durability gate closes: five power cuts, no acknowledged write lost

**Evidence:** [`docs/evidence/wm2-jetson-tierc-20260803/`](../evidence/wm2-jetson-tierc-20260803/),
self-verifying via `sha256sum -c SHA256SUMS`. Jetson Orin NX, `ext4` on
`/dev/nvme0n1p1`, a **fresh** database at `/var/lib/kirra/wm2/powercut-v2.sqlite`,
harness at `6eaeb643e3f8`.

The last gate, and the only one no software could produce. `SIGKILL` leaves the
page cache intact and tier B discards the whole WAL; neither can tell a
filesystem that honoured `fsync` from a device cache that acknowledged the write
and buffered it. Five physical cuts at the power source:

| Trial | Arm id | Fsynced prefix | Recovered | Chain | |
|---:|---|---:|---:|---|---|
| 1 | `08c190de…` | 400 | 3 562 064 | intact | PASS |
| 2 | `87c2f1a5…` | 3 562 464 | 4 086 048 | intact | PASS |
| 3 | `ea795da4…` | 4 086 448 | 4 463 024 | intact | PASS |
| 4 | `b28ce3b1…` | 4 463 424 | 4 632 000 | intact | PASS |
| 5 | `f565d7f9…` | 4 632 400 | 4 748 880 | intact | PASS |

`tier C after 5 arming(s) across 5 row(s): PASS`. `PRAGMA integrity_check: ok`.

**In every trial the recovered log was at least as long as the fsynced prefix,
and the chain verified end to end. This device does not acknowledge writes it
has not persisted** — which is the one failure tiers A and B cannot detect, is a
common failure on embedded storage, and no `synchronous` setting compensates for
it. Some un-fsynced tail survived each cut too, which is why `recovered` exceeds
the prefix; losing all of it would have been equally correct.

**The series is checkable, not merely asserted.** Each arming appends its
400-event prefix from `MAX(generation) + 1` of whatever survived the previous
cut, so consecutive rows must satisfy `prefix(n+1) = recovered(n) + 400`. That
holds exactly four times over. Five genuinely independent cuts on a store that
carries forward produce a strictly increasing, gap-free chain of boundaries, and
a replayed marker cannot.

**The prior attempt counted as 1 valid cut, not 3.** An earlier series on
`powercut.sqlite` recorded three `PASS` rows from a *single* physical cut:
`powercut arm` restarted at generation 0, so the second arming died on a
primary-key collision while the first marker survived, and each later `verify`
re-read the same surviving store and appended another pass. Fixed in #1322,
which made a trial mean an *arming* and made the aggregate count distinct arm
ids. Those rows predate arm ids and the corrected harness refuses them as
unattributable, so the exclusion is enforced by the instrument rather than by
recollection. The bundle above is a fresh database and a fresh ledger.

**What this does not establish.** One device, one NVMe — durability is a property
of *that* medium and does not transfer to eMMC or microSD, where lying write
caches are most common. Five cuts clear the drill's floor but cannot bound a
failure *rate*; they cannot distinguish "never" from "rarely". And the store was
the stand-in schema, as everywhere else in this section.

### D-13 — migration cost is the product of events and entities, and it is a property of the SQL

> **`HOST-INDICATIVE-NOT-TARGET`, `citable: false`.** This finding closes no
> gate and is not target evidence. Evidence:
> [`docs/evidence/wm2-host-migration-ladder-20260804/`](../evidence/wm2-host-migration-ladder-20260804/).
> What transfers across architectures is the **shape** and the query plan, both
> algorithmic; the constants do not. **Target re-measurement is now done —
> see D-14, which confirms the product model and the query plan on the Jetson.**

D-6 extrapolated migration linearly in events with entities pinned at 1 000. A
two-axis ladder says that model is wrong:

| Axis | Held fixed | Varied | Behaviour |
|---|---|---|---|
| A | entities = 1 000 | events 6 250 → 50 000 | linear in events (µs/event flat, 200 → 215) |
| B | events = 50 000 | entities 125 → 8 000 | **linear in entities** (ms/entity flat, 11.9 → 11.3 across 64×) |

`k = ms / (events × entities)` is flat within ±10 % across a **64×** entity
spread and an 8× event spread, so **`migration_time ≈ k · events · entities`** — growing
with the *square* of store size when both axes grow together, which is what
constant density means.

**The cause is the backfill's query plan**, not the store:

```
SCAN entities_projection USING COVERING INDEX …
CORRELATED SCALAR SUBQUERY 1
  SEARCH world_events USING INDEX idx_events_kind (kind=?)
```

The planner keys the subquery on `kind='observation'` alone, so every projection
row walks every observation event; `idx_events_subject_valid` goes unused. The
same aggregate as one grouped pass, same database, same 7 955 rows:
**93 132 ms → 30 ms, a 3 100× difference.**

Two consequences, and the second matters more than the first:

1. **The 101-minute figure understates the offline route.** Carrying the host `k`
   to a full store at D-8's 100 events/entity (18.7 M events, ~187 k entities)
   gives days, not minutes — the same *diluting-ladder* error D-8 caught in the
   sweep, appearing again in the migration extrapolation.
2. **Migration cost is a property of the migration statement.** The same schema
   change, written as a grouped pass, is O(events) and finishes in seconds. So
   "how long does a migration take" has no answer at the level of the store —
   only at the level of the SQL. **A strategy chosen from a single migration's
   measured cost would be chosen from an artifact.**

That is why the resolution proposed for open question 8 constrains what a
migration is *allowed to do*, rather than picking a downtime budget to live with.

**The general rule this and the tier C defect share** is recorded in
[the Jetson drill](../hardware/JETSON_WM2_PERSISTENCE_DRILL.md), §4 *Reading the
results*: *every reported number must state its counting unit, its independence
unit, the variables held fixed, and the claim it is allowed to support.* Both
errors were real measurements under a label they had not earned — "rows" read as
"independent trials", "events at fixed entities" read as "store-size scaling" —
and a checksum verifies bytes, not interpretation.

### D-14 — on target: the cost is the statement, and the corrected one is 472–1 184× faster

**Evidence:** [`docs/evidence/wm2-jetson-migration-ladder-20260803/`](../evidence/wm2-jetson-migration-ladder-20260803/),
`JETSON-TARGET-MEASURED`, `citable: true`, `blockers: []` on all 24 records.
Harness `29aa1b2496e9`, `source_digest ec580e2c…` **verified by rebuilding from
that commit**. Self-verifying via `sha256sum -c SHA256SUMS`.

D-13 established the cost *shape* on a development host. This is the same
two-axis ladder on the Jetson, run under both statements.

| Axis | Held fixed | Varied | legacy | grouped |
|---|---|---|---|---|
| A | entities = 1 000 | events 5 000 → 50 000 | `−109.9 ms + 322.1 µs·events` (R² 0.9995) | `1.39 ms + 0.652 µs·events` (R² 0.9995) |
| B | events = 30 000 | entities 100 → 3 200 | `90.2 ms + 9.268 ms·entities` (R² 0.9999) | `19.12 ms + 1.97 µs·entities` (R² 0.9060) |

**Over a 32× increase in entity count at a fixed log size, legacy grows 31.70×
and grouped grows 1.33×.** Legacy is linear in each axis independently — the
signature of a cost proportional to their product, confirming D-13 on target.
Grouped's per-entity term is **4 700× smaller** (1.97 µs against 9.268 ms).

Grouped/axis B fits at **R² 0.9060**, well below the other three. That is
expected rather than concerning — its entity term contributes ~6 ms across the
whole sweep against a ~19 ms floor, so the residual is measurement noise, not a
missing term — but it is quoted here rather than left out, because a table that
prints R² for three rows and omits it for the fourth is a table that has chosen
which numbers to show.

| Configuration | legacy | grouped | speedup |
|---|---:|---:|---:|
| 50 000 events, 1 000 entities | 16 003 ms | 33.9 ms | **472×** |
| 30 000 events, 3 200 entities | 29 674 ms | 25.1 ms | **1 184×** |

Both arms produce the same projection and leave the chain intact
(`chain_intact_after: true`, `future_schema_refused: true`, all 24 records). The
only difference is the query plan.

**The run also reproduces D-6.** Its legacy arm reads 16 003 ms at 50 000
events / 1 000 entities against D-6's archived 16 240 ms — **1.46 % apart**, on
a later commit and a fresh database. That corroborates the original target
measurement *and* confirms the legacy statement here is the one D-6 measured, so
the comparison is like-for-like.

**Consequently the 101-minute figure is retired as an architecture input.** It
measured a quadratic backfill, not an inherent migration cost, and the same
schema change written as one grouped pass is three orders of magnitude cheaper
on the same hardware. This is the target evidence for **R3**, and it removes the
premise the offline-maintenance-window route rested on.

Honest scope: one device, one medium, one migration statement, **a single sample
per rung** (D-10 measured rare multi-second stalls on this device at 2.5 %, which
a single sample cannot exclude), and the 5 000-event legacy rung is a cold-start
outlier excluded from that one fit. The bundle README carries these, and carries
the full-store extrapolation (~12.8 days versus ~12.6 s) explicitly marked as an
order of magnitude rather than planning evidence — it runs ~374× beyond the
largest rung, the same overreach D-6 was corrected for.

### D-15 — the stall is an NVMe completion the host never acted on, and it is not a persistence property

Six configurations, 20 repetitions, 100 000 events — **D-10's protocol exactly**,
re-run on target with the windowed instrument (PR #1332). Evidence bundle:
`docs/evidence/wm2-jetson-oq9-rerun-20260804/`, `JETSON-TARGET-MEASURED`.

**5 stalls in 120 repetitions, and 5 NVMe command timeouts in the kernel log.**

```
[Tue Aug  4 09:52:54 2026] nvme nvme0: I/O 719 QID 6 timeout, completion polled
[Tue Aug  4 10:38:47 2026] nvme nvme0: I/O 510 QID 6 timeout, completion polled
[Tue Aug  4 10:42:05 2026] nvme nvme0: I/O 817 QID 5 timeout, completion polled
[Tue Aug  4 10:43:03 2026] nvme nvme0: I/O 690 QID 6 timeout, completion polled
[Tue Aug  4 10:43:35 2026] nvme nvme0: I/O  56 QID 5 timeout, completion polled
```

The grouping matches too: one early event (`FULL`/b1, which recorded 1 stall)
and four clustered inside five minutes (`NORMAL`/b1, which recorded 4). **Zero
resets, zero I/O errors, zero aborts** — no command ever failed.

Three measurements identify the mechanism.

1. **The duration is a constant.** `nvme_core.io_timeout` is **30**, and the two
   stalls were 30 019.4 ms and 30 182.4 ms — the timeout plus **19.4 ms** and
   **182.4 ms** of handler latency. A timeout-bounded wait cannot come in under
   the timeout and exceeds it only by handling delay.
2. **The device was idle while the host waited.** These are the first stalls
   measured over *their own window* (1 412 and 1 418 samples at 20 ms). Device
   busy-time inside the window: **2.12 %** and **1.05 %** of the stall. On the
   non-stalling rows the same counter reads **74–100 %** of its window.
   Normally this drive is saturated; during the stalls it did essentially
   nothing.

   > **Corrected.** An earlier revision of this paragraph said 107–214 %, from
   > dividing `disk_io_ms` by `worst_commit_ms`. The delta is accumulated over
   > `counter_window_ms`, so that is the only denominator it can be divided by;
   > on the non-stalling rows the window is much wider than the commit, which
   > inflated the figure past 100 % and prompted a claim that the metric exceeds
   > wall time. It does not, in this data. The stall rows are unaffected —
   > there the window and the commit differ by a few milliseconds.
3. **"completion polled" names the failure.** The timeout handler fired at 30 s,
   polled the completion queue, and found the command *already complete*. The
   handler only runs because the command was still outstanding at the timeout,
   so this is a **consequence of the message rather than an inference**: the
   completion was not delivered by the normal interrupt path within 30 s, and
   the device had produced it.

   What the run does **not** discriminate is whether that completion was *lost*
   — recovered only by the poll — or merely *delayed*, arriving near the
   timeout. The 8 496 ms stall below is consistent with delayed delivery. The
   heading says "never acted on" rather than "lost interrupt" for that reason.

Writeback is excluded (peak dirty+writeback 1 220 kB and 4 216 kB against a
262 144 kB threshold) and thermal is excluded (58.0–58.2 °C against 85 °C). Both
stalls report `UNATTRIBUTED`, and here that is **positive evidence**: everything
the harness can observe says nothing was happening.

**Durability is unaffected.** Every timed-out command had completed — nothing
was lost, nothing was retried. This is a latency and availability defect, and
D-11's five power cuts stand independently of it.

**What is not established** is the *root* cause of the lost interrupt. The device
identifies as **Realtek `10ec:5765`** (`nvme id-ctrl`: vid/ssvid `0x10ec`, IEEE
OUI `00e04c`, model `SSD NVME 256GB`, FW `VC400622`) — an RTS5765-class
**DRAM-less** controller, which the boot log corroborates: `nvme nvme0:
allocated 64 MiB host memory buffer`. A DRAM-less controller keeps its mapping
tables in host RAM over HMB, so it sustains materially more host-side DMA than a
DRAM-equipped drive. That is a **lead, not a conclusion**: candidates remain
controller firmware, the HMB path, PCIe ASPM power-state transitions, and MSI-X
routing on the Tegra host controller, and this run does not discriminate between
them.

**Refinement, same day — the timeout bounds RECOVERY, it is not the cause, and
sub-timeout stalls of the same signature exist.**

A follow-up run of `NORMAL`/b1 (20 repetitions) recorded **1 stall of 8 496 ms**
with the device **0.61 % busy** across a usable 8 502 ms / 400-sample window —
the same host-waiting-on-an-idle-device signature — and **no kernel timeout
entry**, correctly, because 8.5 s never reached the 30 s bound.

That reframes the finding. The three measurements above establish that the five
30 s stalls were *recovered* by the timeout handler; they do not establish that
30 s is intrinsic to the fault. The 8 496 ms event shows the underlying stall can
resolve on its own, so:

> **The fault is a lost or delayed completion. `io_timeout` is the backstop that
> bounds how long the host waits for one, not the thing that causes the wait.**

A mechanism consistent with both, offered as **hypothesis**: NVMe completion
processing drains the whole completion queue, so a stranded completion can be
picked up by any later interrupt on the same queue. At batch=1 the harness has
nothing else in flight, so recovery depends on unrelated system I/O landing on
that queue — arriving early gives an 8.5 s stall, never arriving gives 30 s. This
predicts that stall durations below the timeout should be *distributed* rather
than clustered, which the two runs are consistent with but cannot confirm at
n=6.

**Consequence for mitigation:** lowering `io_timeout` caps the worst case but
cannot remove stalls, because events shorter than the timeout do not touch it.
An attempt to lower it on the bench also **failed silently** — writing
`/sys/module/nvme_core/parameters/io_timeout` set the parameter to 5 while the
live queue stayed at 30 000 ms, since the value is latched at namespace probe.
See `AOU-WM2-STORAGE-COMPLETION-001`.

**Mitigation measured — the model holds at a second timeout value.** With
`/sys/block/nvme0n1/queue/io_timeout` genuinely set to **5 000 ms** (the
per-queue file, verified before *and* after the run), `NORMAL`/b1 over **60**
repetitions produced:

| Prediction, stated before the run | Result |
|---|---|
| Stalls persist — capping recovery does not stop completions being lost | **2/60** |
| Worst commit caps near 5 000 ms plus handler latency | **5 254.5 ms = 5 000 + 254.5** |
| Fresh kernel timeout entries dated inside the run | **3 new** (12:07:00, 12:15:01, 12:15:15) |

The stall tracks whatever `io_timeout` is set to, now observed at **two
different values** — a considerably stronger test than the original correlation.
Worst case 30 s → 5.25 s is a **5.7×** cut in the observation gap (~300 → ~53
observations at 10 Hz). The mitigation is a **sysfs setting that does not
survive reboot**; the persistent form is `nvme_core.io_timeout=` on the kernel
command line, which applies at probe.

Three results the predictions did not cover, recorded as observations:

1. **Three kernel timeouts, two counted stalls.** 12:15:01 and 12:15:15 are 14 s
   apart, inside one ~20 s repetition. `stalls_observed` counts *repetitions
   that stalled*, so two stalls in one repetition collapse to one —
   **`stalls_observed` undercounts stalls**, demonstrated here on live data.
2. **The rate appears to fall and the model does not explain it.** 4/20 → 2/60,
   Fisher exact two-sided **p = 0.032**. A timeout value should change how long
   a lost completion takes to recover, not how often one occurs. Two reasons not
   to read this as the mitigation working: the "2" undercounts (point 1), and
   this configuration is demonstrably unstable between runs (point 3).
   **Unexplained.**
3. **Device busy inside the stall window rose to 34.3 %** (1 804 ms of 5 254 ms),
   against 1.0–2.1 % for the 30 s stalls. This is a **live risk to the
   instrument**: `IO_BUSY_FRACTION` is 0.5, so a stall of this kind is now one
   modest step from being attributed `IO-DEVICE` — reinstating exactly the false
   attribution the windowing fix removed, this time from a correctly-windowed
   measurement. The threshold was calibrated against 30 s stalls and does not
   obviously transfer to shorter windows, where background I/O is a larger
   fraction of the span.

   **Addressed 2026-08-04, by a control rather than a recalibration.** The
   attribution now also asks whether the device was *at least as busy as usual*,
   compared with a baseline measured on the same device in the same run with the
   stall window's share removed. That is what `IO-DEVICE` claims, and it is
   self-calibrating: the same metric on both sides, so window length cancels.

   **So the risk described above no longer applies as stated.** Where a baseline
   can be drawn, `IO-DEVICE` requires **both** tests — the 0.5 absolute bar
   *and* at-or-above baseline — so clearing 0.5 is no longer sufficient, and a
   5 s stall of the kind measured here is refused on the baseline arm at ~0.40.
   The 0.5 bar only decides alone in the **fallback** case, where no baseline is
   available, and there the verdict discloses that (`NO BASELINE`). The
   paragraph above is retained as the record of why the control was added, not
   as a live risk.
   Against it the three measured stalls sit at roughly **0.02, 0.01 and 0.40** of
   baseline — a wide margin rather than the coin flip the absolute test had
   become. Deeper than the calibration problem: `/proc/diskstats` field 13
   accumulates per-I/O *service time*, so the ratio has no fixed ceiling and 0.5
   was never a probability.

   **The absolute constant was NOT recalibrated, deliberately.** One short-window
   measurement cannot re-derive it, and the known failure mode is a *false*
   `IO-DEVICE`, so the relative arm is applied **in conjunction** — it can only
   remove verdicts, never add them. `IO_BUSY_FRACTION = 0.5` remains an
   uncalibrated constant that no longer decides anything on its own; where no
   baseline can be drawn, the verdict stays reachable but is reported as
   `NO BASELINE: absolute test only, the weaker evidence`.

**`NORMAL`/b1's median throughput is bimodal across four target runs** — 5 143
(D-10), 9 485, 9 821, 5 006 ev/s — two clusters roughly 2× apart. A median is
robust to stalls, so this is not a tail artefact. It was recorded above as an
unexplained +84 % against D-10; with four points it is better described as a
**bistable configuration**, and it remains unexplained.

**No rate law is claimed.** The events concentrate where more I/O is issued —
all five at batch=1, none at batch=64, and `OFF` now at **0 stalls in 80
repetitions** across both runs. But five events cannot support a rate model, and
the simplest version fails: `NORMAL` fsyncs *less* than `FULL` in WAL mode yet
recorded four stalls against one.

**Two things moved against D-10 and are recorded rather than explained.** Five of
six throughput medians reproduce within 3 %, which makes the comparison
like-for-like; `NORMAL`/b1 did not, going 5 143 → 9 485 ev/s (**+84 %**) while
recording *more* stalls, which is the wrong direction for a stall artefact since
a median is robust to them. And the stall distribution shifted: D-10 saw 3/120
at `FULL`/b64 (2) and `NORMAL`/b1 (1); this run saw 5/120 at `FULL`/b1 (1) and
`NORMAL`/b1 (4). Both runs agree only that `OFF` never stalls.

**The batch=64 inversion reproduced** — `OFF` 54 636 > `FULL` 30 545 >
`NORMAL` 19 881, the same ordering as D-10 with all three within 3 %, from
configurations with zero stalls in both runs. That is a second independent
observation of an effect no durability model predicts, so it **strengthens**
open question 1's blocker rather than removing it.

**Platform caveat.** The filesystem holding the test database reports `clean
with errors` with `Errors behavior: Continue`, unchecked in 13 months and 50
mounts. It does not affect this finding — the failure is in the NVMe driver,
below the filesystem, and the mechanism rests on kernel-log correlation — but a
durability evidence platform configured to continue past filesystem errors is a
gap to close before the next evidence run.

### D-16 — R2's alongside rebuild costs a *dial*, not a number: write amplification 2.8×–35.8×

`HOST-INDICATIVE-NOT-TARGET.` The acceptance record's outstanding obligation
asks what alongside-rebuild-and-swap costs "in code, in peak disk, in write
amplification and in cutover latency". The protocol answered *code*. The
`rebuild` harness command answers the rest.

**The measurement has a control**, because it has to. A rebuild runs while
ingest continues, and both write through the same process — `/proc/self/io`
cannot say which bytes belong to which, and neither can the file size. So two
arms run **identical ingest**, one with the rebuild and one without, and every
cost below is the *difference*. Without that control the numbers would be "bytes
written while a rebuild happened to be running", which overstates the rebuild by
the entire ingest load.

Every transition is driven by the protocol's own state machine, so what is
measured is the procedure R2 specifies rather than something resembling it.

**The measured window covers the alongside tables' creation**, including their
indexes. It did not in the first version — the DDL ran before the counter
snapshot, so the rebuild arm's window started later than the control arm's and
the two were not symmetric. Corrected under review. The effect turned out to be
small (amplification moved 14.0× → 14.1× at the mid configuration, which is what
two empty tables and two empty indexes should cost), but the claim being made
was that indexes are included, and a measurement that excludes what it names is
wrong independently of by how much.

#### Write amplification is a tunable, and quoting one number for it would mislead

Total ingest held constant at 16 000 events; only the fold's chunk count varies:

| Fold chunks | Write amplification | Extra writes | Cutover | Catch-up laps |
|---:|---:|---:|---:|---:|
| 2 | **2.8×** | 3.3 MB | 2.29 ms | 1 |
| 4 | **5.3×** | 6.4 MB | 2.28 ms | 2 |
| 8 | **14.1×** | 16.8 MB | 2.33 ms | 3 |
| 16 | **20.5×** | 24.2 MB | 2.06 ms | 6 |
| 32 | **35.8×** | 42.7 MB | 2.19 ms | 12 |

Reproduced exactly (1.00× spread over repeated runs at identical parameters:
`--events 40000 --entities 2000`, total ingest 16 000). The parameters are
stated because an earlier check re-ran this sweep on the harness *defaults* —
a different seed size and entity count — and the control arm moved 1.4×,
which very nearly got attributed to a code change. Two sweeps that differ in
what is held fixed are two experiments, not a before and after.

Amplification is **not a property of the rebuild**. It rises roughly linearly
with how finely the fold is chunked, because each chunk commits a transaction
that rewrites projection pages already written, and each commit adds WAL frames
that a checkpoint later writes again. A single figure — the first run gave
14.1× — would have described one arbitrary point on a curve spanning **12.8×**.

**The engineering consequence is a trade-off, now quantified.** Coarser chunks
cost far less I/O; finer chunks hold each fold transaction for less time. An
implementation has to pick, and on flash the pick matters: at the ≈306 MiB
duplicate-projection figure, a 3× rebuild is ~0.9 GB of device writes and a 35×
rebuild is ~10.7 GB. That is a wear question, not a throughput one.

#### Cutover is flat, which is what R2's availability claim needs

**≈2.0–2.4 ms** — the swap is a rename pair inside one transaction, so no rows
move. Against a 525–929 ms rebuild that is **≈0.3 %**. The claim R2 rests on —
the robot keeps serving throughout, and the only moment it cannot is the swap —
is supported rather than assumed. Retiring the old tables (`DROP`) is measured
separately at ~0.9 ms and is reclamation, not cutover.

**Quote this as "about 2 ms", not to three figures.** Unlike amplification,
which reproduces exactly, cutover carries **1.21× run-to-run spread** at a
fixed configuration. That is the number that licenses the word *flat*: the
variation across a 16× range of chunk counts (2.06–2.33 ms) is no larger than
the variation between repeats of a *single* configuration, so the measurement
cannot distinguish chunking as an influence — which is the honest form of
"independent of chunking". An earlier draft of this entry gave the range to
three significant figures, which implied a precision the instrument does not
have.

**What that figure excludes, and it is not a rounding detail.** SQLite renames
tables but not their indexes, so a rename-pair cutover leaves the live projection
carrying the rebuild's index names and the next cycle collides. Every resolution
costs something — recreating indexes inside the blackout window turns the swap
into an index rebuild, name ping-pong makes the live schema depend on cycle
parity, and pointer-based cutover avoids it but is a different design. The
prototype takes none of them: it runs one cycle and **refuses a second with a
stated reason**, so 1.50–1.83 ms is the cost of a rename pair and nothing else.
Read it as a floor. The choice is now explicit implementation work under the
protocol's S-2 (`docs/design/WM2_PROJECTION_REBUILD_PROTOCOL.md` §8).

#### Peak disk: use the free-list measure, not the file size

The duplicate projection measures **2.54–2.61 %** of store size (mean 2.58 %),
and that figure is *stable* across chunk counts (1.03× spread) because it is the
same projection either way.

Two cautions, both learned by getting them wrong first:

- **`DROP TABLE` does not shrink a SQLite file.** It moves pages to the free
  list. Measuring the retired projection by file shrinkage reports **zero**,
  which reads as a projection that cost nothing. The figures above come from the
  free-list delta.
- **Peak on-disk overhead is the noisier measure** (3.52× spread across the same
  runs) because it moves with WAL checkpoint timing. It is emitted, but the
  projection's own size is the number to cite.

**These two figures are DETERMINISTIC, and calling their stability a finding
was wrong.** Both are functions of the seeded data and the schema, not of the
machine: same seed, same code, same bytes. The target run below returned
`2.58 / 2.61 / 2.59 / 2.54 / 2.57 %` and peak ratios `0.0111 / 0.0127 / 0.0390
/ 0.0251 / 0.0259` — identical to every digit printed here, on a different CPU
architecture. So the "stability across chunk counts" noted above is arithmetic
rather than evidence, and host↔target agreement is a **reproducibility check**,
not a measurement of the hardware. Cite the projection fraction as a property
of this schema and workload. It is not a target result and must not be entered
against the ratification checklist as one.

**Against D-2's arithmetic.** The acceptance record derived ≈306 MiB from D-2's
3.74 % projection overhead. This measures **2.58 %** directly — the same order,
about 31 % lower. Neither refutes the other: D-2's figure is bytes-per-event
overhead on its configuration, this is projection pages as a fraction of a store
with a different entity count and event mix. They are two measurements, not one
measurement twice.

#### What the host run does not do

**It is not a target run.** `HOST-INDICATIVE-NOT-TARGET`, so none of the above
is citable against the ratification checklist, and it is over the **stand-in
schema**, so the constants describe that schema and not a ratified one. The
target run below settles part of it and, more usefully, shows which parts were
never the host's to settle.

### D-16a — the target run: cutover holds, and write amplification cannot be measured on this hardware

Evidence: `docs/evidence/wm2-r2-target-20260804/`. Every figure in the table
below was re-verified against that raw result stream after transfer, and the
set carries its own `SHA256SUMS`.

`JETSON-TARGET-MEASURED`, 2026-08-04, Jetson Orin NX / `/dev/nvme0n1p1` ext4,
same parameters as the host sweep (`--events 40000 --entities 2000`, total
ingest 16 000), so the only variable is the hardware.

| Fold chunks | Cutover | Retire | Rebuild wall | Control wall | Catch-up |
|---:|---:|---:|---:|---:|---:|
| 2 | 2.48 ms | 1.70 ms | 495 ms | 297 ms | 1 |
| 4 | 2.42 ms | 1.66 ms | 551 ms | 321 ms | 2 |
| 8 | 2.40 ms | 1.41 ms | 593 ms | 349 ms | 3 |
| 16 | 2.35 ms | 1.19 ms | 703 ms | 429 ms | 6 |
| 32 | 2.33 ms | 1.17 ms | 852 ms | 534 ms | 12 |

**Cutover holds, and it is the claim that needed target evidence.**
2.33–2.48 ms against the host's 2.06–2.33 ms, flat across a 16× range of chunk
counts. The target's spread across that whole range (1.06×) is *smaller than
the host's run-to-run spread at a single configuration* (1.21×), so the two
platforms are not distinguishable on this measure. R2 rests on the robot
serving throughout with the swap as the only blackout; that now has a target
number rather than an inference.

**The protocol itself ran clean on target.** All five configurations reached
`Active` with `completed` true: catch-up converged, equivalence was proven at a
pinned generation, and the state machine's cutover guard accepted only at a
matching head. Nothing in `docs/design/WM2_PROJECTION_REBUILD_PROTOCOL.md`
needed target-specific handling. **No NVMe timeouts occurred during the sweep**
— against D-15's five in 120 repetitions — so this run is not one where the
device's known defect was active.

#### Write amplification is NOT measurable on this target

`process_write_bytes` reads `/proc/self/io`. **That file does not exist on this
kernel** — the Tegra build ships without `CONFIG_TASK_IO_ACCOUNTING` (no
`/proc/config.gz` either). Both arms' counters returned `None`, and the harness
reported `None` rather than `0`, which is the behaviour its doc comment demands:
*"a missing counter must not arrive as zero, which would render as a
flatteringly efficient rebuild."* The fail-closed choice is what kept this from
being recorded as a rebuild that wrote nothing.

**So the R2 obligation stays open on its most consequential dimension.** Write
amplification carries the flash-wear argument — ~0.9 GB of device writes at 3×
versus ~10.7 GB at 35× — and the host's **2.8×–35.8× does not transfer** and
must not be quoted as though it did. A prediction was recorded in advance that
the figure would *move* on target; it did not move, it proved unmeasurable,
which leaves that prediction **untested rather than confirmed or refuted**.

**Ruled 2026-08-04: accept the gap rather than close it.** The alternatives were
weighed and declined:

| Option | Why not |
|---|---|
| `/proc/diskstats` sectors-written | Whole-device, so the attribution problem the control arm exists to solve returns; would need a quiesced machine and still could not separate the arms |
| Rebuild the kernel with `CONFIG_TASK_IO_ACCOUNTING` | Disproportionate for one figure on a substrate ADR, and it would change the platform every other target result here was taken on |

Revisit only if flash wear becomes load-bearing for a deployment decision. Note
also that D-15 established this device's write path drops completions, so a
target amplification number would have needed careful reading even if the
counter had existed.

#### What the target run does not do

It is over the **stand-in schema**, like the host run, so the constants describe
that schema rather than a ratified one. It is a **single sweep**, not repeated,
so the target has no run-to-run spread of its own — the 1.21× figure quoted
above is the host's, and using it to bound the target's noise is an assumption,
not a measurement. And the platform carried a **known-unrepaired filesystem**
(`clean with errors`, `e2fsck` outstanding) under `Errors behavior: Remount
read-only`, so a corrupt region would have aborted the run rather than silently
altering a number.

### D-17 — OQ1's inversion does not reproduce, and the mechanism I proposed for it is refuted

`JETSON-TARGET-MEASURED`. Evidence: `docs/evidence/wm2-oq1-20260804/`.

Open question 1 rests on an anomaly: at batch=64 the medians ran `OFF` > `FULL`
> `NORMAL`, with `NORMAL` slower than `FULL`. Nothing predicts that —
`synchronous=NORMAL` performs strictly fewer fsyncs than `FULL` — and the ADR
holds a per-source-class policy on it. Two instruments were run against it on
target, same session, same parameters D-10 and D-15 used.

#### It does not reproduce

`stall`, 20 repetitions at batch=64, the exact shape D-15 used:

| batch=64 | eps D-15 → now | worst commit | dirty/writeback | stalls |
|---|---|---|---|---|
| FULL | 30 545 → 31 083 (**1.02×**) | 15.23 → 9.11 ms | 892 → 1280 kB | 0 → 1 |
| NORMAL | **19 881 → 35 924 (1.81×)** | 58.88 → 12.28 ms | 4888 → 4572 kB | 0 → 0 |
| OFF | 54 636 → 55 267 (**1.01×**) | 3.85 → 3.77 ms | 42 104 → 44 964 kB | 0 → 0 |

`append` — an independent instrument — agrees **within ~3 %**: FULL 31 776,
NORMAL 36 916, OFF 56 439 (`append` reads slightly *higher* throughout;
`stall`/`append` = 0.97–0.99×). Today's ordering is conventional.

**`FULL` and `OFF` are the internal controls.** They reproduce D-15 within 2 %
while `NORMAL` moves 81 %. One setting shifting while both its neighbours hold
is not device variance, and it is what makes the third number interpretable
rather than merely different.

#### Three explanations ruled out, including my own

- **Not the instrument.** `stall` and `append` agree on target (**within ~3 %**)
  and on a host control at the same settings (**within ~4.5 %**), `NORMAL`
  included. A generic `stall` fault would have shown on the host. This was the
  first hypothesis and the host control refuted it.
- **Not a healthier device.** The NVMe lost-completion defect was **live**:
  `FULL` took a 30 183.9 ms stall and the kernel log carries three
  `completion polled` timeouts during these runs. The device was arguably worse
  for `FULL` today, and `FULL` still reproduced.
- **NOT dirty-page pressure — and this refutes the mechanism proposed while
  investigating.** The argument was that `NORMAL` accumulates dirty pages like
  `OFF` but must still flush them synchronously, so its 5.5× dirty load versus
  `FULL` explained the loss. The re-run kills it: `NORMAL`'s peak
  dirty/writeback is **4888 → 4572 kB, essentially unchanged**, while its
  throughput rose 81 %. That column is a stable property of the setting and
  does not track throughput. Recorded rather than deleted, because a hypothesis
  that survived plausibility and died on data is part of what the next
  investigator needs.

What did change is **commit latency** — `NORMAL`'s median worst commit fell
58.88 → 12.28 ms at unchanged dirty load. No mechanism is offered for that
here. Replacing one refuted explanation with a second unfalsified one would be
worse than leaving it open.

#### The reproducible property, which is what the decision needs

`append` emits the commit-latency distribution that `stall` does not. On
**both** machines:

| NORMAL / FULL, batch=64 | p50 | p99 | max |
|---|---:|---:|---:|
| target | 0.65× | 1.32× | 1.40× |
| host (indicative) | 0.47× | 1.51× | 4.87× |

**`synchronous=NORMAL` buys median throughput by paying tail latency.** It is
faster at the median — as the fsync model requires — and worse in the tail, on
two machines and both batch sizes. That is a stable property of the setting
rather than a property of one machine-day, and for a store whose consumers care
about worst-case behaviour it is the trade a per-source-class policy should
turn on.

#### What this does not do

It does not explain D-15's figure, and OQ1 is **narrowed, not closed**. It is
one machine-day against another — 20 repetitions per setting in both eras, so
like-for-like, but a second observation rather than a distribution. It is over
the stand-in schema. And the platform carried a `clean with errors` filesystem
under `Errors behavior: Remount read-only`, with persistent journald newly
enabled adding modest writes to the same device. **That filesystem was repaired
later the same day — see D-18. Measurements taken after the repair are not
directly comparable with these.**

### D-18 — the measurement platform changed: the root filesystem was repaired

Not a measurement. A recorded **discontinuity in the instrument's environment**,
written down so that a future comparison across it is made deliberately rather
than by accident.

Every target figure in this ADR — D-1 through D-17 — was taken on a root
filesystem carrying `clean with errors`, unchecked since `2025-06-26`, through
53 mounts and 918 GB of lifetime writes. On **2026-08-04 20:43** it was checked
and repaired. It now reads `clean`.

#### Why it had never been checked

`systemd-fsck-root.service` carries `ConditionPathIsReadWrite=!/` and so runs
only against a read-only root. On this platform `ro` on the kernel command line
*does* reach the kernel, but NVIDIA's L4T initrd mounts the rootfs read-write
before switch-root — so the condition failed on every boot and the check never
ran. `fsck.repair=yes` and `fsck.mode=force` are both inert here: they configure
a service that never starts. The repair was performed by a `systemd-shutdown`
hook, which runs after every filesystem has been remounted read-only. Procedure
and the approaches that do **not** work:
[`docs/hardware/JETSON_ROOTFS_FSCK.md`](../hardware/JETSON_ROOTFS_FSCK.md).

#### What was actually wrong

Allocation accounting, and only that:

| Finding | Direction |
|---|---|
| 17 deleted inodes with zero `dtime` | — |
| Block and inode bitmap differences | **all** "marked in use, actually free" |
| Free block/inode counts wrong across ~90 groups | undercounting free space |
| 2 extent trees narrowed | optimisation, not a defect |

Passes 2, 3 and 4 — directory structure, connectivity, reference counts —
produced no output. `/lost+found` was empty. Nothing was orphaned or lost; the
filesystem had been miscounting what it owned.

**Free blocks 10 449 148 → 10 701 568**: 252 420 blocks of 4 KiB, about
0.96 GiB, plus 128 inodes. Counting unit is the 4 KiB filesystem block;
the independence unit is one filesystem at one instant, so this is a single
observation of the *size* of the accounting error at the moment of the check.
It supports no claim about accumulation rate, and none about whether the error
was present during any particular earlier measurement.

#### What this does and does not do to D-1 … D-17

It does **not** invalidate them, and it is not a correction. Nobody measured
whether a wrong free-block map affected any recorded figure, and this entry does
not assert that it did.

> **Since measured — see D-19.** A post-repair baseline at OQ1's exact
> parameters, on the same commit and a digest-gated identical instrument,
> reproduces every D-17 figure within 1.3 %. The boundary named below is
> **crossable**; the caution in the next paragraph is retained as the reason the
> measurement was taken rather than as an outstanding doubt.

What it does is make one variable **no longer held fixed** across the boundary.
Anything comparing a post-repair run with D-1 … D-17 must state that the
allocator's free-space picture differs by ~0.96 GiB and that the filesystem had
been running unchecked with an error flag set. The evidence bundles under
`docs/evidence/` record `clean with errors` in their `ENVIRONMENT.txt`; those
remain accurate statements about *those* runs and should not be edited.

#### One consistency observation, offered as no more than that

Leaked allocations with intact directory structure is the signature of lost
**metadata** writes. D-15 identified lost NVMe completions
(`nvme0: I/O N QID M timeout, completion polled`) as the stall mechanism on this
same device, and that defect was still live during the D-17 run. The two
observations are consistent, from layers the WM-2 work otherwise never connected.

That is suggestive, not conclusive. No common cause was demonstrated: the
filesystem damage carries no timestamps tying it to any observed completion
timeout, and the repair fixes the filesystem, not the device. The defect remains.

### D-19 — the repair moved nothing: the D-18 boundary is crossable

D-18 recorded the filesystem repair as a discontinuity and deliberately declined
to say whether it affected any measured figure, because nobody had measured it.
This does. Evidence bundle: `docs/evidence/wm2-postrepair-20260804/`,
`JETSON-TARGET-MEASURED`.

**Instrument identity was gated, not assumed.** The run refused to proceed unless
the harness reported `source_digest 8882f659…` and the same stand-in schema as
the OQ1 run. It matched, and the device's checkout is still at `83998315`, so the
binary is the **same commit** — not merely the same source.

#### Nothing moved

`append`, median of 3 repetitions, against D-17:

| durability | b=1 post | b=1 D-17 | ratio | b=64 post | b=64 D-17 | ratio |
|---|---:|---:|---:|---:|---:|---:|
| FULL | 3 270 | 3 246 | 1.007 | 31 778 | 31 665 | 1.004 |
| NORMAL | 9 936 | 9 924 | 1.001 | 36 405 | 36 870 | 0.987 |
| OFF | 15 077 | 15 089 | 0.999 | 56 403 | 56 406 | 1.000 |

Every cell within **1.3 %**, five of six within **0.7 %**. The latency shape
reproduces too: NORMAL/FULL at batch=64 gives p50 **0.64×**, p99 **1.30×**, max
**1.42×**, against D-17's 0.65× / 1.32× / 1.40×.

**Counting unit** events/second; **independence unit** one machine-day at one
filesystem state (three repetitions inside a run are repetitions, not independent
observations); **held fixed** instrument, commit, parameters, seed, store
location, device; **changed** the filesystem. **The claim supported:** the repair
did not move these figures, so D-1…D-17 and post-repair runs are comparable. It
supports no claim that the allocation errors were harmless in general — only that
they are not visible here at this precision.

#### D-15's `NORMAL` is now one observation against two

`stall`, batch=64, 20 repetitions:

| batch=64 | D-15 | D-17 | post-repair |
|---|---:|---:|---:|
| FULL | 30 545 | 31 083 | **30 697** |
| NORMAL | 19 881 | 35 924 | **35 992** |
| OFF | 54 636 | 55 267 | **55 834** |

Post-repair `NORMAL` lands **0.2 %** from D-17. This does **not** explain D-15,
and open question 1's residual stays open as worded. What changes is the weight:
a reading that was one of two competing eras is now one anomalous observation
against two that agree.

#### The dirty-page mechanism is refuted a second time

`NORMAL` peak dirty/writeback: **4 888 → 4 572 → 5 224 kB**. The post-repair run
carries the *highest* dirty load of the three while its throughput matches the
*fast* era. D-17 refuted this mechanism by holding dirty constant while
throughput rose 81 %; this refutes it from the opposite direction. The hypothesis
is wrong under two independent tests and stays on the record rather than being
dropped.

#### What does not fit

`FULL` recorded **4 stalls in 20 repetitions** (D-17: 1), durations `1942.5,
5926.0, 15077.1, 30091.8` ms. The 30 091.8 ms one is D-15's signature exactly —
`nvme_core.io_timeout` is 30, so timeout plus handler latency.

**But the kernel log carries only ONE `completion polled` event for this run.**
Three of the four stalls have no corresponding NVMe timeout, and the nvme lines
run unbroken from boot to 2 400 s, so this is not a truncated ring buffer.

D-15's mechanism rested on 5 stalls coinciding with 5 timeouts; here the
correspondence is **1 of 4**. Either there is a second stall population D-15 did
not separate, or the sub-30 s stalls have another cause. **This evidence cannot
resolve it and none is asserted** — it is recorded because a mechanism finding
that fits 5 of 5 in one run and 1 of 4 in another is not yet settled.

Median throughput is unchanged despite four times the stalls, which is D-15's own
point — stalls are a device property, not a persistence property — now observed
on a repaired filesystem.

#### Confounders

journald was **growing** during this run (227.0 M against a 200 M cap, from
194 M), as it was during OQ1; the NVMe defect was **live**, which is what makes
the comparison like-for-like rather than "quieter machine"; free space differs by
the reclaimed ~0.96 GiB plus this run's databases; stand-in schema throughout;
one machine-day.

### D-20 — the ratified schema costs 1.24×–1.34× more per event, and OQ2's allocation no longer fits

Evidence: `docs/evidence/wm2-schema-growth-20260805/`. This discharges the
obligation `KIRRA-WM2-SCHEMA-001` §8.4 recorded when the event schema was
ratified: **every figure in D-2, and every horizon OQ2 derived from it, was
measured against the harness's deliberate stand-in schema.** The ratified
schema (`502b5460…`, merged in #1350) adds six columns.

Two arms, one host, one session, same SQLite 3.45.0, same event stream —
D-2's own parameters (seed `20260803`, 100 000 events, 1 000 entities, 96-byte
payload). Log-only in both, because `kirra-world-store` has no projections yet.

| Arm | B/event | Days to fill 8 GiB @ 10 Hz | Ratio |
|---|---:|---:|---:|
| stand-in (D-2's schema) | 458.50624 | 21.68 | 1.000× |
| ratified, `lean` | **566.55872** | 17.55 | **1.236×** |
| ratified, `populated` | **612.18816** | 16.24 | **1.335×** |

Counting unit: bytes of database per appended event (`main` + `-wal` + `-shm`
after a TRUNCATE checkpoint — the harness's own `db_bytes`). Independence unit:
one database build; events within a build are not independent and **no
per-event variance is claimed**. Both arms rebuilt and reproduced
**bit-identically**, so the quantity is deterministic. Held fixed: platform,
SQLite build, event stream, log-only. Varied: schema, and the fill of the
added columns.

**Why two ratified numbers.** Four of the six added columns are variable-width
TEXT and two are nullable, so the figure is a function of how much of that
width real traffic carries — which nothing has measured. `lean` is a raw
non-spatial observation (`provenance` `[]`, no frame, no map; SD-4 permits a
NULL frame for exactly those). `populated` is a perception-derived spatial
claim (one provenance citation, frame and map set; SD-4 makes the frame
**mandatory** there). Both are real configurations. **Horizons take the
`populated` end** — a horizon says when a disk fills, and the lean end gives
the least margin.

#### The consequence

OQ2's budget of 18 033 812 events is 8 GiB at D-2's *with-projections* figure.
Against the ratified schema's *log-only* figure the budget falls to
**15 161 596** (`lean`, 0.841×) or **14 031 527** (`populated`, 0.778×).
OQ2 allocated 11 664 000 to `raw` and 3 784 320 to the protected classes —
**15 448 320** together, with a stated 14 % headroom.

| Against | Headroom |
|---|---:|
| ratified `lean` | **−286 724 (−1.9 %)** |
| ratified `populated` | **−1 416 793 (−10.1 %)** |

**The headroom is gone and the allocation overruns at both ends of the band** —
and the overrun is understated, because these are log-only figures against a
budget that included projections. The ratified store has no projections, so its
with-projections figure cannot be measured, only bounded below; the real
deficit is larger.

#### Confounders and scope

Host run, `x86_64`, not target — the harness would label it
`HOST-INDICATIVE-NOT-TARGET` and that label is not being argued around. Two
things make the bundle usable and neither is a proof: the control arm
reproduced D-2's Jetson `log_only_bytes` **byte-for-byte** (45 850 624), which
is expected for a logical file length but is an empirical identity on one pair;
and the reported result is a **ratio taken within one host**, so any platform
dependence divides out. The instrument refuses to emit a ratio at all unless a
same-host control figure is supplied. A target run with `--assert-target` is
still owed before any figure here is entered against the ratification
checklist. Nothing about latency, throughput, durability or stalls was
measured. `populated` cites one upstream observation; a derivation-heavy
workload sits above this band.

### D-12 — design implications the measurement forces

Not decisions, and not part of this ADR's proposal. They follow from D-9 and
D-10 and belong in WM-2's design:

| Implication | Because |
|---|---|
| **Semantic persistence must not block safety or actuation** | a 10.5 s p99 query and a 19.6 s stall are equally fatal to a loop that waits on either, and this holds whether or not the stall is ever explained |
| **Bounded queue** between producers and the store | so a slow write applies backpressure to a queue rather than to the caller |
| **Explicit backpressure / shed semantics** | dropping observations must be a declared behaviour with a recorded reason, never an emergent one |
| **Latency watchdog** on store operations | the stall is intermittent at 2.5 %; only continuous monitoring catches it in the field |
| **Writer isolation** | the checker and actuation path must not share a thread, connection or lock with the world-model writer |
| **SMART telemetry capture** | `nvme smart-log` was absent, and it is what would confirm or refute the `IO-DEVICE` reading |
| **Bounded interactive queries** | temporal p99 is 10.5 s at the top rung — a query with no declared bound has no bounded cost, whatever its scaling verdict |

**Interactive temporal queries require bounded result contracts,
pagination/window limits, or purpose-built indexing. Neither graph nor temporal
queries may sit directly on a control or safety deadline path.** The rows above
isolate the store from the real-time path; this one bounds the query itself, and
both are needed — isolation stops a slow query blocking a deadline, but it does
not stop the query being slow, and D-9 measured 10.5 s p99 at 100 000 entities
with `LINEAR` scaling. A verdict about *shape* places no ceiling on *cost*: the
only thing that bounds an unbounded scan is a bound. Which mechanism —
a result contract, a window limit, or an index built for the access pattern —
is a WM-2 design choice this ADR does not make.

### O-1 — the original ~29 second write stall (SUPERSEDED by D-10)

> **Retained as the historical record.** The theory below — that the stall was a
> property of `NORMAL`/batch=64 — was **tested on target and rejected**: that
> configuration produced zero stalls in 20 repetitions. See D-10. What survives
> is the observation itself and the reasoning about why `max` beside `p99`
> mattered; the attribution does not.

| Durability | Batch | Throughput | p99 | max |
|---|---|---|---|---|
| FULL | 1 | 3 279 ev/s | 0.67 ms | 6.60 ms |
| FULL | 64 | 31 809 ev/s | 8.09 ms | 8.75 ms |
| NORMAL | 1 | 9 945 ev/s | 0.48 ms | 15.76 ms |
| **NORMAL** | **64** | **3 123 ev/s** | 11.35 ms | **29 271.78 ms** |
| OFF | 1 | 15 250 ev/s | 0.10 ms | 1.25 ms |
| OFF | 64 | 56 260 ev/s | 3.44 ms | 3.68 ms |

At a fixed batch the expected ordering is `OFF > NORMAL > FULL`. It holds at
batch=1. **At batch=64 it is inverted — `NORMAL` is the slowest of the three**,
which no durability model predicts.

The cause is visible in the `max`: a single commit took **29.27 seconds**. That
one event is ~91 % of the stage's wall time; excluding it, throughput is
~36 400 ev/s, consistent with `FULL`/64 and `OFF`/64. So this is **not a
throughput regime — it is one stall**, and the throughput figure for that row
should not be used.

This matters more than the benchmark it corrupted. A 29-second write stall on
target means **~293 observations at 10 Hz that cannot be recorded**, or a
writer blocked for 29 s. Candidate causes — ext4 journal commit, NVMe garbage
collection, thermal or power management — are distinguishable only with another
run. **Until characterised, no `synchronous` policy should be ratified from
this data.** Recorded as **open question 9**.

That the tail was *visible at all* is the argument for reporting `max`
alongside `p99`: at p99 this row reads 11.35 ms and looks unremarkable.
