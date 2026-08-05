# WM-2 — the with-projections figure, and what it does to the OQ2 ruling

| | |
|---|---|
| **Captured** | 2026-08-05, x86_64 build host, ext4 |
| **Evidence status** | `HOST-MEASURED` — see *Admissibility* |
| **Ratified schema digest** | `502b5460331d842b8363d89c81856e2333bb54060091526f6d98a932ece66203` |
| **Written up as** | ADR-0041 **D-21** |
| **Bears on** | ADR-0041 **OQ2** (the 2026-08-05 re-ruling), **D-20** |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

## What this closes

Both D-20 and the OQ2 re-ruling carried the same caveat, in the same words:
their budget was **log-only**, compared against an original (D-2) taken **with
projections**. `kirra-world-store` had no read path, so the ratified
with-projections figure was *unmeasurable* — only bounded below. Both documents
recorded themselves as optimistic by an unknown amount.

The read path now exists, so the number exists.

| Arm | log-only | with projections | Δ |
|---|---:|---:|---:|
| `lean` | 566.23104 | **582.77888** | +16.54784 (+2.92 %) |
| `populated` | 611.86048 | **629.63712** | +17.77664 (+2.91 %) |

Days to fill 8 GiB at 10 Hz, `populated`: **16.25 → 15.79**.

The projection holds **4 886 rows** for 100 000 events — the fold is keyed on
`(subject, predicate)`, and the generated stream has 1 000 entities across a
small predicate set, so the projection is bounded by the entity count rather
than by the log length. That is the property that makes it affordable at all,
and it is why the overhead is ~3 % rather than ~100 %.

## What it does to the OQ2 ruling

The ruling allocated 8 294 400 events to `raw` (30 days at ≤ 3.20 /s) and
3 784 320 to the protected classes, and stated **14 % headroom**.

| Basis | Budget | Headroom vs the 12 078 720 allocation |
|---|---:|---:|
| log-only (what the ruling used) | 14 039 041 | 1 960 321 (**14.0 %**) |
| with projections (correct) | **13 642 675** | 1 563 955 (**11.5 %**) |

**The ruling holds.** The allocation still fits, with real margin. But its
stated headroom was **14 %, and the true figure is 11.5 %** — the optimism it
named is now quantified at 2.82 % of budget, and the ADR's table has been
corrected rather than left reading a number known to be wrong.

Restoring a full 14 % headroom under the honest budget would need **3.07 /s**
rather than 3.20 /s — ~3.3× coalescing instead of ~3.1×. That is a further
tightening of the same lever, not a different decision, and **it is not made
here**: the ruling's own reopening condition said to re-derive when this figure
landed, and re-deriving is the owner's call.

## Method

Identical to D-20's, with one arm added **strictly afterwards** so it cannot
disturb the first:

1. Append 100 000 events (seed `20260803`, 1 000 entities, 96-byte payload).
2. Close the writer, `wal_checkpoint(TRUNCATE)`, measure → `log_only_bytes`.
3. Reopen, `fold()`, close, checkpoint, measure → `with_projections_bytes`.

Step 3 can only run after step 2 because `kirra-world-store` installs the
projection tables **lazily, in the fold**. A store that has never folded holds
no projection tables at all, which is what keeps the log-only figure comparable
to D-2's and is asserted by `open_leaves_no_projection_tables`.

The instrument refuses a run where `with_projections_bytes < log_only_bytes` —
a fold cannot shrink the database, so that ordering would mean one of the two
measurements is wrong rather than that projections are free.

**Counting unit / independence unit / held fixed:** unchanged from D-20. One
database build per arm; no per-event variance claimed; platform, SQLite build
and event stream held fixed; schema and fill varied.

## Admissibility

**Host run.** D-20 established, by measurement, that this quantity is
platform-invariant: the ratified arms reproduced byte-for-byte on aarch64 and
x86_64. That result licenses reading these numbers as the target numbers **for
this quantity specifically** — but it is an established result about bytes, not
a general licence, and a target re-run is cheap (~2 minutes) and should be done
before this figure is entered against the ratification checklist.

`fold_elapsed_s` (0.69 / 0.73 s) is recorded as run cost and is **not** a
performance claim: it is one fold of 100 000 events on a build host, and
ADR-0041's rebuild-cost work (D-16, R2) is where that question is actually
asked.

## What is NOT established

- **One projection.** `world_current` is the only materialized view. ADR-0041
  contemplates others; each would add its own cost, so this figure is a floor
  for a multi-projection store, not a total.
- **Projection size is entity-bounded here by construction.** 4 886 rows for
  100 000 events reflects the generated stream's 1 000 entities. A workload
  with a large or unbounded subject space would grow the projection toward the
  log's own size, and the ~3 % figure would not survive it. Nothing here
  measures that regime.
- **No rebuild-cost claim**, no query-latency claim, nothing about compaction
  interacting with projections.
- **No candidate projection.** Candidates are deliberately excluded from the
  fold and are read straight from the log, so they contribute nothing to this
  figure.
