# Jetson WM-2 migration ladder — legacy vs grouped backfill — 2026-08-03

Device-produced target evidence comparing the legacy correlated projection
backfill with the grouped single-pass backfill, on the corrected harness.

The JSONL files are copied byte-for-byte from R2. **No result value in this
README was transcribed from a report — every figure below is recomputed from
the archived records** by the script in *Reproduce the analysis*.

## Target and provenance

| | |
|---|---|
| `evidence_status` | **`JETSON-TARGET-MEASURED`** on all 24 records (`citable: true`, `blockers: []`) |
| Device | NVIDIA Jetson Orin NX Engineering Reference Developer Kit Super |
| Kernel / arch | `5.15.148-tegra`, `aarch64`, L4T R36.4.3 |
| Storage | `ext4` on `/dev/nvme0n1p1` (NVMe), 75 % used |
| Build | release, `rustc 1.94.1` |
| Harness commit | `29aa1b2496e9` |
| `source_digest` | `ec580e2c6aac12f54da2586d1aab8aba5d93ec279fa2ac942379446d94f9506e` |
| `standin_schema_digest` | `630eb690aaef3df32690c39c283f6c3b30c60ac04264a84d95051cfaf29c3292` |

**`source_digest` was verified independently:** building the harness from
`29aa1b24` reproduces `ec580e2c…` exactly, so these runs used the merged
instruments from the recorded commit. Every record in both files carries one
commit, one source digest and one schema digest — a field that disagreed between
records would surface as a conflict rather than a single row above. None did.

> The `standin_schema_digest` is load-bearing. These numbers describe the
> harness's **stand-in** schema, not a ratified one. It matches the tier C and
> scale-sweep bundles, so all three describe the same stand-in.

## Method — two axes, each run twice

| Axis | Held fixed | Varied |
|---|---|---|
| **A** | entities = 1 000 | events ∈ {5 000, 10 000, 20 000, 30 000, 40 000, 50 000} |
| **B** | events = 30 000 | entities ∈ {100, 200, 400, 800, 1 600, 3 200} |

Each axis was run under `--migration-sql legacy` and `--migration-sql grouped`,
with `--assert-target`. The harness times `migrate_to_v2_using()` alone — the
`ALTER TABLE` plus the backfill `UPDATE` — not the store setup preceding it.

### Rung identity comes from record order, and why

**The harness version used here did not emit `entities` on `migrate` records.**
The first six migrate records in each file are Axis A; the final six are Axis B,
in the order listed above. `RUN_PARAMETERS.txt` is therefore *required* to label
the rungs, and the analysis script asserts each record's `events` field against
the expected rung as a consistency check (it agrees for all 24).

This is a weakness of the record format, not of the run, and it is fixed
forward: `migrate` records now carry `entities` alongside `events`, so later
bundles are self-describing and need no order reconstruction. Bundles produced
before that fix — this one — still need it.

## Result

### Axis A — entities fixed at 1 000, events vary

| Events | legacy | µs/event | grouped | µs/event |
|---:|---:|---:|---:|---:|
| 5 000 | 905.82 ms | 181.16 | 4.82 ms | 0.96 |
| 10 000 | 3 085.04 ms | 308.50 | 7.87 ms | 0.79 |
| 20 000 | 6 284.69 ms | 314.23 | 14.30 ms | 0.71 |
| 30 000 | 9 732.50 ms | 324.42 | 20.68 ms | 0.69 |
| 40 000 | 12 653.50 ms | 316.34 | 27.89 ms | 0.70 |
| 50 000 | **16 003.47 ms** | 320.07 | **33.90 ms** | 0.68 |

### Axis B — events fixed at 30 000, entities vary

| Entities | legacy | ms/entity | grouped | ms/entity |
|---:|---:|---:|---:|---:|
| 100 | 936.19 ms | 9.36 | 18.87 ms | 0.189 |
| 200 | 1 877.95 ms | 9.39 | 20.35 ms | 0.102 |
| 400 | 3 731.29 ms | 9.33 | 19.38 ms | 0.048 |
| 800 | 7 723.33 ms | 9.65 | 20.11 ms | 0.025 |
| 1 600 | 14 985.06 ms | 9.37 | 23.36 ms | 0.015 |
| 3 200 | **29 673.66 ms** | 9.27 | **25.06 ms** | 0.008 |

**Over a 32× increase in entity count at a fixed log size, legacy grows 31.70×
and grouped grows 1.33×.** That single comparison is the finding.

### Fitted models

| | Model | R² |
|---|---|---:|
| legacy, axis A (excl. the 5 000 rung, see below) | `−109.9 ms + 322.1 µs × events` | 0.99952 |
| legacy, axis B | `90.2 ms + 9.268 ms × entities` | 0.99988 |
| **grouped, axis A** | **`1.39 ms + 0.652 µs × events`** | **0.99953** |
| **grouped, axis B** | **`19.12 ms + 1.97 µs × entities`** | 0.90604 |

Legacy is linear in *each* axis independently — the signature of a cost
proportional to their product. Grouped is linear in events with a ~1.4 ms fixed
cost, and its entity term is **4 700× smaller per entity** than legacy's
(1.97 µs vs 9.268 ms). The lower R² on grouped/axis B is expected and not a
concern: the entity term there contributes ~6 ms across the whole sweep against
a ~19 ms floor, so the fit is dominated by measurement noise rather than by a
missing term.

### Measured speedups

| Configuration | legacy | grouped | speedup |
|---|---:|---:|---:|
| 50 000 events, 1 000 entities | 16 003 ms | 33.9 ms | **472×** |
| 30 000 events, 3 200 entities | 29 674 ms | 25.1 ms | **1 184×** |

The speedup grows with entity count, exactly as a product-vs-sum cost predicts.

### The run reproduces ADR-0041 D-6

D-6 archived **16 240 ms** for a v1→v2 migration at 50 000 events / 1 000
entities. The legacy arm of this ladder, on a later commit and a fresh database,
reads **16 003 ms** — **1.46 % apart.**

That matters twice over: it independently corroborates the original target
measurement, and it confirms the legacy statement here is the *same* statement
D-6 measured, so the comparison against grouped is like-for-like rather than a
comparison with something that drifted.

## What this establishes

**The 101-minute figure described a quadratic SQL defect, not an inherent
migration cost.** Both arms produce the same projection and both leave the chain
intact (`chain_intact_after: true`, `future_schema_refused: true` on all 24
records). The only difference is the query plan.

This is target evidence for R3 of the drafted open-question-8 resolution — that
a migration statement must be O(events) and never O(events × entities) — and it
removes the premise the offline-window route rested on.

## Limitations

- **The 5 000-event legacy rung is a cold-start outlier.** At 181 µs/event
  against ~320 for every other rung, it is roughly half-cost — first run of the
  series, cold page cache. It is *excluded* from the legacy axis-A fit and shown
  in the table anyway rather than dropped. It does not affect the axis B result
  or either speedup.
- **Single sample per configuration.** One migration per rung, no repetition, so
  nothing here bounds variance. The `stall` investigation (D-10) found rare
  multi-second outliers on this device at 2.5 %; a single sample cannot exclude
  one having landed in a rung.
- **Stand-in schema.** Every figure describes the harness's stand-in schema and
  its one `entities_projection` backfill. Another migration with a different
  backfill has a different cost function — the general finding is that the cost
  function must be derived per migration, not that every rewrite is 1 000×.
- **One device, one medium.** A single Jetson Orin NX on one NVMe at 75 % full.
- **Rung labels depend on `RUN_PARAMETERS.txt`**, per *Rung identity* above.
- **The extrapolation below is not planning evidence.**

### Extrapolation, clearly labelled

Carrying the fitted models to a full 8 GiB store at D-8's constant density
(18 734 608 events across ~187 346 entities):

| | Extrapolated |
|---|---:|
| legacy (product model, k ≈ 3.15 × 10⁻⁴ ms) | **~12.8 days** |
| grouped (`a + b_ev·events + b_en·entities`) | **~12.6 s** |

**This runs ~374× beyond the largest rung measured**, which is precisely the
overreach D-6 was corrected for. It is offered as an order of magnitude to show
the two routes are not on the same scale, **not as a number to plan against.**
The citable facts are the measured 472× and 1 184×.

## Files

| File | |
|---|---|
| `legacy.jsonl` | 12 `run` + 12 `migrate`, `--migration-sql legacy` |
| `grouped.jsonl` | 12 `run` + 12 `migrate`, `--migration-sql grouped` |
| `RUN_PARAMETERS.txt` | rung definitions and the record-order convention |
| `ENVIRONMENT.txt` | kernel, L4T release, device model, mount, disk usage |
| `GIT_COMMIT`, `GIT_STATUS` | tree state on the device at run time |
| `SHA256SUMS` | relative digests over every file above |

```sh
sha256sum -c SHA256SUMS
```

`GIT_STATUS` shows the result directory as untracked (`??`) — expected, since
the files were produced into the working tree before being committed here. No
tracked source file was modified, so `GIT_COMMIT` describes the code that ran.

### Reproduce the analysis

Every table above is recomputed from the records:

```sh
python3 - <<'EOF'
import json
AE=[5000,10000,20000,30000,40000,50000]; BE=[100,200,400,800,1600,3200]
def load(f):
    m=[r for r in (json.loads(l) for l in open(f)) if r.get('record')=='migrate']
    assert len(m)==12
    for i,e in enumerate(AE): assert m[i]['events']==e        # order/consistency check
    for i in range(6):        assert m[6+i]['events']==30000
    return ([(AE[i],1000,m[i]['timing']['total_ms']) for i in range(6)],
            [(30000,BE[i],m[6+i]['timing']['total_ms']) for i in range(6)])
for f in ("legacy.jsonl","grouped.jsonl"):
    A,B=load(f); print(f)
    for e,n,ms in A: print(f"  A events={e:>6} ms={ms:>10.2f}")
    for e,n,ms in B: print(f"  B entities={n:>5} ms={ms:>10.2f}")
    print(f"  axis B growth over 32x entities: {B[-1][2]/B[0][2]:.2f}x")
EOF
```

### Reproduce the run

```sh
for e in 5000 10000 20000 30000 40000 50000; do
  wm2-persistence-harness migrate --db <db> --out <out> \
      --events $e --entities 1000 --migration-sql <legacy|grouped> --assert-target
done
for n in 100 200 400 800 1600 3200; do
  wm2-persistence-harness migrate --db <db> --out <out> \
      --events 30000 --entities $n --migration-sql <legacy|grouped> --assert-target
done
```

## Status

These measurements **support the migration resolution but did not, by
themselves, ratify ADR-0041** — evidence and acceptance are separate acts.

The resolution (R1–R5) was adopted and ADR-0041 was accepted on **2026-08-04**,
after this bundle was produced. **The alongside-rebuild and atomic-cutover spike
remains open**, carried as an outstanding obligation of that acceptance: if it
shows the protocol impractical, open question 8 reopens. See the ADR's
*Acceptance record*.
