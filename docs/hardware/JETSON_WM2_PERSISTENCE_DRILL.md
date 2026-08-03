# Jetson WM-2 persistence drill (ADR-0041)

The on-device measurement that ADR-0041 is blocked on. That ADR proposes a
SQLite append-only event log with materialized projections for Kirra World and
then refuses to ratify itself on argument:

> **Proposed. Measurement-gated.** Accepted only when **all** are recorded […]
> **No implementation should begin merely because this proposed ADR exists.**

This drill produces those recordings. The instrument is
[`tools/wm2-persistence-harness`](../../tools/wm2-persistence-harness/).

> **The harness is not the Kirra World store.** Its schema is a stand-in, its
> hash chain is a deliberately-different local SHA-256, and it is
> workspace-detached so nothing can depend on it. Lifting code out of it into
> `kirra-world-store` is a defect, not a shortcut — the store must use
> `kirra-audit-hash` and a ratified schema, neither of which this has.

---

## 0. What this drill can and cannot establish

| Ratification gate | Established here | By |
|---|---|---|
| Measured Jetson prototype | ✅ | the whole run, if it reports `JETSON-TARGET-MEASURED` |
| Replay benchmark (full + checkpointed) | ✅ | `replay` |
| Query benchmark, one per §12 family | ✅ | `query` |
| Corruption / restart experiment | ⚠️ **partly** | `crash` tiers A and B; **tier C is manual, §8** |
| Storage growth estimate | ✅ | `growth` |
| Migration proof of concept | ✅ | `migrate` |
| Compaction-with-citation (§11.3; not a numbered gate, but the retention policy the growth number makes urgent) | ✅ | `compact` |
| Disk-pressure + reclamation behaviour (ADR-0041 SQLite-config table; not a numbered gate) | ⚠️ **partly** | `pressure` proves clean refusal at the SQLite layer; a genuinely full partition needs the device |
| Scale assumptions confirmed or corrected | ⚠️ **partly** | the §9 sweep informs it; the operational answer needs real deployment |

Two of the seven are deliberately marked partial. Marking them complete on the
strength of this harness alone would be the exact failure the measurement gate
exists to prevent.

---

## 1. Prerequisites

- **A Jetson Orin** (NX or AGX). Per
  [`TARGET_PLATFORM_MATRIX.md`](TARGET_PLATFORM_MATRIX.md) a Jetson is a *doer*,
  never the governor's cert target — which is exactly right here: Kirra World
  runs on the doer side, and this is the machine it will actually live on.
- **Real storage.** The database must sit on the medium the deployment will use
  (eMMC, NVMe, or the microSD the robot actually boots from). The harness
  **refuses** target status on `tmpfs`, `overlay`, `ramfs`, `aufs` and
  `squashfs` — see §3.
- **A release build.** A debug build measures rustc, and the harness refuses
  that too.
- Rust toolchain on the Orin (or cross-compile for `aarch64-unknown-linux-gnu`).
- Roughly **2 GiB free** at the default volume, and more for the §9 sweep.

```sh
cd tools/wm2-persistence-harness
cargo build --release
```

The harness has exactly one dependency (`rusqlite`, bundled). That is a
constraint, not a coincidence: `ci/check_kirra_world_bidirectional_fence.py`
walks this manifest as an extra Fence A root, so adding a transport crate to it
reds CI the same as adding one to `kirra-world` itself.

---

## 2. Run it

```sh
sudo mkdir -p /var/lib/kirra/wm2 && sudo chown "$USER" /var/lib/kirra/wm2

./target/release/wm2-persistence-harness all \
    --db   /var/lib/kirra/wm2/bench.sqlite \
    --out  ~/wm2-results-$(date +%Y%m%d).jsonl \
    --events 100000 \
    --entities 1000 \
    --assert-target
```

Expect **tens of minutes**. The append benchmark alone runs six configurations
(three `synchronous` settings × two batch sizes), and the `FULL`/`batch=1` case
is one fsync per event by construction.

Individual stages run on their own: `platform`, `append`, `replay`, `query`,
`growth`, `migrate`, `compact`, `pressure`, `crash`. Start with `platform` — it prints, in about a
millisecond, whether anything you run afterwards will be citable.

Exit status is `1` if any measurement failed **or was unusable** — a query family
that matched nothing, a non-deterministic rebuild, a crash tier that failed *or
came back `INCONCLUSIVE`*. An inconclusive tier never established its
precondition, so neither a pass nor a failure would mean anything, and letting
the run exit 0 would leave a results file that looks complete with a
load-bearing gate silently missing.

Tier C is the one exemption: it is *always* `NOT-RUN` by construction (§8), so
counting it would make every run exit 1 and the exit code would stop carrying
information.

---

## 3. `--assert-target` is an assertion, not a flag

A run is citable in ADR-0041's checklist only when it reports
`JETSON-TARGET-MEASURED`, which requires **both**:

1. the machine corroborates it — aarch64, Tegra evidence
   (`/etc/nv_tegra_release` or a Jetson/Tegra device-tree model), a durable
   filesystem under the database, a release build; **and**
2. you pass `--assert-target`.

Neither half is sufficient, and the asymmetry is deliberate. Detection alone
would let a dev-rig run under conditions nobody inspected become evidence
silently. Assertion alone would make the flag a rubber stamp — so it cannot
override the facts, and a laptop run with `--assert-target` is still
`HOST-INDICATIVE-NOT-TARGET`.

**What you are asserting** when you pass it: this is the real device, the
database is on the storage the deployment will use, the board is under
representative thermal and power conditions, and nothing else is saturating the
same storage. The harness can check the first of those; the rest are yours.

**Why the filesystem forfeits target status rather than warning.** SQLite's
durability cost is dominated by `fsync`, and `fsync` on a Jetson spans more than
an order of magnitude between microSD, eMMC and NVMe. A `tmpfs` run does not
fsync to anything — it would produce the *best* numbers the harness can emit
while measuring none of the property being decided. That is the most dangerous
false positive available here, so it is refused outright.

---

## 4. Reading the results

JSON Lines, one record per measurement, each stamped with `evidence_status`,
`standin_schema_digest`, `sqlite_version`, `build_profile`, `arch`,
`device_model`, `db_fs_type`, `db_fs_source` and `seed`.

The **`standin_schema_digest`** is the field that keeps this honest over time. A
number produced against the stand-in schema reads exactly like one produced
against the ratified schema, and in six months nobody will remember which. When
the real schema lands, its digest differs, and an old measurement becomes
visibly about something else rather than quietly authoritative.

```sh
# Everything at a glance
jq -r 'select(.record=="append")
       | "\(.durability)\tbatch=\(.batch)\t\(.events_per_second|floor) ev/s\tp99=\(.timing.p99_us)us"' results.jsonl

# The gate that matters more than any timing
jq -r 'select(.record=="replay") | "deterministic=\(.deterministic)"' results.jsonl

# Nothing may be cited unless this is true
jq -r 'select(.record=="run") | "citable=\(.citable) \(.evidence_status)"' results.jsonl
```

### What to look at, and why

| Record | Field | Why it decides something |
|---|---|---|
| `append` | `events_per_second` per `durability` | ADR-0041 open question 1 — the `synchronous` policy per source class. The FULL-vs-NORMAL gap *is* the cost of durability, and the ADR forbids inheriting the verifier's answer. |
| `append` | `timing.max_us` | The tail, not the mean. A robot is hurt by the one commit that blocked, not the average. |
| `append` | `hash_share_percent` | Honesty check on the harness's local SHA-256. Small → the substitution does not move the decision. Large → re-run against `kirra-audit-hash` before concluding anything. |
| `replay` | `deterministic` | **Load-bearing.** `false` should stop ratification regardless of how good the milliseconds are: a rebuild that disagrees with the incremental fold makes every other number meaningless. |
| `replay` | `cold_rebuild` vs `checkpointed_resume` | Risk R2 (rebuild too slow at startup) and whether checkpointing actually bounds it. |
| `query` | `graph_bounded_reach` vs the rest | The single most contestable claim in ADR-0041 — that the graph shape belongs in an *index*, not the durable substrate. If bounded traversal is orders of magnitude worse than the other families at realistic scale, Option B gets stronger. |
| `query` | `rows_matched_total` | A family that matched nothing is a broken benchmark, not a fast one. Zero here is reported as a failure. |
| `growth` | `bytes_per_event`, `days_to_fill_budget` | Risk R7. This is what decides whether compaction (§11.3) is a future concern or an immediate one. |
| `migrate` | `future_schema_refused` | The fail-closed policy surviving contact with a populated store. |
| `crash` | tier outcomes | §7 and §8 below. |

### A host baseline, for orientation only

Recorded on an x86-64 development container, ext4, `--events 20000
--entities 500`. **`HOST-INDICATIVE-NOT-TARGET` — not citable, and a Jetson will
be slower on every line.** It is here so you can tell a broken run from a slow
one, and nothing else.

| Measurement | Host-indicative value |
|---|---|
| append, `FULL`, batch=1 | ~850 ev/s, p99 3.4 ms, max 87 ms |
| append, `NORMAL`, batch=1 | ~7 500 ev/s |
| append, `OFF`, batch=1 | ~39 000 ev/s |
| append, `FULL`, batch=64 | ~12 800 ev/s |
| cold rebuild / checkpointed resume | 39 ms / 2.0 ms (2 000-event tail) |
| point (latest / time-travel) | 4.5 µs / 12.9 µs median |
| set | 6.0 µs median |
| **graph, bounded depth 4** | **1 033 µs median** |
| temporal `changes_since` | 5.3 ms median |
| growth | 459 B/event at a 96-byte payload |
| hash share, `FULL` batch=1 | 0.2 % |
| compaction | 9 898 events over 104 spans, **49.6 % reclaimed**, 329 ms + 34 ms re-verify |

Two things in that table are worth reading twice even as indicative numbers.

**The graph family is ~200× the point family.** That is precisely where
ADR-0041 is most likely to be wrong, and it is measurable now rather than after
the store is built.

**459 bytes per event, against a 96-byte payload.** At 10 Hz that is an 8 GiB
budget exhausted in about **22 days**. If that survives target measurement,
compaction-with-citation is not a future concern — it is load-bearing inside a
month, and ADR-0041's deferred retention thresholds become urgent rather than
prudent.

---

## 5. Compaction with citation (`compact`)

ADR-0041 §11.3 calls this "the one place P2 (append-only forever) is knowingly
bounded" and defers every threshold until measured. What neither it nor the
blueprint settles is the *mechanism*, and there is a real problem in the gap:

**Deleting events from a hash-chained append-only log breaks the chain.**

Re-chaining everything after the window rewrites history — precisely what §11.3
forbids — and costs O(n). Leaving the hole makes the log unverifiable past the
first compaction, destroying the tamper evidence the design rests on.

### The mechanism under test

A compacted span `[lo, hi]` becomes one `Summary` event at `lo`, plus a
`compaction_citations` row carrying `event_count`, a `range_digest` over the
removed events' canonical bytes, and the chain digests on **both** sides.
Verification links the summary from `chain_before` and resumes from
`chain_after`, so events written after the window still verify against the links
they were originally computed from. Nothing is re-chained.

The pass compacts every maximal raw span in a retention horizon, not one
abstract window — protected classes are scattered through the log and partition
the compactable traffic, which is what a real policy operates on.

### What is lost, and how the run says so

After compaction you can no longer verify the *contents* of a removed span, only
that a span was removed, how large it was, and what it hashed to. Full tamper
evidence degrades to **tamper-evident citation of a removed span**. Two
mechanisms keep that visible rather than silent:

- the chain verifier reports `compacted_windows` and `redacted` counts, so a
  compacted log never reads as a plain "intact";
- a time-travel query into a compacted window returns `DegradedSummary`, never a
  value and never a bare `Unknown` — `Unknown` is also what you get for
  something never observed, and conflating "we had this and compacted it" with
  "we never saw it" destroys the one fact an incident investigator most needs.

### The nine checks in the record

Each is a §11.3 sentence turned into a condition; any `false` fails the run.

| Field | What it establishes |
|---|---|
| `citation_digest_reverifies` | every citation matches a digest computed independently before deletion |
| `protected_window_refused` | a window containing a protected retention class is refused **whole** |
| `protected_events_survived` | no pre-existing protected event was destroyed |
| `chain_intact_across_window` | the log verifies across the holes, not merely up to the first one |
| `all_windows_reported` | the verifier counts every compaction, not some |
| `query_into_window_is_degraded` | degraded resolution, never silent fabrication |
| `query_outside_window_is_unknown` | and `Unknown` still means never-observed |
| `redaction_keeps_chain_intact` | a redaction leaves a tombstone; absence is never how deletion is represented |
| `tampered_summary_breaks_chain` | **the non-vacuity control** — without it every row above is unfalsifiable |

### Two findings from the host run, worth confirming on target

**Compaction alone reclaims no disk.** `bytes_after` came back byte-identical to
`bytes_before`: deleting rows leaves free pages inside the file. The 49.6 %
reclaim appears only after a `VACUUM`, which rewrites the entire database. On an
embedded device sharing storage with perception that is a significant
operational cost with its own I/O and power profile, and it means "compact"
and "reclaim" are two operations that must be scheduled separately. Measure the
`VACUUM` on target before assuming a retention policy is affordable.

**Retention class is inside the chained bytes.** Downgrading a protected event
to `raw` — the obvious way to make it deletable — breaks the chain at that
generation. The protection is structural rather than procedural, which is what
you want, but it also means a retention *policy* change cannot be applied
retroactively by relabelling: it has to be a forward-looking decision.

---

## 6. Disk pressure and reclamation (`pressure`)

ADR-0041's SQLite-configuration table asserts two things that nothing had ever
exercised:

| Setting | Proposal |
|---|---|
| Read-only degraded mode | Serve projections read-only if the log is unwritable — **never** silently drop writes |
| Disk-full | Refuse new observations with `Unavailable`; never overwrite |

Both are claims about the worst moment in a store's life. A robot that fills its
disk mid-mission and *silently* stops recording observations — while continuing
to answer queries as though its knowledge were current — is a far worse failure
than one that refuses loudly.

### How a full disk is simulated

`PRAGMA max_page_count` caps the database at a fixed page count; writing past it
returns `SQLITE_FULL`, the same error through the same code path a genuinely
full filesystem produces. That makes the experiment deterministic and
privilege-free.

**The honest limit:** this exercises SQLite's full-*database* behaviour, not the
filesystem's ENOSPC behaviour, and not what happens when a Jetson's eMMC is
genuinely at 100 % — where the WAL, the journal and every other process sharing
that mount are also failing. It establishes the store refuses cleanly rather
than corrupting; it does not establish the device stays healthy. Confirming the
second needs the real device, under a deliberately filled partition.

### The seven checks

| Field | What it establishes |
|---|---|
| `write_refused` | the append past the cap errored rather than silently succeeding |
| `refusal_is_disk_full` | the error names a full database, not an unrelated fault |
| **`partial_batch_rolled_back`** | **the one that matters most** — see below |
| `chain_intact_after_refusal` | the refusal did not corrupt the chain |
| `reads_serve_while_full` | read-only degraded mode: log *and* materialized projections still answer |
| `recovers_when_space_returns` | full is a condition, not a state the store gets stuck in |
| `chain_intact_after_recovery` | and recovery does not fork the chain |

`append_batch` writes N events in **one** transaction. If a full database
half-committed a batch, the generation sequence would be torn and the hash chain
would fork — turning a recoverable out-of-space condition into permanent
evidence corruption. That is the check to read first.

`reads_serve_while_full` requires `projection_rows_while_full > 0`. Without that,
it would pass on a store that had no projections to serve and therefore proved
nothing.

### A design gap this surfaced, not yet resolved

A projection **fold writes**, so it is subject to the same refusal as an append.
What a store that fills *mid-fold* leaves behind is an open question: the
projections would be partial, with nothing marking them as such, and a consumer
could not tell a partial projection from a complete one. ADR-0041's "no
projection-only fact" rule does not cover this case. Worth settling before the
real store is built; recorded here rather than papered over.

### Reclamation cost (`reclaim` record)

The `VACUUM` is now timed separately from compaction, because the ADR models
them as two operations (see *Compaction is not reclamation*) and a reclamation
that must be scheduled against power, thermal and mission state needs its own
number to be scheduled against.

`bytes_freed` may be zero or negative on a store with nothing to reclaim. That
is reported rather than clamped, because a `VACUUM` that costs seconds and frees
nothing is precisely the case a scheduler must avoid.

On target, measure this on a store at realistic occupancy, and record it
alongside the device and medium — a rewrite of the whole database has an I/O and
power profile that does not transfer between eMMC, microSD and NVMe.

---

## 7. Crash tiers A and B (automated)

Both run under `crash`.

**Tier A — crash consistency.** The harness re-execs itself as a child that
appends in a loop, `SIGKILL`s it mid-append, reopens the database and verifies
the hash chain. A real child process, not a simulated one: you cannot kill a
thread in a way that leaves SQLite in the state a crash leaves it in, and
simulating the crash inside the process under test means testing the simulation.

**Tier B — prefix validity.** A durable prefix is checkpointed into the main
database file, more events are appended into an un-checkpointed WAL, and the
main file *alone* is copied and reopened. Recovery must yield exactly the
durable prefix: intact, shorter, never torn or forked.

Tier B is deliberately **conservative** — it discards the whole WAL, which is
more than a real power cut takes (under `synchronous=FULL` the WAL is fsynced
per commit and would survive). Passing it is a stronger result than power loss
requires.

`INCONCLUSIVE` is a distinct outcome from `PASS` and must be treated as one. It
means the experiment never established its precondition — the child was killed
before committing anything, or the tail was checkpointed away so nothing was at
risk. An inconclusive run silently counted as a pass is how a drill becomes
decoration.

---

## 8. Tier C — the power cut (manual, and the only real durability test)

**Nothing in software can distinguish a filesystem that honoured `fsync` from
one that acknowledged it and buffered the write in a device cache.** Lying write
caches are common on embedded storage, especially microSD. `SIGKILL` leaves the
page cache intact, so tier A proves nothing about durability. The only
instrument is a power switch.

The harness always reports tier C as `NOT-RUN` with that reason. It cannot be
made to report anything else, so a results file can never imply this happened.

### Procedure

1. Boot the Orin normally, on the storage under test.
2. Append a known, counted prefix at the durability setting you intend to ship:
   ```sh
   ./wm2-persistence-harness append \
       --db /var/lib/kirra/wm2/power.sqlite \
       --durability full --events 50000 --batch 1 --assert-target
   ```
3. Record the committed count:
   ```sh
   sqlite3 /var/lib/kirra/wm2/power.sqlite 'SELECT COUNT(*), MAX(generation) FROM world_events;'
   ```
4. Start a second append run and, **while it is running**, cut power at the
   source — pull the barrel jack or kill the bench supply. Do **not** use
   `poweroff`, `reboot`, or a long-press: each of those flushes.
5. Restore power, boot, and re-verify:
   ```sh
   ./wm2-persistence-harness crash --db /var/lib/kirra/wm2/power.sqlite --assert-target
   sqlite3 /var/lib/kirra/wm2/power.sqlite 'PRAGMA integrity_check;'
   ```
6. **Pass** = the chain verifies intact, the count is ≥ the step-3 count, and
   `integrity_check` reports `ok`. **Fail** = a broken chain, a count *below*
   the committed prefix, or any corruption. A count below step 3 at
   `synchronous=FULL` means the storage is lying about `fsync` — that is a
   hardware finding, and it invalidates the durability half of every other
   result on that medium.
7. Repeat **at least five times**. One power cut that happens to land between
   commits proves nothing.
8. Record the outcome in the ADR conversation with the medium named
   (`db_fs_source` from the results file). Durability is a property of *that
   medium*, and it does not transfer to a different one.

---

## 9. Scale sweep — the reopening condition

ADR-0041's assumptions are provisional: hundreds-to-low-thousands of entities,
thousands-to-millions of observations. It states its own reopening condition —
*"if measurement shows entities in the millions or genuinely unbounded ad-hoc
traversal, Option B becomes materially stronger and this ADR should be
reopened."*

Sweep past the assumption rather than confirming it:

```sh
for e in 1000 10000 100000; do
  ./target/release/wm2-persistence-harness query \
      --db /var/lib/kirra/wm2/scale.sqlite \
      --out ~/wm2-scale.jsonl \
      --events 1000000 --entities "$e" --assert-target
done
```

Then look at how `graph_bounded_reach` and `temporal_changes_since` grow against
entity count. Sub-linear or gently linear supports Option A. A knee supports
reopening in favour of Option B.

The sweep informs the scale question; it does not settle it. What entity counts
the deployed robot actually reaches is an operational fact this harness cannot
produce.

---

## 10. Recording the result

Attach the JSONL file to the ADR-0041 ratification conversation, with:

- the `evidence_status` line, quoted verbatim;
- the device and medium (`device_model`, `db_fs_source`);
- the tier C outcome and how many power cuts were performed, or an explicit
  statement that tier C was not run;
- the `standin_schema_digest`, so a later reader can tell whether the schema
  measured is the schema ratified.

**Do not tick a box in ADR-0041 from a `HOST-INDICATIVE-NOT-TARGET` run**, and
do not tick the corruption/restart box from tiers A and B alone. Say what was
measured and what was not — a partially satisfied gate recorded honestly is
worth more than a fully ticked one that nobody can reconstruct.

Whatever comes out of this, ADR-0041 stays **Proposed** until the deciders named
in its header accept it. This drill produces evidence; it does not ratify
anything.

---

## Cross-references

- [`docs/adr/0041-world-model-persistence-architecture.md`](../adr/0041-world-model-persistence-architecture.md) — the proposal and its gates
- [`docs/adr/0039-world-model-bidirectional-governor-fence.md`](../adr/0039-world-model-bidirectional-governor-fence.md) — Fence A, which covers this harness
- [`docs/design/WORLD_MODEL_ARCHITECTURE.md`](../design/WORLD_MODEL_ARCHITECTURE.md) §11–13 — the time model, retention, and the persistence recommendation
- [`tests/audit_chain_prefix_on_kill.rs`](../../tests/audit_chain_prefix_on_kill.rs) — the existing crash-consistency drill whose two-tier structure this follows
- [`docs/hardware/TARGET_PLATFORM_MATRIX.md`](TARGET_PLATFORM_MATRIX.md) — why a Jetson is the doer, never the governor's cert target
- [`tools/qnx-rtm-harness/QNX_MAPPING.md`](../../tools/qnx-rtm-harness/QNX_MAPPING.md) — the `TBD-QNX-TARGET` convention this harness's evidence status is modelled on
