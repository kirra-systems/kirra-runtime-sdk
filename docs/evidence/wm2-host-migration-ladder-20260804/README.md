# WM-2 migration ladder — HOST-INDICATIVE, NOT TARGET — 2026-08-04

> ## ⚠️ This bundle may not be cited against any ADR-0041 gate.
>
> Every record reads `evidence_status: HOST-INDICATIVE-NOT-TARGET`, `citable:
> false`, on `x86_64` with no Tegra evidence. ADR-0041's checklist says plainly:
> *"no gate below is satisfied, and none may be ticked from a
> `HOST-INDICATIVE-NOT-TARGET` run."* Nothing here changes a gate.
>
> What it establishes is a **structural** claim — the *shape* of the migration
> cost function and the query plan that produces it. Shape is a property of the
> algorithm and the SQLite planner, so it carries across architectures in a way
> the constants do not. Every absolute figure below is host-only and must be
> re-measured on the Jetson before it informs anything.

## Why this was run

ADR-0041 D-6 measured a v1→v2 migration at **16.24 s for 50 000 events** on
target and extrapolated **linearly in events** to `18.7 M events → 101 min`. The
extrapolation holds the entity count fixed at the measured 1 000.

D-8 had already established, for the scale sweep, that holding one axis fixed
while growing the other measures the wrong thing — the deployment-realistic
shape is **constant density** (observations per entity held constant, so
entities and events grow together). This ladder asks whether the migration
extrapolation has the same defect.

It does, and worse than expected.

## Method

Two axes, using only existing harness flags (`migrate` caps `--events` at
50 000, so both axes stay inside it):

- **Axis A** — `--entities 1000` fixed, `--events` ∈ {6 250, 12 500, 25 000, 50 000}
- **Axis B** — `--events 50000` fixed, `--entities` ∈ {125, 250, 500, 1 000, 2 000, 4 000}

The harness times `migrate_to_v2()` alone — the `ALTER TABLE` plus the backfill
`UPDATE` — not the store setup that precedes it.

## Result — cost is the PRODUCT of events and entities

### Axis A — entities fixed, events vary

| Events | Migrate | µs/event |
|---:|---:|---:|
| 6 250 | 1 249 ms | 199.9 |
| 12 500 | 2 512 ms | 201.0 |
| 25 000 | 5 549 ms | 222.0 |
| 50 000 | 10 755 ms | 215.1 |

Linear in events — consistent with D-6's assumption, *as long as entities hold still*.

### Axis B — events fixed, entities vary

| Entities | Migrate | ms/entity |
|---:|---:|---:|
| 125 | 1 488 ms | 11.90 |
| 250 | 2 957 ms | 11.83 |
| 500 | 5 784 ms | 11.57 |
| 1 000 | 11 729 ms | 11.73 |
| 2 000 | 23 590 ms | 11.80 |
| 4 000 | 48 534 ms | 12.13 |

**Also linear — in entities, at a fixed event count.** Doubling the entity count
doubles the migration time while the log stays exactly the same size.

### Both together

`k = ms / (events × entities)` is flat to within ±10 % across a 32× spread in
entities and an 8× spread in events:

| Events | Entities | k (×10⁻⁴ ms) |
|---:|---:|---:|
| 6 250 | 1 000 | 2.00 |
| 12 500 | 1 000 | 2.01 |
| 25 000 | 1 000 | 2.22 |
| 50 000 | 1 000 | 2.15 |
| 50 000 | 125 | 2.38 |
| 50 000 | 250 | 2.37 |
| 50 000 | 500 | 2.31 |
| 50 000 | 1 000 | 2.35 |
| 50 000 | 2 000 | 2.36 |
| 50 000 | 4 000 | 2.43 |

**`migration_time ≈ k · events · entities`.** Not linear in the store's size —
quadratic in it, when both axes grow together.

## Why: the query plan

`SCHEMA_V2_STEP` backfills with a correlated subquery. SQLite's plan:

```
SCAN entities_projection USING COVERING INDEX sqlite_autoindex_entities_projection_1
CORRELATED SCALAR SUBQUERY 1
  SEARCH world_events USING INDEX idx_events_kind (kind=?)
```

The planner picks `idx_events_kind` and keys on `kind='observation'` alone — so
for **every** projection row it walks **every** observation event. The
`idx_events_subject_valid (subject, valid_from_ms)` index that would make this a
per-entity lookup is not chosen.

The same aggregate as a single grouped pass, on the same database:

| | Time | Rows |
|---|---:|---:|
| correlated subquery (what the migration runs) | **93 132 ms** | 7 955 |
| `SELECT subject, COUNT(*) … GROUP BY subject` | **30 ms** | 7 955 |

**3 100×**, same data, same answer. (The grouped figure times the read; the
`UPDATE` would add a write per projection row, which is O(entities) and small
beside either number.)

## What this implies for the 101-minute figure

Under constant density — D-8's own rule, at the 100 events/entity the sweep used
— a full 8 GiB store is 18 734 608 events across ~187 346 entities, not 1 000.
Carrying the host `k` forward:

| Extrapolation | Result |
|---|---|
| D-6 as written — linear in events, entities pinned at 1 000 | 101 min |
| Product model, constant density, **host k** | **~9 days** |

Roughly **130× worse**, and it is the model that is wrong, not the 16.24 s
measurement. The Jetson was ~1.5× slower than this host at the shared
configuration, so a target figure would be worse still.

**But the number is a property of the migration statement, not of the store.**
Rewritten as a grouped pass the same migration is O(events) and finishes in
seconds. Both figures are real; they describe different SQL, not different
databases.

## Limitations

- **Host, not target.** `x86_64`, `ext4` on `/dev/vda`, no Tegra. The
  constants do not transfer; the shape and the query plan do. A target run is
  required before any figure here is quoted as a WM-2 number.
- **One migration, one statement.** This is `SCHEMA_V2_STEP`. Another migration
  with a different backfill has a different cost function. The general finding
  is that the cost function must be derived per migration — not that every
  migration is quadratic.
- **k is not perfectly flat.** It drifts ~2.00 → ~2.43 across the ladder (~20 %).
  The product model is a good fit, not an exact law; some per-row and cache
  effects are folded in.
- **Extrapolated 374× beyond the largest rung measured.** The 50 000-event cap
  on `migrate` is a harness limit. The ~9 days follows the fitted model out well
  past the data, exactly the kind of step this bundle criticises D-6 for — it is
  offered as an order of magnitude to motivate re-measurement, not as a figure
  to plan against.
- **Single sample per configuration.** The harness times one migration per run;
  there is no repetition, so nothing here bounds variance.

## Files

| File | |
|---|---|
| `ladder.jsonl` | 11 `run` + 11 `migrate` records — the raw ladder |
| `SHA256SUMS` | relative digests |

```sh
sha256sum -c SHA256SUMS
```

Reproduce (any host; expect different constants, same shape):

```sh
for e in 6250 12500 25000 50000; do
  wm2-persistence-harness migrate --db /tmp/a.sqlite --out ladder.jsonl \
      --events $e --entities 1000
done
for n in 125 250 500 1000 2000 4000; do
  wm2-persistence-harness migrate --db /tmp/b.sqlite --out ladder.jsonl \
      --events 50000 --entities $n
done
```
