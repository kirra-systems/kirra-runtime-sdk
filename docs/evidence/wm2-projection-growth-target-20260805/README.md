# WM-2 — D-21 on target: the with-projections figure, confirmed byte-for-byte

| | |
|---|---|
| **Captured** | 2026-08-05, Jetson Orin NX (`yahboom`), aarch64, ext4 on `/dev/nvme0n1p1` |
| **Evidence status** | `TARGET-MEASURED` (aarch64) — see *Admissibility* for the one caveat |
| **Repo commit** | `12c5c0fd` (#1354) |
| **Ratified schema digest** | `502b5460331d842b8363d89c81856e2333bb54060091526f6d98a932ece66203` |
| **SQLite** | 3.45.0 (bundled) |
| **Supersedes** | `docs/evidence/wm2-projection-growth-20260805/` (host) |
| **Confirms** | ADR-0041 **D-21** |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

## What this settles

The host bundle recorded a target re-run as owed. This is that run, and it
**confirms rather than revises** — every figure reproduces to the byte.

| Arm | host (x86_64) | Jetson (aarch64) | Δ |
|---|---:|---:|---:|
| `lean` `with_projections_bytes` | 58 277 888 | 58 277 888 | **0** |
| `populated` `with_projections_bytes` | 62 963 712 | 62 963 712 | **0** |
| `projected_rows` | 4 886 | 4 886 | **0** |

So the ratified figures, now target-confirmed:

| Arm | log-only | with projections | Days to fill 8 GiB @ 10 Hz |
|---|---:|---:|---:|
| `lean` | 566.23104 | **582.77888** | 17.06 |
| `populated` | 611.86048 | **629.63712** | **15.79** |

The log-only arms also reproduced D-20 exactly — the **fourth** independent
reproduction of those figures, across two architectures.

## What it does to OQ2 — unchanged, and now target-backed

D-21 corrected the OQ2 ruling's headroom from a log-only 14.0 % to a
with-projections **11.5 %**, on host figures. Those figures are now target
figures, so the correction stands without the "host-measured" qualifier:

| Basis | Budget | Headroom vs the 12 078 720 allocation |
|---|---:|---:|
| log-only (what the ruling first used) | 14 039 041 | 1 960 321 (14.0 %) |
| with projections (correct) | **13 642 675** | 1 563 955 (**11.5 %**) |

**The ruling holds.** Nothing here reopens it.

## Why the numbers are identical across architectures

Expected, and now measured for a third quantity. `bytes_per_event` is the
*logical* length of a SQLite database, determined by the SQLite build, page
size, schema and data — none of which differ between the two machines. D-20
established this for the log-only figures; this extends it to the
with-projections figures and the projection's row count.

It says nothing about **timing**. `fold_elapsed_s` is 0.76 / 0.79 s here against
0.69 / 0.73 s on host — the Jetson is slower, as expected, and that is exactly
the axis where D-15 shows platform matters. Both are run cost, not performance
claims.

## Admissibility

Target hardware, ratified schema, and the instrument's own guard satisfied (a
same-host control was supplied, so the paired ratios are real ratios).

**One honest caveat**, unchanged from D-20's bundle and still owed:
`wm2-schema-growth` has **no `--assert-target` of its own**. Its records carry
`arch`/`os` and nothing more, so `TARGET-MEASURED` above is an operator
assertion in this README rather than a classification the instrument made and
could refuse to make. The harness can refuse; this tool cannot. That remains the
follow-up, and it is why this bundle says `TARGET-MEASURED` rather than
borrowing the harness's `JETSON-TARGET-MEASURED` token — the two are not
attested the same way and should not read as though they are.

## Files

| File | What it is |
|---|---|
| `results.jsonl` | 4 records: two `growth` arms, two `paired_ratio` |
| `SHA256SUMS` | Digests of the above |

Reproduce on target:

```
cd tools/wm2-schema-growth && cargo run --release -- \
  --events 100000 --entities 1000 --payload-bytes 96 --seed 20260803 \
  --standin-bpe 458.50624 --db /tmp/wm2-growth.sqlite
```

## What is NOT established

Unchanged from the host bundle, and none of it is affected by moving to target:

- **One projection.** `world_current` is the only materialized view; a
  multi-projection store costs more, so ~3 % is a floor.
- **Entity-bounded by construction.** 4 886 rows reflects the stream's 1 000
  entities. A workload with an unbounded subject space would grow the projection
  toward the log's own size and ~3 % would not survive. That regime is
  unmeasured.
- **No rebuild-cost, query-latency, or compaction-interaction claim.**
- **No candidate projection.** Candidates are excluded from the fold by design
  and contribute nothing to this figure.
- **Nothing about retention.** #1354 landed compaction after this run's schema
  was fixed; a compacted store's growth profile is a separate measurement.
