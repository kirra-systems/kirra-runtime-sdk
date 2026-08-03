# ADR-0041 (WM-2): Use a SQLite event log with deterministic projections

| Field | Value |
|---|---|
| Status | **Proposed — NOT ratified on merge.** Ratification is **measurement-gated**; see *Ratification criteria*. Merging records the proposal; it does not ratify it and authorizes no implementation. |
| Date | 2026-08-02 |
| Blueprint | `KIRRA-WM-ARCH-001` §7, §10, §11, §13 (WM-2) — [`docs/design/WORLD_MODEL_ARCHITECTURE.md`](../design/WORLD_MODEL_ARCHITECTURE.md) |
| Deciders | World Model owner · architecture owner · deployment owner |
| Depends on | [`ADR-0039`](0039-world-model-bidirectional-governor-fence.md) (WM-6) · [`ADR-0040`](0040-world-model-ownership-and-boundary.md) (WM-1) |
| **Clarified by** | **[ADR-0042](0042-world-model-terminology-and-safety-boundary-scope.md)** — canonical terminology (Decision 1). The persistence recommendation is unchanged. |
| Cross-refs | [`crates/kirra-persistence`](../../crates/kirra-persistence/) (migrations, WAL, durability tiers) · [`crates/kirra-audit-hash`](../../crates/kirra-audit-hash/) (shared chain primitives) · [`src/audit_chain.rs`](../../src/audit_chain.rs) · [`ADR-0038`](0038-postgres-shared-state-hybrid.md) (hybrid backend precedent) · [`ADR-0037`](0037-epoch-fenced-generation-ordering.md) |

> **Convention deviation** — as ADR-0039/0040: *not* ratified on merge. This one
> additionally requires **measured evidence on target hardware**. A merged
> document is not a benchmark.

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
2. Retention classes: exact list and their durations. The harness models the
   §11.3 protected set (safety, incident, calibration, adjudication, operator)
   and enforces it — a window containing any of them is refused whole — but the
   *durations* remain unset. **D-2 now bounds them**: on 8 GiB at 10 Hz the
   store fills in 21.7 days, so any horizon beyond three weeks requires the
   event rate to fall (2.4 events/s for 90 days). The durations and the
   sampling policy are a single coupled decision, not two.
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
7. **Partial projections under disk pressure.** A fold writes, so it is refused
   when the store is full. A fold interrupted that way leaves partial
   projections with nothing marking them as incomplete. Needs either a
   fold-in-progress marker, a transactional whole-fold, or an explicit rule that
   projections are invalid until a checkpoint confirms them.
8. **Migration strategy — blocking for acceptance (D-6).** Measured at
   0.325 ms/event, a whole-store offline migration costs ~101 minutes at the
   8 GiB ceiling. WM-2 must choose: bound the store, migrate lazily/online, or
   accept a documented maintenance window. The third conflicts with an
   available robot, so it needs stating explicitly if chosen.
9. **Multi-second write stalls (O-1, D-10).** **Measured on target; the
   original theory is rejected and the mechanism is partly unresolved.**
   `NORMAL`/batch=64 — the suspect — produced **zero** stalls in 20 repetitions.
   The stalls appeared in `FULL`/batch=64 and `NORMAL`/batch=1 instead: 3 events
   in 120 repetitions, crossing both fsyncing modes and both batch sizes.
   Classified **intermittent and block-device/environment-correlated**. Thermal
   is *ruled out by measurement* (hottest zone 59.6 °C against an 85 °C
   threshold). What remains unknown is the mechanism: one `IO-DEVICE`
   attribution held loosely, one `UNATTRIBUTED`, PSI unavailable on this kernel,
   and no SMART data. See the follow-ups in *Design implications*.
10. **Graph and temporal query placement (D-1, D-9).** Neither may sit on a
   deadline path, and the sweep sharpens by how much. At 100 000 entities the
   graph family is 159 ms p99 (1.6× a 10 Hz period) and the temporal family is
   **10.5 s p99** (105×). Both scale acceptably in *shape*; neither is
   interactive at that size. Whether a planned consumer needs either at tick
   rate is unresolved — if one does, that is a re-evaluation trigger, and it is
   now a question about absolute latency rather than about scaling.

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
`fsync` from a device cache that acknowledged and buffered it. The harness
always reports that tier as `NOT-RUN` with the reason attached, so a results
file can never imply a durability test that did not happen.

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
| [ ] | **Corruption / restart experiment** — power-loss-class behaviour, in the spirit of the existing audit-chain crash-consistency drill | `crash` tiers A and B; **tier C is manual** (drill §8) and this gate is not complete without it | **Partial** — A and B PASS; **tier C `NOT-RUN`, 0 of 5 trials** |
| [x] | **Storage growth estimate** — observations/day at realistic sensor rates, projected against the device budget | `growth` | **Met — and adverse.** See D-2 |
| [x] | **Migration proof of concept** — a schema change applied to a populated store, fail-closed on a future schema version | `migrate` | **Met — and adverse.** See D-6 |
| [x] | Scale assumptions confirmed or corrected; if materially wrong, Option B is re-evaluated before acceptance | the drill §9 sweep, emitting a **computed** verdict against this ADR's own reopening condition | **Met — and Option A survives.** Graph `SUBLINEAR` (0.45), temporal `LINEAR` (1.14), both on target. See D-9. What the deployed robot actually reaches remains an operational fact no benchmark produces |

**One gate remains open, and it cannot be closed from software.** Tier C
requires five physical power cuts (drill §8, `powercut arm` / `powercut verify`);
the harness reports `NOT-RUN` at `0/5` and will continue to. Until it is closed
this ADR stays **Proposed**.

Six of the seven gates now read *Met*. That is not an argument for acceptance —
the remaining one is the durability gate, and no quantity of scale and latency
evidence substitutes for it.

**No implementation should begin merely because this proposed ADR exists**, and
in particular not because six of the seven gates now read *Met*. The domain-logic
gate (ADR-0042 Decision 5) is a separate and independent hold.

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

**The stall-robust medians resolve the corrupted benchmark row** — and expose
something else. `NORMAL`/batch=64 reads 19 351 ev/s across 20 runs against the
original 3 123, confirming that row was a stall artefact. But at batch=64 the
medians run `OFF` 55 275 > `FULL` 31 497 > `NORMAL` 19 351: **`NORMAL` slower
than `FULL`**, from a configuration with zero stalls. No durability model
predicts that. It is not a tail artefact and it is not explained; open question
1 should not be settled at batch=64 until it is.

### D-11 — design implications the measurement forces

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
