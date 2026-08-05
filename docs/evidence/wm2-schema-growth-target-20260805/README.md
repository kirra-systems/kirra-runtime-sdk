# WM-2 — bytes/event on TARGET, and a −32 768 B correction to the host set

| | |
|---|---|
| **Captured** | 2026-08-05, Jetson Orin NX (`yahboom`), aarch64, ext4 on `/dev/nvme0n1p1` |
| **Evidence status** | `JETSON-TARGET-MEASURED` — the harness arm reports `citable:true`, `blockers:[]` |
| **Repo commit** | `4736cb5c` (#1351) |
| **Ratified schema digest** | `502b5460331d842b8363d89c81856e2333bb54060091526f6d98a932ece66203` |
| **Stand-in schema digest** | `630eb690aaef3df32690c39c283f6c3b30c60ac04264a84d95051cfaf29c3292` |
| **SQLite** | 3.45.0 (bundled), identical in both arms and on both machines |
| **Supersedes** | `docs/evidence/wm2-schema-growth-20260805/` (host, pre-fix — see §Correction) |
| **Written up as** | ADR-0041 **D-20** (revised) |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

## What this run settles

The host bundle owed a target run before any figure could be entered against
ADR-0041's ratification checklist. This is that run. It settles two things and
corrects a third.

| Arm | Schema | B/event | `page_count` | Days to fill 8 GiB @ 10 Hz | Ratio |
|---|---|---:|---:|---:|---:|
| control | stand-in (D-2's) | **458.50624** | — | 21.68 | 1.000× |
| ratified, `lean` | ratified | **566.23104** | 13 824 | 17.56 | **1.2349×** |
| ratified, `populated` | ratified | **611.86048** | 14 938 | 16.25 | **1.3345×** |

### 1. The control arm reproduced D-2 exactly, again

`log_only_bytes` **45 850 624**, `bytes_per_event` **458.50624** — byte-identical
to D-2 (2026-08-03) and to the host control. Note the harness's `source_digest`
has changed since D-2 (`a0c2c1c8…` → `8882f659…`) while its
`standin_schema_digest` has **not** (`630eb690…` both). The harness source moved;
its stand-in *schema* did not. Growth depends on the schema, which is why the
number is stable across a source change — and that is a check, not a
coincidence to wave at.

### 2. Platform invariance is now measured, not inferred

The host bundle argued platform-independence from a single control-arm identity
and was careful to call that "an empirical identity on one pair, **not** a proof
of platform invariance." It is now demonstrated on the ratified schema itself.
Re-running the *fixed* instrument on the x86_64 host gives:

| arm | Jetson (aarch64) | host (x86_64) | Δ |
|---|---:|---:|---:|
| lean `log_only_bytes` | 56 623 104 | 56 623 104 | **0** |
| populated `log_only_bytes` | 61 186 048 | 61 186 048 | **0** |

Byte-for-byte, both arms, two architectures. Expected — the quantity is the
logical length of a SQLite file, fixed by the SQLite build, page size, schema
and data — but expected is not measured, and now it is measured.

## Correction to the host bundle

**The host figures were 32 768 bytes high in each arm.** Corrected:

| Arm | Host bundle (published) | Correct | Δ |
|---|---:|---:|---:|
| `lean` | 566.55872 | **566.23104** | −0.32768 B/event (−0.058 %) |
| `populated` | 612.18816 | **611.86048** | −0.32768 B/event (−0.054 %) |

### Why, and why it is not a platform difference

`page_count` is **identical** across host and Jetson in both arms (13 824 and
14 938). The schema-attributable content never differed. Both deltas are exactly
32 768 bytes — the fixed size of a SQLite `-shm` file, which `db_bytes` counts
along with the main file and `-wal`.

The host bundle's run predates the `drop(store)` change made during review of
#1351. Before it, `checkpoint()` opened its second connection while the store
still held its own, so the `-shm` file still existed when `db_bytes` read the
directory and its 32 KiB was counted as if it were data. After the fix the last
connection closes before the measurement, SQLite removes `-shm`, and
`log_only_bytes` equals `page_count × page_size` **exactly** — 13 824 × 4 096 =
56 623 104 and 14 938 × 4 096 = 61 186 048.

Worth recording plainly: that change was made on a *predicted* failure
(`SQLITE_BUSY` under lock contention) which never actually occurred. Its real
effect was different and larger — it removed a 32 KiB non-data file from the
measured quantity. The prediction was wrong about the mechanism and right about
the fix.

### What the correction does and does not change

Nothing qualitative. It moves each figure by ~0.06 %:

| | Host bundle | Corrected |
|---|---:|---:|
| `lean` ratio | 1.235662× | 1.234947× |
| `populated` ratio | 1.335180× | 1.334465× |
| Budget, `lean` | 15 161 596 | 15 170 370 |
| Budget, `populated` | 14 031 527 | **14 039 041** |
| Headroom vs OQ2's 15 448 320 allocation | −1 416 793 (−10.1 %) | **−1 409 279 (−10.0 %)** |

**OQ2's allocation still does not close**, at both ends of the band, and still
by ~10 % at the end horizons must be taken from. Every lever keeps its value to
the precision it was quoted at: holding 30 days still needs **3.20 /s**
(~3.1× coalescing), holding ~4.5 /s still gives **21.3 days**, and restoring the
ruled allocation still needs **~9.9 GiB** (quoted as ~10.3 GiB from the
uncorrected figure — the one number that moves visibly).

The host bundle's `results.jsonl` is left **unedited**. It is a faithful record
of what that instrument emitted on that day, and rewriting it would destroy the
only evidence that the `-shm` inclusion ever happened. Its README carries a
pointer here.

## Method

Unchanged from the host bundle, and restated so this file stands alone.

**Counting unit:** bytes of on-disk database per appended event, where bytes is
`len(main) + len(-wal) + len(-shm)` after a `wal_checkpoint(TRUNCATE)` — the
harness's own `db_bytes`, reused verbatim so both arms measure the same thing.
As of the fix, `-wal` and `-shm` are both absent at measurement time, so this
now equals `page_count × page_size`.

**Independence unit:** one database build. Events within a build are not
independent (SQLite page fill depends on insertion order); **no per-event
variance is claimed**. The quantity is deterministic — reproduced bit-identically
across two architectures, which is a stronger statement than a variance estimate
would have been.

**Held fixed:** SQLite build, event stream (seed `20260803`, 100 000 events,
1 000 entities, 96-byte payload), log-only condition. **Varied:** the schema, the
fill of the added columns, and — between the two bundles — the platform.

**Supports:** the multiplicative change in log-only growth attributable to the
ratified schema, on target hardware. Nothing about latency, throughput,
durability or crash behaviour was measured. `append_elapsed_s` (39.65 / 40.00 s)
is recorded for run-cost only and is **not** a throughput result: it is 100 000
individual `synchronous=FULL` commits with no batching, on a machine whose stall
behaviour D-15/D-19 characterise separately.

## A provenance caveat on `git_commit`

The harness records `git_commit: 83998315016f` while the tree was at
`4736cb5c`. That field is stamped at **build** time, and cargo did not rebuild
the harness because its source had not changed. It is not wrong, but it must not
be read as "the repository state when this number was produced." Instrument
identity here rides on the content digests — `source_digest` and
`standin_schema_digest` — both of which match the host run exactly.

## Files

| File | What it is |
|---|---|
| `results.jsonl` | 6 records: harness `run` + `growth` control, two ratified `growth` arms, two `paired_ratio` |
| `SHA256SUMS` | Digests of the above |

Reproduce on target:

```
cd tools/wm2-persistence-harness && cargo run --release -- growth \
  --events 100000 --entities 1000 --payload-bytes 96 --seed 20260803 --assert-target

cd ../wm2-schema-growth && cargo run --release -- \
  --events 100000 --entities 1000 --payload-bytes 96 --seed 20260803 \
  --standin-bpe 458.50624 --db /tmp/wm2-growth.sqlite
```

`--db /tmp/...` is deliberate: the harness measures on `/tmp`, which its own
record shows is ext4 on `/dev/nvme0n1p1`, so both arms land on one filesystem.

## What is NOT established

- **No with-projections figure for the ratified schema.** Not measurable until
  projections exist. The budget comparison is therefore optimistic — it is a
  log-only figure against a budget that counted projections.
- **`wm2-schema-growth` had no `--assert-target` when this bundle was made.**
  Its records carried `arch`/`os` and nothing more, so the target status of this
  bundle is inherited from the paired harness arm, **by operator assertion in
  this README** — the instrument could not refuse to be cited as target evidence
  the way the harness can.

  **The follow-up has since been done** (2026-08-05): the tool now runs the
  harness's classifier — the *same module*, `#[path]`-included rather than
  copied, so the token cannot come to mean two things — and stamps
  `evidence_status` plus the corroborating facts into every record.

  **This does not retroactively attest this bundle.** The records below were
  produced by the earlier build and carry no classification; they remain
  operator-asserted, exactly as described above. A re-run on target would
  produce instrument-attested records, and that is what a future bundle should
  cite. Recorded rather than quietly upgraded, because "we fixed the tool" and
  "this measurement is attested" are different claims.
- **No claim about where in the band real traffic sits.** `lean` and `populated`
  bound a modelling choice; nothing measures the middle.
- **No deep-provenance case.** `populated` cites one upstream observation; SD-3
  permits many, and a derivation-heavy workload sits above this band.
- **One machine, one day.** Platform invariance is shown across two
  architectures for *this* quantity; it says nothing about any timing quantity,
  where the opposite is true.
