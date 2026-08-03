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
| Read-only degraded mode | Serve projections read-only if the log is unwritable — **never** silently drop writes | |
| Corruption response | `integrity_check` on open; refuse to serve rather than serve partial evidence | |
| Disk-full | Refuse new observations with `Unavailable`; never overwrite | |
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

1. `synchronous` policy per source class — measurement required.
2. Retention classes: exact list and their durations. The harness models the
   §11.3 protected set (safety, incident, calibration, adjudication, operator)
   and enforces it — a window containing any of them is refused whole — but the
   *durations* remain unmeasured and unset.
3. Whether projections live in the same database file or a rebuildable
   sidecar file (a corrupt sidecar is cheaper to discard).
4. Index rebuild strategy: eager at startup vs lazy per query family.
5. Does ADR-0038's hybrid path ever apply — shared world knowledge in Postgres
   for a fleet, local evidence staying local?
6. Event payload encoding — the blueprint does not fix one; JSON is
   inspectable, a binary format is compact.

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

| | Gate | Produced by |
|---|---|---|
| [ ] | **Measured Jetson prototype** — the log + projection path on target hardware, not a development host | the whole run, only when it reports `JETSON-TARGET-MEASURED` |
| [ ] | **Replay benchmark** — full rebuild time at the assumed observation volume, with the checkpointed case measured separately | `replay` (also asserts rebuild-equals-incremental determinism, which gates the rest) |
| [ ] | **Representative query benchmark** — one measurement per query family in blueprint §12 (point, set, graph, temporal) | `query`, plus a separate bitemporal point query the projection cannot answer |
| [ ] | **Corruption / restart experiment** — power-loss-class behaviour, in the spirit of the existing audit-chain crash-consistency drill | `crash` tiers A and B; **tier C is manual** (drill §6) and this gate is not complete without it |
| [ ] | **Storage growth estimate** — observations/day at realistic sensor rates, projected against the device budget | `growth` |
| [ ] | **Migration proof of concept** — a schema change applied to a populated store, fail-closed on a future schema version | `migrate` |
| [ ] | Scale assumptions confirmed or corrected; if materially wrong, Option B is re-evaluated before acceptance | the drill §7 sweep informs it; what the deployed robot actually reaches is an operational fact the harness cannot produce |

**No implementation should begin merely because this proposed ADR exists.**
Merging this PR satisfies none of the above, and neither does the existence of
the harness.
